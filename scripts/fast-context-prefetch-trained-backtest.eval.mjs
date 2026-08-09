#!/usr/bin/env node
// Chronological real-session train/holdout backtest for OnlinePrefetchModel.
import { spawn } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";

const REPO = resolve(import.meta.dirname, "..");
const SESSION_DIR = join(homedir(), ".nova", "alkaid", "sessions");
const NAPI = join(REPO, "src-tauri", "resources", "nova-tools-napi.node");
const RUN = join(tmpdir(), `nova-prefetch-trained-backtest-${process.pid}`);
const REPORT = join(REPO, "scripts", "fast-context-prefetch-trained-backtest.report.json");
const MAX_CALLS = Number(process.env.PREFETCH_BACKTEST_CALLS ?? 60);
const TRAIN_RATIO = Number(process.env.PREFETCH_TRAIN_RATIO ?? 0.65);

function extractCalls() {
  const files = readdirSync(SESSION_DIR)
    .filter((name) => name.endsWith(".slim.json"))
    .map((name) => join(SESSION_DIR, name))
    .sort((a, b) => statSync(a).mtimeMs - statSync(b).mtimeMs);
  const rows = [];
  for (const path of files) {
    let data;
    try { data = JSON.parse(readFileSync(path, "utf8")); } catch { continue; }
    for (const message of data.fullMessages ?? []) {
      for (const part of message.content ?? []) {
        if (part?.type !== "toolCall" || part.name !== "fast_context") continue;
        const args = part.arguments ?? {};
        if (!(args.keywords?.length || args.task || args.files?.length)) continue;
        rows.push({ session: basename(path), args });
      }
    }
  }
  return rows.slice(-MAX_CALLS);
}

const runner = `
import { callNapiTool } from ${JSON.stringify(join(REPO, "scripts/nova-napi-tools.mjs"))};
const calls = JSON.parse(process.env.CALLS); const elapsed=[];
for (const row of calls) { const s=Date.now(); await callNapiTool("fast_context", ${JSON.stringify(REPO)}, row.args).catch(()=>{}); elapsed.push(Date.now()-s); }
await new Promise(r=>setTimeout(r,250));
const metrics=await callNapiTool("prefetch_metrics", ${JSON.stringify(REPO)}).catch(()=>null);
console.log(JSON.stringify({elapsed,totalMs:elapsed.reduce((a,b)=>a+b,0),metrics}));
`;

function runSequence(name, calls, modelDir, prefetch) {
  return new Promise((resolveRun) => {
    const env = {
      ...process.env,
      NOVA_TOOLS_NAPI_PATH: NAPI,
      NOVA_CONTEXT_LEARNING: "1",
      NOVA_CONTEXT_LEARNING_OWNER: "1",
      NOVA_CONTEXT_LEARNING_DIR: modelDir,
      NOVA_CTX_PREFETCH: prefetch ? "1" : "0",
      CALLS: JSON.stringify(calls),
    };
    const child = spawn(process.execPath, ["--input-type=module", "-e", runner], { cwd: REPO, env, stdio: ["ignore", "pipe", "pipe"] });
    let stdout="", stderr="";
    child.stdout.on("data", (d)=>stdout+=d);
    child.stderr.on("data", (d)=>stderr+=d);
    child.on("close", (code)=>{
      let result={}; try { result=JSON.parse(stdout.trim().split("\n").at(-1)); } catch {}
      resolveRun({ name, code, ...result, stderrTail: stderr.slice(-1000) });
    });
  });
}

rmSync(RUN, { recursive:true, force:true }); mkdirSync(RUN, { recursive:true });
const calls=extractCalls();
const split=Math.max(6, Math.min(calls.length-6, Math.floor(calls.length*TRAIN_RATIO)));
const train=calls.slice(0,split), test=calls.slice(split);
const trainedDir=join(RUN,"trained"); mkdirSync(trainedDir,{recursive:true});
const training=await runSequence("train-prefetch-on", train, trainedDir, true);
const armA=join(RUN,"arm-a"), armB=join(RUN,"arm-b"); mkdirSync(armA); mkdirSync(armB);
if (existsSync(trainedDir)) { cpSync(trainedDir,armA,{recursive:true}); cpSync(trainedDir,armB,{recursive:true}); }
const A=await runSequence("A-trained-prefetch-off",test,armA,false);
const B=await runSequence("B-trained-prefetch-on",test,armB,true);
const pct=(d,b)=>b?d/b*100:null;
const report={
  ranAt:new Date().toISOString(), chronological:true, totalCalls:calls.length, trainCalls:train.length, holdoutCalls:test.length,
  trainSessions:[...new Set(train.map(x=>x.session))], holdoutSessions:[...new Set(test.map(x=>x.session))], training,A,B,
  delta:{ totalMs:(B.totalMs??0)-(A.totalMs??0), totalPct:pct((B.totalMs??0)-(A.totalMs??0),A.totalMs??0),
    avoidedDiskReads:(B.metrics?.avoidedDiskReads??0)-(A.metrics?.avoidedDiskReads??0), useful:(B.metrics?.useful??0)-(A.metrics?.useful??0),
    wastedBytes:(B.metrics?.wastedBytes??0)-(A.metrics?.wastedBytes??0) }
};
writeFileSync(REPORT,JSON.stringify(report,null,2));
console.log(JSON.stringify({report,...report},null,2));
if (!process.env.NOVA_KEEP_AB_TEMP) rmSync(RUN,{recursive:true,force:true});
