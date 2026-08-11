// DeepSeek paired A/B for Lyra's final-answer policy.
// A runs the master binary; B runs the current worktree binary. Each pair uses the same prompt.
import { spawn } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const WORKTREE = resolve(HERE, "..");
const MASTER = process.env.LYRA_AB_MASTER_REPO ?? "D:\\code\\nova-client-master";
const MODEL = process.env.LYRA_AB_MODEL ?? "opencode/deepseek-v4-flash/variant/high";
const RUNS = Number(process.env.LYRA_AB_RUNS ?? 2);
const TIMEOUT_MS = Number(process.env.LYRA_AB_TIMEOUT_MS ?? 10 * 60_000);
const CONFIG_DIR = process.env.LYRA_AB_CONFIG_DIR ?? join(process.env.USERPROFILE ?? "", ".nova");
const OUT = process.env.LYRA_AB_OUT ?? join(HERE, "lyra-concise-deepseek-ab.report.json");
const bin = (repo) => join(repo, "src-tauri", "target", "debug", "nova.exe");

const CASES = [
  {
    id: "simple-success",
    prompt: "请只回答结论：一个普通单文件修复已完成且测试通过。给用户最终回复。不要调用工具。",
    must: ["完成", "通过"],
    exception: [],
  },
  {
    id: "multi-file-success",
    prompt: "一个跨 4 个文件的功能开发已经完成，单元测试和类型检查都通过，没有兼容性变化，也不需要用户操作。请给用户最终结论。不要调用工具。",
    must: ["完成", "通过"],
    exception: [],
  },
  {
    id: "unverified",
    prompt: "代码修改已经完成，但因为本机缺少数据库服务无法运行集成测试；上线前必须在 CI 验证。请给用户最终结论。不要调用工具。",
    must: ["完成", "无法", "CI"],
    exception: ["数据库"],
  },
  {
    id: "action-required",
    prompt: "配置修改已经完成并验证通过，但只对新会话生效，用户需要重启 Nova。请给用户最终结论。不要调用工具。",
    must: ["完成", "重启"],
    exception: ["新会话"],
  },
  {
    id: "failed",
    prompt: "任务未完成：构建失败，因为依赖服务返回 401；用户需要更新访问令牌后重试。请给用户最终结论。不要调用工具。",
    must: ["未完成", "401", "令牌"],
    exception: [],
  },
];

const lowValuePatterns = [
  /文件|函数|行号/g,
  /测试命令|执行命令|运行了/g,
  /首先|其次|然后|接下来/g,
  /建议后续|持续关注|无额外风险|没有额外风险/g,
  /总结(?:一下|来说)?/g,
];

function runOne(repo, arm, test, run) {
  return new Promise((resolveRun) => {
    const started = Date.now();
    const child = spawn(bin(repo), ["lyra"], {
      cwd: repo,
      env: {
        ...process.env,
        NOVA_DATA_DIR: CONFIG_DIR,
        LYRA_FINAL_NOTE: "off",
        LYRA_SPECULATE: "off",
      },
      stdio: ["pipe", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    let finalText = "";
    let usage = null;
    let done = false;

    const finish = (extra = {}) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      try { child.kill(); } catch {}
      const text = finalText.trim();
      const mustRecall = test.must.filter((term) => text.includes(term));
      const exceptionRecall = test.exception.filter((term) => text.includes(term));
      const lowValueHits = lowValuePatterns.reduce(
        (sum, pattern) => sum + [...text.matchAll(pattern)].length,
        0,
      );
      resolveRun({
        arm,
        run,
        id: test.id,
        wallMs: Date.now() - started,
        text,
        chars: [...text].length,
        lines: text ? text.split(/\r?\n/).filter((line) => line.trim()).length : 0,
        mustRecall,
        mustRecallRate: test.must.length ? mustRecall.length / test.must.length : 1,
        exceptionRecall,
        exceptionRecallRate: test.exception.length ? exceptionRecall.length / test.exception.length : 1,
        lowValueHits,
        usage,
        stderr: stderr.slice(-500),
        ...extra,
      });
    };

    const timer = setTimeout(() => finish({ timeout: true }), TIMEOUT_MS);
    let buffer = "";
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
      buffer += chunk;
      let newline;
      while ((newline = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, newline).trim();
        buffer = buffer.slice(newline + 1);
        if (!line) continue;
        let event;
        try { event = JSON.parse(line); } catch { continue; }
        if (event.type === "item" && event.item?.type === "agent_message") {
          finalText = event.item.text ?? finalText;
        }
        if (event.type === "done") {
          usage = event.usage ?? usage;
          finish({ cancelled: event.cancelled ?? false });
        } else if (event.ok === false) {
          finish({ errorEvent: event });
        }
      }
    });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.on("error", (error) => finish({ spawnError: String(error), stdout: stdout.slice(-500) }));
    child.on("close", (code) => finish({ exitCode: code, stdout: stdout.slice(-500) }));
    child.stdin.end(`${JSON.stringify({
      action: "prompt",
      cwd: repo,
      mode: "plan",
      model: MODEL,
      sessionId: `concise-ab-${test.id}-${arm}-${run}-${Date.now()}`,
      parts: [{ type: "text", text: test.prompt }],
    })}\n`);
  });
}

const rows = [];
for (let run = 1; run <= RUNS; run += 1) {
  for (const test of CASES) {
    process.stderr.write(`[${run}/${RUNS}] ${test.id}\n`);
    const [A, B] = await Promise.all([
      runOne(MASTER, "A", test, run),
      runOne(WORKTREE, "B", test, run),
    ]);
    rows.push({ id: test.id, run, A, B });
  }
}

function aggregate(arm) {
  const values = rows.map((row) => row[arm]);
  const average = (key) => values.reduce((sum, value) => sum + Number(value[key] ?? 0), 0) / values.length;
  const usage = values.reduce((totals, value) => {
    for (const [key, amount] of Object.entries(value.usage ?? {})) {
      if (Number.isFinite(amount)) totals[key] = (totals[key] ?? 0) + amount;
    }
    return totals;
  }, {});
  return {
    samples: values.length,
    avgChars: average("chars"),
    avgLines: average("lines"),
    avgMustRecallRate: average("mustRecallRate"),
    avgExceptionRecallRate: average("exceptionRecallRate"),
    lowValueHits: values.reduce((sum, value) => sum + value.lowValueHits, 0),
    failures: values.filter((value) => value.timeout || value.spawnError || value.errorEvent || value.exitCode).length,
    usage,
  };
}

const A = aggregate("A");
const B = aggregate("B");
const report = {
  ranAt: new Date().toISOString(),
  model: MODEL,
  runs: RUNS,
  cases: CASES.map(({ id, must, exception }) => ({ id, must, exception })),
  binaries: { A: bin(MASTER), B: bin(WORKTREE) },
  totals: {
    A,
    B,
    delta: {
      avgCharsPct: A.avgChars ? ((B.avgChars - A.avgChars) / A.avgChars) * 100 : null,
      avgLinesPct: A.avgLines ? ((B.avgLines - A.avgLines) / A.avgLines) * 100 : null,
      mustRecallPoints: (B.avgMustRecallRate - A.avgMustRecallRate) * 100,
      exceptionRecallPoints: (B.avgExceptionRecallRate - A.avgExceptionRecallRate) * 100,
      lowValueHits: B.lowValueHits - A.lowValueHits,
    },
  },
  rows,
};
mkdirSync(dirname(OUT), { recursive: true });
writeFileSync(OUT, JSON.stringify(report, null, 2));
console.log(JSON.stringify(report.totals, null, 2));
console.log(`report -> ${OUT}`);
