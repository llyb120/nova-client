// Lyra 流式投机执行特性基准。
// 用法: node bench/lyra-bench.mjs [--cells t1,t2,t3] [--runs 2]
// 每个 cell 跑 baseline（关闭投机）与 treatment（开启投机），输出 bench/results-<ts>.json。
import { spawn } from "child_process";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const EXE_DEFAULT = path.join(
  __dirname,
  "..",
  "src-tauri",
  "target",
  "debug",
  process.platform === "win32" ? "nova.exe" : "nova",
);
// A/B 二进制对比：设置 BENCH_EXE_BASELINE / BENCH_EXE_TREATMENT 后按变体选二进制，
// 且两侧都保持投机开启（不再用 LYRA_SPECULATE=off 做对照）。
const EXE_BY_VARIANT = {
  baseline: process.env.BENCH_EXE_BASELINE || EXE_DEFAULT,
  treatment: process.env.BENCH_EXE_TREATMENT || EXE_DEFAULT,
};
const EXE_AB = Boolean(process.env.BENCH_EXE_BASELINE || process.env.BENCH_EXE_TREATMENT);
const REPO = path.resolve(__dirname, "..");
const FIXTURE = path.join(__dirname, "fixture");
const WORK = path.join(__dirname, "work");
// 截图中的 OpenCode Go / DeepSeek V4 Flash - High。
const MODEL = process.env.BENCH_MODEL || "opencode/deepseek-v4-flash/variant/high";
// 通过环境变量注入用户指定的临时凭证，绝不把 key 写入仓库或结果文件。
const BENCH_OPENCODE_API_KEY = process.env.BENCH_OPENCODE_API_KEY || "";
// 任意 provider 注入：BENCH_CONFIG_JSON 为完整 {model, provider:{...}} JSON 字符串，
// 优先级高于内置 opencode 模板，用于跑 bai / deepseek 官方等其他端点。
const BENCH_CONFIG_JSON = process.env.BENCH_CONFIG_JSON || "";
const BENCH_CONFIG_MODEL = "opencode/deepseek-v4-flash";

const TASKS = {
  // 探索型任务（只读意图）：多轮 read/polaris，大结果触发转存，参数流长触发投机。
  t1: {
    cwd: () => REPO,
    text: "调查这个仓库里 lyra 的 read 工具实现：输出格式（含行号前缀规则）、分段读取的 offset/limit/hasMore 语义、超限治理（govern）的触发条件与归档行为、以及 legacy 模式分别由哪些环境变量控制。给出每一项对应的文件与行号依据。",
  },
  // 小编辑任务：edit + bash 验证。
  t2: {
    cwd: () => freshSandbox("t2"),
    text: "这是一个 Node 项目。把 src/greet.js 的问候语从 hello 改成 hi，同步更新使用它和测试它的文件，然后运行 npm test 确认全部通过。",
  },
  // 多文件特性任务：更多轮次与更大的工具结果。
  t3: {
    cwd: () => freshSandbox("t3"),
    text: "这是一个 Node 项目。给 src/cart.js 的购物车加打折功能：addItem 支持第三个可选参数 discountRate（0-1 的折扣率，如 0.8 表示八折，缺省为 1），total() 应用各自折扣，list() 用 src/format.js 的 money() 格式化并标注折扣。同步更新 test/run.js 覆盖新行为，更新 src/index.js 演示，然后运行 npm test 确认全部通过。",
  },

  // 真实历史任务：来自本机 thread 6a0bc774... 的第一个用户请求。
  r1: {
    cwd: () => "D:\\code\\nova-client",
    mode: "plan",
    text: "fastcontext还有无优化空间？例如目前只给了起始行号没有给终点，模型还要小段read来补全",
  },
  // 真实历史任务：来自本机 thread 1269204c... 的第二个用户请求。
  r2: {
    cwd: () => REPO,
    mode: "plan",
    text: "现在lyra的工具不是并行的吗？",
  },
  // 真实历史任务：来自本机 thread 1269204c... 的第一条用户请求。
  r3: {
    cwd: () => REPO,
    mode: "plan",
    text: "还有没有什么鬼才办法能给lyra提效，让它用尽量少的工具调用干更多的事情",
  },
};

function freshSandbox(tag) {
  const dir = path.join(WORK, `${tag}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`);
  fs.cpSync(FIXTURE, dir, { recursive: true });
  return dir;
}

function benchDataRoot(tag) {
  if (BENCH_CONFIG_JSON) {
    const root = path.join(WORK, `data-${tag}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`);
    const alkaid = path.join(root, "alkaid");
    fs.mkdirSync(alkaid, { recursive: true });
    fs.writeFileSync(path.join(alkaid, "config.jsonc"), BENCH_CONFIG_JSON);
    return root;
  }
  if (!BENCH_OPENCODE_API_KEY) return null;
  const root = path.join(WORK, `data-${tag}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`);
  const alkaid = path.join(root, "alkaid");
  fs.mkdirSync(alkaid, { recursive: true });
  const config = {
    model: BENCH_CONFIG_MODEL,
    provider: {
      opencode: {
        npm: "@ai-sdk/openai-compatible",
        name: "OpenCode Go",
        options: {
          baseURL: "https://opencode.ai/zen/go/v1",
          apiKey: BENCH_OPENCODE_API_KEY,
        },
        models: {
          "deepseek-v4-flash": {
            name: "DeepSeek V4 Flash (OpenCode Go)",
            reasoning: true,
            modalities: { input: ["text"], output: ["text"] },
            limit: { context: 1000000, output: 384000 },
            options: { reasoningEffort: "high" },
            variants: {
              high: { reasoningEffort: "high" },
              max: { reasoningEffort: "max" },
            },
          },
        },
      },
    },
  };
  fs.writeFileSync(path.join(alkaid, "config.jsonc"), JSON.stringify(config, null, 2));
  return root;
}

