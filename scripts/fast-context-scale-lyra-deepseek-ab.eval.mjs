// Paired model-level A/B through the native Lyra Rust CLI and configured DeepSeek model.
import { spawn } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";

const REPO = join(import.meta.dirname, "..");
const EXE = join(REPO, ".cargo-target", "debug", "nova.exe");
const MODEL = process.env.LYRA_AB_MODEL ?? "deepseek/deepseek-v4-flash";
const REPEATS = Number(process.env.LYRA_AB_REPEATS ?? 2);
const CASES = [
  { id: "scope", prompt: "先且只调用一次 fast_context，keywords 使用 search_text_scopes、scope_dirs，task 说明分析 scoped 文本检索如何约束扫描范围。然后仅依据工具输出说明调用链，不要再调用任何工具。", expect: ["search_text_scopes", "scope_dirs"] },
  { id: "graph", prompt: "先且只调用一次 fast_context，keywords 使用 search_text_scopes、reverse_from_files，task 说明分析 scoped 检索与反向 import 图。然后仅依据输出解释两者如何降低大仓库成本，不要再调用工具。", expect: ["search_text_scopes", "reverse_from_files"] },
  { id: "packing", prompt: "先且只调用一次 fast_context，keywords 使用 UnitCandidate、scale_optimizations_enabled，task 说明分析候选单元的预算打包。然后仅依据输出总结算法，不要再调用工具。", expect: ["UnitCandidate", "scale_optimizations_enabled"] },
];

function run(test, arm, repeat) {
  return new Promise((resolve) => {
    const child = spawn(EXE, ["lyra"], { cwd: REPO, env: { ...process.env, NOVA_CTX_SCALE_OPT: arm === "B" ? "1" : "0", LYRA_SPECULATE: "0" }, stdio: ["pipe", "pipe", "pipe"] });
    const started = Date.now(); let buf = "", stderr = "", text = "", usage = null; const tools = []; let done = false;
    const finish = (extra={}) => { if (done) return; done = true; clearTimeout(timer); try { child.kill(); } catch {} resolve({ id:test.id, arm, repeat, wallMs:Date.now()-started, text, usage, tools, recall:test.expect.filter(x=>text.includes(x)).length/test.expect.length, stderr:stderr.slice(-1000), ...extra }); };
    const timer = setTimeout(() => finish({ timeout:true }), 8*60_000);
    child.stdout.on("data", chunk => { buf += chunk; let i; while ((i=buf.indexOf("\n"))>=0) { const line=buf.slice(0,i); buf=buf.slice(i+1); let m; try {m=JSON.parse(line)} catch {continue}; if (m.type==="item" && m.item?.type==="mcp_tool_call" && !tools.includes(m.item.tool)) tools.push(m.item.tool); if (m.type==="done") { usage=m.usage??null; finish(); } if (m.type==="item" && m.item?.type==="agent_message") text=m.item.text??text; if (m.ok===false) finish({error:m.error}); }});
    child.stderr.on("data", c => stderr += c);
    child.on("close", code => { if (!done) finish({ exitCode:code, error:code?`exit ${code}`:undefined }); });
    child.stdin.end(JSON.stringify({ action:"prompt", cwd:REPO, mode:"plan", model:MODEL, sessionId:`scale-ab-${test.id}-${arm}-${repeat}-${Date.now()}`, parts:[{type:"text",text:test.prompt}] })+"\n");
  });
}
const rows=[];
for (let repeat=1; repeat<=REPEATS; repeat++) for (const test of CASES) { process.stderr.write(`run ${repeat}/${REPEATS} ${test.id}\n`); const [A,B]=await Promise.all([run(test,"A",repeat),run(test,"B",repeat)]); rows.push({id:test.id,repeat,A,B}); }
const tokens = r => Object.values(r.usage??{}).filter(Number.isFinite).reduce((a,b)=>a+b,0);
const sum = (arm,key) => rows.reduce((n,r)=>n+(key==="tokens"?tokens(r[arm]):r[arm][key]??0),0);
const report={ranAt:new Date().toISOString(),model:MODEL,repeats:REPEATS,rows,totals:{A:{wallMs:sum("A","wallMs"),tokens:sum("A","tokens"),recall:sum("A","recall")/rows.length},B:{wallMs:sum("B","wallMs"),tokens:sum("B","tokens"),recall:sum("B","recall")/rows.length}}};
writeFileSync(join(import.meta.dirname,"fast-context-scale-lyra-deepseek-ab.report.json"),JSON.stringify(report,null,2));
console.log(JSON.stringify(report.totals,null,2));
