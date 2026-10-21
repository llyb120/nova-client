// Verify all adapter registrations retain the new fields and automatic visual feedback.
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile,mkdtemp,writeFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {createNovaBatchTools} from './nova-batch-tools.mjs';
import {automationMcpResult} from './automation-mcp-result.mjs';
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
    assert(!listed.some(t=>t.name==='webview'));
    const removed=await client.callTool({name:'webview',arguments:{operation:'open',url:'https://example.com'}});
    assert.equal(removed.isError,true);
    assert.match(removed.content[0].text,/Unknown tool: webview/);
    for(const name of ['chrome','jianlai']) {
      const actual=listed.find(t=>t.name===name),expected=await tool(name);
      assert(actual,`missing bundled ${name}`);
      assert.deepEqual(actual.inputSchema,expected.inputSchema,'Run npm run build:nova-tools-mcp before compiling Nova');
      assert.equal(actual.description,expected.description);
    }
  } finally {await client.close();}
});
test('Chrome retains image coordinates, fast scope and bounded batch schemas',async()=>{
  const chrome=await tool('chrome');
  for(const name of ['action','imageId','scope','region','visual','maxEdge']) assert(chrome.inputSchema.properties[name]);
  const withoutDescriptions=value=>JSON.parse(JSON.stringify(value,(key,value)=>key==='description'?undefined:value));
  assert.deepEqual(withoutDescriptions(chrome.inputSchema.properties.actions.items),withoutDescriptions(chrome.inputSchema.properties.action));
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
test('Chrome and Jianlai run only executes pre-confirmed steps', async()=>{
  const [chrome,jianlai]=await Promise.all(['chrome','jianlai'].map(tool));
  for (const browser of [chrome]) {
    assert.match(browser.description,/升序前N不能替换为最高N项倒序展示/);
    assert.doesNotMatch(browser.description,/JEV|advise|runTemplate|DOM排障|Altair|vision/);
    assert.deepEqual(browser.inputSchema.properties.plan.required,['task','steps']);
    assert.equal(browser.inputSchema.properties.action.properties.target,undefined);
    assert.equal(browser.inputSchema.properties.actions.items.properties.target,undefined);
    assert(browser.inputSchema.properties.operation.enum.includes('experience_save'));
  }
  assert.match(jianlai.description,/Windows的type整段粘贴，目标读取后自动恢复原剪贴板，从不逐字模拟按键/);
  assert.doesNotMatch(jianlai.description,/Unicode/);
  assert.match(jianlai.description,/clipboard\.text.*相等就不重输/);
  assert.match(jianlai.description,/未核验前不发送/);
  assert.match(jianlai.inputSchema.properties.snapshotId.description,/act\/run必填/);
  assert.match(jianlai.description,/漏snapshotId只返回补参提示/);
  assert.equal(jianlai.inputSchema.properties.plan,undefined);
  assert.deepEqual(jianlai.inputSchema.properties.actions.items.allOf[0].then,{required:['x','y']});
  assert.equal(chrome.inputSchema.properties.plan.properties.steps.maxItems,24);
  assert.match(chrome.inputSchema.properties.tabTag.description,/本会话snapshotId/);
  for (const key of ['anyOf','oneOf','allOf']) assert.equal(chrome.inputSchema[key],undefined,'Keep the tool compatible with Claude Code');
  assert.equal(chrome.inputSchema.properties.plan.properties.maxActions.default,32);
  assert.equal(chrome.inputSchema.properties.plan.properties.inputs.maxItems,8);
  assert.deepEqual(chrome.inputSchema.properties.plan.properties.steps.items.required,['action']);
  for(const tool of [chrome,jianlai]) {
    assert(!tool.inputSchema.properties.operation.enum.includes('advise'));
    assert.equal(tool.inputSchema.properties.advice,undefined);
    assert(tool.inputSchema.properties.operation.enum.includes('run'));
    assert.equal(tool.inputSchema.properties.vision,undefined);
    assert.equal(tool.inputSchema.properties.look,undefined);
  }
});
test('browser run transport outlives the decision budget without changing normal calls',async t=>{
  const {createServer}=await import('node:net');
  const {randomUUID}=await import('node:crypto');
  const endpoint=process.platform==='win32' ? `\\\\.\\pipe\\nova-altair-${randomUUID()}` : join(tmpdir(),`nova-altair-${randomUUID()}.sock`);
  const server=createServer(socket=>socket.once('data',()=>socket.end(JSON.stringify({ok:true,result:{status:'handoff'}}))));
  await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(endpoint,resolve);});
  const saved=[process.env.NOVA_CONTEXT_SERVICE_ENDPOINT,process.env.NOVA_CONTEXT_SERVICE_TOKEN];
  process.env.NOVA_CONTEXT_SERVICE_ENDPOINT=endpoint;process.env.NOVA_CONTEXT_SERVICE_TOKEN='local-test';
  const durations=[],original=globalThis.setTimeout;
  t.mock.method(globalThis,'setTimeout',(fn,ms,...args)=>{durations.push(ms);return original(fn,ms,...args);});
  try {
    const tools=createNovaBatchTools(process.cwd());
    await tools.chrome.execute({operation:'run'});
    await tools.chrome.execute({operation:'inspect'});
    assert.deepEqual(durations,[210000,45000]);
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
    const result=await automationMcpResult(JSON.stringify({status:'needs_review',completedActions:1,verification:'unverified',visualReason:'canvas',snapshotId:'new',images:[{imageId:'new-0',path,x:100,y:200,width:300,height:200,pixelWidth:600,pixelHeight:400}]}));
    assert.equal(result.content.filter(c=>c.type==='image').length,1);
    const text=JSON.parse(result.content[0].text);assert.equal(text.status,'needs_review');assert.equal(text.images[0].imageId,'new-0');assert.equal(text.images[0].pixelWidth,600);
  } finally {await rm(dir,{recursive:true,force:true});}
});