function runOnce({ cell, variant, task }) {
  return new Promise((resolve) => {
    const env = { ...process.env };
    if (task.dataRoot) env.NOVA_DATA_DIR = task.dataRoot;
    if (variant === "baseline" && !EXE_AB) {
      env.LYRA_SPECULATE = "off";
    }
    const started = Date.now();
    const child = spawn(EXE_BY_VARIANT[variant] || EXE_DEFAULT, ["lyra"], { env, stdio: ["pipe", "pipe", "pipe"] });
    let stderr = "";
    child.stderr.on("data", (d) => (stderr += d));
    const events = [];
    let buffer = "";
    let done = false;
    const finish = (reason) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      try { child.kill(); } catch {}
      const items = events.filter((e) => e.type === "item").map((e) => e.item);
      // 工具调用：started/completed 两次 item 同 id，按 id 去重；名称在 tool 字段。
      const toolById = new Map();
      for (const i of items) if (i && i.tool) toolById.set(i.id, i);
      const tools = [...toolById.values()];
      const toolErrors = tools.filter((i) => i.isError).length;
      const countByTool = (name) => tools.filter((i) => i.tool === name).length;
      const agentIds = items.filter((i) => i && i.type === "agent_message").map((i) => i.id);
      // 模型 API 回合数：agent_message-N / reasoning-N 的 N 以 MessageStart 递增。
      let maxIndex = 0;
      for (const i of items) {
        const m = i && typeof i.id === "string" && i.id.match(/^(?:agent_message|reasoning)-(\d+)$/);
        if (m) maxIndex = Math.max(maxIndex, Number(m[1]));
      }
      const timings = events.filter((e) => e.type === "timing").map((e) => e.phase);
      const doneEvent = events.find((e) => e.type === "done");
      const finalText = items.filter((i) => i && i.type === "agent_message").map((i) => i.text || "").pop() || "";
      resolve({
        cell,
        variant,
        reason,
        wallMs: Date.now() - started,
        apiRounds: timings.filter((p) => p === "provider_turn").length,
        indexedMessageCount: maxIndex,
        agentMessages: new Set(agentIds).size,
        toolCalls: toolById.size,
        toolErrors,
        reads: countByTool("read"),
        edits: countByTool("edit"),
        bashes: countByTool("bash"),
        toolSequence: tools.map((i) => i.tool + (i.isError ? "!" : "")).join(","),
        specHits: timings.filter((p) => p === "spec_hit").length,

        usage: (doneEvent && doneEvent.usage) || null,
        cancelled: doneEvent ? doneEvent.cancelled : null,
        finalText: finalText.slice(-600),
        errorEvent: events.find((e) => e.ok === false) || null,
        stderr: stderr.slice(-400),
      });
    };
    const timer = setTimeout(() => finish("timeout"), 10 * 60 * 1000);
    child.stdout.on("data", (d) => {
      buffer += d;
      let idx;
      while ((idx = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, idx).trim();
        buffer = buffer.slice(idx + 1);
        if (!line) continue;
        try {
          const ev = JSON.parse(line);
          events.push(ev);
          if (ev.type === "done" || ev.ok === false) finish("done");
        } catch {}
      }
    });
    child.on("error", (e) => { stderr += String(e); finish("spawn-error"); });
    child.on("close", () => finish(done ? undefined : "closed"));
    const request = {
      action: "prompt",
      cwd: task.cwdPath,
      mode: task.mode || "build",
      model: MODEL,
      parts: [{ type: "text", text: task.text }],
    };
    child.stdin.write(JSON.stringify(request) + "\n");
  });
}

async function main() {
  const args = process.argv.slice(2);
  const cellsArg = (args.find((a, i) => args[i - 1] === "--cells") || "t1,t2,t3").split(",");
  const runs = Number(args.find((a, i) => args[i - 1] === "--runs") || 2);
  const results = [];
  for (const cell of cellsArg) {
    const def = TASKS[cell];
    if (!def) continue;
    for (const variant of ["baseline", "treatment"]) {
      for (let run = 1; run <= runs; run++) {
        const task = {
          text: def.text,
          cwdPath: def.cwd(),
          dataRoot: benchDataRoot(`${cell}-${variant}-${run}`),
        };
        console.log(`[${cell}/${variant}#${run}] cwd=${task.cwdPath}`);
        const result = await runOnce({ cell, variant, task });
        result.run = run;
        results.push(result);
        console.log(
          `  -> ${result.reason} wall=${(result.wallMs / 1000).toFixed(1)}s providerTurns=${result.apiRounds} tools=${result.toolCalls} errors=${result.toolErrors} reads=${result.reads} specHits=${result.specHits} usage=${JSON.stringify(result.usage)}`
        );
        // 让服务端 prompt 缓存与速率稳定一点
        await new Promise((r) => setTimeout(r, 3000));
      }
    }
  }
  const out = path.join(__dirname, `results-${Date.now()}.json`);
  // 不序列化配置/key；结果仅保留模型选择和可比指标。
  fs.writeFileSync(out, JSON.stringify({ model: MODEL, results }, null, 2));
  console.log(`results -> ${out}`);
}

main();
