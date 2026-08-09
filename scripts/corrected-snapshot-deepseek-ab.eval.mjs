#!/usr/bin/env node
// Strict paired A/B: one corrected RankNet training replay, immutable before/after snapshots,
// then repeated DeepSeek runs over the same real-session-derived cases.
import { spawn } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";

const REPO = resolve(import.meta.dirname, "..");
const MODEL = process.argv.slice(2).find((arg) => !arg.startsWith("--")) || "opencode/deepseek-v4-flash";
const REPEATS = Number(process.env.NOVA_AB_REPEATS || 5);
const SESSION_DIR = join(homedir(), ".nova", "alkaid", "sessions");
const CURRENT_MODEL_DIR = join(homedir(), ".nova", "alkaid", "context-learning");
const NAPI = join(REPO, "src-tauri", "resources", "nova-tools-napi.node");
const RUN = join(tmpdir(), `corrected-snapshot-deepseek-ab-${process.pid}`);
const BEFORE_DIR = join(RUN, "before");
const AFTER_DIR = join(RUN, "after");
const REPORT = join(REPO, "scripts", "corrected-snapshot-deepseek-ab.report.json");

const CASES = [
  ["R1", "6097d0a6-916d-48ad-ac16-b68c320116d6.slim.json", "这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 fast_context_run、co_changed_files，分析还能如何提高 fast_context 效率并减少后续 read。只基于工具输出回答，不再调用其它检索工具，不改代码。"],
  ["R2", "6097d0a6-916d-48ad-ac16-b68c320116d6.slim.json", "这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 native_edit、edit、fast_context，分析原生 edit 使用独立工具实现时如何消费 fast_context 编辑锚点。只基于工具输出回答，不再检索，不改代码。"],
  ["R3", "09774430-c287-4f78-bfc0-bfc9116cd4e7.slim.json", "这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 buildAlkaidSystemPrompt、fast_context，分析为什么 Agent 开始会用 fast_context、后续却可能不再使用。只基于工具输出回答，不再检索，不改代码。"],
  ["R4", "bd12ca61-0c21-46f3-ad2d-cf85194a7a05.slim.json", "这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 remote、model、switch、session，task 描述理解远程控制会话与模型切换功能，分析在远程控制会话内支持切换模型需要改哪些地方。只基于工具输出回答，不再调用其它检索工具，不改代码。"],
  ["R5", "84cfea80-586d-499f-802b-6d2bd8623476.slim.json", "这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 证据链、画布、canvas、evidence，task 描述证据链画布组件的布局与渲染优化占屏空间，分析该怎么优化。只基于工具输出回答，不再调用其它检索工具，不改代码。"],
  ["R6", "60123777-76d9-4124-a601-773d9b618065.slim.json", "这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 edit_files、executeTools、parallel tool、applyEdit，task 描述查找 pi 工具执行代码判断 edit 是否并发执行，分析 pi 的 edit 是否也会并发执行。只基于工具输出回答，不再调用其它检索工具，不改代码。"],
  ["R7", "bf15e349-390e-4184-a014-dec30badb709.slim.json", "这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 远程控制、会话排序、remote control、session，task 描述远程控制的会话列表排序改为按用户最后输入提示词时间倒序，分析当前排序逻辑和需要改的地方。只基于工具输出回答，不再调用其它检索工具，不改代码。"],
  ["R8", "da76f639-644d-471c-85dc-a0b2f55ae929.slim.json", "这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 workflow、工作流、edge、transition、handoff，task 描述简化工作流配置让引擎隐式补充会话结论接力，分析当前工作流配置和运行引擎的结构。只基于工具输出回答，不再调用其它检索工具，不改代码。"],
].map(([id, sourceSession, prompt]) => ({ id, sourceSession, prompt }));

