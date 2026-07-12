// 探针 4：真实代码库上的大任务（大量 Read/Search），统计有多少 toolCallId 始终没收到 completed
import { spawn } from "node:child_process";
import { writeFileSync, appendFileSync } from "node:fs";
import { join } from "node:path";

const MODEL = process.argv[2] || "glm-5-1";
const CWD = process.argv[3] || process.cwd();
const LOG = join(process.cwd(), "scripts", "probe-big.log");
writeFileSync(LOG, "");
const log = (s) => {
  appendFileSync(LOG, s + "\n");
  console.log(s);
};

const child = spawn("devin", ["acp"], { stdio: ["pipe", "pipe", "pipe"], shell: true });
child.stderr.on("data", () => {});

let nextId = 1;
const pending = new Map();
function request(method, params) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
  });
}

const lastStatus = new Map();
const titles = new Map();

let buf = "";
child.stdout.on("data", (chunk) => {
  buf += chunk.toString();
  let idx;
  while ((idx = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, idx).trim();
    buf = buf.slice(idx + 1);
    if (!line) continue;
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      continue;
    }
    if (msg.method === "session/update") {
      const u = msg.params.update;
      const k = u.sessionUpdate;
      if (k === "tool_call" || k === "tool_call_update") {
        if (u.title) titles.set(u.toolCallId, u.title);
        if (u.status) lastStatus.set(u.toolCallId, u.status);
        else if (!lastStatus.has(u.toolCallId)) lastStatus.set(u.toolCallId, "(初始无状态)");
        log(
          `${new Date().toISOString().slice(11, 23)} ${k.padEnd(17)} …${String(u.toolCallId).slice(-8)} status=${u.status ?? "(无)"} ${u.title ? "t=" + JSON.stringify(u.title) : ""}`,
        );
      }
    } else if (msg.method && msg.id !== undefined) {
      if (msg.method === "session/request_permission") {
        const opt = msg.params.options?.[0]?.optionId;
        log(`>> 权限请求，自动选择 ${opt}`);
        child.stdin.write(
          JSON.stringify({
            jsonrpc: "2.0",
            id: msg.id,
            result: { outcome: { outcome: "selected", optionId: opt } },
          }) + "\n",
        );
      } else {
        child.stdin.write(
          JSON.stringify({
            jsonrpc: "2.0",
            id: msg.id,
            error: { code: -32601, message: "unsupported" },
          }) + "\n",
        );
      }
    } else if (msg.id !== undefined) {
      const p = pending.get(msg.id);
      if (p) {
        pending.delete(msg.id);
        if (msg.error) p.reject(new Error(msg.error.message));
        else p.resolve(msg.result);
      }
    }
  }
});

try {
  await request("initialize", {
    protocolVersion: 1,
    clientInfo: { name: "probe", title: "Probe", version: "0.0.1" },
    clientCapabilities: { fs: { readTextFile: false, writeTextFile: false } },
  });
  const sess = await request("session/new", { cwd: CWD, mcpServers: [] });
  const sid = sess.sessionId;
  log(`session/new sid=${sid} cwd=${CWD}`);
  await request("session/set_config_option", { sessionId: sid, configId: "model", value: MODEL });
  await request("session/set_mode", { sessionId: sid, modeId: "bypass" }).catch(() => {});

  const main = request("session/prompt", {
    sessionId: sid,
    prompt: [
      {
        type: "text",
        text: "全面调研这个代码库：把 src/ 和 src-tauri/src/ 下的每一个源码文件都完整读一遍（可以并行读取），也用搜索工具找找所有 TODO/FIXME，最后输出一份简短的架构说明。只读不写。",
      },
    ],
  });

  // 20 秒后注入一条引导，贴近真实使用
  setTimeout(() => {
    log(">>> 注入引导消息");
    request("session/prompt", {
      sessionId: sid,
      prompt: [{ type: "text", text: "补充：架构说明里顺便列出用到的第三方依赖。" }],
    }).then(
      (r) => log(`--- 引导返回 stopReason=${r.stopReason} ---`),
      (e) => log(`--- 引导报错 ${e.message} ---`),
    );
  }, 20000);

  const r = await main;
  log(`--- 主 prompt 返回 stopReason=${r.stopReason} ---`);
  // 等几秒收尾部事件
  await new Promise((x) => setTimeout(x, 3000));

  log("\n=== 最终状态统计 ===");
  const byStatus = {};
  for (const [id, st] of lastStatus) {
    byStatus[st] = (byStatus[st] || 0) + 1;
    if (st !== "completed" && st !== "failed")
      log(`未完成: …${id.slice(-8)} → ${st} ${JSON.stringify(titles.get(id) ?? "")}`);
  }
  log(JSON.stringify(byStatus));
} catch (e) {
  log("出错: " + e.message);
} finally {
  child.kill();
  process.exit(0);
}
