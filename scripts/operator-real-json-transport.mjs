// Test API transport, same settings for both arms. Never rewrites model actions.
import {mkdir,writeFile} from 'node:fs/promises';
const MODEL='deepseek/deepseek-v4.1-flash';
const ENDPOINT='https://api.commandcode.ai/provider/v1/chat/completions';
export const transportAudit=[];
export const correction={revision:'json-wire-bounded-retry-v2',responseFormat:'json_object',maxTokens:8192,reasoningEffort:'low',inferenceRetries:2,noAutomaticActionRepair:true,noNativeActionReplay:true,scope:'Same API adapter in both arms; production validation and native guards remain authoritative.'};
const GUI=`\nOUTPUT WIRE CONTRACT: one JSON object, no DSML/XML or native function calls. Top-level keys only: kind, evidenceId, params, checkpoint, result, reason, requiresConfirmation. IMPORTANT: evidenceId is TOP LEVEL, NEVER in params. params.snapshotId is the native snapshot. Example act SHAPE (replace placeholders with CURRENT observed values and a valid native action): {"kind":"act","evidenceId":"CURRENT_OBSERVATION_EVIDENCE_ID","params":{"operation":"act","snapshotId":"CURRENT_NATIVE_SNAPSHOT_ID","actions":[{"action":"press","key":"Tab"}]},"requiresConfirmation":false}. The example is not an instruction to press Tab. For observe only chrome tabs/inspect/screenshot/status, or jianlai windows/screenshot; ignore catalog operations outside this list. Native action variants are strict: press only has action/key, type has action/text; never add frame/ref to press. Use the channel's exact key names. Fill only editable inputs, not SELECT elements; use observed UI and keyboard for native dropdowns, verify the selected value. On Linux spell Ctrl+a with lowercase a to avoid an unintended shift modifier. Screenshots can change between monitor and window scopes: always use CURRENT image coordinates and imageId. Do not repeat a drag when its outcome already meets the target; observe the success message, including below-fold content. requiresConfirmation is a boolean. LastDecisionError indicates no input was sent by THAT rejected decision; prior real actions must not be replayed. No invented targets or permissions.\n`;
const REPLAY=`\n必须是一个合法JSON对象，不要DSML/XML。工具调用：{"kind":"tool","tool":"polaris","args":{"task":"用户真实问题"}}；最终：{"kind":"finish","answer":"证据支持的结论","claims":[],"unresolved":[]}。只读工具由宿主执行，每次最多一个。最后一轮必须finish。\n`;
export function installDecisionTransport(){
 const original=globalThis.fetch;
 globalThis.fetch=async(input,options={})=>{
  if(String(input)!==ENDPOINT||options.method!=='POST')return original(input,options);
  const body=JSON.parse(options.body);
  if(body.model!==MODEL)throw Error('Different model denied');
  body.response_format={type:'json_object'};body.max_tokens=8192;
  const system=body.messages.find(m=>m.role==='system');
  if(system&&typeof system.content==='string')system.content+=system.content.includes("Nova's isolated interface operator")?GUI:REPLAY;
  const encoded=JSON.stringify(body);
  for(let attempt=0;;attempt++){
   const start=performance.now();let response;
   try{response=await original(input,{...options,body:encoded});}
   catch(e){transportAudit.push({attempt,status:null,elapsedMs:performance.now()-start,error:'transport error'});throw e;}
   transportAudit.push({attempt,status:response.status,elapsedMs:performance.now()-start,inputBytes:Buffer.byteLength(encoded)});
   if(![429,503].includes(response.status)||attempt>=2)return response;
   await response.arrayBuffer();
   await new Promise(r=>setTimeout(r,4000*(attempt+1)));
   options.signal?.throwIfAborted();
  }
 };
 return ()=>{globalThis.fetch=original};
}
export async function preflight(apiKey,outDir='validation'){
 await mkdir(outDir,{recursive:true});
 const report={kind:'json-adapter-preflight',model:MODEL,correction,startedAt:new Date().toISOString()};
 const response=await fetch(ENDPOINT,{method:'POST',redirect:'error',headers:{Authorization:`Bearer ${apiKey}`,'Content-Type':'application/json'},body:JSON.stringify({model:MODEL,messages:[{role:'system',content:"You are Nova's isolated interface operator. Return JSON only."},{role:'user',content:'No observation exists. Return an observe decision to take a Jianlai screenshot, reason one sentence, requiresConfirmation false. Do not invent evidence.'}],temperature:0,reasoning_effort:'low',max_tokens:8192,stream:false}),signal:AbortSignal.timeout(70000)});
 const r=await response.json();report.httpStatus=response.status;report.returnedModel=r.model;report.usage=r.usage;report.finishReason=r.choices?.[0]?.finish_reason;report.text=r.choices?.[0]?.message?.content??'';
 let d;try{d=JSON.parse(report.text)}catch{}
 report.passed=response.ok&&r.model===MODEL&&report.finishReason==='stop'&&d?.kind==='observe'&&d?.params?.operation==='screenshot'&&d?.requiresConfirmation===false;
 await writeFile(`${outDir}/json-adapter-preflight.json`,JSON.stringify(report,null,2),{mode:0o600});
 if(!report.passed)throw Error('Preflight failed; no GUI/replay requests will be made');
}
