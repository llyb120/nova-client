/** Live-model A/B on synthetic DOM decisions, NOT a real GUI benchmark.
 * Production Rust projector generates the inputs. No app settings are introduced.
 * Real mode requires explicit --codebuddy <executable> --model <parent model id>.
 */
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { performance } from 'node:perf_hooks';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const args = process.argv.slice(2);
const option = (name, fallback) => { const i=args.indexOf(name); return i<0 ? fallback : args[i+1]; };
const selfTest = args.includes('--self-test');
if(args.includes('--help')) {
  console.log('node scripts/operator-live-ab.mjs --codebuddy <executable> --model <current model ID> [--repeats 2] [--out report.json]\n--self-test uses deterministic decisions, no model or GUI. Real mode makes paid model calls using your existing CLI login.');
  process.exit(0);
}
const model=option('--model','');
const cli=option('--codebuddy','codebuddy');
const effort=option('--effort',null);
const repeats=Number(option('--repeats','2'));
if(!Number.isInteger(repeats)||repeats<1||repeats>10)throw new Error('--repeats must be 1..10');
if(!selfTest&&!model)throw new Error('Provide the current parent model ID with --model for a reproducible standalone benchmark; no app configuration is changed');
const out=resolve(option('--out',selfTest?'operator-ab-self-test.json':'operator-live-ab.json'));