function sessions() {
  return readdirSync(SESSION_DIR).filter((x) => x.endsWith(".slim.json"))
    .map((x) => join(SESSION_DIR, x)).sort((a, b) => statSync(a).mtimeMs - statSync(b).mtimeMs);
}
function rel(raw) {
  if (!raw) return null;
  const s = String(raw).replaceAll("\\", "/");
  if (!s.startsWith("/")) return s;
  const i = s.indexOf("/nova-client/");
  return i >= 0 ? s.slice(i + "/nova-client/".length) : null;
}
function trainingTraces() {
  const traces = [];
  for (const path of sessions()) {
    let data; try { data = JSON.parse(readFileSync(path, "utf8")); } catch { continue; }
    let pending = null;
    for (const message of data.fullMessages ?? []) for (const part of message.content ?? []) {
      if (part?.type !== "toolCall") continue;
      if (part.name === "fast_context") pending = part.arguments ?? {};
      else if (part.name === "edit" && pending) {
        const edit = rel(part.arguments?.path);
        if (edit && existsSync(join(REPO, edit))) traces.push({ session: basename(path), context: pending, edit });
        pending = null;
      }
    }
  }
  return traces.slice(-30);
}
function child(script, env, timeoutMs = 180_000) {
  return new Promise((resolveChild) => {
    const p = spawn(process.execPath, ["--input-type=module", "-e", script], { cwd: REPO, env: { ...process.env, ...env }, stdio: ["ignore", "pipe", "pipe"] });
    let out = "", err = "", done = false;
    const finish = (extra = {}) => { if (done) return; done = true; clearTimeout(timer); resolveChild({ code: p.exitCode, stdout: out, stderr: err.slice(-1500), ...extra }); };
    const timer = setTimeout(() => { try { p.kill("SIGKILL"); } catch {} finish({ timeout: true }); }, timeoutMs);
    p.stdout.on("data", (d) => out += d); p.stderr.on("data", (d) => err += d);
    p.on("close", (code) => finish({ code }));
  });
}
async function train(dir, traces) {
  const script = `import {callNapiTool} from ${JSON.stringify(join(REPO, "scripts/nova-napi-tools.mjs"))};const rows=JSON.parse(process.env.ROWS);let contexts=0,updates=0;for(const row of rows){await callNapiTool('fast_context',${JSON.stringify(REPO)},row.context).catch(()=>{});contexts++;const r=await callNapiTool('observe_context_feedback',${JSON.stringify(REPO)},{action:'edit',path:row.edit}).catch(()=>null);updates+=Number(r?.updated??0)}await callNapiTool('observe_context_feedback',${JSON.stringify(REPO)},{action:'settle'}).catch(()=>{});console.log(JSON.stringify({contexts,updates}));`;
  return child(script, { ROWS: JSON.stringify(traces), NOVA_TOOLS_NAPI_PATH: NAPI, NOVA_CONTEXT_LEARNING: "1", NOVA_CONTEXT_LEARNING_OWNER: "1", NOVA_CONTEXT_LEARNING_DIR: dir, NOVA_CONTEXT_EDIT_RANKNET_DIRECTION: "corrected", NOVA_CTX_PREFETCH: "0" }, 300_000);
}
function runCase(testCase, arm, repeat) {
  return new Promise((resolveCase) => {
    const dir = arm === "A" ? BEFORE_DIR : AFTER_DIR;
    const env = { ...process.env, NOVA_TOOLS_NAPI_PATH: NAPI, NOVA_CONTEXT_LEARNING: "0", NOVA_CONTEXT_LEARNING_OWNER: "0", NOVA_CONTEXT_LEARNING_DIR: dir, NOVA_CONTEXT_EDIT_RANKNET_DIRECTION: "corrected", NOVA_CTX_PREFETCH: "0", LYRA_SPECULATE: "0" };
    const p = spawn(process.execPath, [join(REPO, "scripts", "alkaid.mjs")], { cwd: REPO, env, stdio: ["pipe", "pipe", "pipe"] });
    const start = Date.now(); let buffer = "", stderr = "", usage = null, text = "", done = false; const tools = [];
    const finish = (extra = {}) => { if (done) return; done = true; clearTimeout(timer); try { p.kill(); } catch {} resolveCase({ id: testCase.id, arm, repeat, wallMs: Date.now() - start, usage, tools, textTail: text.slice(-200), stderr: stderr.slice(-800), ...extra }); };
    const timer = setTimeout(() => finish({ timeout: true }), 150_000);
    p.stdout.on("data", (d) => { buffer += d; let n; while ((n = buffer.indexOf("\n")) >= 0) { const line = buffer.slice(0, n); buffer = buffer.slice(n + 1); let m; try { m = JSON.parse(line); } catch { continue; } if (m.type === "tool_start") tools.push({ name: m.name }); else if (m.type === "tool_end") { const t = [...tools].reverse().find((x) => x.name === m.name && x.durationMs === undefined); if (t) { t.durationMs = m.durationMs ?? 0; t.isError = !!m.isError; } } else if (m.type === "done") { usage = m.usage ?? null; text = m.text ?? ""; finish({ reportedWallMs: m.wallMs }); } else if (m.type === "error") finish({ error: m.error }); } });
    p.stderr.on("data", (d) => stderr += d); p.on("close", (code) => { if (!done) finish({ exitCode: code, error: code ? `exit ${code}` : undefined }); });
    p.stdin.end(`${JSON.stringify({ prompt: testCase.prompt, model: MODEL, readOnly: true, cwd: REPO, sessionId: `corrected-snapshot-${repeat}-${testCase.id}-${arm}-${Date.now()}` })}\n`);
  });
}
function compact(r) { return { wallMs: r.wallMs, input: r.usage?.input ?? 0, output: r.usage?.output ?? 0, cacheRead: r.usage?.cacheRead ?? 0, totalTokens: r.usage?.totalTokens ?? 0, toolCalls: r.tools.length, fastContextCalls: r.tools.filter((t) => t.name === "fast_context").length, toolTimeMs: r.tools.reduce((s, t) => s + (t.durationMs ?? 0), 0), timeout: !!r.timeout, error: r.error }; }
function sum(rows, arm, key) { return rows.reduce((s, row) => s + row[arm][key], 0); }
function stats(values) { const sorted = [...values].sort((a,b)=>a-b), mean = values.reduce((a,b)=>a+b,0)/Math.max(1,values.length); return { mean, median: sorted[Math.floor(sorted.length/2)] ?? 0, min: sorted[0] ?? 0, max: sorted.at(-1) ?? 0 }; }

