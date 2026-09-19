// Test-only transport correction after real run 35446616489 failed BEFORE input.
// No production code, native permission/observation guard, model, or Reasonix changes.
// Both A/B receive the same response_format, max_tokens, and envelope clarification.
import {mkdir,writeFile} from 'node:fs/promises';
const MODEL='deepseek/deepseek-v4.1-flash';
const ENDPOINT='https://api.commandcode.ai/provider/v1/chat/completions';
export const correction={revision:'json-output-8192-v1',responseFormat:'json_object',maxTokens:8192,reasoningEffort:'low',noAutomaticRepair:true,noNativeActionReplay:true,scope:'HTTP test adapter only; JSON validity is requested, not assumed. Production guards remain authoritative.'};
const GUI=`\nOUTPUT WIRE CONTRACT (test adapter): return one JSON object, NEVER DSML/XML/native tool-call markup or multiple JSON objects. The only allowed top-level keys are kind, params, checkpoint, evidenceId, result, reason, requiresConfirmation. requiresConfirmation MUST be a JSON boolean, not a string. checkpoint may be a compact object containing facts, records, coverage and unresolved issues. Do not copy input keys contract/currentObservation/decisionFormat/tool into your response. For observe use params.operation: Chrome tabs/inspect/screenshot/status; Jianlai windows/screenshot. For act use params.operation=act, the CURRENT evidenceId and snapshotId, plus native action/actions. experience_search/save/feedback, navigation, recall and all other operations in the raw tool catalog are NOT exposed by this Operator runtime. Ignore catalog advice to call unavailable operations. This narrows available operations; it never grants extra permission. Follow the native parameter schema for allowed operations. Finish/blocked must not execute actions. Do not include thinking prose or a step-by-step plan; reason is one short sentence.\nValid structural example (values are illustrative, never current evidence): {"kind":"observe","params":{"operation":"screenshot"},"checkpoint":{"unresolved":["Need current observation"]},"reason":"Observe before operating","requiresConfirmation":false}. A finish result must use the required task result structure and verified current evidenceId.\n`;
const REPLAY=`\n输出协议补充：必须是一个合法 JSON 对象，禁止 DSML/XML/tool_call 标记和额外文字。顶层工具调用示例：{"kind":"tool","tool":"polaris","args":{"task":"用户的真实问题"}}。最终结果示例：{"kind":"finish","answer":"根据实际读取证据给出结论","claims":[],"unresolved":[]}。模型不直接调用原生函数；测试宿主执行该 JSON 描述的只读工具。不要把多次工具调用塞入一个答案。最后一轮必须 finish。\n`;
export function installDecisionTransport(){
 const original=globalThis.fetch;
 globalThis.fetch=async(input,options={})=>{
  if(String(input)!==ENDPOINT||options.method!=='POST')return original(input,options);
  const body=JSON.parse(options.body);
  if(body.model!==MODEL)throw Error('Test adapter refuses a different model');
  body.response_format={type:'json_object'};body.max_tokens=8192;
  const system=body.messages.find(m=>m.role==='system');
  if(system&&typeof system.content==='string')system.content+=system.content.includes("Nova's isolated interface operator")?GUI:REPLAY;
  return original(input,{...options,body:JSON.stringify(body)});
 };
 return ()=>{globalThis.fetch=original};
}
export async function preflight(apiKey,outDir='validation'){
 await mkdir(outDir,{recursive:true});
 const report={kind:'json-adapter-preflight',model:MODEL,correction,modelCalls:1,startedAt:new Date().toISOString()};
 const response=await fetch(ENDPOINT,{method:'POST',redirect:'error',headers:{Authorization:`Bearer ${apiKey}`,'Content-Type':'application/json'},body:JSON.stringify({model:MODEL,messages:[{role:'system',content:"You are Nova's isolated interface operator. Return JSON only."},{role:'user',content:'No observation exists. Return an observe decision to take a Jianlai screenshot, reason one sentence, requiresConfirmation false. Do not invent evidence.'}],temperature:0,reasoning_effort:'low',max_tokens:8192,stream:false}),signal:AbortSignal.timeout(70000)});
 const r=await response.json();report.httpStatus=response.status;report.returnedModel=r.model;report.usage=r.usage;report.finishReason=r.choices?.[0]?.finish_reason;report.text=r.choices?.[0]?.message?.content??'';
 let d;try{d=JSON.parse(report.text)}catch{}
 report.passed=response.ok&&r.model===MODEL&&report.finishReason==='stop'&&d?.kind==='observe'&&d?.params?.operation==='screenshot'&&d?.requiresConfirmation===false;
 await writeFile(`${outDir}/json-adapter-preflight.json`,JSON.stringify(report,null,2),{mode:0o600});
 if(!report.passed)throw Error('Structured adapter preflight failed; stop before GUI/replay budget');
}
