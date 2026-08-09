#!/usr/bin/env node
// Seed the pre-change real edit-trained model, train only the new prefetch head chronologically,
// then compare prefetch OFF/ON on real-session holdout calls.
import { spawn } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
const REPO=resolve(import.meta.dirname,"..");
const SESSION_DIR=join(homedir(),".nova","alkaid","sessions");
const CURRENT_MODEL_DIR=join(homedir(),".nova","alkaid","context-learning");
const OLD_REPORT=join(REPO,"scripts","context-learning-ab.report.json");
const NAPI=join(REPO,"src-tauri","resources","nova-tools-napi.node");
const RUN=join(tmpdir(),`nova-old-edit-prefetch-backtest-${process.pid}`);
const REPORT=join(REPO,"scripts","old-edit-trained-prefetch-backtest.report.json");
const MAX=Number(process.env.OLD_EDIT_BACKTEST_CALLS??72), RATIO=Number(process.env.OLD_EDIT_TRAIN_RATIO??0.75);
const oldEdit=JSON.parse(readFileSync(OLD_REPORT,"utf8")).pretraining?.model;
if(!oldEdit?.weights) throw new Error("missing pre-change edit model in context-learning-ab.report.json");
const modelName=readdirSync(CURRENT_MODEL_DIR).find(x=>x.endsWith(".json"));
if(!modelName) throw new Error("cannot determine repository learning model filename");
function calls(){const rows=[];for(const p of readdirSync(SESSION_DIR).filter(x=>x.endsWith('.slim.json')).map(x=>join(SESSION_DIR,x)).sort((a,b)=>statSync(a).mtimeMs-statSync(b).mtimeMs)){let d;try{d=JSON.parse(readFileSync(p,'utf8'))}catch{continue}for(const m of d.fullMessages??[])for(const x of m.content??[]){if(x?.type==='toolCall'&&x.name==='fast_context'&&(x.arguments?.keywords?.length||x.arguments?.task||x.arguments?.files?.length))rows.push({session:basename(p),args:x.arguments})}}return rows.slice(-MAX)}
const runner=`import {callNapiTool} from ${JSON.stringify(join(REPO,'scripts/nova-napi-tools.mjs'))};const calls=JSON.parse(process.env.CALLS),elapsed=[];for(const row of calls){const s=Date.now();await callNapiTool('fast_context',${JSON.stringify(REPO)},row.args).catch(()=>{});elapsed.push(Date.now()-s)}await new Promise(r=>setTimeout(r,250));const metrics=await callNapiTool('prefetch_metrics',${JSON.stringify(REPO)}).catch(()=>null);console.log(JSON.stringify({elapsed,totalMs:elapsed.reduce((a,b)=>a+b,0),metrics}));`;
function run(name,seq,dir,on){return new Promise(resolveRun=>{const env={...process.env,NOVA_TOOLS_NAPI_PATH:NAPI,NOVA_CONTEXT_LEARNING:'1',NOVA_CONTEXT_LEARNING_OWNER:'1',NOVA_CONTEXT_LEARNING_DIR:dir,NOVA_CTX_PREFETCH:on?'1':'0',CALLS:JSON.stringify(seq)};const p=spawn(process.execPath,['--input-type=module','-e',runner],{cwd:REPO,env,stdio:['ignore','pipe','pipe']});let o='',e='';p.stdout.on('data',d=>o+=d);p.stderr.on('data',d=>e+=d);p.on('close',code=>{let r={};try{r=JSON.parse(o.trim().split('\n').at(-1))}catch{}resolveRun({name,code,...r,stderrTail:e.slice(-500)})})})}
rmSync(RUN,{recursive:true,force:true});mkdirSync(RUN,{recursive:true});const all=calls(),split=Math.max(6,Math.min(all.length-6,Math.floor(all.length*RATIO))),train=all.slice(0,split),holdout=all.slice(split);
const seed=join(RUN,'seed');mkdirSync(seed);writeFileSync(join(seed,modelName),JSON.stringify(oldEdit,null,2));
const training=await run('train-prefetch-from-old-edit',train,seed,true);
const trainedFile=JSON.parse(readFileSync(join(seed,modelName),'utf8'));
const editAfter=trainedFile.edit??trainedFile;
const editUnchanged=JSON.stringify(editAfter.weights)===JSON.stringify(oldEdit.weights)&&editAfter.bias===oldEdit.bias&&editAfter.observations===oldEdit.observations&&editAfter.positives===oldEdit.positives;
const armA=join(RUN,'a'),armB=join(RUN,'b');mkdirSync(armA);mkdirSync(armB);cpSync(seed,armA,{recursive:true});cpSync(seed,armB,{recursive:true});const A=await run('A-old-edit-prefetch-off',holdout,armA,false),B=await run('B-old-edit-prefetch-on',holdout,armB,true);
const report={ranAt:new Date().toISOString(),oldEditSeed:{observations:oldEdit.observations,positives:oldEdit.positives,bias:oldEdit.bias,weights:oldEdit.weights},editUnchangedDuringPrefetchTraining:editUnchanged,totalCalls:all.length,trainCalls:train.length,holdoutCalls:holdout.length,trainSessions:[...new Set(train.map(x=>x.session))],holdoutSessions:[...new Set(holdout.map(x=>x.session))],training,A,B,delta:{totalMs:(B.totalMs??0)-(A.totalMs??0),totalPct:A.totalMs?((B.totalMs-A.totalMs)/A.totalMs*100):null,avoidedDiskReads:(B.metrics?.avoidedDiskReads??0)-(A.metrics?.avoidedDiskReads??0),useful:(B.metrics?.useful??0)-(A.metrics?.useful??0),wastedBytes:(B.metrics?.wastedBytes??0)-(A.metrics?.wastedBytes??0)}};writeFileSync(REPORT,JSON.stringify(report,null,2));console.log(JSON.stringify({report,...report},null,2));if(!process.env.NOVA_KEEP_AB_TEMP)rmSync(RUN,{recursive:true,force:true});
