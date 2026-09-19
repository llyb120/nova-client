import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { MODEL, sanitize, requestJson, bodyFor, runBenchmark, summarize } from './operator-commandcode-ab.mjs';
const key='user_fixture_not_a_real_credential';
const ok=data=>({ok:true,status:200,text:async()=>JSON.stringify(data)});
test('exact model/settings are shared by both arms',()=>{
  const a=bodyFor({observationHistory:['old'],currentObservation:'new'}),b=bodyFor({currentObservation:'new'});
  assert.equal(a.model,MODEL);assert.equal(b.model,MODEL);assert.equal(a.temperature,b.temperature);assert.equal(a.reasoning_effort,b.reasoning_effort);assert.deepEqual(a.messages[0],b.messages[0]);
});
test('credential goes only to fixed official host, with redirects refused',async()=>{
  await requestJson('/chat/completions',{apiKey:key,body:bodyFor({}),fetchImpl:async(url,opt)=>{assert.equal(url,'https://api.commandcode.ai/provider/v1/chat/completions');assert.equal(opt.headers.Authorization,`Bearer ${key}`);assert.equal(opt.redirect,'error');assert.ok(!opt.body.includes(key));return ok({});}});
});
test('errors redact credentials echoed by provider',async()=>{
  await assert.rejects(requestJson('/chat/completions',{apiKey:key,body:{},fetchImpl:async()=>({ok:false,status:401,text:async()=>JSON.stringify({error:{message:`Invalid ${key}`}})})}),e=>e.status===401&&!e.message.includes(key)&&e.message.includes('[REDACTED]'));
  assert.equal(sanitize(`token ${key}`,key),'token [REDACTED]');
});
test('403 stops before fixtures; no model fallback or fake successful samples',async()=>{
  const dir=await mkdtemp(join(tmpdir(),'operator-api-test-'));
  try{const fp=join(dir,'f.json'),out=join(dir,'out.json');await writeFile(fp,JSON.stringify([{id:'x',A:{},B:{}}]));let posts=0;
    const report=await runBenchmark({apiKey:key,fixturesPath:fp,outPath:out,fetchImpl:async(url)=>{if(url.endsWith('/models'))return ok({data:[{id:MODEL}]});posts++;return {ok:false,status:403,text:async()=>JSON.stringify({error:{code:'upgrade_required',message:'Access denied'}})};}});
    assert.equal(posts,1);assert.equal(report.status,'blocked');assert.equal(report.modelCallsSucceeded,0);assert.equal(report.rows.length,0);assert.ok(!(await readFile(out,'utf8')).includes(key));
  }finally{await rm(dir,{recursive:true,force:true});}
});
test('paired ordering, reversed repeat, reported usage, no invented correctness',async()=>{
  const dir=await mkdtemp(join(tmpdir(),'operator-api-test-'));
  try{const fp=join(dir,'f.json'),out=join(dir,'out.json');await writeFile(fp,JSON.stringify([{id:'one',A:{arm:'A'},B:{arm:'B'}},{id:'two',A:{arm:'A'},B:{arm:'B'}}]));
    const report=await runBenchmark({apiKey:key,fixturesPath:fp,outPath:out,fetchImpl:async(url)=>url.endsWith('/models')?ok({data:[{id:MODEL}]}):ok({id:'fake',model:MODEL,choices:[{message:{content:'{"kind":"blocked"}'},finish_reason:'stop'}],usage:{prompt_tokens:100,completion_tokens:10,prompt_tokens_details:{cached_tokens:20}}})});
    assert.equal(report.modelCallsAttempted,9);assert.equal(report.modelCallsSucceeded,9);assert.equal(report.realGuiActions,0);
    assert.deepEqual(report.rows.map(r=>`${r.repeat}-${r.case}-${r.arm}`),['0-one-A','0-one-B','0-two-A','0-two-B','1-two-B','1-two-A','1-one-B','1-one-A']);
    assert.equal(report.summary.A.promptTokens,400);assert.equal(report.summary.B.cachedPromptTokens,80);assert.equal(report.summary.A.correct,0);
    assert.equal(report.status,'inference_completed_pending_production_validation');
  }finally{await rm(dir,{recursive:true,force:true});}
});
test('unknown token usage stays unknown rather than estimated',()=>{
  const r=summarize({rows:[{arm:'A',httpOk:true,inputBytes:2,elapsedMs:1,usage:null},{arm:'B',httpOk:true,inputBytes:2,elapsedMs:1,usage:null}]});assert.equal(r.summary.A.promptTokens,null);assert.equal(r.summary.B.completionTokens,null);
});
