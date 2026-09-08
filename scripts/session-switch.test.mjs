import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { transformSync } from "esbuild";
import { createMemo, createRoot, createSignal } from "solid-js/dist/solid.js";
import { createStore, reconcile } from "solid-js/store/dist/store.js";
import { virgoChains } from "../src/virgoChains.ts";

const store = readFileSync(new URL("../src/store.ts", import.meta.url), "utf8");
const sidebar = readFileSync(new URL("../src/components/Sidebar.tsx", import.meta.url), "utf8");
const js = (source) => transformSync(source, { loader: "ts", target: "esnext" }).code;

test("多 stage 同链反复切换复用导航列表，新增阶段后更新，断链和成环不挂起", () => {
  const chat = readFileSync(new URL("../src/components/ChatView.tsx", import.meta.url), "utf8");
  const source = chat.slice(chat.indexOf("  const stageIndex ="), chat.indexOf("  /** 是否工作流/Fire/员工事件链"));
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
        setCurrentId(`s${i % 200}`);
        assert.equal(stages(), first);
      }
      assert.equal(scans, initialScans);
      setThreads([...initial, { id: "new", parentThreadId: "s199", createdAt: 200 }]);
      assert.equal(stages().length, 201);
      setThreads([{ id: "s199", parentThreadId: "missing", createdAt: 0 }]);
      assert.equal(stages().length, 1);
      setThreads([{ id: "s199", parentThreadId: "b", createdAt: 0 }, { id: "b", parentThreadId: "s199", createdAt: 1 }]);
      assert.equal(stages().length, 2);
      setCurrentId("missing");
      assert.deepEqual(stages(), []);
    } finally { dispose(); }
  });
});

test("队列镜像移除旧 key，清空或挂起后整条链退出室女座", () => {
  const [state, setState] = createStore({ promptQueued: {} });
  const source = store.slice(store.indexOf("export function setPromptQueuedThreads("), store.indexOf("export async function openThread("));
  const update = new Function("state", "setState", "reconcile", `${js(source.replace("export ", ""))}; return setPromptQueuedThreads;`)(state, setState, reconcile);
  update(new Set(["child"]));
  const hidden = () => virgoChains({
    threads: [{ id: "root" }, { id: "child", parentThreadId: "root" }],
    isRunning: () => false, advancingRoots: [], unfinishedRoots: [],
    queuedThreads: Object.keys(state.promptQueued),
  }).hidden;
  assert.equal(hidden().size, 2);
  update(new Set(["other"]));
  assert.deepEqual(Object.keys(state.promptQueued), ["other"]);
  update(new Set());
  assert.equal(hidden().size, 0);
  assert.deepEqual(Object.keys(state.promptQueued), []);
});

test("侧栏链索引只构建一次，重复查询不扫描历史，成环也能结束", () => {
  let scans = 0;
  const threads = [{ id: "root", parentThreadId: "child" }, { id: "child", parentThreadId: "root" }, ...Array.from({ length: 10000 }, (_, i) => ({ id: `plain-${i}` }))];
  const state = { get threads() { scans++; return threads; } };
  const indexSource = sidebar.slice(sidebar.indexOf("  const childrenById ="), sidebar.indexOf("  const threadOf ="));
  const chainSource = sidebar.slice(sidebar.indexOf("    const chainThreads ="), sidebar.indexOf("    // 整条链的忙碌态"));
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
