// 探针 3：跑完一轮后杀掉 devin，新进程 session/load 同一会话，
// 观察重放事件的状态序列，以及重放是否在 session/load 响应之后仍在继续（泄漏窗口）
import { spawn } from "node:child_process";
import { mkdirSync, writeFileSync, appendFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const MODEL = process.argv[2] || "glm-5-1";
const LOG = join(process.cwd(), "scripts", "probe-load.log");
writeFileSync(LOG, "");
const log = (s) => {
  appendFileSync(LOG, s + "\n");
  console.log(s);
};

const cwd = join(tmpdir(), "fd-load-" + Date.now());
mkdirSync(cwd, { recursive: true });
for (const f of ["a.txt", "b.txt", "c.txt"]) {
  writeFileSync(join(cwd, f), `文件 ${f} 内容\n`);
}

function connect(tag) {
  const child = spawn("devin", ["acp"], { stdio: ["pipe", "pipe", "pipe"], shell: true });
  child.stderr.on("data", () => {});
  let nextId = 1;
  const pending = new Map();
  const state = { loadReturned: false };
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
        const leak = state.loadReturned ? "  <<< 在 session/load 返回之后!" : "";
        if (k === "tool_call" || k === "tool_call_update") {
          log(
            `[${tag}] ${k.padEnd(17)} …${String(u.toolCallId).slice(-8)} status=${u.status ?? "(无)"}${leak}`,
          );
        } else {
          log(`[${tag}] [update] ${k}${leak}`);
        }
      } else if (msg.method && msg.id !== undefined) {
        if (msg.method === "session/request_permission") {
          const opt = msg.params.options?.[0]?.optionId;
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
  return { child, request, state };
}

try {
  // 第一个进程：建会话跑一轮
  const c1 = connect("旧进程");
  await c1.request("initialize", {
    protocolVersion: 1,
    clientInfo: { name: "probe", title: "Probe", version: "0.0.1" },
    clientCapabilities: { fs: { readTextFile: false, writeTextFile: false } },
  });
  const sess = await c1.request("session/new", { cwd, mcpServers: [] });
  const sid = sess.sessionId;
  log(`session/new sid=${sid}`);
  await c1.request("session/set_config_option", { sessionId: sid, configId: "model", value: MODEL });
  await c1.request("session/set_mode", { sessionId: sid, modeId: "bypass" }).catch(() => {});
  const r1 = await c1.request("session/prompt", {
    sessionId: sid,
    prompt: [{ type: "text", text: "依次读取 a.txt、b.txt、c.txt（各调用一次读取工具），然后一句话总结。" }],
  });
  log(`第一轮结束 stopReason=${r1.stopReason}`);
  c1.child.kill();
  await new Promise((r) => setTimeout(r, 1500));

  // 第二个进程：session/load 恢复
  log(`\n=== 新进程 session/load ===`);
  const c2 = connect("新进程");
  await c2.request("initialize", {
    protocolVersion: 1,
    clientInfo: { name: "probe", title: "Probe", version: "0.0.1" },
    clientCapabilities: { fs: { readTextFile: false, writeTextFile: false } },
  });
  const t0 = Date.now();
  await c2.request("session/load", { sessionId: sid, cwd, mcpServers: [] });
  c2.state.loadReturned = true;
  log(`session/load 已返回（${Date.now() - t0}ms），之后到达的事件会标记泄漏`);
  // 等 5 秒看是否还有迟到的重放事件
  await new Promise((r) => setTimeout(r, 5000));

  // 再发一轮，确认新轮次正常
  log(`\n=== 恢复后再发一轮 ===`);
  const r2 = await c2.request("session/prompt", {
    sessionId: sid,
    prompt: [{ type: "text", text: "再读一次 a.txt，然后说「完成」。" }],
  });
  log(`第二轮结束 stopReason=${r2.stopReason}`);
  c2.child.kill();
} catch (e) {
  log("出错: " + e.message);
} finally {
  process.exit(0);
}
