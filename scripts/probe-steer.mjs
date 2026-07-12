// 探针 2：运行中注入第二个 session/prompt（模拟 Nova 的「引导」），
// 观察 tool_call 状态是否会丢失 completed 更新
import { spawn } from "node:child_process";
import { mkdirSync, writeFileSync, appendFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const MODEL = process.argv[2] || "glm-5-1";
const LOG = join(process.cwd(), "scripts", "probe-steer.log");
writeFileSync(LOG, "");
const log = (s) => {
  appendFileSync(LOG, s + "\n");
  console.log(s);
};

const cwd = join(tmpdir(), "fd-steer-" + Date.now());
mkdirSync(cwd, { recursive: true });
const names = ["a", "b", "c", "d", "e", "f", "g", "h"].map((n) => n + ".txt");
for (const f of names) writeFileSync(join(cwd, f), `文件 ${f}：${"内容".repeat(20)}\n`);

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

// 记录每个 toolCallId 的最后状态
const lastStatus = new Map();

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
        const short = String(u.toolCallId).slice(-8);
        if (u.status) lastStatus.set(u.toolCallId, u.status);
        else if (!lastStatus.has(u.toolCallId)) lastStatus.set(u.toolCallId, "(初始无状态)");
        log(
          `${new Date().toISOString().slice(11, 23)} ${k.padEnd(17)} …${short} status=${u.status ?? "(无)"} title=${JSON.stringify(u.title ?? null)}`,
        );
      } else if (k === "agent_message_chunk") {
        // 静默
      } else if (k === "agent_thought_chunk") {
        // 静默
      } else {
        log(`${new Date().toISOString().slice(11, 23)} [update] ${k}`);
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
  const sess = await request("session/new", { cwd, mcpServers: [] });
  const sid = sess.sessionId;
  log(`session/new 完成 sid=${sid}`);
  await request("session/set_config_option", { sessionId: sid, configId: "model", value: MODEL });
  await request("session/set_mode", { sessionId: sid, modeId: "bypass" }).catch(() => {});

  log(`--- 发送主 prompt ---`);
  const main = request("session/prompt", {
    sessionId: sid,
    prompt: [
      {
        type: "text",
        text: "请逐个读取 a.txt 到 h.txt 共 8 个文件（每个文件单独调用一次读取工具，禁止并行，一个读完再读下一个），全部读完后用一句话总结。",
      },
    ],
  }).then(
    (r) => log(`--- 主 prompt 返回 stopReason=${r.stopReason} ---`),
    (e) => log(`--- 主 prompt 报错 ${e.message} ---`),
  );

  // 3 秒后注入引导消息
  await new Promise((r) => setTimeout(r, 3000));
  log(`>>> 注入引导消息`);
  const steer = request("session/prompt", {
    sessionId: sid,
    prompt: [{ type: "text", text: "补充：总结时请顺便说明每个文件有多少个字。" }],
  }).then(
    (r) => log(`--- 引导 prompt 返回 stopReason=${r.stopReason} ---`),
    (e) => log(`--- 引导 prompt 报错 ${e.message} ---`),
  );

  await Promise.allSettled([main, steer]);
  log("\n=== 各 toolCallId 的最终状态 ===");
  for (const [id, st] of lastStatus) log(`…${id.slice(-8)} → ${st}`);
} catch (e) {
  log("出错: " + e.message);
} finally {
  child.kill();
  process.exit(0);
}