rmSync(RUN, { recursive: true, force: true }); mkdirSync(BEFORE_DIR, { recursive: true }); mkdirSync(AFTER_DIR, { recursive: true });
const modelName = readdirSync(CURRENT_MODEL_DIR).find((x) => x.endsWith(".json")); if (!modelName) throw new Error("cannot determine learning model filename");
const defaultEdit = { version: 3, weights: [1.2,1,1.2,.7,.8,.4,.2,.5,.6], bias: -2, observations: 0, positives: 0 };
writeFileSync(join(BEFORE_DIR, modelName), JSON.stringify(defaultEdit)); cpSync(join(BEFORE_DIR, modelName), join(AFTER_DIR, modelName));
const traces = trainingTraces(); const training = await train(AFTER_DIR, traces);
const beforeFile = JSON.parse(readFileSync(join(BEFORE_DIR, modelName), "utf8")); const afterFile = JSON.parse(readFileSync(join(AFTER_DIR, modelName), "utf8"));
const beforeModel = beforeFile.edit ?? beforeFile, afterModel = afterFile.edit ?? afterFile;
const rows = []; const started = Date.now();
for (let repeat = 1; repeat <= REPEATS; repeat++) {
  const round = await Promise.all(CASES.map(async (testCase) => { const [a,b] = await Promise.all([runCase(testCase,"A",repeat),runCase(testCase,"B",repeat)]); return { repeat, id:testCase.id, sourceSession:testCase.sourceSession, A:compact(a), B:compact(b) }; }));
  rows.push(...round);
}
const keys = ["wallMs","input","output","cacheRead","totalTokens","toolCalls","fastContextCalls","toolTimeMs"];
const totals = { A:{}, B:{}, delta:{} }; for (const key of keys) { totals.A[key]=sum(rows,"A",key); totals.B[key]=sum(rows,"B",key); totals.delta[key]=totals.B[key]-totals.A[key]; }
const paired = {}; for (const key of ["wallMs","input","output","totalTokens","toolTimeMs"]) paired[key]=stats(rows.map((r)=>r.B[key]-r.A[key]));
const snapshotStable = JSON.stringify(JSON.parse(readFileSync(join(AFTER_DIR, modelName),"utf8"))) === JSON.stringify(afterFile);
const report = { ranAt:new Date().toISOString(), model:MODEL, repeats:REPEATS, cases:CASES.length, pairedRuns:rows.length, evaluationElapsedMs:Date.now()-started, direction:"corrected-add", prefetch:false, immutableEvaluation:true, training:{ traces:traces.length, sessions:[...new Set(traces.map((x)=>x.session))], result:training, beforeModel, afterModel }, snapshotStable, totals, paired, rows };
writeFileSync(REPORT, JSON.stringify(report,null,2)); console.log(JSON.stringify({ report, summary:{ repeats:REPEATS, pairedRuns:rows.length, trainingObservations:afterModel.observations, snapshotStable, totals, paired } },null,2));
if (!process.env.NOVA_KEEP_AB_TEMP) rmSync(RUN,{recursive:true,force:true});
