import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {createServer} from 'node:net';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {randomUUID} from 'node:crypto';
import {createNovaBatchTools} from './nova-batch-tools.mjs';
import {callGlobalContextTool} from './nova-context-client.mjs';
const withEnv=async(values,fn)=>{const old={};for(const [k,v] of Object.entries(values)){old[k]=process.env[k];if(v==null)delete process.env[k];else process.env[k]=v;}try{return await fn();}finally{for(const[k,v]of Object.entries(old)){if(v===undefined)delete process.env[k];else process.env[k]=v;}}};
const env={NOVA_CONTEXT_SERVICE_ENDPOINT:'fixture',NOVA_CONTEXT_SERVICE_TOKEN:'fixture-token',NOVA_OPERATOR_SCOPE:'host-session-generation'};
test('bound operator replaces two interactive tools, not code tools',()=>withEnv(env,()=>{const t=createNovaBatchTools(process.cwd());assert.ok(t.operator);assert.ok(t.polaris);assert.ok(t.webview);assert.ok(!t.chrome&&!t.jianlai);assert.deepEqual(t.operator.inputSchema.required,['goal']);assert.ok(!('model'in t.operator.inputSchema.properties));assert.ok(!('owner'in t.operator.inputSchema.properties));}));
test('unbound external adapters retain native tools',()=>withEnv({...env,NOVA_OPERATOR_SCOPE:null},()=>{const t=createNovaBatchTools(process.cwd());assert.ok(t.chrome&&t.jianlai);assert.ok(!t.operator);}));
test('read-only mode exposes no operator or raw input tools',()=>withEnv(env,()=>{const t=createNovaBatchTools(process.cwd(),{readOnly:true});assert.ok(!t.operator&&!t.chrome&&!t.jianlai&&!t.webview);assert.ok(t.polaris);}));
test('new contract preserves explicit user single-tool restriction',async()=>{const tool=JSON.parse(await readFile(new URL('./operator-tool.json',import.meta.url),'utf8'));assert.deepEqual(tool.inputSchema.properties.allowedTools.items.enum,['chrome','jianlai']);assert.equal(tool.inputSchema.additionalProperties,false);assert.ok(!tool.inputSchema.properties.nativeArgs);});
test('operator transport uses host scope outside model args and never replays disconnect',async()=>{
 const endpoint=process.platform==='win32'?`\\\\.\\pipe\\nova-operator-test-${randomUUID()}`:join(tmpdir(),`op-${randomUUID()}.sock`);
 let received;let connections=0;
 const server=createServer(socket=>{connections++;let body='';socket.on('data',chunk=>{body+=chunk; if(body.includes('\n')){received=JSON.parse(body.trim());socket.end();}});});
 await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(endpoint,resolve);});
 try{await withEnv({...env,NOVA_CONTEXT_SERVICE_ENDPOINT:endpoint},async()=>{await assert.rejects(callGlobalContextTool('operator',process.cwd(),{goal:'fixture',operator_scope:'forged'},'client-A'));assert.equal(connections,1);assert.equal(received.operator_scope,'host-session-generation');assert.equal(received.params.operator_scope,'forged');});}
 finally{await new Promise(resolve=>server.close(resolve));}
});
test('native schemas keep already-existing batching capability for all profiles',async()=>{for(const name of ['chrome','jianlai']){const t=JSON.parse(await readFile(new URL(`./${name}-tool.json`,import.meta.url),'utf8'));assert.equal(t.inputSchema.properties.actions.maxItems,8);}});
