// Small image-capability probe, not a desktop or browser benchmark.
import { readFile, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { MODEL, requestJson, sanitize } from './operator-commandcode-ab.mjs';
// Expected strings are only the oracle, never put in the model's text prompt.
export async function runProbe({apiKey,fixturesPath,outPath}){
  const images=JSON.parse(await readFile(fixturesPath,'utf8'));
  if(images.length!==2)throw new Error('Expected exactly two reviewed images');
  const report={kind:'commandcode-exact-model-image-capability-probe',model:MODEL,sourceCommit:process.env.GITHUB_SHA,modelCallsAttempted:0,modelCallsSucceeded:0,realGuiActions:0,rows:[],status:'running',startedAt:new Date().toISOString()};
  for(const fixture of images){
    if(createHash('sha256').update(Buffer.from(fixture.data,'base64')).digest('hex')!==fixture.sha256)throw new Error('Image digest mismatch');
    const row={id:fixture.id,imageSha256:fixture.sha256,expected:fixture.expected,correct:false};
    const body={model:MODEL,messages:[{role:'user',content:[{type:'text',text:'Read the six digits visible in this image. Return exactly one JSON object {"digits":"..."}. Do not guess from metadata. If the image is unavailable return {"digits":null}.'},{type:'image_url',image_url:{url:`data:image/png;base64,${fixture.data}`}}]}],temperature:0,reasoning_effort:'low',max_tokens:512,stream:false};
    report.modelCallsAttempted++;
    try{
      const r=await requestJson('/chat/completions',{apiKey,body});report.modelCallsSucceeded++;row.httpOk=true;row.elapsedMs=r.elapsedMs;row.returnedModel=r.data.model;row.usage=r.data.usage??null;row.answer=sanitize(r.data.choices?.[0]?.message?.content??'',apiKey);row.finishReason=r.data.choices?.[0]?.finish_reason;
      try{const answer=JSON.parse(row.answer.trim().replace(/^```(?:json)?\s*\n/,'').replace(/\n```$/,''));row.correct=answer.digits===fixture.expected;}catch{row.parseError=true;}
    }catch(e){row.httpOk=false;row.httpStatus=e.status??null;row.errorCode=e.code??null;row.error=sanitize(e.message,apiKey);row.elapsedMs=e.elapsedMs;}
    report.rows.push(row);await writeFile(outPath,sanitize(report,apiKey),{mode:0o600});
    if([400,401,403,422,429].includes(row.httpStatus))break;
  }
  report.status=report.rows.every(r=>r.httpOk)?'completed':'rejected';report.completedAt=new Date().toISOString();await writeFile(outPath,sanitize(report,apiKey),{mode:0o600});return report;
}
