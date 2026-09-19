import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile, mkdtemp, rm } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createNovaBatchTools } from './nova-batch-tools.mjs';
import definition from './operator-tool.json' with { type: 'json' };

const envNames = ['NOVA_CONTEXT_SERVICE_ENDPOINT','NOVA_CONTEXT_SERVICE_TOKEN','NOVA_OPERATOR_SCOPE'];
async function withService(fn) {
  const dir = await mkdtemp(join(tmpdir(), 'nova-operator-test-'));
  const endpoint = process.platform === 'win32' ? `\\\\.\\pipe\\nova-operator-${Date.now()}` : join(dir, 'service.sock');
  const before = Object.fromEntries(envNames.map(k => [k, process.env[k]]));
  const calls=[];
  const server = createServer(socket => {
    let text=''; socket.setEncoding('utf8');
    socket.on('data', chunk => { text+=chunk; if (!text.includes('\n')) return;
      calls.push(JSON.parse(text.trim())); socket.end(`${JSON.stringify({ok:true,result:{taskId:'fixture',status:'completed',result:{value:42}}})}\n`);
    });
  });
  await new Promise((resolve,reject) => {server.once('error',reject);server.listen(endpoint,resolve);});
  process.env.NOVA_CONTEXT_SERVICE_ENDPOINT=endpoint;process.env.NOVA_CONTEXT_SERVICE_TOKEN='fixture-token';delete process.env.NOVA_OPERATOR_SCOPE;
  try {await fn(calls);} finally {
    for(const k of envNames) {if(before[k]===undefined)delete process.env[k];else process.env[k]=before[k];}
    await new Promise(done=>server.close(done));await rm(dir,{recursive:true,force:true});
  }
}

test('operate automatically uses a trusted session scope, not model arguments', async()=>{
  await withService(async calls=>{
    const tools=createNovaBatchTools(process.cwd(),{operatorScope:'parent-scope',fastContext:false});
    assert.ok(tools.operate);
    const result=JSON.parse(await tools.operate.execute({op:'run',goal:'Read',channel:'chrome',requestKey:'r',acceptance:['Read']}));
    assert.equal(result.result.value,42);assert.equal(calls.length,1);
    assert.equal(calls[0].operatorScope,'parent-scope');assert.equal(calls[0].root,resolve(process.cwd()));
    assert.equal(calls[0].params.model,undefined);assert.equal(calls[0].params.agent,undefined);
    assert.equal(result.images,undefined);assert.equal(result.details,undefined);
  });
});

test('no operator setting and no tool in read-only or unbound sessions',async()=>{
  await withService(async()=>{
    assert.equal(createNovaBatchTools(process.cwd(),{fastContext:false}).operate,undefined);
    assert.equal(createNovaBatchTools(process.cwd(),{readOnly:true,operatorScope:'parent'}).operate,undefined);
    assert.equal(definition.inputSchema.additionalProperties,false);
    for(const k of ['model','agent','apiKey','operatorScope','enabled'])assert.equal(definition.inputSchema.properties[k],undefined);
  });
});

test('MCP child inherits scope from its own environment; other client has a different owner',async()=>{
  await withService(async calls=>{
    process.env.NOVA_OPERATOR_SCOPE='A';const a=createNovaBatchTools(process.cwd());
    process.env.NOVA_OPERATOR_SCOPE='B';const b=createNovaBatchTools(process.cwd());
    await a.operate.execute({op:'status',taskId:'x'});await b.operate.execute({op:'status',taskId:'y'});
    assert.deepEqual(calls.map(c=>c.operatorScope),['A','B']);assert.notEqual(calls[0].owner,calls[1].owner);
  });
});

test('Reasonix algorithm and storage are byte-identical after removing explicit tool-routing additions',async()=>{
  const golden=JSON.parse(await readFile(new URL('./operator-reasonix-baseline.json',import.meta.url),'utf8'));
  for(const [file,expected] of Object.entries(golden)){
    let text=await readFile(new URL(`../${file}`,import.meta.url),'utf8');
    if(file.endsWith('.mjs')) {
      text=text.replaceAll(', operatorScope: request.operatorScope','').replaceAll(', operatorScope: request?.operatorScope','');
      text=text.replace('${request?.operatorScope ? `\\0${request.operatorScope}` : ""}','');
    }
    assert.equal(createHash('sha256').update(text).digest('hex'),expected,`${file}: changed outside the reviewed routing additions`);
  }
});

test('Cursor prewarm identity includes the parent scope',async()=>{
  for(const name of ['cursor-context-reasonix.mjs','cursor-context-super.mjs']){
    const text=await readFile(new URL(name,import.meta.url),'utf8');
    const body=text.match(/function agentFingerprint\(request\) \{([\s\S]*?)\n\}/)[1];
    const fingerprint=new Function('request',body);
    const shared={model:'m',cwd:'/same',mode:'build'};
    assert.notEqual(fingerprint({...shared,operatorScope:'A'}),fingerprint({...shared,operatorScope:'B'}));
    assert.equal(fingerprint(shared),'m\0/same\0agent');
  }
});

test('all native GUI entry points enforce Operator ownership without changing tool signatures',async()=>{
  for(const name of ['jianlai.rs','native_browser.rs']){
    const text=await readFile(new URL(`../src-tauri/src/${name}`,import.meta.url),'utf8');assert.match(text,/crate::operator::check_access\(owner\)\?/);
  }
});


test('Cursor worker uses inherited selection with no tools or parent history',async()=>{
  const {cursorDecision}=await import('./operator-cursor.mjs');let options,prompt,closed=0;
  const sdk={create:async value=>{options=value;return {send:async value=>{prompt=value;return {wait:async()=>({status:'completed',result:'{"kind":"blocked"}'})};},close:async()=>closed++};}};
  const request={model:'model-id::effort=high',cwd:'/isolated',system:'operator-system',context:{goal:'task'},images:[{data:'fixture',mimeType:'image/png'}]};
  assert.equal(await cursorDecision(request,sdk,id=>({id})), '{"kind":"blocked"}');
  assert.deepEqual(options.model,{id:request.model});assert.deepEqual(options.tools,[]);
  assert.deepEqual(options.mcpServers,{});assert.deepEqual(options.local.settingSources,[]);
  assert.equal(options.local.cwd,'/isolated');assert.deepEqual(prompt.images,request.images);
  assert.equal(prompt.text,JSON.stringify(request.context));assert.equal(closed,1);
});

test('Cursor worker rejects Auto and closes after inference errors',async()=>{
  const {cursorDecision}=await import('./operator-cursor.mjs');let created=0,closed=0;
  const sdk={create:async()=>{created++;return {send:async()=>{throw new Error('fixture failure');},close:async()=>closed++};}};
  await assert.rejects(cursorDecision({model:'__cursor_auto__'},sdk,id=>({id})),/resolved parent model/);assert.equal(created,0);
  await assert.rejects(cursorDecision({model:'same',context:{}},sdk,id=>({id})),/fixture failure/);assert.equal(closed,1);
});


test('ACP desktop guidance prefers delegation only when operate is present',async()=>{
  const source=await readFile(new URL('../src-tauri/src/acp.rs',import.meta.url),'utf8');
  const guidance=source.match(/fn direct_desktop_guidance\(\) -> &'static str \{([\s\S]*?)\n\}/)[1];
  assert.ok(guidance.includes('如果工具列表包含 operate'));
  assert.ok(guidance.includes('channel=jianlai'));
  assert.ok(guidance.includes('用户明确要求单步操作'));
  assert.ok(guidance.includes('needs_review 不得自动重放'));
});
