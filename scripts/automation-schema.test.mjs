// Verify all adapter registrations retain the new fields and automatic visual feedback.
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile,mkdtemp,writeFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {createNovaBatchTools} from './nova-batch-tools.mjs';
import {webviewMcpResult} from './webview-mcp-result.mjs';
import {Client} from '@modelcontextprotocol/sdk/client/index.js';
import {StdioClientTransport} from '@modelcontextprotocol/sdk/client/stdio.js';
const tool = async name=>JSON.parse(await readFile(new URL(`./${name}-tool.json`,import.meta.url),'utf8'));
test('embedded CodeBuddy MCP advertises the current browser schemas and instructions', {timeout:15000}, async()=>{
  const client=new Client({name:'nova-bundle-parity-test',version:'1.0'});
  const transport=new StdioClientTransport({command:process.execPath,
    args:[fileURLToPath(new URL('../src-tauri/resources/nova-tools-mcp.mjs',import.meta.url))],
    env:{...process.env,NOVA_CONTEXT_SERVICE_ENDPOINT:'unused-test-endpoint',NOVA_CONTEXT_SERVICE_TOKEN:'unused-test-token',NOVA_TOOLS_READ_ONLY:'0'}});
  try {
    await client.connect(transport);
    const listed=(await client.listTools()).tools;
    for(const name of ['chrome','webview']) {
      const actual=listed.find(t=>t.name===name),expected=await tool(name);
      assert(actual,`missing bundled ${name}`);
      assert.deepEqual(actual.inputSchema,expected.inputSchema,'Run npm run build:nova-tools-mcp before compiling Nova');
      assert.equal(actual.description,expected.description);
    }
  } finally {await client.close();}
});
test('Chrome and WebView share image coordinates, fast scope and bounded batch schemas',async()=>{
  const [chrome,webview]=await Promise.all(['chrome','webview'].map(tool));
  for(const name of ['action','imageId','scope','region','visual','maxEdge']) assert.deepEqual(chrome.inputSchema.properties[name],webview.inputSchema.properties[name]);
  assert.deepEqual(chrome.inputSchema.properties.actions.items,webview.inputSchema.properties.actions.items);
  assert.equal(chrome.inputSchema.properties.actions.maxItems,16);
  assert.equal(chrome.inputSchema.properties.incognito.type,"boolean");
  assert.equal(chrome.inputSchema.properties.actions.items.properties.duration_ms.maximum,1500);
  const saved={endpoint:process.env.NOVA_CONTEXT_SERVICE_ENDPOINT,token:process.env.NOVA_CONTEXT_SERVICE_TOKEN};
  process.env.NOVA_CONTEXT_SERVICE_ENDPOINT='test-endpoint';process.env.NOVA_CONTEXT_SERVICE_TOKEN='test-token';
  try {
    const tools=createNovaBatchTools(process.cwd());
    assert.deepEqual(tools.chrome.inputSchema,chrome.inputSchema);
    assert.deepEqual(tools.jianlai.inputSchema.properties.regionSpace.enum,['source','image']);
  } finally {
    for(const [name,value] of [['NOVA_CONTEXT_SERVICE_ENDPOINT',saved.endpoint],['NOVA_CONTEXT_SERVICE_TOKEN',saved.token]]) {
      if(value===undefined)delete process.env[name];else process.env[name]=value;
    }
  }
});
test('JEV browser decisions share bounded plans; desktop remains advisory', async()=>{
  const [chrome,webview,jianlai]=await Promise.all(['chrome','webview','jianlai'].map(tool));
  assert.deepEqual(webview.inputSchema.properties.plan,chrome.inputSchema.properties.plan);
  assert.deepEqual(webview.inputSchema.properties.experience,chrome.inputSchema.properties.experience);
  assert.deepEqual(webview.inputSchema.properties.advice,chrome.inputSchema.properties.advice);
  for (const browser of [chrome,webview]) {
    assert.match(browser.description,/按情况选择/);
    assert.match(browser.description,/知识图谱/);
    assert(browser.inputSchema.properties.operation.enum.includes('experience_save'));
  }
  assert.deepEqual(chrome.inputSchema.properties.advice,jianlai.inputSchema.properties.advice);
  assert.equal(chrome.inputSchema.properties.plan.properties.steps.maxItems,8);
  assert.deepEqual(chrome.inputSchema.properties.plan.required,['task','authorization','expectedText']);
  assert.equal(chrome.inputSchema.properties.plan.properties.maxActions.default,32);
  assert.equal(chrome.inputSchema.properties.plan.properties.maxActions.maximum,64);
  assert.equal(chrome.inputSchema.properties.plan.properties.inputs.maxItems,8);
  assert.equal(chrome.inputSchema.properties.plan.properties.controlNames.maxItems,16);
  assert.equal(chrome.inputSchema.properties.plan.properties.useExperience.default,false);
  assert.deepEqual(chrome.inputSchema.properties.plan.properties.steps.items.required,['action','name','role','expectedText']);
  for(const tool of [chrome,webview,jianlai]) {
    assert(tool.inputSchema.properties.operation.enum.includes('advise'));
    assert(tool.inputSchema.properties.operation.enum.includes('run'));
    assert.deepEqual(tool.inputSchema.properties.advice.required,['task','state','choices']);
    assert.equal(tool.inputSchema.properties.advice.properties.choices.maxProperties,32);
    assert.match(tool.description,/不自动执行/);
  }
});
test('browser run transport outlives the decision budget without changing normal calls',async t=>{
  const {createServer}=await import('node:net');
  const {randomUUID}=await import('node:crypto');
  const endpoint=process.platform==='win32' ? `\\\\.\\pipe\\nova-jev-${randomUUID()}` : join(tmpdir(),`nova-jev-${randomUUID()}.sock`);
  const server=createServer(socket=>socket.once('data',()=>socket.end(JSON.stringify({ok:true,result:{status:'handoff'}}))));
  await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(endpoint,resolve);});
  const saved=[process.env.NOVA_CONTEXT_SERVICE_ENDPOINT,process.env.NOVA_CONTEXT_SERVICE_TOKEN];
  process.env.NOVA_CONTEXT_SERVICE_ENDPOINT=endpoint;process.env.NOVA_CONTEXT_SERVICE_TOKEN='local-test';
  const durations=[],original=globalThis.setTimeout;
  t.mock.method(globalThis,'setTimeout',(fn,ms,...args)=>{durations.push(ms);return original(fn,ms,...args);});
  try {
    const tools=createNovaBatchTools(process.cwd());
    for(const name of ['chrome','webview']) {
      await tools[name].execute({operation:'run'});
      await tools[name].execute({operation:'inspect'});
    }
    assert.deepEqual(durations,[210000,45000,210000,45000]);
  } finally {
    t.mock.restoreAll();
    for(const [i,key] of ['NOVA_CONTEXT_SERVICE_ENDPOINT','NOVA_CONTEXT_SERVICE_TOKEN'].entries()) {
      if(saved[i]===undefined)delete process.env[key];else process.env[key]=saved[i];
    }
    await new Promise(resolve=>server.close(resolve));
  }
});
test('inspect automatic Canvas images are delivered without losing imageId or partial-execution status',async()=>{
  const dir=await mkdtemp(join(tmpdir(),'nova-visual-schema-'));
  try {
    const path=join(dir,'canvas.png');
    await writeFile(path,Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aZioAAAAASUVORK5CYII=','base64'));
    const result=await webviewMcpResult(JSON.stringify({status:'needs_review',completedActions:1,verification:'unverified',visualReason:'canvas',snapshotId:'new',images:[{imageId:'new-0',path,x:100,y:200,width:300,height:200,pixelWidth:600,pixelHeight:400}]}));
    assert.equal(result.content.filter(c=>c.type==='image').length,1);
    const text=JSON.parse(result.content[0].text);assert.equal(text.status,'needs_review');assert.equal(text.images[0].imageId,'new-0');assert.equal(text.images[0].pixelWidth,600);
  } finally {await rm(dir,{recursive:true,force:true});}
});
