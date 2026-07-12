// 探针：直连 `devin acp`，观察 tool_call / tool_call_update 的状态流转
// 用法：node scripts/probe-acp.mjs [model]
import { spawn } from "node:child_process";
import { mkdirSync, writeFileSync, appendFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const MODEL = process.argv[2] || "";
const LOG = join(process.cwd(), "scripts", "probe-acp.log");
writeFileSync(LOG, "");
const log = (s) => {
  appendFileSync(LOG, s + "\n");
  console.log(s);
};

// 准备工作目录和测试文件
const cwd = join(tmpdir(), "fd-probe-" + Date.now());
mkdirSync(cwd, { recursive: true });
for (const f of ["a.txt", "b.txt", "c.txt"]) {
  writeFileSync(join(cwd, f), `这是文件 ${f} 的内容：${f.repeat(3)}\n`);
}

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
        log(
          `${new Date().toISOString().slice(11, 23)} ${k.padEnd(17)} id=${u.toolCallId} status=${u.status ?? "(无)"} title=${JSON.stringify(u.title ?? null)} kind=${u.kind ?? "(无)"} content=${u.content ? u.content.length : "(无)"}`,
        );
      } else if (k === "agent_message_chunk" || k === "agent_thought_chunk") {
        // 静默
      } else {
        log(`${new Date().toISOString().slice(11, 23)} [update] ${k}`);
      }
    } else if (msg.method && msg.id !== undefined) {
      // server 请求（权限等）：一律允许第一个选项
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

const t0 = Date.now();
try {
  await request("initialize", {
    protocolVersion: 1,
    clientInfo: { name: "probe", title: "Probe", version: "0.0.1" },
    clientCapabilities: { fs: { readTextFile: false, writeTextFile: false } },
  });
  log(`[${Date.now() - t0}ms] initialize 完成`);

  const sess = await request("session/new", { cwd, mcpServers: [] });
  const sid = sess.sessionId;
  log(`[${Date.now() - t0}ms] session/new 完成 sid=${sid}`);
  const models = sess.configOptions?.find((o) => o.id === "model")?.options ?? [];
  log("可用模型: " + models.map((m) => m.value).join(", "));

  if (MODEL) {
    await request("session/set_config_option", { sessionId: sid, configId: "model", value: MODEL });
    log(`已设置模型 ${MODEL}`);
  }
  await request("session/set_mode", { sessionId: sid, modeId: "bypass" }).catch((e) =>
    log("set_mode 失败: " + e.message),
  );

  log(`--- 发送 prompt（工作目录 ${cwd}）---`);
  const resp = await request("session/prompt", {
    sessionId: sid,
    prompt: [
      {
        type: "text",
        text: "请依次读取 a.txt、b.txt、c.txt 三个文件的内容（每个文件单独调用一次读取工具），然后用一句话总结。不要做其他任何事。",
      },
    ],
  });
  log(`--- 轮次结束 stopReason=${resp.stopReason} ---`);
} catch (e) {
  log("出错: " + e.message);
} finally {
  child.kill();
  process.exit(0);
}
