#!/usr/bin/env node
import { spawn } from 'node:child_process';
import { readFileSync, readdirSync, statSync, writeFileSync, mkdirSync, rmSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const REPO=resolve(import.meta.dirname,'..');
const NAPI=join(REPO,'src-tauri/resources/nova-tools-napi.node');
const SESSION_DIR=join(homedir(),'.nova/alkaid/sessions');
const MODEL_DIR=join(homedir(),'.nova/alkaid/context-learning');
const REPORT_MODEL=join(REPO,'scripts/context-learning-ab.report.json');
let trainedModel=null;try{trainedModel=JSON.parse(readFileSync(REPORT_MODEL,'utf8')).pretraining?.model??null}catch{}
const calls=[];const seen=new Set();
for(const f of readdirSync(SESSION_DIR).filter(x=>x.endsWith('.slim.json')).map(x=>join(SESSION_DIR,x)).sort((a,b)=>statSync(b).mtimeMs-statSync(a).mtimeMs).slice(0,50)){
 let d;try{d=JSON.parse(readFileSync(f,'utf8'))}catch{continue}
 for(const m of d.fullMessages??[])for(const p of m.content??[]){if(p?.type!=='toolCall'||p.name!=='fast_context')continue;const a=p.arguments??{};const k=JSON.stringify([a.keywords??[],a.task??'',a.files??[]]);if(seen.has(k))continue;seen.add(k);calls.push(a)}
}
const selected=calls.slice(0,20);
const MIN_OBSERVATIONS=Number(process.env.PREFETCH_AB_MIN_OBSERVATIONS??42);
const runner=`
import { callNapiTool } from ${JSON.stringify(join(REPO,'scripts/nova-napi-tools.mjs'))};
const calls=JSON.parse(process.env.CALLS);let elapsed=[];
for(const args of calls){const s=Date.now();await callNapiTool('fast_context',${JSON.stringify(REPO)},args).catch(()=>{});elapsed.push(Date.now()-s)}
console.log(JSON.stringify({elapsed,total:elapsed.reduce((a,b)=>a+b,0)}));
`;
function arm(name,prefetch){return new Promise((resolveArm)=>{const dir=join(tmpdir(),`ctx-prefetch-ab-${process.pid}-${name}`);mkdirSync(dir,{recursive:true});
 // Copy trained model files so both arms use identical OnlineEditModel.
 try{for(const f of readdirSync(MODEL_DIR))if(f.endsWith('.json')){const model=structuredClone(trainedModel??JSON.parse(readFileSync(join(MODEL_DIR,f),'utf8')));model.observations=Math.max(Number(model.observations)||0,MIN_OBSERVATIONS);writeFileSync(join(dir,f),JSON.stringify(model))}}catch{}
 const env={...process.env,NOVA_TOOLS_NAPI_PATH:NAPI,NOVA_CONTEXT_LEARNING:'1',NOVA_CONTEXT_LEARNING_OWNER:'1',NOVA_CONTEXT_LEARNING_DIR:dir,NOVA_CTX_PREFETCH:prefetch?'1':'0',NOVA_CTX_DEBUG_STATS:'1',CALLS:JSON.stringify(selected)};
 const p=spawn(process.execPath,['--input-type=module','-e',runner],{cwd:REPO,env,stdio:['ignore','pipe','pipe']});let out='',err='';p.stdout.on('data',d=>out+=d);p.stderr.on('data',d=>err+=d);p.on('close',()=>{const stats=[...err.matchAll(/\[ctx-stats\] source_disk_reads=(\d+) source_cache_hits=(\d+) prefetch_disk_reads=(\d+) prefetch_cache_hits=(\d+) demand_disk_reads=(\d+) demand_cache_hits=(\d+)/g)].map(m=>m.slice(1).map(Number));let result={};try{result=JSON.parse(out.trim().split('\n').at(-1))}catch{}const last=stats.at(-1)??[0,0,0,0,0,0];rmSync(dir,{recursive:true,force:true});resolveArm({name,prefetch,...result,sourceDiskReads:last[0],sourceCacheHits:last[1],prefetchDiskReads:last[2],prefetchCacheHits:last[3],demandDiskReads:last[4],demandCacheHits:last[5],stderrTail:err.slice(-500)})})})}
const A=await arm('A-off',false);const B=await arm('B-on',true);const report={ranAt:new Date().toISOString(),calls:selected.length,minObservations:MIN_OBSERVATIONS,A,B,delta:{totalMs:B.total-A.total,demandDiskReads:B.demandDiskReads-A.demandDiskReads,demandCacheHits:B.demandCacheHits-A.demandCacheHits,prefetchDiskReads:B.prefetchDiskReads-A.prefetchDiskReads}};const path=join(REPO,'scripts/fast-context-prefetch-real-ab.report.json');writeFileSync(path,JSON.stringify(report,null,2));console.log(JSON.stringify({path,...report},null,2));
