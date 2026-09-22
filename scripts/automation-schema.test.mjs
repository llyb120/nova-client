// Verify all adapter registrations retain the new fields and automatic visual feedback.
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile,mkdtemp,writeFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createNovaBatchTools} from './nova-batch-tools.mjs';
import {webviewMcpResult} from './webview-mcp-result.mjs';
const tool = async name=>JSON.parse(await readFile(new URL(`./${name}-tool.json`,import.meta.url),'utf8'));
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
test('JEV advice is available through both existing tool schemas with identical bounded inputs', async()=>{
  const [chrome,jianlai]=await Promise.all(['chrome','jianlai'].map(tool));
  assert.deepEqual(chrome.inputSchema.properties.advice,jianlai.inputSchema.properties.advice);
  assert.equal(chrome.inputSchema.properties.plan.properties.steps.maxItems,8);
  assert.deepEqual(chrome.inputSchema.properties.plan.required,['task','authorization','expectedText']);
  assert.equal(chrome.inputSchema.properties.plan.properties.inputs.maxItems,8);
  assert.deepEqual(chrome.inputSchema.properties.plan.properties.steps.items.required,['action','name','role','expectedText']);
  for(const tool of [chrome,jianlai]) {
    assert(tool.inputSchema.properties.operation.enum.includes('advise'));
    assert(tool.inputSchema.properties.operation.enum.includes('run'));
    assert.deepEqual(tool.inputSchema.properties.advice.required,['task','state','choices']);
    assert.equal(tool.inputSchema.properties.advice.properties.choices.maxProperties,32);
    assert.match(tool.description,/不自动执行/);
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
