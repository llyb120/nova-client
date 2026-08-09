#!/usr/bin/env node
// Real-session chronological A/B for using corrected OnlineEditModel as the prefetch predictor.
import { spawn } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";

const REPO = resolve(import.meta.dirname, "..");
const SESSION_DIR = join(homedir(), ".nova", "alkaid", "sessions");
const CURRENT_MODEL_DIR = join(homedir(), ".nova", "alkaid", "context-learning");
const NAPI = join(REPO, "src-tauri", "resources", "nova-tools-napi.node");
const RUN = join(tmpdir(), `edit-model-prefetch-ab-${process.pid}`);
const REPORT = join(REPO, "scripts", "edit-model-prefetch-real-ab.report.json");
const MAX_CALLS = Number(process.env.EDIT_PREFETCH_AB_CALLS ?? 100);
const TRAIN_RATIO = Number(process.env.EDIT_PREFETCH_TRAIN_RATIO ?? 0.65);
const REPEATS = Number(process.env.EDIT_PREFETCH_AB_REPEATS ?? 5);

function rel(raw) {
  if (!raw) return null;
  const s = String(raw).replaceAll("\\", "/");
  if (!s.startsWith("/")) return s;
  const marker = "/nova-client/", index = s.indexOf(marker);
  return index >= 0 ? s.slice(index + marker.length) : null;
}
function history() {
  const calls = [], traces = [];
  for (const path of readdirSync(SESSION_DIR).filter(x => x.endsWith(".slim.json")).map(x => join(SESSION_DIR, x)).sort((a,b) => statSync(a).mtimeMs - statSync(b).mtimeMs)) {
    let data; try { data = JSON.parse(readFileSync(path, "utf8")); } catch { continue; }
    let pending = null;
    for (const message of data.fullMessages ?? []) for (const part of message.content ?? []) {
      if (part?.type !== "toolCall") continue;
      if (part.name === "fast_context") {
        const args = part.arguments ?? {};
        if (args.keywords?.length || args.task || args.files?.length) {
          calls.push({ session: basename(path), args });
          pending = args;
        }
      } else if (part.name === "edit" && pending) {
        const edit = rel(part.arguments?.path);
        if (edit && existsSync(join(REPO, edit))) traces.push({ context: pending, edit });
        pending = null;
      }
    }
  }
  return { calls: calls.slice(-MAX_CALLS), traces: traces.slice(-40) };
}
const runner = `
import { callNapiTool } from ${JSON.stringify(join(REPO, "scripts", "nova-napi-tools.mjs"))};
const mode=process.env.MODE, rows=JSON.parse(process.env.ROWS), elapsed=[];
if(mode==='train') for(const row of rows){await callNapiTool('fast_context',${JSON.stringify(REPO)},row.context).catch(()=>{});await callNapiTool('observe_context_feedback',${JSON.stringify(REPO)},{action:'edit',path:row.edit}).catch(()=>{});await callNapiTool('observe_context_feedback',${JSON.stringify(REPO)},{action:'settle'}).catch(()=>{});}
else for(const row of rows){const s=Date.now();await callNapiTool('fast_context',${JSON.stringify(REPO)},row.args).catch(()=>{});elapsed.push(Date.now()-s);}
await new Promise(r=>setTimeout(r,300));
const metrics=await callNapiTool('prefetch_metrics',${JSON.stringify(REPO)}).catch(()=>null);
console.log(JSON.stringify({elapsed,totalMs:elapsed.reduce((a,b)=>a+b,0),metrics}));`;
function run(name, mode, rows, dir, prefetch) {
  return new Promise(resolveRun => {
    const env = { ...process.env, MODE: mode, ROWS: JSON.stringify(rows), NOVA_TOOLS_NAPI_PATH: NAPI,
      NOVA_CONTEXT_LEARNING: "1", NOVA_CONTEXT_LEARNING_OWNER: "1", NOVA_CONTEXT_LEARNING_DIR: dir,
      NOVA_CONTEXT_EDIT_RANKNET_DIRECTION: "corrected", NOVA_CTX_PREFETCH: prefetch ? "1" : "0",
      NOVA_CTX_PREFETCH_MODEL: "edit", NOVA_CTX_DEBUG_STATS: "1" };
    const child = spawn(process.execPath, ["--input-type=module", "-e", runner], { cwd: REPO, env, stdio: ["ignore", "pipe", "pipe"] });
    let out="", err=""; child.stdout.on("data", d => out += d); child.stderr.on("data", d => err += d);
    child.on("close", code => { let result={}; try { result=JSON.parse(out.trim().split("\n").at(-1)); } catch {} resolveRun({name,code,...result,stderrTail:err.slice(-1200)}); });
  });
}

