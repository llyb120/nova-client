import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { transformSync } from "esbuild";
import ts from "typescript";
import { createEffect, createMemo, createRoot, createSignal, createStore, flush, onFlush, reconcile } from "@solidjs/signals";
import { taskChains } from "../src/taskChains.ts";
import { splitEffort } from "../src/modelEfforts.ts";

const store = readFileSync(new URL("../src/store.ts", import.meta.url), "utf8");
const sidebar = readFileSync(new URL("../src/components/Sidebar.tsx", import.meta.url), "utf8");
const js = (source) => transformSync(source, { loader: "ts", target: "esnext" }).code;

test("Composer 带上下文用量重新挂载时先初始化模型依赖，保留本地和漫游用量更新", () => {
  const file = ts.createSourceFile("Composer.tsx", readFileSync(new URL("../src/components/Composer.tsx", import.meta.url), "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const component = file.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "Composer");
  const names = new Set(["contextUsage", "isGuest", "usesPeerModels", "guestModels", "guestModelSource"]);
  // Keep source declaration order: evaluating the memo before its dependencies caused REACTIVITY_HALTED.
  const declarations = component.body.statements.filter(node => ts.isVariableStatement(node)
    && node.declarationList.declarations.some(declaration => names.has(declaration.name.getText(file))));
  assert.equal(declarations.length, names.size);
  const mount = new Function("createMemo", "state", "shownModel", "modelChoices", "splitEffort", `${js(declarations.map(node => node.getText(file)).join("\n"))}; return contextUsage;`);
  const options = limit => ({ configOptions: [{ id: "model", options: [{ value: "model", _meta: { contextWindow: limit } }] }] });
  for (const guest of [false, true]) for (const used of [null, 0, 128000]) {
    const [state, setState] = createStore({
      currentId: "thread", agentKind: "lyra", roamingPeer: guest ? "peer" : null,
      threads: [{ id: "thread", roamingRole: guest ? "guest" : null }],
      items: [{ type: "turn", contextTokens: used }], liveUsage: null,
      modelOptions: { lyra: options(1000000) }, peerModels: { peer: { options: { lyra: options(2000000) } } },
    });
    const limit = guest ? 2000000 : 1000000;
    for (let visit = 0; visit < 2; visit++) {
      let dispose;
      try {
        const usage = createRoot(cleanup => {
          dispose = cleanup;
          return mount(createMemo, state, () => "model", (kind, source) =>
            (source === undefined ? state.modelOptions[kind] : source)?.configOptions[0].options ?? [], splitEffort);
        });
        assert.deepEqual(usage(), { used, limit });
        setState(s => { s.liveUsage = { contextTokens: 256000 }; }); flush();
        assert.deepEqual(usage(), { used: 256000, limit });
        setState(s => { s.liveUsage = null; }); flush();
        assert.deepEqual(usage(), { used, limit });
      } finally { dispose?.(); }
    }
  }
});

test("Composer 上下文缺少用量或模型上限时仍可见，同模型不同档位共用上限", () => {
  const file = ts.createSourceFile("Composer.tsx", readFileSync(new URL("../src/components/Composer.tsx", import.meta.url), "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const component = file.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "Composer");
  const names = new Set(["contextUsage", "isGuest", "usesPeerModels", "guestModels", "guestModelSource"]);
  const declarations = component.body.statements.filter(node => ts.isVariableStatement(node)
    && node.declarationList.declarations.some(declaration => names.has(declaration.name.getText(file))));
  const mount = new Function("createMemo", "state", "shownModel", "modelChoices", "splitEffort", `${js(declarations.map(node => node.getText(file)).join("\n"))}; return contextUsage;`);
  const state = { currentId: "thread", threads: [], agentKind: "lyra", items: [], liveUsage: null };
  let model = "provider/model/variant/high", choices = [];
  const usage = mount(fn => fn, state, () => model, () => choices, splitEffort);
  assert.deepEqual(usage(), { used: null, limit: null });
  state.liveUsage = { contextTokens: 1234 };
  assert.deepEqual(usage(), { used: 1234, limit: null }, "未知上限时不能隐藏已知用量");
  choices = [{ value: "provider/model/variant/low", _meta: { contextWindow: 128000 } }];
  assert.deepEqual(usage(), { used: 1234, limit: 128000 });
  choices = [{ value: "provider/other/variant/high", _meta: { contextWindow: 256000 } }];
  assert.deepEqual(usage(), { used: 1234, limit: null }, "不能借用其他模型的上限");
  model = "provider/model/variant/nested/variant/high";
  choices = [{ value: "provider/model/variant/other/variant/low", _meta: { contextWindow: 256000 } }];
  assert.deepEqual(usage(), { used: 1234, limit: null }, "模型 ID 内的 variant 路径不是强度档位");
});

test("会话标题每笔请求立即更新用量与费用，落库时只接替不叠加", async () => {
  const file = ts.createSourceFile("ChatView.tsx", readFileSync(new URL("../src/components/ChatView.tsx", import.meta.url), "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const format = file.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "fmtSessionCost");
  const component = file.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "ChatView");
  const names = new Set(["displayedItems", "tokenStats", "liveUsage", "totalTokens", "legacyPartsKey", "[legacyPrice, setLegacyPrice]", "sessionEstimate", "sessionCost"]);
  const declarations = component.body.statements.filter(node => (ts.isVariableStatement(node)
    && node.declarationList.declarations.some(declaration => names.has(declaration.name.getText(file))))
    || (ts.isExpressionStatement(node) && node.expression.getText(file).startsWith("createEffect(legacyPartsKey,")));
  assert.equal(declarations.length, names.size + 1);
  const mount = new Function("createMemo", "createSignal", "createEffect", "state", "previewItems", "api", "window",
    `${js([format, ...declarations].map(node => node.getText(file)).join("\n"))}; return { totalTokens, sessionCost };`);
  const [state, setState] = createStore({ currentId: "first", model: "p/m", items: [{ type: "turn", totalTokens: 100, inputTokens: 90, outputTokens: 10 }], liveUsage: null });
  const [previewItems, setPreviewItems] = createSignal(null);
  const requests = [];
  const api = { priceSession: (model, parts) => new Promise(resolve => requests.push({ model, parts, resolve })) };
  let dispose;
  const badge = createRoot(cleanup => { dispose = cleanup; return mount(createMemo, createSignal, createEffect, state, previewItems, api, globalThis); });
  const check = (tokens, price) => assert.deepEqual([badge.totalTokens(), badge.sessionCost()], [tokens, price]);
  try {
    flush();
    assert.equal(requests.length, 1);
    requests[0].resolve(0.25); await Promise.resolve(); flush();
    check(100, "$0.2500");
    setState(s => { s.liveUsage = { totalTokens: 20, costUsd: 0.5 }; }); flush();
    check(120, "$0.7500");
    setState(s => { s.liveUsage = { totalTokens: 30, costUsd: 0.75, unknownRequests: 1 }; }); flush();
    check(130, "≥$1.00");
    setState(s => { s.items.push({ type: "turn", ...s.liveUsage }); s.liveUsage = null; }); flush();
    check(130, "≥$1.00");
    assert.equal(requests.length, 1, "live requests and persisted receipts must not reprice history");
    setPreviewItems([]); flush();
    check(0, null);
    setPreviewItems(null); flush();
    setState(s => { s.currentId = "other"; s.model = "p/n"; s.items = []; }); flush();
    check(0, null);
  } finally { dispose?.(); }
});

test("Composer 缓存命中率按当前会话累计，并随进行中用量更新", () => {
  const file = ts.createSourceFile("Composer.tsx", readFileSync(new URL("../src/components/Composer.tsx", import.meta.url), "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const component = file.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "Composer");
  const declaration = component.body.statements.find(node => ts.isVariableStatement(node)
    && node.declarationList.declarations.some(item => item.name.getText(file) === "cacheUsage"));
  const mount = new Function("createMemo", "state", "running", `${js(declaration.getText(file))}; return cacheUsage;`);
  const [state, setState] = createStore({ running: false, items: [
    { type: "turn", inputTokens: 1000, cacheReadTokens: 500, cacheWriteTokens: 200 },
    { type: "turn", inputTokens: 100, cacheReadTokens: 90 },
  ], liveUsage: null });
  let dispose;
  const usage = createRoot(cleanup => { dispose = cleanup; return mount(createMemo, state, () => state.running); });
  try {
    const history = { input: 1100, read: 590, rate: "53.6" };
    assert.deepEqual(usage(), history, "按 token 加权，而不是对各轮命中率取平均");
    setState(s => { s.running = true; s.liveUsage = null; }); flush();
    assert.deepEqual(usage(), history, "本轮未上报时保留会话已累计用量");
    setState(s => { s.liveUsage = { inputTokens: 400, cacheReadTokens: 100, cacheWriteTokens: 40 }; }); flush();
    assert.deepEqual(usage(), { input: 1500, read: 690, rate: "46.0" }, "实时用量只加一次，输入已包含缓存读写");
    setState(s => { s.liveUsage = { inputTokens: 200, cacheReadTokens: 0 }; }); flush();
    assert.deepEqual(usage(), { input: 1300, read: 590, rate: "45.4" });
    setState(s => { s.liveUsage = { inputTokens: 100 }; }); flush();
    assert.deepEqual(usage(), { input: 1200, read: 590, rate: "49.2" }, "缺失缓存明细时输入仍计入、缓存读取按零计");
    for (const invalid of [
      { inputTokens: 0, cacheReadTokens: 0 }, { inputTokens: 100, cacheReadTokens: 101 },
      { inputTokens: 100, cacheReadTokens: -1 }, { inputTokens: Infinity, cacheReadTokens: 1 },
    ]) {
      setState(s => { s.liveUsage = invalid; }); flush();
      assert.deepEqual(usage(), history, "无效用量不能污染已累计结果");
    }
    setState(s => { s.liveUsage = { inputTokens: 400, cacheReadTokens: 100 }; }); flush();
    setState(s => { s.items.push({ type: "turn", inputTokens: 400, cacheReadTokens: 100 }); s.liveUsage = null; }); flush();
    assert.deepEqual(usage(), { input: 1500, read: 690, rate: "46.0" }, "轮次落定后不能重复计算实时用量");
    setState(s => { s.running = false; s.items.push({ type: "turn" }); }); flush();
    assert.deepEqual(usage(), { input: 1500, read: 690, rate: "46.0" }, "缺少用量的轮次不能清空此前统计");
    setState(s => { s.items = [{ type: "turn", inputTokens: 2000, cacheReadTokens: 100 }]; s.liveUsage = { inputTokens: 100, cacheReadTokens: 100 }; }); flush();
    assert.deepEqual(usage(), { input: 2000, read: 100, rate: "5.0" }, "切换会话后不混入旧会话或非运行状态的实时用量");
    setState(s => { s.items = [{ type: "turn", inputTokens: 100 }]; s.liveUsage = null; }); flush();
    assert.equal(usage(), null, "没有任何缓存明细时不虚报命中率");
    setState(s => { s.running = true; s.liveUsage = { inputTokens: 100, cacheReadTokens: 0 }; }); flush();
    assert.deepEqual(usage(), { input: 200, read: 0, rate: "0.0" });
  } finally { dispose(); }
});

test("Composer 缓存命中显示在速度之后，结束后仍显示会话累计", () => {
  const source = readFileSync(new URL("../src/components/Composer.tsx", import.meta.url), "utf8");
  assert.match(source, /缓存命中率（当前会话累计）/);
  const file = ts.createSourceFile("Composer.tsx", source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let stats;
  const visit = node => {
    if (ts.isJsxElement(node) && node.openingElement.attributes.properties.some(attr => attr.getText(file) === 'class="composer-run-stats"')) stats = node.parent;
    ts.forEachChild(node, visit);
  };
  visit(file);
  assert.ok(stats);
  const code = transformSync(`const view = ${stats.getText(file)};`, { loader: "tsx", jsxFactory: "h", jsxFragment: "Fragment", target: "esnext" }).code;
  const render = new Function("h", "Show", "Fragment", "running", "contextUsage", "cacheUsage", "runElapsed", "tokenSpeed", "waitingForResults", "fmtTokens", "formatContextTokens", `${code}; return view;`);
  const h = (tag, props, ...children) => typeof tag === "function" ? tag({ ...props, children }) : children.join("");
  const Show = ({ when, children, fallback }) => when ? children.map(child => typeof child === "function" ? child(() => when) : child).join("") : fallback ?? "";
  for (const running of [true, false]) for (const used of [null, 0, 128000]) for (const limit of [null, 1000000]) for (const cached of [false, true]) {
    const usage = { used, limit };
    const cache = cached ? { input: 1000, read: 500, rate: "50.0" } : null;
    const context = `上下文 ${used ?? "—"}/${limit ?? "—"}`;
    for (const waiting of [false, true]) {
      const text = render(h, Show, "fragment", () => running, () => usage, () => cache, () => "1:23", () => 35, () => waiting, String, String);
      const base = running ? `1:23·${context}·${waiting ? "等待结果" : "≈35 tok/s"}` : context;
      assert.equal(text, `${base}${cache ? "·50.0%" : running ? "·—%" : ""}`);
    }
  }
});

test("首页和会话输入框都不再展示看板选择按钮", () => {
  for (const name of ["HomeView", "Composer"]) {
    const source = readFileSync(new URL(`../src/components/${name}.tsx`, import.meta.url), "utf8");
    assert.doesNotMatch(source, /LayoutPicker/);
  }
});

test("Composer 的轮次计时不被轮中引导重置", () => {
  const file = ts.createSourceFile("Composer.tsx", readFileSync(new URL("../src/components/Composer.tsx", import.meta.url), "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const component = file.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "Composer");
  const declaration = component.body.statements.find(node => ts.isVariableStatement(node)
    && node.declarationList.declarations.some(item => item.name.getText(file) === "runStartTs"));
  const mount = new Function("createMemo", "state", `${js(declaration.getText(file))}; return runStartTs;`);
  const runStart = (items, running = true) => mount(fn => fn, { currentId: "thread", running: { thread: running }, items })();
  // 轮中引导是同一轮的补充：起点仍是本轮最早那条 user。
  assert.equal(runStart([{ type: "user", ts: 1000 }, { type: "assistant", ts: 2000 }, { type: "user", ts: 9000 }, { type: "assistant", ts: 9500 }]), 1000);
  // 收尾的 turn 之后才有新起点，不能让计时跳回上一轮。
  assert.equal(runStart([{ type: "user", ts: 1000 }, { type: "turn", ts: 2000 }]), null);
  assert.equal(runStart([{ type: "user", ts: 1000 }, { type: "turn", ts: 2000 }, { type: "user", ts: 5000 }]), 5000);
  // 快照未加载（尾部只有流）不能退回当前时间，未运行不显示计时。
  assert.equal(runStart([{ type: "assistant", ts: 1000 }]), null);
  assert.equal(runStart([{ type: "user", ts: 1000 }], false), null);
});

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

test("过期缓存不先上屏：等一次权威快照，旧尾巴不会在切过去后原地跳一下", () => {
  const expression = store.match(/const staleCache = ([^\n]+);/)[1];
  const staleCache = new Function("cached", "staleThreadSnapshots", "id", `return ${expression};`);
  assert.equal(staleCache(undefined, new Set(), "x"), false, "缓存未命中走冷启动");
  assert.equal(staleCache({ subagent: true }, new Set(), "x"), true, "分身每次都校准快照");
  assert.equal(staleCache({ subagent: false }, new Set(["x"]), "x"), true, "后台跑过的会话缓存已过期");
  assert.equal(staleCache({ subagent: false }, new Set(["y"]), "x"), false, "未过期缓存仍然立即上屏");
  // 过期缓存必须同时跳过“清屏”和“立即上屏”，否则会先画旧尾巴再被替换。
  assert.match(store, /if \(switching && \(!cached \|\| staleCache\)\) \{/);
  assert.match(store, /if \(cached && !staleCache\) \{/);
});

test("多 stage 同链反复切换复用导航列表，新增阶段后更新，断链和成环不挂起", () => {
  const chat = readFileSync(new URL("../src/components/ChatView.tsx", import.meta.url), "utf8");
  const source = chat.slice(chat.indexOf("  const stageIndex ="), chat.indexOf("  const isStageTitle ="));
  const initial = Array.from({ length: 200 }, (_, i) => ({ id: `s${i}`, parentThreadId: i ? `s${i - 1}` : null, createdAt: i }));
  const [threads, setThreads] = createSignal(initial);
  const [currentId, setCurrentId] = createSignal("s0");
  let scans = 0, dispose;
  const state = { get threads() { scans++; return threads(); }, get currentId() { return currentId(); } };
  const stages = createRoot(cleanup => {
    dispose = cleanup;
    return new Function("state", "createMemo", `${js(source)}; return stageThreads;`)(state, createMemo);
  });
  try {
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

test("search reveal waits for bottom-follow state to commit and ignores a departed session", async t => {
  const chat = readFileSync(new URL("../src/components/ChatView.tsx", import.meta.url), "utf8");
  const from = chat.indexOf("  // 会话搜索点击命中：");
  const source = chat.slice(from, chat.indexOf("  // 会话加载和新增轮次", from));
  const [hit, setHit] = createSignal(null);
  const [following, setFollowing] = createSignal(true);
  const [id, setId] = createSignal('a');
  const calls = [];
  createRoot(dispose => {
    t.after(dispose);
    new Function('createEffect', 'searchReveal', 'setSearchReveal', 'state', 'transcriptReady', 'previewItems', 'cancelBottomFollow', 'transcriptRef', source)(
      createEffect, hit, setHit, { get currentId() { return id(); }, loadingThread: false }, () => true, () => null,
      () => { setFollowing(false); }, { revealMatch(...args) { assert.equal(following(), false); calls.push(args); } });
  });
  flush(() => setHit({ threadId: 'a', itemId: 42, query: 'target' }));
  await new Promise(setImmediate);
  assert.deepEqual(calls, [[42, 'target']]);
  flush(() => setHit({ threadId: 'a', itemId: 43, query: 'stale' }));
  flush(() => setId('b'));
  await new Promise(setImmediate);
  assert.deepEqual(calls, [[42, 'target']]);
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

test("关闭文件标签前提交编辑快照，未保存内容必须确认且保存失败不丢草稿", async t => {
  const workspace = readFileSync(new URL("../src/components/WorkspacePanel.tsx", import.meta.url), "utf8");
  const actions = workspace.slice(workspace.indexOf("  const snapshot ="), workspace.indexOf("  const revealLine ="));
  const mount = (options = {}) => {
    const writes = [];
    const api = { saveWorkspaceFile: async (...args) => {
      writes.push(args);
      if (options.saveError) throw Error("save failed");
    } };
    return { writes, ...new Function("createSignal", "flush", "api", "options", `
      const initial = (options.ids ?? ["a"]).map(path => ({ file: { path, kind: "text", text: "original" }, original: "original", draft: "original" }));
      const current = initial[0], props = { threadId: "thread" };
      const [tabs, setTabs] = createSignal(initial), [preview, setPreview] = createSignal(current.file);
      const [draft, setDraft] = createSignal(options.clean ? "original" : "edited");
      const [original, setOriginal] = createSignal("original"), [closing, setClosing] = createSignal();
      const [editing, setEditing] = createSignal(true), [source, setSource] = createSignal(false);
      const [saved, setSaved] = createSignal(false), [error, setError] = createSignal("");
      const [saving, setSaving] = createSignal(false), [artifacts, setArtifacts] = createSignal([]);
      const setLoading = () => {}, mode = () => "files", setMode = () => {};
      const panel = { querySelector: () => null }, codeView = undefined;
      const drafts = new Map(options.clean ? [] : [["a", { text: "edited" }]]);
      const draftKey = path => path, normalized = text => text.replace(/\\r\\n/g, "\\n"), commitSheet = async () => true;
      let request = 0;
      ${js(actions)}
      return { closeTab, resolveClose, tabs, preview, closing, drafts, error };
    `)(createSignal, flush, api, options) };
  };
  await t.test("dirty active tab asks before closing; discard only happens after confirmation", async () => {
    const panel = mount();
    await panel.closeTab("a"); flush();
    assert.equal(panel.closing(), "a");
    assert.equal(panel.tabs()[0].draft, "edited");
    assert.equal(panel.drafts.has("a"), true);
    await panel.resolveClose(false); flush();
    assert.equal(panel.tabs().length, 0);
    assert.equal(panel.drafts.has("a"), false);
  });
  await t.test("confirmed save writes the current edit before closing", async () => {
    const panel = mount();
    await panel.closeTab("a"); flush();
    await panel.resolveClose(true); flush();
    assert.deepEqual(panel.writes, [["thread", "a", "original", "edited"]]);
    assert.equal(panel.tabs().length, 0);
  });
  await t.test("failed save keeps the tab and recovery buffer", async () => {
    const panel = mount({ saveError: true });
    await panel.closeTab("a"); flush();
    await panel.resolveClose(true); flush();
    assert.equal(panel.tabs()[0].draft, "edited");
    assert.equal(panel.drafts.has("a"), true);
    assert.match(panel.error(), /save failed/);
  });
  await t.test("closing a clean inactive tab preserves the active edit", async () => {
    const panel = mount({ ids: ["a", "b"] });
    await panel.closeTab("b"); flush();
    assert.deepEqual(panel.tabs().map(tab => tab.file.path), ["a"]);
    assert.equal(panel.tabs()[0].draft, "edited");
    assert.equal(panel.drafts.has("a"), true);
    assert.equal(panel.closing(), undefined);
  });
  await t.test("clean active tab closes without a confirmation", async () => {
    const panel = mount({ clean: true, ids: ["a", "b"] });
    await panel.closeTab("a"); flush();
    assert.deepEqual(panel.tabs().map(tab => tab.file.path), ["b"]);
    assert.equal(panel.preview().path, "b");
    assert.equal(panel.closing(), undefined);
  });
});

test("中转模型预加载等待连接及成员状态提交，覆盖事件与初始化", async t => {
  const ensure = store.slice(store.indexOf("export function ensurePeerModels("), store.indexOf("/** worktree「基于分支」"));
  const preload = store.slice(store.indexOf("export function preloadPeerModels("), store.indexOf("/** 检查更新"));
  const handlers = store.slice(store.indexOf('    listen<RelayStatus>("relay:status"'), store.indexOf("    // 漫游：对端回传其可选模型"));
  const initialStart = store.lastIndexOf("  void api", store.indexOf("    .getRelayPeers()"));
  const initial = store.slice(initialStart, store.indexOf("  void refreshInbox();", initialStart));
  const actions = js(`${ensure}\n${preload}\nconst subscriptions = [${handlers}];\nconst loadInitialPeers = () => { ${initial} };`.replace(/export /g, ""));
  const peer = { token: "peer", online: true };
  const mount = (connected, peers, cached = {}) => {
    const [state, setState] = createStore({ relay: { connected }, peers, settings: { relayToken: "self" }, peerModels: cached });
    const calls = [], events = new Map();
    const api = { requestPeerModels: async token => { calls.push(token); }, getRelayPeers: async () => [peer] };
    const actionsForState = new Function("state", "setState", "api", "onFlush", "listen", "normalizePeers", "refreshInbox", `${actions}; return { preloadPeerModels, loadInitialPeers };`)(
      state, setState, api, onFlush, (name, handler) => events.set(name, handler), raw => Array.isArray(raw) ? raw : raw.peers, async () => {});
    return { calls, events, ...actionsForState };
  };
  const settle = () => new Promise(setImmediate);
  await t.test("reconnect refreshes even a cached online peer", async () => {
    const relay = mount(false, [peer], { peer: {} });
    relay.events.get("relay:status")({ payload: { connected: true } });
    await settle();
    assert.deepEqual(relay.calls, ["peer"]);
  });
  await t.test("presence uses the new peers, skips self/offline and preserves force semantics", async () => {
    const relay = mount(true, [{ token: "departed", online: true }], { peer: {} });
    relay.events.get("relay:peers")({ payload: { peers: [peer, { token: "self", online: true }, { token: "offline", online: false }] } });
    await settle();
    assert.deepEqual(relay.calls, ["peer"]);
    relay.preloadPeerModels(); await settle();
    assert.deepEqual(relay.calls, ["peer"], "normal preloading still reuses the cache");
  });
  await t.test("startup preloads newly loaded peers without waiting for the timer", async () => {
    const relay = mount(true, []);
    relay.loadInitialPeers(); await settle();
    assert.deepEqual(relay.calls, ["peer"]);
  });
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
