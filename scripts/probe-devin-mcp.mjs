// 验证 Devin ACP 是否会注册启动目录 .devin/mcp_config.local.json 里的 MCP 服务器。
// 用法: node scripts/probe-devin-mcp.mjs <launchDir> <sessionCwd>
import { spawn } from "node:child_process";
import readline from "node:readline";

const [launchDir, sessionCwd, promptText] = process.argv.slice(2);
if (!launchDir || !sessionCwd) {
  console.error("usage: node scripts/probe-devin-mcp.mjs <launchDir> <sessionCwd>");
  process.exit(2);
}

const child = spawn("devin", ["acp"], {
  cwd: launchDir,
  stdio: ["pipe", "pipe", "inherit"],
  shell: true,
});

let nextId = 1;
const pending = new Map();
function request(method, params) {
  const id = nextId++;
  child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
  return new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
}

const rl = readline.createInterface({ input: child.stdout });
let sessionId = null;
const toolCalls = [];
rl.on("line", (line) => {
  let msg;
  try { msg = JSON.parse(line); } catch { return; }
  if (msg.id !== undefined && pending.has(msg.id)) {
    const { resolve, reject } = pending.get(msg.id);
    pending.delete(msg.id);
    if (msg.error) reject(new Error(JSON.stringify(msg.error)));
    else resolve(msg.result);
    return;
  }
  if (msg.method === "session/update") {
    const update = msg.params?.update;
    if (update?.sessionUpdate === "tool_call") {
      toolCalls.push({ id: update.toolCallId, title: update.title, status: update.status });
      console.log(`[tool_call] ${update.title} (${update.status})`);
    } else if (update?.sessionUpdate === "tool_call_update") {
      const text = JSON.stringify(update.content ?? update.rawOutput ?? "").slice(0, 400);
      console.log(`[tool_update] ${update.toolCallId} ${update.status}: ${text}`);
      const call = toolCalls.find((c) => c.id === update.toolCallId);
      if (call) { call.status = update.status; call.output = text; }
    } else if (update?.sessionUpdate === "agent_message_chunk") {
      process.stdout.write(update.content?.text ?? "");
    }
  } else if (msg.method === "session/request_permission") {
    // 自动批准，避免挂起
    const options = msg.params?.options ?? [];
    const allow = options.find((o) => String(o.kind ?? "").startsWith("allow")) ?? options[0];
    child.stdin.write(`${JSON.stringify({
      jsonrpc: "2.0",
      id: msg.id,
      result: { outcome: allow ? { outcome: "selected", optionId: allow.optionId } : { outcome: "cancelled" } },
    })}\n`);
  }
});

try {
  await request("initialize", {
    protocolVersion: 1,
    clientInfo: { name: "nova-probe", title: "Nova Probe", version: "0.0.1" },
    clientCapabilities: { fs: { readTextFile: false, writeTextFile: false }, terminal: false },
  });
  const session = await request("session/new", { cwd: sessionCwd, mcpServers: [] });
  sessionId = session.sessionId;
  console.log(`\n[session] ${sessionId}`);
  const result = await request("session/prompt", {
    sessionId,
    prompt: [{
      type: "text",
      text: promptText ?? "请调用 mcp_list_tools 查看 nova-tools 服务器有哪些工具（不要调用其它工具），然后用一句话汇报工具名列表。",
    }],
  });
  console.log(`\n[prompt done] stopReason=${result?.stopReason}`);
  const hit = toolCalls.find((c) => /mcp_list_tools|nova-tools/i.test(c.title ?? ""));
  console.log(hit
    ? `[verdict] 见到 MCP 相关工具调用: ${hit.title} -> ${hit.status}\n${hit.output ?? ""}`
    : "[verdict] 本轮没有 mcp_list_tools 调用");
} catch (error) {
  console.error(`[error] ${error.message}`);
  process.exitCode = 1;
} finally {
  child.kill();
}
