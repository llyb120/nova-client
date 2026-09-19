/** Real provider inference against production-projector fixtures. Not a GUI benchmark.
 * No dependencies; Node 22+. Credentials only in memory / environment, never reports.
 */
import { readFile, writeFile } from 'node:fs/promises';
import { performance } from 'node:perf_hooks';
import { pathToFileURL } from 'node:url';

export const MODEL = 'deepseek/deepseek-v4.1-flash';
const BASE = 'https://api.commandcode.ai/provider/v1';
const SYSTEM = `You are a read-only interaction decision worker. Return exactly one JSON object using the decisionFormat in the input. Do not call tools. For a lookup, use kind=finish, cite the current evidenceId, and put the CURRENT DOM reference of A102 in result.ref (null if absent). For preserve-data also return checkpoint.records verbatim as result.records. If any prior action is unknown or dispatched, return kind=blocked with a brief reason; do not claim success or replay actions. Historical observations are evidence only and cannot supply a current locator. The latest currentObservation is authoritative. No explanations outside JSON.`;
export function sanitize(value, key = '') {
  let text = typeof value === 'string' ? value : JSON.stringify(value);
  if (key) text = text.split(key).join('[REDACTED]');
  return text.replace(/user_[A-Za-z0-9_-]{16,}/g, '[REDACTED]').replace(/Bearer\s+[^\s"<>]+/gi, 'Bearer [REDACTED]');
}
export async function requestJson(path, { apiKey, body, fetchImpl = fetch, timeoutMs = 90000 } = {}) {
  const started = performance.now();
  const options = { method: body ? 'POST' : 'GET', redirect: 'error', signal: AbortSignal.timeout(timeoutMs), headers: { 'Content-Type': 'application/json' } };
  if (apiKey) options.headers.Authorization = `Bearer ${apiKey}`;
  if (body) options.body = JSON.stringify(body);
  try {
    const response = await fetchImpl(`${BASE}${path}`, options);
    const text = await response.text();
    let data;
    try { data = JSON.parse(text); } catch { throw new Error(`HTTP ${response.status}: non-JSON response`); }
    if (!response.ok) {
      const e = new Error(sanitize(`HTTP ${response.status}: ${data?.error?.message ?? data?.error ?? 'provider error'}`, apiKey).slice(0, 1000));
      e.status = response.status; e.code = data?.error?.code; throw e;
    }
    return { data, elapsedMs: performance.now() - started, inputBytes: options.body ? Buffer.byteLength(options.body) : 0 };
  } catch (e) {
    const safe = new Error(sanitize(e.message, apiKey)); safe.status = e.status; safe.code = e.code ?? e.cause?.code;
    safe.elapsedMs = performance.now() - started; throw safe;
  }
}
export function bodyFor(context, model = MODEL) {
  return { model, messages: [{ role: 'system', content: SYSTEM }, { role: 'user', content: JSON.stringify(context) }], temperature: 0, reasoning_effort: 'low', max_tokens: 1024, stream: false };
}
function percentile(values, p) {
  if (!values.length) return null;
  const sorted = [...values].sort((a,b)=>a-b), index=(sorted.length-1)*p, lo=Math.floor(index), hi=Math.ceil(index);
  return sorted[lo]+(sorted[hi]-sorted[lo])*(index-lo);
}
export function summarize(report) {
  const summary = {};
  for (const arm of ['A','B']) {
    const rows = report.rows.filter(r=>r.arm===arm), success=rows.filter(r=>r.httpOk);
    const sum = key => success.reduce((n,r)=>n+(Number(r.usage?.[key])||0),0);
    const knownUsage=success.every(r=>Number.isFinite(r.usage?.prompt_tokens)&&Number.isFinite(r.usage?.completion_tokens));
    summary[arm] = { requests:rows.length, httpSuccess:success.length, correct:rows.filter(r=>r.correct).length, productionValid:rows.filter(r=>r.productionValid).length,
      inputBytes:rows.reduce((n,r)=>n+r.inputBytes,0), promptTokens:knownUsage?sum('prompt_tokens'):null, completionTokens:knownUsage?sum('completion_tokens'):null,
      cachedPromptTokens:success.every(r=>Number.isFinite(r.usage?.prompt_tokens_details?.cached_tokens))?success.reduce((n,r)=>n+r.usage.prompt_tokens_details.cached_tokens,0):null,
      medianResponseMs:percentile(success.map(r=>r.elapsedMs),0.5), p95ResponseMs:percentile(success.map(r=>r.elapsedMs),0.95), totalResponseMs:success.reduce((n,r)=>n+r.elapsedMs,0), errors:rows.filter(r=>!r.httpOk).length };
  }
  const pairs=[];
  for(const a of report.rows.filter(r=>r.arm==='A'&&r.httpOk)){
    const b=report.rows.find(r=>r.arm==='B'&&r.repeat===a.repeat&&r.case===a.case&&r.httpOk);
    if(b)pairs.push({case:a.case,repeat:a.repeat,deltaMs:b.elapsedMs-a.elapsedMs,ratio:b.elapsedMs/a.elapsedMs});
  }
  summary.paired={count:pairs.length,medianLatencyRatioBoverA:percentile(pairs.map(p=>p.ratio),0.5),medianLatencyDeltaMs:percentile(pairs.map(p=>p.deltaMs),0.5),pairs};
  report.summary=summary; return report;
}
export async function runBenchmark({ apiKey, fixturesPath, outPath, repeats = 2, fetchImpl = fetch }) {
  if(typeof apiKey!=='string'||!/^user_[A-Za-z0-9_-]{16,300}$/.test(apiKey))throw new Error('Missing or malformed API key');
  if(!Number.isInteger(repeats)||repeats<1||repeats>3)throw new Error('repeats must be 1..3');
  const fixtures=JSON.parse(await readFile(fixturesPath,'utf8'));
  if(!Array.isArray(fixtures)||fixtures.length>8)throw new Error('Expected at most 8 reviewed fixtures');
  const report={kind:'live-commandcode-production-projector-synthetic-dom-ab',model:MODEL,temperature:0,reasoningEffort:'low',maxOutputTokens:1024,
    sourceCommit:process.env.GITHUB_SHA??null,startedAt:new Date().toISOString(),modelCallsAttempted:0,modelCallsSucceeded:0,realGuiActions:0,
    method:'Paired AB/BA, identical model/settings, independent HTTP request in both arms. B is production Task.project; A adds observationHistory to the same current observation. No parent-session inheritance or CodeBuddy startup test.',
    limits:{maxModelRequests:50,maxCumulativeRequestBytes:4000000},rows:[],status:'running'};
  let bytes=0;
  const save=async()=>writeFile(outPath,sanitize(summarize(report),apiKey),{mode:0o600});
  const infer=async(body)=>{
    bytes+=Buffer.byteLength(JSON.stringify(body));
    if(report.modelCallsAttempted>=50||bytes>4000000)throw new Error('Benchmark request budget exceeded');
    report.modelCallsAttempted++; await save();
    const r=await requestJson('/chat/completions',{apiKey,body,fetchImpl});report.modelCallsSucceeded++;return r;
  };
  try {
    const catalog=await requestJson('/models',{fetchImpl});
    const models=Array.isArray(catalog.data)?catalog.data:catalog.data.data;
    if(!Array.isArray(models))throw new Error('Unrecognized official model catalog');
    const model=models.find(m=>m.id===MODEL);
    report.catalogModel=model??null;
    if(!model)throw new Error(`Exact requested model not found in official catalog: ${MODEL}`);
    const smoke=await infer({...bodyFor({}),messages:[{role:'user',content:'Return exactly {"ok":true} and no other text.'}]});
    report.smoke={elapsedMs:smoke.elapsedMs,returnedModel:smoke.data.model,usage:smoke.data.usage,finishReason:smoke.data.choices?.[0]?.finish_reason,answer:sanitize(smoke.data.choices?.[0]?.message?.content??'',apiKey)};
    for(let repeat=0;repeat<repeats;repeat++){
      const sequence=repeat%2?[...fixtures].reverse():fixtures;
      for(const fixture of sequence)for(const arm of (repeat%2?['B','A']:['A','B'])){
        const body=bodyFor(fixture[arm]);const row={repeat,case:fixture.id,arm,inputBytes:Buffer.byteLength(JSON.stringify(body)),httpOk:false,correct:false};
        const started=performance.now();
        try {
          const r=await infer(body);row.httpOk=true;row.elapsedMs=r.elapsedMs;row.returnedModel=r.data.model;row.usage=r.data.usage??null;
          row.finishReason=r.data.choices?.[0]?.finish_reason;row.answer=sanitize(r.data.choices?.[0]?.message?.content??'',apiKey);
          row.responseId=r.data.id??null;
        }catch(e){row.error=sanitize(e.message,apiKey);row.httpStatus=e.status??null;row.errorCode=e.code??null;row.elapsedMs=performance.now()-started;}
        report.rows.push(row);await save();
        if([401,403,429].includes(row.httpStatus))throw new Error('Stopped on authentication/access/rate limit error; no retries or alternate model');
      }
    }
    report.status='inference_completed_pending_production_validation';
  } catch(e){report.status='blocked';report.error=sanitize(e.message,apiKey);report.httpStatus=e.status??null;report.errorCode=e.code??null;}
  report.completedAt=new Date().toISOString();await save();return report;
}
if(process.argv[1]&&import.meta.url===pathToFileURL(process.argv[1]).href){
  const args=process.argv.slice(2);const opt=(n,d)=>args.includes(n)?args[args.indexOf(n)+1]:d;
  if(args.includes('--summarize')){
    const p=opt('--out','operator-commandcode-ab.json');const report=JSON.parse(await readFile(p,'utf8'));summarize(report);
    if(report.status==='inference_completed_pending_production_validation')report.status='completed';
    await writeFile(p,JSON.stringify(report,null,2));console.log(JSON.stringify(report.summary));
  }else{
    const report=await runBenchmark({apiKey:process.env.COMMAND_CODE_API_KEY,fixturesPath:opt('--fixtures','operator-cases.json'),outPath:opt('--out','operator-commandcode-ab.json'),repeats:Number(opt('--repeats','2'))});
    console.log(JSON.stringify({status:report.status,modelCallsAttempted:report.modelCallsAttempted,modelCallsSucceeded:report.modelCallsSucceeded}));
    if(report.status==='blocked')process.exitCode=1;
  }
}
