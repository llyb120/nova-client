import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { roamingWorkflowPrompt, parseRoamingWorkflowPrompt } from "../src/workflow/roamingProtocol.ts";

const workflow = { id: 'remote/id"不同于本机', name: "对方的 多 Stage 工作流" };
const goal = "第一行\n第二行 -- 保留原文\n/nova-workflow 只是目标内容";
assert.deepEqual(parseRoamingWorkflowPrompt(roamingWorkflowPrompt(workflow, goal)), { ...workflow, goal });
assert.deepEqual(parseRoamingWorkflowPrompt(roamingWorkflowPrompt(workflow, "")), { ...workflow, goal: "" });
assert.deepEqual(parseRoamingWorkflowPrompt(roamingWorkflowPrompt(workflow, "").trim()), { ...workflow, goal: "" });
for (const invalid of ["普通提示词", '/nova-workflow {}\ngoal', '/nova-workflow null\ngoal', '/nova-workflow {"id":3,"name":"x"}\ngoal']) {
  assert.throws(() => parseRoamingWorkflowPrompt(invalid));
}

// 跨端接力回归契约：不能只放开选择器，漏掉独立 Stage 的创建和重同步。
const relay = readFileSync(new URL("../src-tauri/src/relay.rs", import.meta.url), "utf8");
const lib = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
const runtime = readFileSync(new URL("../src/workflow/runtime.ts", import.meta.url), "utf8");
assert.match(relay, /"roaming\.stage" => self\.on_roaming_stage/);
assert.match(relay, /thread\.parent_thread_id = Some\(parent_id\.to_string\(\)\)/);
assert.match(relay, /parent\.roaming_peer\.as_deref\(\) != Some\(env\.from\.as_str\(\)\)/);
assert.match(relay, /self\.publish_roaming_stage\(&child\)/);
assert.match(relay, /self\.send_snapshot\(&child\.id, &child_guest\)/);
assert.equal((lib.match(/inherit_roaming_stage\(&mut thread\)/g) ?? []).length, 2);
assert.equal((lib.match(/publish_roaming_stage\(&thread\)/g) ?? []).length, 2);
assert.match(runtime, /root\.roamingRole === "guest" \|\| root\.quotaPeerName/);
assert.match(runtime, /api\.checkRoamingWorkflow\(originThreadId \?\? root\.id\)/);
// 使用真实运行时、内存 IPC 检查两阶段接力；不需要启动 Tauri 或实际模型。
const { build } = await import("esbuild");
const threads = new Map([["root", {
  id: "root", cwd: "remote-project", agentKind: "lyra", model: "default", mode: "build", roamingRole: "host", items: [],
}]]);
const sent = [];
const checked = [];
const running = new Set();
const definition = {
  id: "remote-workflow", name: "remote workflow", entry: "first", version: 1, maxTotalStages: 3,
  stages: [
    { id: "first", name: "first", promptTemplate: "first {{goal}}", agentKind: "codex", model: "remote-model", transitions: [{ id: "next", when: { kind: "always" }, to: "second" }], x: 0, y: 0 },
    { id: "second", name: "second", promptTemplate: "review {{prev}}", transitions: [{ id: "done", when: { kind: "always" }, to: "$done" }], x: 0, y: 0 },
  ],
};
const localStorageBefore = globalThis.localStorage;
globalThis.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
globalThis.__roamingTest = {
  definition,
  api: {
    async getThread(id) { return structuredClone(threads.get(id)); },
    async checkRoamingWorkflow(id) { checked.push(id); },
    async getSettings() { return {}; },
    async setThreadAgent(id, agentKind, model, mode) { Object.assign(threads.get(id), { agentKind, model, mode }); },
    async setThreadModel(id, model) { threads.get(id).model = model; },
    async setThreadMode(id, mode) { threads.get(id).mode = mode; },
    async renameThread(id, title) { threads.get(id).title = title; },
    async sendPrompt(id, text, images) { sent.push({ id, text, images }); },
    async generateThreadTitle() {},
    async notifyWorkflowDone() {},
    async createThread(cwd, agentKind, model, mode, effort, ephemeral, worktree, branch, base, clue, parentThreadId) {
      const thread = { id: "child", cwd, agentKind, model, mode, parentThreadId, roamingRole: "host", items: [] };
      threads.set(thread.id, thread);
      return thread;
    },
  },
};
try {
  const bundled = await build({
    entryPoints: [fileURLToPath(new URL("../src/workflow/runtime.ts", import.meta.url))],
    bundle: true, write: false, platform: "node", format: "esm",
    plugins: [{ name: "memory-ipc", setup(build) {
      build.onResolve({ filter: /^(\.\.\/ipc|\.\/storage)$/ }, ({ path }) => ({ path, namespace: "memory" }));
      build.onLoad({ filter: /.*/, namespace: "memory" }, ({ path }) => ({ contents: path.endsWith("ipc")
        ? "export const api = globalThis.__roamingTest.api;"
        : "export const getWorkflow = () => globalThis.__roamingTest.definition; export const isWorkflowEnabled = () => true; export const unregisterTransientWorkflow = () => {};" }));
    } }],
  });
  const engine = await import(`data:text/javascript;base64,${Buffer.from(bundled.outputFiles[0].text).toString("base64")}`);
  engine.initWorkflowRuntime({
    currentId: () => null, isRunning: (id) => running.has(id),
    setRunning: (id, value) => value ? running.add(id) : running.delete(id),
    async refreshThreads() {}, async openThread() {}, bumpScrollToBottom() {}, clearProposedPlan() {},
  });
  await engine.startWorkflow(definition.id, { goal: "goal" }, "root", [{ name: "image" }]);
  assert.equal(sent[0].text, "first goal");
  assert.equal(threads.get("root").agentKind, "codex");
  assert.equal(threads.get("root").mode, "build", "切换后端保留原会话权限模式");
  assert.equal(sent[0].images.length, 1);
  threads.get("root").items = [{ type: "assistant", id: 1, text: "first conclusion" }];
  running.delete("root");
  assert.equal(engine.handleTurnEnd("root", "end_turn"), true);
  for (let attempt = 0; sent.length < 2 && attempt < 100; attempt++) await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(sent[1]?.id, "child");
  assert.equal(sent[1]?.text, "review first conclusion");
  assert.equal(threads.get("child").parentThreadId, "root");
  assert.equal(threads.get("child").agentKind, "lyra", "跟随节点使用启动前的远端配置");
  assert.equal(checked.length, 2);
  for (const mode of ["build", "plan"]) {
    const id = `mode-${mode}`;
    threads.set(id, { ...threads.get("root"), id, agentKind: "lyra", mode: "build", items: [] });
    definition.stages[0].mode = mode;
    await engine.startWorkflow(definition.id, { goal: "goal" }, id);
    assert.equal(threads.get(id).mode, mode, "节点显式模式不能被后端切换清空");
  }
  threads.set("guest", { ...threads.get("root"), id: "guest", roamingRole: "guest" });
  await assert.rejects(engine.startWorkflow(definition.id, { goal: "goal" }, "guest"), /执行端/);
} finally {
  if (localStorageBefore === undefined) delete globalThis.localStorage;
  else globalThis.localStorage = localStorageBefore;
  delete globalThis.__roamingTest;
}
console.log("roaming workflow protocol, host Stage execution and wiring checks passed");