rmSync(RUN,{recursive:true,force:true}); mkdirSync(RUN,{recursive:true});
const modelName = readdirSync(CURRENT_MODEL_DIR).find(x => x.endsWith(".json")); if(!modelName) throw new Error("missing learning model filename");
const seed = join(RUN,"seed"); mkdirSync(seed); writeFileSync(join(seed,modelName),JSON.stringify({version:3,weights:[1.2,1,1.2,.7,.8,.4,.2,.5,.6],bias:-2,observations:0,positives:0}));
const {calls,traces}=history(); const split=Math.max(6,Math.min(calls.length-6,Math.floor(calls.length*TRAIN_RATIO)));
const trainCalls=calls.slice(0,split), holdout=calls.slice(split);
const training=await run("corrected-edit-training","train",traces,seed,false);
const trained=JSON.parse(readFileSync(join(seed,modelName),"utf8")); const edit=trained.edit??trained;
const rounds=[];
for(let repeat=1;repeat<=REPEATS;repeat++){
  const a=join(RUN,`a-${repeat}`),b=join(RUN,`b-${repeat}`);mkdirSync(a);mkdirSync(b);cpSync(seed,a,{recursive:true});cpSync(seed,b,{recursive:true});
  const [A,B]=await Promise.all([run(`A-off-${repeat}`,"eval",holdout,a,false),run(`B-edit-prefetch-${repeat}`,"eval",holdout,b,true)]);
  rounds.push({repeat,A,B,deltaMs:(B.totalMs??0)-(A.totalMs??0)});
}
const sum=(arm,key)=>rounds.reduce((s,r)=>s+(r[arm]?.[key]??0),0); const deltas=rounds.map(r=>r.deltaMs).sort((a,b)=>a-b);
const report={ranAt:new Date().toISOString(),predictor:"corrected OnlineEditModel",repeats:REPEATS,totalCalls:calls.length,trainCalls:trainCalls.length,holdoutCalls:holdout.length,trainingTraces:traces.length,editModel:{observations:edit.observations,positives:edit.positives,bias:edit.bias,weights:edit.weights},training,rounds,summary:{A:{totalMs:sum("A","totalMs")},B:{totalMs:sum("B","totalMs")},deltaMs:sum("B","totalMs")-sum("A","totalMs"),deltaPct:(sum("B","totalMs")-sum("A","totalMs"))/Math.max(1,sum("A","totalMs"))*100,roundMedianDeltaMs:deltas[Math.floor(deltas.length/2)]??0,roundWins:deltas.filter(x=>x<0).length,predicted:rounds.reduce((s,r)=>s+(r.B.metrics?.predicted??0),0),useful:rounds.reduce((s,r)=>s+(r.B.metrics?.useful??0),0),avoidedDiskReads:rounds.reduce((s,r)=>s+(r.B.metrics?.avoidedDiskReads??0),0),wastedBytes:rounds.reduce((s,r)=>s+(r.B.metrics?.wastedBytes??0),0)}};
writeFileSync(REPORT,JSON.stringify(report,null,2));console.log(JSON.stringify({report:REPORT,summary:report.summary,editModel:report.editModel},null,2));
if(!process.env.NOVA_KEEP_AB_TEMP)rmSync(RUN,{recursive:true,force:true});
