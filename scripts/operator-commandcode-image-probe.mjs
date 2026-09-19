// Small image-capability probe, not a desktop or browser benchmark.
import { writeFile } from 'node:fs/promises';
import { MODEL, requestJson, sanitize } from './operator-commandcode-ab.mjs';
const fixtures=[{"id":"image-1","expected":"731946","mimeType":"image/png","width":360,"height":110,"sha256":"fb1815916e924c93d6e93f7ce5f43518517c7c3573a346b4dd04a6bdcbff213e","data":"iVBORw0KGgoAAAANSUhEUgAAAWgAAABuAQAAAAAisLTRAAAB3klEQVR42u2YP3KcMBSHP8mKTcd2cZMJR9jS3eKb+CQOm4vEN4k8kyKlj6AjyB3rETwXEmzwBu1ukZl4DA0IPh7v/d4fGJRwxqZ5nzTtGezX82wbdYaED+9VwYVe6IX+2LSkE49NWjaTyzsZNw8bx0baCxFKEV+KZ7O/Lj/e2nY88Ru6NKn9Mb9baggE8OCytIMQj3pwYLN0NT4gjvUm74kOPQAlYBFkli7F8UXDJfh1tKqmvkxtu7pScANQNzQHj/5bLi1Qg0BvcrSdRFWcUCd6UKc/vOGAlqS7s2E1aD9Db6EfdK+pMGLyNWhMitS2EF66eVo0cD0uayim2TyM8m6Q0Tfgc9kBeADgqUn152bp3gwlvXoGZU7Qe/xgsQ0LhwqV447NQSS+gW02dqdK/qbCP4BoYHGZt2+6FjCAr4NADt/X/KgB8cS6vyxtQKaUKSdQ5C85jgnXp02B3gEYos3+retjmmVXX2b4Ptzbj6eGSoQHKTY/mG+M3Sq99pjryAW444fwdjVQITgLIcN62yWazHuCV18bpvsqq15Lu6xAR4dkqCLoPFOI3THe+yZlVZBXyO8uopzacBUVGOCxZUWgJbb3Kb6Nce2wC3tjBlim7zXzYpZqqqqUfWdql+qqqrrTt4ksrmZvr8neqIneqLfIF2r/Ld/M/Iu/m79Bm1QAHQ7sm9qAAAAAElFTkSuQmCC"}];
// Fixtures are loaded from a checked, separate JSON file in CI. The in-memory
// expected strings are used only as the oracle and are never put in the prompt.
export async function runProbe({apiKey,fixturesPath,outPath}){
  const {readFile}=await import('node:fs/promises');
  const images=JSON.parse(await readFile(fixturesPath,'utf8'));
  if(images.length!==2)throw new Error('Expected exactly two reviewed images');
  const report={kind:'commandcode-exact-model-image-capability-probe',model:MODEL,sourceCommit:process.env.GITHUB_SHA,modelCallsAttempted:0,modelCallsSucceeded:0,realGuiActions:0,rows:[],status:'running',startedAt:new Date().toISOString()};
  for(const fixture of images){
    const {createHash}=await import('node:crypto');
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
