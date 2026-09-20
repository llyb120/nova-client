// Paired before/after production runtime benchmark. Only the fixture driver is adapted.
import {readFile,writeFile,mkdir,appendFile} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
import {resolve} from 'node:path';
const out='validation/optimization';
await mkdir(out,{recursive:true});
let key=process.env.COMMAND_CODE_API_KEY;
delete process.env.COMMAND_CODE_API_KEY;delete process.env.GITHUB_TOKEN;delete process.env.GH_TOKEN;
if(!process.argv.includes('--prepare-only')&&!/^user_[A-Za-z0-9_-]{16,300}$/.test(key??''))throw Error('Missing test credential');
const clean=s=>String(s).split(key).join('[REDACTED]').replace(/user_[A-Za-z0-9_-]{16,}/g,'[REDACTED]');
const save=(p,v)=>writeFile(p,clean(JSON.stringify(v,null,2)),{mode:0o600});
function replaceOnce(s,a,b){if(s.split(a).length!==2)throw Error('Pinned fixture changed: '+a.slice(0,70));return s.replace(a,b);}
let fixture=await readFile('_fixture/scripts/operator-native-gui-ab.mjs','utf8');
fixture=replaceOnce(fixture,"for(const channel of ['chrome','jianlai'])for(const test of cases)","for(const channel of [process.env.OPERATOR_CASE_CHANNEL])for(const test of cases.filter(t=>t.id===process.env.OPERATOR_CASE_NAME))");
fixture=replaceOnce(fixture,"const arms=n++%2?['B','A']:['A','B'];","const arms=['B']; // Both binary variants use projected context, not accumulated histories.");
fixture=replaceOnce(fixture,"}catch(e){row.status='error';row.error=redact(e.message,apiKey);}","}catch(e){row.status='error';row.error=redact(e.message,apiKey);if(e.message.includes(' timeout'))throw e;}");
await writeFile('scripts/operator-optimization-fixture.mjs',fixture);
let wire=await readFile('_fixture/scripts/operator-real-json-transport.mjs','utf8');
wire=replaceOnce(wire,"?GUI:REPLAY","?(system.content.includes('Prefer checkpointPatch')?GUI.replace('params, checkpoint, result','params, checkpoint, checkpointPatch, verified, result'):GUI):REPLAY");
// The connectivity probe receives an explicit native parameter schema, as real GUI requests do.
wire=replaceOnce(wire,'No observation exists. Return an observe decision to take a Jianlai screenshot, reason one sentence, requiresConfirmation false. Do not invent evidence.','No observation exists. Return an observe decision to take a Jianlai screenshot, reason one sentence, requiresConfirmation false. Do not invent evidence. Available native params schema for this probe: {type:object,properties:{operation:{const:screenshot}},required:[operation],additionalProperties:false}. The decision kind is observe, whereas params.operation is screenshot. No native input is executed by this connectivity probe.');
await writeFile('scripts/operator-optimization-transport.mjs',wire);
if(process.argv.includes('--prepare-only')){console.log('Pinned fixture adaptation validated; zero API requests.');process.exit(0);}
const {runGui}=await import(pathToFileURL(resolve('scripts/operator-optimization-fixture.mjs')));
const {installDecisionTransport,preflight,correction,transportAudit}=await import(pathToFileURL(resolve('scripts/operator-optimization-transport.mjs')));
const plan=[['chrome','canvas'],['jianlai','form'],['chrome','orders'],['jianlai','canvas'],['chrome','form'],['jianlai','orders'],['chrome','orders'],['chrome','canvas'],['jianlai','form']].map(([channel,test],i)=>({pair:i+1,channel,test,order:i%2?['optimized','baseline']:['baseline','optimized']}));
const report={kind:'paired-production-before-after',baseline:process.env.BASELINE_SHA,optimized:process.env.OPTIMIZED_SHA,fixture:process.env.FIXTURE_SHA,workflowCommit:process.env.GITHUB_SHA,model:'deepseek/deepseek-v4.1-flash',transport:correction,plan,startedAt:new Date().toISOString(),rows:[],status:'running',scope:'Actual production Rust runtime and native Chromium/Jianlai input in an isolated Tauri fixture. Both variants use projected context. Not full Windows client or CodeBuddy CLI.'};
const sum=(a,f)=>a.reduce((v,x)=>v+(Number(f(x))||0),0);
const restore=installDecisionTransport();
try{
 await save(out+'/plan.json',report);
 await preflight(key,out);
 for(const p of plan)for(const variant of p.order){
  if(sum(report.rows,r=>r.totalTokens)>2000000||transportAudit.length>=270)throw Error('Prespecified global API budget reached');
  process.env.OPERATOR_CASE_CHANNEL=p.channel;process.env.OPERATOR_CASE_NAME=p.test;
  process.env.OPERATOR_GUI_EXE=process.env[variant==='baseline'?'BASELINE_EXE':'OPTIMIZED_EXE'];
  if(!process.env.OPERATOR_GUI_EXE)throw Error('Missing pinned binary');
  const id=`p${p.pair}-${p.channel}-${p.test}-${variant}`,dir=out+'/'+id;
  console.log('BEGIN '+id);
  const auditStart=transportAudit.length;
  const g=await runGui({apiKey:key,outDir:dir});
  g.sourceCommit=report[variant];g.workflowCommit=report.workflowCommit;g.variant=variant;g.pair=p.pair;g.arms={B:'Production projected context for '+variant};
  g.productionBaseline=report.baseline;
  await save(dir+'/report.json',g);
  const calls=g.decisions??[],audit=transportAudit.slice(auditStart),r=g.runs[0]??{};
  const row={id,pair:p.pair,variant,channel:p.channel,case:p.test,status:r.status??g.status,error:g.error??r.error,success:r.actualTaskSuccess===true,falseCompletion:r.falseCompletion===true,elapsedMs:r.elapsedMs??null,modelCalls:calls.length,nativeCalls:r.nativeCalls??0,promptTokens:sum(calls,d=>d.usage?.prompt_tokens),completionTokens:sum(calls,d=>d.usage?.completion_tokens),reasoningTokens:sum(calls,d=>d.usage?.completion_tokens_details?.reasoning_tokens),modelMs:sum(calls,d=>d.elapsedMs),usageComplete:calls.length>0&&audit.length===calls.length&&audit.every(a=>a.status===200&&a.attempt===0)&&calls.every(d=>Number.isFinite(d.usage?.prompt_tokens)&&Number.isFinite(d.usage?.completion_tokens)),emptyDecisions:calls.filter(d=>!d.text).length};
  row.totalTokens=row.promptTokens+row.completionTokens;report.rows.push(row);
  await save(out+'/summary.json',report);await save(out+'/transport-audit.json',transportAudit);
  console.log('RESULT '+JSON.stringify(row));
  if(g.status==='blocked')throw Error('Fixture blocked; no overlapping tasks: '+(g.error??'unknown'));
 }
 report.status='completed';
}catch(e){report.status='blocked';report.error=clean(e.message);console.error(report.error);process.exitCode=1;}
finally{
 report.completedAt=new Date().toISOString();
 report.variants={};
 for(const variant of ['baseline','optimized']){
  const rows=report.rows.filter(r=>r.variant===variant);
  report.variants[variant]={tasks:rows.length,successes:rows.filter(r=>r.success).length,falseCompletions:rows.filter(r=>r.falseCompletion).length,elapsedMs:sum(rows,r=>r.elapsedMs),modelCalls:sum(rows,r=>r.modelCalls),promptTokens:sum(rows,r=>r.promptTokens),completionTokens:sum(rows,r=>r.completionTokens),totalTokens:sum(rows,r=>r.totalTokens),usageComplete:rows.length>0&&rows.every(r=>r.usageComplete)};
 }
 report.pairs=plan.map(p=>({pair:p.pair,channel:p.channel,case:p.test,baseline:report.rows.find(r=>r.pair===p.pair&&r.variant==='baseline'),optimized:report.rows.find(r=>r.pair===p.pair&&r.variant==='optimized')}));
 await save(out+'/summary.json',report);await save(out+'/transport-audit.json',transportAudit);
 console.log('FINAL '+JSON.stringify({status:report.status,variants:report.variants}));
 if(process.env.GITHUB_STEP_SUMMARY)await appendFile(process.env.GITHUB_STEP_SUMMARY,'## Paired production before/after\n\n```json\n'+JSON.stringify({status:report.status,variants:report.variants},null,2)+'\n```\n');
 if(report.status!=='completed'||report.rows.length!==18||report.rows.some(r=>!r.success))process.exitCode=1;
 restore();key=null;
}