async function collect(command,argv,cwd){
  return new Promise((done,reject)=>{
    const child=spawn(command,argv,{cwd,stdio:['ignore','pipe','inherit'],windowsHide:true});
    let output='';child.stdout.setEncoding('utf8');child.stdout.on('data',s=>{output+=s;if(output.length>4_000_000)child.kill();});
    child.once('error',reject);child.once('exit',code=>code===0?done(output):reject(new Error(`Fixture generator failed (${code})`)));
  });
}
async function infer(context){
  const cwd=await mkdtemp(join(tmpdir(),'nova-operator-live-ab-'));
  const env={...process.env};for(const key of ['NOVA_OPERATOR_SCOPE','NOVA_CONTEXT_SERVICE_ENDPOINT','NOVA_CONTEXT_SERVICE_TOKEN'])delete env[key];
  // A local JS CLI path also works on Windows without invoking cmd.exe.
  const js=/\.[cm]?js$/i.test(cli);
  const system='Return exactly one JSON object {"ref":"current DOM reference or null"}. Find order A102 in the CURRENT observation. Old observations are historical evidence and cannot supply a clickable reference. Never call tools.';
  const command=js?process.execPath:cli;
  const argv=[...(js?[resolve(cli)]:[]),'--acp','--acp-transport','stdio','--tools','','--strict-mcp-config','--mcp-config','{"mcpServers":{}}','--no-session-persistence','--system-prompt',system];
  let child,lines,timer;
  try {
    child=spawn(command,argv,{cwd,env,stdio:['pipe','pipe','ignore'],windowsHide:true,detached:process.platform!=='win32'});
    const pending=new Map();let next=0,text='',fatal;const usage=new Map();
    const fail=e=>{fatal=e;for(const p of pending.values())p.reject(e);pending.clear();};
    child.on('error',fail);child.stdin.on('error',fail);child.on('exit',code=>fail(new Error(`ACP exited (${code})`)));
    const send=v=>child.stdin.write(`${JSON.stringify({jsonrpc:'2.0',...v})}\n`);
    const call=(method,params)=>new Promise((resolve,reject)=>{if(fatal)return reject(fatal);const id=++next;pending.set(id,{resolve,reject});send({id,method,params});});
    lines=createInterface({input:child.stdout});
    lines.on('line',line=>{
      if(line.length>4_000_000)return fail(new Error('Oversized ACP line'));
      let m;try{m=JSON.parse(line);}catch{return;}
      if(m.method==='session/update'){
        const u=m.params?.update;
        if(['tool_call','tool_call_update'].includes(u?.sessionUpdate))return fail(new Error('Decision agent attempted a tool'));
        if(u?.sessionUpdate==='agent_message_chunk'){
          text+=u.content?.text??'';if(text.length>48_000)return fail(new Error('Oversized answer'));
          const meta=u._meta??m.params?._meta;const id=meta?.['codebuddy.ai/messageId'];
          if(id&&Number.isFinite(meta?.usage?.prompt_tokens)&&Number.isFinite(meta?.usage?.completion_tokens))usage.set(id,meta.usage);
        }
      }else if(m.method&&m.id!==undefined){
        if(m.method==='session/request_permission')send({id:m.id,result:{outcome:{outcome:'cancelled'}}});
        else send({id:m.id,error:{code:-32601,message:'No client tools in benchmark'}});
      }else if(pending.has(m.id)){
        const p=pending.get(m.id);pending.delete(m.id);m.error?p.reject(new Error(m.error.message??'ACP error')):p.resolve(m.result);
      }
    });
    timer=setTimeout(()=>fail(new Error('ACP decision timeout')),120_000);
    await call('initialize',{protocolVersion:1,clientInfo:{name:'nova-operator-ab',version:'1'},clientCapabilities:{}});
    const session=await call('session/new',{cwd,mcpServers:[]});if(!session.sessionId)throw new Error('Missing session ID');
    const set=await call('session/set_config_option',{sessionId:session.sessionId,configId:'model',value:model});
    if(set.configOptions?.find(o=>o.id==='model')?.currentValue!==model)throw new Error('Inherited model was not confirmed');
    if(effort){
      const set=await call('session/set_config_option',{sessionId:session.sessionId,configId:'thought_level',value:effort});
      if(set.configOptions?.find(o=>o.id==='thought_level')?.currentValue!==effort)throw new Error('Reasoning effort not confirmed');
    }
    const result=await call('session/prompt',{sessionId:session.sessionId,prompt:[{type:'text',text:JSON.stringify(context)}]});
    if(result.stopReason!=='end_turn')throw new Error(`Unexpected stop: ${result.stopReason}`);
    return {text,usage:usage.size?[...usage.values()]:null};
  } finally {
    clearTimeout(timer);lines?.close();
    if(child?.pid){
      if(process.platform==='win32')spawn('taskkill',['/PID',String(child.pid),'/T','/F'],{stdio:'ignore',windowsHide:true});
      else {try{process.kill(-child.pid,'SIGKILL');}catch{child.kill('SIGKILL');}}
    }
    await rm(cwd,{recursive:true,force:true});
  }
}
const fixtures=JSON.parse(await collect('cargo',['run','--quiet','--manifest-path','bench/operator-harness/Cargo.toml','--','--emit-live-cases'],root));
const report={kind:selfTest?'protocol-independent-benchmark-self-test':'live-codebuddy-synthetic-dom-ab',model:selfTest?null:model,reasoningEffort:effort,modelCalls:0,realGuiActions:0,method:'paired AB/BA ordering; fresh isolated ACP process in both arms; production projector B; historical observations A; not a GUI or unmodified-agent benchmark',rows:[]};
for(let repeat=0;repeat<repeats;repeat++)for(const fixture of fixtures){
  const order=repeat%2?['B','A']:['A','B'];
  for(const arm of order){
    const started=performance.now();const row={repeat,case:fixture.id,arm,inputBytes:Buffer.byteLength(JSON.stringify(fixture[arm])),correct:false};
    try{
      if(!selfTest)report.modelCalls++;
      const response=selfTest?{text:JSON.stringify({ref:fixture.expectedRef}),usage:null}:await infer(fixture[arm]);
      const decision=JSON.parse(response.text.trim().replace(/^```(?:json)?\s*\n/,'').replace(/\n```$/,''));
      row.correct=decision.ref===fixture.expectedRef;row.usage=response.usage;
    }catch(e){row.error=e.message;}
    row.elapsedMs=performance.now()-started;report.rows.push(row);
    await writeFile(out,JSON.stringify(report,null,2),{mode:0o600});
  }
}
console.log(JSON.stringify({report:out,modelCalls:report.modelCalls,realGuiActions:0,correct:report.rows.filter(r=>r.correct).length,total:report.rows.length}));
if(selfTest&&report.rows.some(r=>!r.correct))process.exitCode=1;
