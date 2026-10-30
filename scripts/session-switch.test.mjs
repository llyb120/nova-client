import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { transformSync } from "esbuild";
import { createEffect, createMemo, createRoot, createSignal, createStore, flush, reconcile } from "@solidjs/signals";
import { taskChains } from "../src/taskChains.ts";

const store = readFileSync(new URL("../src/store.ts", import.meta.url), "utf8");
const sidebar = readFileSync(new URL("../src/components/Sidebar.tsx", import.meta.url), "utf8");
const js = (source) => transformSync(source, { loader: "ts", target: "esnext" }).code;

test("文件面板开关跨会话保持，重新挂载恢复全局偏好", () => {
  const chat = readFileSync(new URL("../src/components/ChatView.tsx", import.meta.url), "utf8");
  const source = chat.slice(chat.indexOf("  const workspaceOpen ="), chat.indexOf("  const previewFile ="));
  const saved = new Map();
  const storage = { getItem: key => saved.get(key), setItem: (key, value) => saved.set(key, value) };
  const layoutSource = readFileSync(new URL('../src/workspaceLayout.ts', import.meta.url), 'utf8').replace(/^import .*;\r?\n/gm, '').replace(/export /g, '');
  const loadLayout = () => new Function('createStore', 'createSignal', 'localStorage', `${js(layoutSource)}; return {workspaceLayout, setWorkspaceLayout, defaultWorkspaceLayout};`)(createStore, createSignal, storage);
  let layout = loadLayout();
  const [currentId, setCurrentId] = createSignal('a');
  const state = { get currentId() { return currentId(); } };
  const mount = () => createRoot(dispose => ({ dispose, ...new Function('createSignal', 'createEffect', 'state', 'workspaceLayout', 'setWorkspaceLayout', `${js(source)}; return {workspaceOpen, setWorkspaceOpen, workspaceRequest, setWorkspaceRequest};`)(createSignal, createEffect, state, layout.workspaceLayout, layout.setWorkspaceLayout) }));
  let panel = mount();
  flush();
  try {
    assert.equal(panel.workspaceOpen(), false);
    panel.setWorkspaceOpen(true);
    panel.setWorkspaceRequest({path:'a.rs'});
    setCurrentId('b'); flush();
    assert.equal(panel.workspaceOpen(), true);
    assert.equal(panel.workspaceRequest(), null, 'do not carry a file request to another session');
    panel.dispose(); layout = loadLayout(); panel = mount();
    assert.equal(panel.workspaceOpen(), true, 'reopening chat restores global open preference');
    panel.setWorkspaceOpen(false);
    setCurrentId('a'); flush();
    assert.equal(panel.workspaceOpen(), false);
    panel.dispose(); layout = loadLayout(); panel = mount();
    assert.equal(panel.workspaceOpen(), false, 'closed preference survives remount too');
    layout.setWorkspaceLayout({open:true,widthRatio:.6,minimap:false,softWrap:false}); flush();
    assert.equal(panel.workspaceOpen(), true, 'appearance settings update the open chat immediately');
    assert.equal(loadLayout().workspaceLayout.widthRatio, .6);
    layout.setWorkspaceLayout(layout.defaultWorkspaceLayout); flush();
    assert.equal(panel.workspaceOpen(), false);
    assert.equal(loadLayout().workspaceLayout.widthRatio, null);
    assert.equal(loadLayout().workspaceLayout.minimap, true);
  } finally { panel.dispose(); }
});

test("空闲子 Agent 重新打开时校准快照，显示后台追加的排队和关闭记录", () => {
  const condition = store.match(/if \((cached && switching[^\n]+)\) \{/)[1];
  const skipRefresh = new Function("cached", "switching", "staleThreadSnapshots", "id", `return ${condition};`);
  assert.equal(skipRefresh({ subagent: true }, true, new Set(), "agent"), false);
  assert.equal(skipRefresh({ subagent: false }, true, new Set(), "ordinary"), true);
  assert.equal(skipRefresh({}, true, new Set(["ordinary"]), "ordinary"), false);
});

test("多 stage 同链反复切换复用导航列表，新增阶段后更新，断链和成环不挂起", () => {
  const chat = readFileSync(new URL("../src/components/ChatView.tsx", import.meta.url), "utf8");
  const source = chat.slice(chat.indexOf("  const stageIndex ="), chat.indexOf("  const isStageTitle ="));
  createRoot((dispose) => {
    try {
      const initial = Array.from({ length: 200 }, (_, i) => ({ id: `s${i}`, parentThreadId: i ? `s${i - 1}` : null, createdAt: i }));
      const [threads, setThreads] = createSignal(initial);
      const [currentId, setCurrentId] = createSignal("s0");
      let scans = 0;
      const state = { get threads() { scans++; return threads(); }, get currentId() { return currentId(); } };
      const stages = new Function("state", "createMemo", `${js(source)}; return stageThreads;`)(state, createMemo);
      const first = stages();
      assert.equal(first.length, 200);
      const initialScans = scans;
      for (let i = 0; i < 1000; i++) {
        setCurrentId(`s${i % 200}`); flush();
        assert.equal(stages(), first);
      }
      assert.equal(scans, initialScans);
      setThreads([...initial, { id: "new", parentThreadId: "s199", createdAt: 200 }]); flush();
      assert.equal(stages().length, 201);
      setThreads([{ id: "s199", parentThreadId: "missing", createdAt: 0 }]); flush();
      assert.equal(stages().length, 1);
      setThreads([{ id: "s199", parentThreadId: "b", createdAt: 0 }, { id: "b", parentThreadId: "s199", createdAt: 1 }]); flush();
      assert.equal(stages().length, 2);
      setCurrentId("missing"); flush();
      assert.deepEqual(stages(), []);
    } finally { dispose(); }
  });
});

test("队列镜像移除旧 key，清空或挂起后整条链结束", () => {
  const [state, setState] = createStore({ promptQueued: {} });
  const source = store.slice(store.indexOf("export function setPromptQueuedThreads("), store.indexOf("const historyLoads ="));
  const update = new Function("state", "setState", "reconcile", `${js(source.replace("export ", ""))}; return setPromptQueuedThreads;`)(state, setState, reconcile);
  update(new Set(["child"])); flush();
  const active = () => taskChains({
    threads: [{ id: "root" }, { id: "child", parentThreadId: "root" }],
    isRunning: () => false,
    queuedThreads: Object.keys(state.promptQueued),
  }).active;
  assert.equal(active().size, 2);
  update(new Set(["other"])); flush();
  assert.deepEqual(Object.keys(state.promptQueued), ["other"]);
  update(new Set()); flush();
  assert.equal(active().size, 0);
  assert.deepEqual(Object.keys(state.promptQueued), []);
});

test("侧栏链索引只构建一次，重复查询不扫描历史，成环也能结束", () => {
  let scans = 0;
  const threads = [{ id: "root", parentThreadId: "child" }, { id: "child", parentThreadId: "root" }, ...Array.from({ length: 10000 }, (_, i) => ({ id: `plain-${i}` }))];
  const state = { get threads() { scans++; return threads; } };
  const indexSource = sidebar.slice(sidebar.indexOf("  const childrenById ="), sidebar.indexOf("  const threadOf ="));
  const chainStart = sidebar.indexOf("    const chainThreads =");
  const chainSource = sidebar.slice(chainStart, sidebar.indexOf("    const running =", chainStart));
  createRoot((dispose) => {
    try {
      const chain = new Function("state", "createMemo", "t", `${js(indexSource + chainSource)}; return chainThreads;`)(state, createMemo, threads[0]);
      assert.deepEqual(chain().map((t) => t.id), ["root", "child"]);
      const first = chain();
      for (let i = 0; i < 1000; i++) assert.equal(chain(), first);
      assert.equal(scans, 1);
    } finally { dispose(); }
  });
});
