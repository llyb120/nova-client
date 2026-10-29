import { createEffect, createSignal, For, JSX, on, onCleanup, onMount, Show } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Portal } from "solid-js/web";
import { openThread, showToast, state } from "../store";
import type { Peer } from "../types";
import { Markdown } from "./Markdown";
import "./EmployeeView.css";
import { TitleBarPortal } from "./TitleBar";
import VaultPop from "./VaultPop";

/** 交代的人；token 为空是员工所在机器的用户。 */
type Owner = { token: string; name: string };
type Duty = { id: string; text: string; enabled: boolean; note: string; everyMinutes: number; times?: string[]; events?: string[]; nextCheckAt: number; lastRunAt: number; lastResult: string; owner?: Owner; plan?: Todo[] };
/** 待办即任务树节点：parent 上级、after 先完成的兄弟、origin 衍生出它的节点；status 非空为已收尾（在 finished 里）。 */
type Todo = { id: string; text: string; confirm: boolean; dutyId?: string; failures?: number; profile?: string; createdAt: number; threadId?: string | null; nextCheckAt?: number; desktop?: boolean; askAfter?: number;
  owner?: Owner; waiting?: string; replyTo?: { token: string; name: string; id: string } | null; shot?: string;
  parent?: string; after?: string[]; origin?: string; branch?: string; status?: string; result?: string; finishedAt?: number; relayed?: boolean; planning?: boolean; checkpoint?: string };
const from = (o?: Owner) => (o?.token ? o.name || o.token : "");
/** chain：同一任务接力出的各段会话共用的首段会话 id，旧记录为空（自成一组）。 */
type Run = { at: number; dutyId: string; label?: string; status?: string; result: string; threadId: string; chain?: string; scene?: string; ended?: number };
/** 团队成员查看员工会话：只有交代时的原文与这段的结论。 */
type Entry = { kind: "ask" | "help" | "answer" | "confirm" | "approve" | "result"; who?: string; text: string; at: number; scene?: string; status?: string };
type Transcript = { title: string; entries?: Entry[] };
type Memory = { id: string; text: string; at: number; importance?: number; strength?: number; usedAt?: number };
/** 可订阅的事件（与后端 employee::EVENTS 一致）。 */
const EVENTS: [string, string][] = [["orbit.added", "星轨新增事项"], ["orbit.done", "星轨事项完成"], ["orbit.changed", "星轨任何更新"]];
const IMPORTANCE = ["", "", "重要", "长期"];
// 与后端 Memory::retention 同一公式（MemoryBank 艾宾浩斯曲线），只用于展示。
const retention = (m: Memory, now: number) => {
  const importance = Math.min(Math.max(m.importance ?? 1, 1), 3);
  if (importance >= 3) return 1;
  const days = Math.max(0, now - Math.max(m.usedAt ?? 0, m.at)) / 86_400_000;
  return Math.exp(-days / (5 * Math.max(m.strength ?? 1, 1) * importance));
};
type Task = { threadId: string; itemId: string; queued: boolean; desktop: "using" | "asking" | "reserved" | null };
type Snapshot = {
  employee: {
    enabled: boolean; workStart: string; workEnd: string; idleMinutes: number; takeoverSeconds?: number; public?: boolean;
    duties: Duty[]; inbox: Todo[]; runs: Run[]; memories?: Memory[]; finished?: Todo[];
  };
  status: "working" | "duty" | "rest";
  tasks: Task[];
  parallel: number;
  nextCheckAt: number;
  /** 各职责下次触发；有未收尾的任务时为 0。 */
  dues: Record<string, number>;
  idleSupported: boolean;
};

const STATUS = { working: "工作中", duty: "值班中", rest: "休息中" } as const;
const DESKTOP = { using: "正在操作电脑", asking: "等你确认操作电脑", reserved: "电脑已排给它，动手前会问你" } as const;
const LEGACY = { inbox: "待办", say: "对员工说", approve: "批准执行" } as Record<string, string>;
const SHOWN = 30;
const time = (ms: number) => (ms ? new Date(ms).toLocaleString(undefined, { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" }) : "—");

// 放在模块里：进会话再返回时还是原来的 tab 和员工。
const [tab, setTab] = createSignal<"local" | "team">("local");
const [peer, setPeer] = createSignal<Peer | null>(null);

export default function EmployeeView() {
  return (
    <div class="employee-page">
      <div class="employee-tabs" role="tablist" aria-label="数字员工">
        <button type="button" role="tab" aria-selected={tab() === "local"} onClick={() => setTab("local")}>本机</button>
        <button type="button" role="tab" aria-selected={tab() === "team"} onClick={() => setTab("team")}>团队</button>
      </div>
      <Show when={tab() === "team"} fallback={<EmployeeBoard />}>
        <Show when={peer()} keyed fallback={<TeamList onPick={setPeer} />}>
          {(p) => <EmployeeBoard peer={p} onBack={() => setPeer(null)} />}
        </Show>
      </Show>
    </div>
  );
}

type PeerInfo = { status: keyof typeof STATUS; tasks: number };

/** 团队里公开的数字员工：向在线成员打听，未公开或旧版本的不显示。 */
function TeamList(props: { onPick: (peer: Peer) => void }) {
  const me = () => state.settings?.relayToken ?? "";
  // 排除自己：云端员工请用单独的 token 连中转站。
  const peers = () => state.peers.filter((p) => p.online && p.token !== me());
  const [info, setInfo] = createStore<Record<string, PeerInfo | false>>({});
  const probe = () => peers().forEach((p) => void invoke<PeerInfo>("starlink_call", { peer: p.token, command: "info", args: {} })
    .then((v) => setInfo(p.token, v), () => setInfo(p.token, false)));
  createEffect(on(() => peers().map((p) => p.token).join(), probe));
  const shown = () => peers().filter((p) => info[p.token]);
  const probing = () => peers().some((p) => info[p.token] === undefined);
  return (
    <div class="employee-content">
      <TitleBarPortal><header class="employee-header">
        <div>
          <div class="employee-title"><h1>团队的数字员工</h1></div>
          <p class="employee-hint">团队成员公开的员工（StarLink），在对方电脑或云端按同样的规则运行。你交代的事缺信息时会请你或你的本地员工补充，待确认事项由你批准。</p>
        </div>
        <div class="employee-header-actions"><button type="button" onClick={probe}>刷新</button></div>
      </header></TitleBarPortal>
      <Show when={state.settings?.relayToken?.trim()} fallback={<p class="employee-empty">先在「设置」里配置团队中转站，才能看到团队的数字员工。</p>}>
        <div class="employee-team">
          <For each={shown()}>{(p) => {
            const i = () => info[p.token] as PeerInfo;
            return (
              <button type="button" class="employee-card employee-peer" onClick={() => props.onPick(p)}>
                <span class="employee-title"><strong>{p.name}</strong><span class={`employee-badge ${i().status}`}>{STATUS[i().status] ?? i().status}</span></span>
                <small class="employee-hint">进行中 {i().tasks} 项</small>
              </button>
            );
          }}</For>
        </div>
        <Show when={!shown().length}>
          <p class="employee-empty">{probing() ? "正在寻找团队里公开的数字员工…" : "暂无在线成员公开数字员工。"}</p>
        </Show>
      </Show>
    </div>
  );
}

/** 一个员工的看板：本机，或经 StarLink 调用的团队成员的员工（同名命令在对方执行）。 */
function EmployeeBoard(props: { peer?: Peer; onBack?: () => void }) {
  const peer = props.peer;
  const call = <T,>(command: string, args: Record<string, unknown> = {}) =>
    peer ? invoke<T>("starlink_call", { peer: peer.token, command, args }) : invoke<T>(command, args);
  // 自己在这个员工眼里的身份：本机为空，团队里是自己的中转站 token。
  const me = peer ? state.settings?.relayToken ?? "" : "";
  // 不用 createResource：页面在 <Suspense> 里，轮询 refetch 会让整页切到 fallback 闪烁并丢失输入焦点；
  // reconcile 保持行对象稳定，列表不重建，展开的菜单也不会被关掉。
  const [store, setStore] = createStore<{ snap?: Snapshot }>({});
  const [now, setNow] = createSignal(Date.now());
  const [failed, setFailed] = createSignal("");
  const data = () => store.snap;
  const refetch = () => call<Snapshot>("employee_get").then(
    (snap) => { setStore("snap", reconcile(snap)); setNow(Date.now()); setFailed(""); },
    (error) => { setFailed(`读取员工数据失败：${String(error)}`); if (!peer) showToast(failed()); },
  );
  void refetch();
  const [say, setSay] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [addingDuty, setAddingDuty] = createSignal(false);
  const [transcript, setTranscript] = createSignal<Transcript | null>(null);
  // 右侧详情选中的任务 / 职责 / 会话；模块级的不用：换了员工（团队）就该重新选。
  const [pick, setPick] = createSignal<Pick | null>(null);
  let sayInput: HTMLTextAreaElement | undefined;
  // 团队员工的会话在对方机器上：取文字记录展示。
  const open = (threadId: string) => peer
    ? void call<Transcript>("transcript", { threadId }).then(setTranscript, (error) => showToast(String(error)))
    : void openThread(threadId);

  onMount(() => {
    const unlisten = listen("employee:changed", () => { if (!peer) void refetch(); });
    // ponytail: 团队员工靠轮询全量快照；人多、快照大时改成对方变化时推送。
    const timer = setInterval(() => void refetch(), peer ? 10000 : 5000);
    // 「⋯」菜单是 <details>：点到菜单外部时关掉所有已展开的菜单。
    // 用 composedPath 判断：点击会切换弹层内容（如密码本「新增」换成表单），被点的元素已移出 DOM，contains 会误判为外部。
    const closeMenus = (ev: Event) => { const path = ev.composedPath(); document.querySelectorAll<HTMLDetailsElement>(".employee-more[open], .employee-pop[open]").forEach((d) => { if (!path.includes(d)) d.open = false; }); };
    const onKey = (ev: KeyboardEvent) => { if (ev.key === "Escape") { closeMenus(ev); setTranscript(null); } };
    document.addEventListener("click", closeMenus);
    document.addEventListener("keydown", onKey);
    onCleanup(() => { void unlisten.then((off) => off()); clearInterval(timer); document.removeEventListener("click", closeMenus); document.removeEventListener("keydown", onKey); });
  });

  const run = async (command: string, args: Record<string, unknown>) => {
    let ok = true;
    try {
      const out = await call(command, args);
      if (typeof out === "string" && out) showToast(out);
    } catch (error) { showToast(String(error)); ok = false; }
    void refetch();
    return ok;
  };
  const act = (action: string, id?: string) => run("employee_do", { action, id: id ?? null });
  const set = (patch: Record<string, unknown>) => run("employee_set", { patch });
  const send = async () => {
    const text = say().trim();
    if (!text || busy()) return;
    setBusy(true);
    try { await call("employee_say", { text }); setSay(""); } catch (error) { showToast(String(error)); }
    setBusy(false);
    void refetch();
  };

  return (
      <Show when={data()} fallback={<p class="employee-empty">{failed() || "加载中…"}</p>}>
        {(snap) => {
          const e = () => snap().employee;
          const label = (r: Run) => r.label || flat(e().duties.find((d) => d.id === r.dutyId)?.text ?? "") || LEGACY[r.dutyId] || r.dutyId;
          // 每个会话只看最新一条运行记录（续做会在同一会话追加记录）。
          const latest = () => {
            const seen = new Set<string>();
            return [...e().runs].reverse().filter((r) => !seen.has(r.threadId) && !!seen.add(r.threadId));
          };
          const live = (r: Run) => snap().tasks.some((t) => t.threadId === r.threadId);
          const busyItems = () => new Set(snap().tasks.map((t) => t.itemId));
          const running = () => snap().tasks.filter((t) => !t.queued);
          const queued = () => snap().tasks.filter((t) => t.queued);
          const runOf = (threadId: string) => latest().find((r) => r.threadId === threadId);
          // 同一任务接力出的各段会话归成一组（旧 → 新），按最新一段排序，组的状态看最新一段。
          const groups = () => {
            const by = new Map<string, Run[]>();
            for (const r of latest()) by.set(r.chain || r.threadId, [r, ...(by.get(r.chain || r.threadId) ?? [])]);
            return [...by.values()];
          };
          const last = (g: Run[]) => g[g.length - 1];
          const segmentsOf = (threadId: string) => groups().find((g) => g.some((r) => r.threadId === threadId)) ?? [];
          const confirms = () => e().inbox.filter((t) => t.confirm);
          const waits = () => e().inbox.filter((t) => !t.confirm && t.waiting && !busyItems().has(t.id));
          const nodes = () => [...e().inbox, ...(e().finished ?? [])].filter((t) => !t.confirm);
          const nodeOf = (id: string) => nodes().find((t) => t.id === id);
          const stateOf = (t: Todo) => nodeState(t, nodes(), snap().tasks, now());
          const duties = () => [...e().duties].sort((a, b) => Number(b.enabled) - Number(a.enabled));
          // 每个任务家族（树根 + 它衍生出的独立任务树）在列表里占一行。
          const tops = () => {
            const seen = new Set<string>();
            return nodes().map((t) => familyTop(t.id, nodes())).filter((t): t is Todo => !!t && !seen.has(t.id) && !!seen.add(t.id));
          };
          const familyRow = (top: Todo): Row => {
            const all = familyNodes(top, nodes());
            const states = all.map(stateOf);
            const state = ["planning", "running", "queued", "help", "deferred"].find((s) => states.includes(s))
              ?? (top.status && all.some((t) => !t.status) ? "kids" : stateOf(top));
            const finished = states.filter((s) => s === "done").length;
            const desk = snap().tasks.find((x) => x.desktop && all.some((t) => t.id === x.itemId))?.desktop;
            const checkAt = Math.min(...all.filter((t) => stateOf(t) === "deferred").map((t) => t.nextCheckAt!));
            const meta = [top.dutyId ? "职责" : top.origin ? "衍生" : "待办", all.length > 1 ? `${finished}/${all.length} 完成` : "",
              Number.isFinite(checkAt) ? `${time(checkAt)} 检查` : "", desk ? DESKTOP[desk] : "", time(top.finishedAt || top.createdAt)];
            return { key: top.id, title: flat(top.text), state, meta: meta.filter(Boolean).join(" · "), pick: { kind: "task", id: top.id } };
          };
          const runRow = (g: Run[]): Row => {
            const r = last(g);
            const task = snap().tasks.find((t) => t.threadId === r.threadId);
            const state = task ? (task.queued ? "queued" : "running") : !r.status || r.status === "done" ? "done" : "stopped";
            return { key: r.threadId, title: label(r), state, meta: time(g[0].at), pick: { kind: "run", id: r.threadId } };
          };
          const RANK: Record<string, number> = { planning: 0, running: 0, queued: 1, help: 2, kids: 3, relayed: 3, resume: 4, deferred: 5, deps: 5, future: 6 };
          const active = (top: Todo) => familyNodes(top, nodes()).some((t) => !t.status);
          // 任务：还没收尾的任务家族，加上不在任务树里、正在跑的会话（对员工说、已批准的事项）。
          const taskRows = () => [
            ...tops().filter(active).map(familyRow),
            ...groups().filter((g) => live(last(g)) && !nodeOf(last(g).dutyId)).map(runRow),
          ].sort((a, b) => (RANK[a.state] ?? 9) - (RANK[b.state] ?? 9));
          // 最近：已经整体收尾的任务家族，加上不在任务树里的已结束会话。
          const recentRows = () => [
            ...tops().filter((t) => !active(t)).map((t) => ({ row: familyRow(t), at: t.finishedAt ?? 0 })),
            ...groups().filter((g) => !live(last(g)) && !nodeOf(last(g).dutyId)).map((g) => ({ row: runRow(g), at: last(g).ended ?? last(g).at })),
          ].sort((a, b) => b.at - a.at).slice(0, SHOWN).map((x) => x.row);
          const dutyRow = (duty: Duty): Row => {
            const tasks = e().inbox.filter((t) => t.dutyId === duty.id);
            const due = snap().dues?.[duty.id] ?? 0;
            const meta = !duty.enabled ? "已停用" : [tasks.length ? `${tasks.length} 轮处理中` : "", due ? `下次 ${time(due)}` : duty.events?.length ? "等事件触发" : tasks.length ? "" : "未安排"].filter(Boolean).join(" · ");
            return { key: duty.id, title: flat(duty.text), state: !duty.enabled ? "stopped" : tasks.length ? familyRow(tasks[tasks.length - 1]).state : "future", meta, pick: { kind: "duty", id: duty.id } };
          };
          // 右侧详情：选中的已不存在时回到第一项任务（没有就第一条职责）。
          const current = (): Pick | null => {
            const p = pick();
            const exists = p && (p.kind === "duty" ? e().duties.some((d) => d.id === p.id) : p.kind === "task" ? !!nodeOf(p.id) : e().runs.some((r) => r.threadId === p.id));
            if (exists) return p;
            return taskRows()[0]?.pick ?? (duties()[0] ? { kind: "duty", id: duties()[0].id } : null);
          };
          const currentKey = () => { const c = current(); return c ? `${c.kind}:${c.id}` : ""; };
          const row = (r: Row) => <RowItem row={r} active={currentKey() === `${r.pick.kind}:${r.pick.id}`} onPick={() => setPick(r.pick)} />;
          const treeProps = { now, open, act, run, local: !peer };
          const dutyDetail = (id: string) => {
            const duty = () => e().duties.find((d) => d.id === id);
            const tasks = () => e().inbox.filter((t) => t.dutyId === id).sort((a, b) => b.createdAt - a.createdAt);
            const history = () => tops().filter((t) => t.dutyId === id && !active(t)).sort((a, b) => (b.finishedAt ?? 0) - (a.finishedAt ?? 0)).slice(0, 10);
            return (
              <Show when={duty()}>{(d) => (
                <>
                  <DutyCard duty={d()} due={d().enabled ? snap().dues?.[d().id] ?? 0 : 0} task={tasks().length > 0} now={now()} run={run} act={act} />
                  <For each={tasks()}>{(t) => <>
                    <h3 class="employee-detail-sub">本轮序列 · {time(t.createdAt)}</h3>
                    <Show when={t.planning}><p class="employee-hint">正在规划本轮步骤与依赖，规划完成后才开始执行；不是接着旧任务做。</p></Show>
                    <TaskTree id={t.id} nodes={nodes()} {...treeProps} tasks={snap().tasks} />
                  </>}</For>
                  <h3 class="employee-detail-sub">下次触发的参考序列</h3>
                  <Show when={d().plan?.length} fallback={<p class="employee-hint">首次触发会先生成并保存完整步骤，再开始执行；规划完成后这里即可预览，不必等整轮做完。</p>}>
                    <p class="employee-hint">最近规划的步骤模板。下次触发会新建独立序列，按当次情况重新规划；运行中随步骤结果调整，不继承旧会话或完成状态。</p>
                    <TaskTree id={`plan:${id}`} nodes={planNodes(d())} readonly {...treeProps} tasks={[]} />
                  </Show>
                  <Show when={history().length}>
                    <h3 class="employee-detail-sub">最近执行</h3>
                    <div class="employee-rows"><For each={history()}>{(t) => row(familyRow(t))}</For></div>
                  </Show>
                </>
              )}</Show>
            );
          };
          const runDetail = (threadId: string) => {
            const g = () => groups().find((x) => x.some((r) => r.threadId === threadId)) ?? [];
            const r = () => last(g());
            const task = () => snap().tasks.find((t) => t.threadId === threadId);
            return (
              <Show when={r()}>{(x) => (
                <>
                  <div class="employee-detail-head">
                    <h2>{label(x())}</h2>
                    <small>{time(g()[0]?.at ?? 0)}</small>
                  </div>
                  <div class="employee-transcript-result"><Markdown text={task() ? "进行中…" : resultOf(x())} /></div>
                  <div class="employee-item-actions">
                    <button type="button" onClick={() => open(x().threadId)}>看会话</button>
                    <Show when={task()}>{(t) => <button type="button" onClick={() => void act("stop", t().threadId)}>停止</button>}</Show>
                  </div>
                </>
              )}</Show>
            );
          };

          return (
            <div class="employee-content">
              {/* 一行头部：标题、状态、开关与不常用的设置/记忆（弹出），把纵向空间留给看板。 */}
              <TitleBarPortal><header class="employee-header">
                <div class="employee-title">
                  <Show when={props.onBack}>{(back) => <button type="button" aria-label="返回团队列表" onClick={() => back()()}>←</button>}</Show>
                  <h1 title={peer ? "你交代的事归你：缺信息时会请你或你的本地员工补充，待确认事项由你或员工主人批准" : undefined}>{peer ? `${peer.name} 的数字员工` : "数字员工"}</h1>
                  <span class={`employee-badge ${snap().status}`}>{STATUS[snap().status]}</span>
                  <span class="employee-chip">下次触发<strong>{time(snap().nextCheckAt)}</strong></span>
                  <span class="employee-chip">进行中<strong>{running().length} / {snap().parallel}</strong></span>
                </div>
                <div class="employee-header-actions">
                  <Show when={!peer}><label class="employee-toggle"><input type="checkbox" role="switch" checked={e().enabled}
                    onChange={(ev) => void set({ enabled: ev.currentTarget.checked })} /> 自动值班</label></Show>
                  <button type="button" title="不等触发时间，立即开始一件已到点的事项" onClick={() => void act("check")}>执行到点事项</button>
                  <details class="employee-pop employee-memory">
                    <summary>记忆 <span class="employee-count">{e().memories?.length ?? 0}</span></summary>
                    <div>
                      <p class="employee-hint">跨任务共享的偏好和经验，用得越多越牢，对员工说“长期记住…”的不会忘。</p>
                      <Show when={e().memories?.length} fallback={<p class="employee-empty employee-empty-inline">还没有记忆。员工在工作中发现以后用得上的信息会记在这里。</p>}>
                        <ul class="employee-list">
                          <For each={e().memories}>{(m) => (
                            <li>
                              <div class="employee-main"><div class="employee-item-title">{m.text}</div>
                                <small>{IMPORTANCE[m.importance ?? 1] ? `${IMPORTANCE[m.importance ?? 1]} · ` : ""}用到 {(m.strength ?? 1) - 1} 次 · 最近 {time(m.usedAt || m.at)} · 记忆度 {Math.round(retention(m, now()) * 100)}%</small></div>
                              <button type="button" onClick={() => void act("forget", m.id)}>忘掉</button>
                            </li>
                          )}</For>
                        </ul>
                      </Show>
                    </div>
                  </details>
                  <Show when={!peer}><VaultPop /></Show>
                  <Show when={!peer}>
                  <details class="employee-pop employee-settings">
                    <summary>值班设置</summary>
                    <div>
                  <Show when={!snap().idleSupported}><p class="employee-hint">当前平台无法检测空闲：操作电脑前总会先询问你。</p></Show>
                  <div class="employee-field">
                    <span id="employee-hours-label">工作时段</span>
                    <div class="employee-time-range" role="group" aria-labelledby="employee-hours-label">
                      <input type="time" aria-label="开始工作时间" value={e().workStart} onChange={(ev) => void set({ workStart: ev.currentTarget.value })} />
                      <span>至</span>
                      <input type="time" aria-label="结束工作时间" value={e().workEnd} onChange={(ev) => void set({ workEnd: ev.currentTarget.value })} />
                    </div>
                  </div>
                  <label class="employee-field">空闲多久算你不在
                    <span class="employee-idle-input"><input type="number" min="1" max="1440" value={e().idleMinutes}
                      onChange={(ev) => void set({ idleMinutes: Number(ev.currentTarget.value) || 10 })} /> 分钟</span>
                  </label>
                  <label class="employee-field" title="操作电脑前会在 Butterfly 上询问你，这么久不选择就自动开始">多久不选自动接管电脑
                    <span class="employee-idle-input"><input type="number" min="3" max="600" value={e().takeoverSeconds ?? 15}
                      onChange={(ev) => void set({ takeoverSeconds: Number(ev.currentTarget.value) || 15 })} /> 秒</span>
                  </label>
                  <details class="employee-help"><summary>工作方式说明</summary><p class="employee-hint">每件事只在安排好的时间触发；职责先规划独立序列再执行，运行中按步骤结果调整后续计划，不会空跑会话去检查。多件事并行处理；操作电脑是单独排队的一步，每次开始前都会在 Butterfly 上提示你，{e().takeoverSeconds ?? 15} 秒内不选择即开始。员工操作电脑时（屏幕边缘呼吸灯亮着）按 Esc 即可叫停；你拒绝或叫停后，员工会在你空闲时、或过一阵再问你，择机补做。</p>
                  <p class="employee-hint">员工从「设置 → 场景模型 → 轻量化」开局，编码交给编码场景，操作电脑交给剑来场景的模型。</p></details>
                  <label class="employee-field employee-public" title="经团队中转站公开：团队成员都能查看并下达指令；谁交代的事缺信息时找谁补充，待确认由谁批准">
                    <input type="checkbox" role="switch" checked={!!e().public} onChange={(ev) => void set({ public: ev.currentTarget.checked })} /> 公开到团队（StarLink）
                  </label>
                    </div>
                  </details>
                  </Show>
                </div>
              </header></TitleBarPortal>

              <div class="employee-layout">
                <aside class="employee-side" aria-label="任务与职责">
                  <form class="employee-say" onSubmit={(ev) => { ev.preventDefault(); void send(); }}>
                    <textarea id="employee-message" ref={sayInput} rows={2} aria-label={peer ? `交给 ${peer.name} 的员工一件事` : "交给员工一件事"} title="Enter 发送，Shift+Enter 换行"
                      placeholder={`${peer ? `交给 ${peer.name} 的员工一件事` : "交给员工一件事"}：派活、新增职责或修改安排，Enter 发送`}
                      value={say()} onInput={(ev) => setSay(ev.currentTarget.value)}
                      onKeyDown={(ev) => { if (ev.key === "Enter" && !ev.shiftKey && !ev.isComposing) { ev.preventDefault(); void send(); } }} />
                    <button type="submit" class="employee-primary" disabled={busy() || !say().trim()}>{busy() ? "发送中…" : "发送"}</button>
                  </form>
                  <Show when={confirms().length + waits().length}>
                    <section class="employee-section" aria-labelledby="employee-sec-attention">
                      <h2 id="employee-sec-attention">需要你处理 <span class="employee-count is-alert">{confirms().length + waits().length}</span></h2>
                      <For each={confirms()}>{(item) => <ConfirmCard item={item} me={me} run={run} act={act} open={open} />}</For>
                      <For each={waits()}>{(item) => <HelpCard item={item} me={me} run={run} open={open} local={!peer} />}</For>
                    </section>
                  </Show>
                  <section class="employee-section" aria-labelledby="employee-sec-tasks">
                    <h2 id="employee-sec-tasks" title={`最多同时进行 ${snap().parallel} 项，只有操作电脑时排队`}>任务 <span class="employee-count">{taskRows().length}</span></h2>
                    <Show when={taskRows().length} fallback={<p class="employee-hint">暂无。<button type="button" class="employee-text-button" onClick={() => sayInput?.focus()}>交给员工一件事</button></p>}>
                      <div class="employee-rows"><For each={taskRows()}>{row}</For></div>
                    </Show>
                  </section>
                  <section class="employee-section" aria-labelledby="employee-sec-duties">
                    <h2 id="employee-sec-duties" title="做什么 + 何时触发；每次触发生成一项任务">职责 <span class="employee-count">{e().duties.length}</span>
                      <Show when={!peer && !addingDuty()}><button type="button" class="employee-text-button" onClick={() => setAddingDuty(true)}>+ 新增</button></Show>
                    </h2>
                    <Show when={addingDuty()}><DutyForm run={run} onClose={() => setAddingDuty(false)} /></Show>
                    <div class="employee-rows"><For each={duties()}>{(d) => row(dutyRow(d))}</For></div>
                  </section>
                  <Show when={recentRows().length}>
                    <section class="employee-section" aria-labelledby="employee-sec-recent">
                      <h2 id="employee-sec-recent">最近</h2>
                      <div class="employee-rows"><For each={recentRows()}>{row}</For></div>
                    </section>
                  </Show>
                </aside>
                <section class="employee-detail" aria-label="详情">
                  <Show when={currentKey()} keyed fallback={<p class="employee-hint">从左边选一个任务或职责，这里会画出它的任务树。</p>}>
                    {(_key) => {
                      const c = current()!;
                      return c.kind === "task" ? <TaskTree id={c.id} nodes={nodes()} {...treeProps} tasks={snap().tasks} />
                        : c.kind === "duty" ? dutyDetail(c.id) : runDetail(c.id);
                    }}
                  </Show>
                </section>
              </div>
              <Show when={transcript()}>{(t) => (
                <div class="employee-transcript" role="dialog" aria-modal="true" aria-label={t().title}
                  onClick={(ev) => { if (ev.target === ev.currentTarget) setTranscript(null); }}>
                  <section>
                    <div class="employee-transcript-head">
                      <button type="button" aria-label="返回" onClick={() => setTranscript(null)}>← 返回</button>
                      <h2>{t().title}</h2>
                    </div>
                    <Timeline entries={t().entries ?? []} />
                  </section>
                </div>
              )}</Show>
            </div>
          );
        }}
      </Show>
  );
}

type RunFn = (command: string, args: Record<string, unknown>) => Promise<boolean>;
type Act = (action: string, id?: string) => Promise<boolean>;
type Open = (threadId: string) => void;

/** 补充信息：等补充的任务（交代的人填），或替团队员工补充的本地待办（直接回复并回传）。 */
function ReplyBox(props: { id: string; run: RunFn; label: string }) {
  const [text, setText] = createSignal("");
  const submit = async () => {
    if (text().trim() && await props.run("employee_do", { action: "reply", id: props.id, text: text() })) setText("");
  };
  return (
    <form class="employee-reply" onSubmit={(ev) => { ev.preventDefault(); void submit(); }}>
      <textarea rows={2} placeholder={props.label} aria-label={props.label} value={text()} onInput={(ev) => setText(ev.currentTarget.value)} />
      <button type="submit" class="employee-approve" disabled={!text().trim()}>提交</button>
    </form>
  );
}

/** 一条任务的关键内容：谁交代/补充了什么（原文）、员工问了什么、每段的结论；最后的结论完整展示。 */
function Timeline(props: { entries: Entry[] }) {
  const firstAsk = () => props.entries.findIndex((x) => x.kind === "ask");
  const finalResult = () => props.entries.map((x) => x.kind === "result" && !!x.text).lastIndexOf(true);
  const head = (x: Entry, i: number) => {
    switch (x.kind) {
      case "ask": return `${x.who} ${i === firstAsk() ? "交代" : "补充"}`;
      case "help": return "员工请求补充信息";
      case "answer": return `${x.who} 回复`;
      case "confirm": return "员工请求确认";
      case "approve": return `${x.who} 批准`;
      default: return `${x.scene || "员工"} · ${x.status === "running" ? "进行中" : SEGMENT[x.status || "done"] ?? x.status}`;
    }
  };
  return (
    <ol class="employee-timeline">
      <Show when={props.entries.length} fallback={<li class="employee-hint">（还没有内容）</li>}>
        <For each={props.entries}>{(x, i) => (
          <li class={`employee-timeline-${x.kind === "result" ? "result" : x.kind === "help" || x.kind === "confirm" ? "employee" : "person"}`}>
            <div class="employee-timeline-head"><b>{head(x, i())}</b><Show when={x.kind !== "result" || x.status !== "running"}><small>{time(x.at)}</small></Show></div>
            <Show when={i() === finalResult()} fallback={<Show when={x.text}><p>{x.text}</p></Show>}>
              <div class="employee-transcript-result"><Markdown text={x.text} /></div>
            </Show>
          </li>
        )}</For>
      </Show>
    </ol>
  );
}

/** 补充请求的现场截图（本机路径，只在本机看板展示）；点击展开/收起。 */
function Shot(props: { path?: string }) {
  const [open, setOpen] = createSignal(false);
  return (
    <Show when={props.path}>{(path) => (
      <img class="employee-shot" classList={{ open: open() }} src={convertFileSrc(path())} alt="现场截图" title="点击展开/收起" onClick={() => setOpen(!open())} />
    )}</Show>
  );
}

function HelpCard(props: { item: Todo; me: string; run: RunFn; open: Open; local: boolean }) {
  const mine = () => (props.item.owner?.token ?? "") === props.me;
  return (
    <article class="employee-task needs-confirm">
      <div class="employee-task-tags">
        <span class="employee-tag confirm">等补充信息</span>
        <Show when={from(props.item.owner)}>{(n) => <span class="employee-tag">来自 {n()}</span>}</Show>
      </div>
      <div class="employee-item-title">{props.item.waiting}</div>
      <small>任务：{props.item.text}</small>
      <Show when={props.local}><Shot path={props.item.shot} /></Show>
      <Show when={mine()} fallback={<small>{props.item.owner?.token ? `已请 ${from(props.item.owner)} 补充：他的本地员工会去查，或由他直接回复` : "等本机用户补充"}</small>}>
        <ReplyBox id={props.item.id} run={props.run} label="补充它要的信息，提交后在原会话接着做" />
      </Show>
      <div class="employee-item-actions">
        <Show when={props.item.threadId}>{(id) => <button type="button" onClick={() => props.open(id())}>看会话</button>}</Show>
      </div>
    </article>
  );
}

const resultOf = (r: Run) => (r.status === "running" || r.status === "queued" ? "已中断（Nova 重启）" : r.result);
const SEGMENT = { relayed: "已接力", running: "进行中", queued: "等电脑", deferred: "已挂起", stopped: "已停止", done: "完成" } as Record<string, string>;

/** 左侧列表的一行：任务家族、职责或不在任务树里的会话。 */
type Pick = { kind: "task" | "duty" | "run"; id: string };
type Row = { key: string; title: string; state: string; meta: string; pick: Pick };

function RowItem(props: { row: Row; active: boolean; onPick: () => void }) {
  const look = () => NODE[props.row.state] ?? NODE.stopped;
  return (
    <button type="button" class="employee-row" aria-current={props.active ? "true" : undefined} onClick={() => props.onPick()}>
      <i class="employee-dot" classList={{ virtual: !!look().virtual, live: props.row.state === "running" }} style={{ "--c": `var(${look().color})` }} />
      <span class="employee-row-main">
        <span class="employee-row-title">{props.row.title}</span>
        <small>{look().label}{props.row.meta ? ` · ${props.row.meta}` : ""}</small>
      </span>
    </button>
  );
}

/** 任务树节点的状态：已收尾的看 status；没收尾的按在跑、等补充、等前置、待执行（还没跑过）、等子任务、待续做。 */
const NODE: Record<string, { label: string; color: string; virtual?: boolean }> = {
  done: { label: "已完成", color: "--ok" },
  planning: { label: "规划中", color: "--accent" },
  running: { label: "执行中", color: "--accent" },
  queued: { label: "等电脑", color: "--seal" },
  help: { label: "等补充信息", color: "--seal" },
  deferred: { label: "等待外部结果", color: "--text-dim", virtual: true },
  kids: { label: "等子任务", color: "--accent" },
  relayed: { label: "已交接", color: "--accent" },
  resume: { label: "待续做", color: "--text-dim" },
  deps: { label: "等前置任务", color: "--text-dim", virtual: true },
  future: { label: "待执行", color: "--text-dim", virtual: true },
  failed: { label: "出错放弃", color: "--red" },
  stopped: { label: "已停止", color: "--text-dim" },
  blocked: { label: "受阻没做", color: "--red", virtual: true },
};

function nodeState(t: Todo, nodes: Todo[], tasks: Task[], now: number) {
  if (t.status) return NODE[t.status] ? t.status : "stopped";
  const task = tasks.find((x) => x.itemId === t.id);
  if (task) return task.queued ? "queued" : t.planning ? "planning" : "running";
  if (t.relayed) return "relayed";
  if (t.waiting) return "help";
  if (t.planning) return "planning";
  if (t.checkpoint && !t.failures && (t.nextCheckAt ?? 0) > now) return "deferred";
  if (t.after?.some((a) => nodes.some((k) => k.id === a && !k.status))) return "deps";
  if (!t.threadId) return "future";
  return nodes.some((k) => k.parent === t.id && !k.status) ? "kids" : "resume";
}

/** 家族最上面的根：沿上级与衍生来源往上找。 */
function familyTop(id: string, nodes: Todo[]) {
  const map = new Map(nodes.map((t) => [t.id, t]));
  let top = map.get(id);
  for (let i = 0; top && i < 50; i++) {
    const up = map.get(top.parent || top.origin || "");
    if (!up) break;
    top = up;
  }
  return top;
}

/** 家族里各棵树的根：最上面的根，加上其中节点衍生出的独立任务树（逐层）。 */
function familyRoots(top: Todo, nodes: Todo[]) {
  const roots = [top];
  const ids = new Set<string>();
  const collect = (id: string) => { ids.add(id); for (const k of nodes) if (k.parent === id && !ids.has(k.id)) collect(k.id); };
  for (let i = 0; i < roots.length; i++) {
    collect(roots[i].id);
    for (const t of nodes) if (!t.parent && t.origin && ids.has(t.origin) && !roots.includes(t)) roots.push(t);
  }
  return { roots, ids };
}

function familyNodes(top: Todo, nodes: Todo[]) {
  const { ids } = familyRoots(top, nodes);
  return nodes.filter((t) => ids.has(t.id));
}

/** 职责记住的序列画成虚的树：树根是职责本身，步骤 parent 为空即挂在树根下。 */
function planNodes(duty: Duty): Todo[] {
  const root: Todo = { id: `plan:${duty.id}`, text: duty.text, confirm: false, dutyId: duty.id, createdAt: 0 };
  return [root, ...(duty.plan ?? []).map((s) => ({ ...s, parent: s.parent || root.id }))];
}

const [NODE_W, NODE_H, GAP_X, GAP_Y, PAD] = [200, 64, 40, 40, 24];
type Placed = { t: Todo; x: number; y: number };
type FlowLink = { from: string; to: string; detached?: boolean };

/** 同一上级下的兄弟：同一条依赖链排在一起（前置在上，等它的紧跟其后），其余按创建时间。 */
function sortSiblings(list: Todo[]) {
  const byTime = [...list].sort((a, b) => a.createdAt - b.createdAt);
  const ids = new Set(list.map((t) => t.id));
  const out: Todo[] = [];
  const seen = new Set<string>();
  const visit = (t: Todo) => {
    // 依赖多个兄弟的，等最后一个前置排完再排。
    if (seen.has(t.id) || t.after?.some((a) => ids.has(a) && !seen.has(a))) return;
    seen.add(t.id);
    out.push(t);
    for (const next of byTime) if (next.after?.includes(t.id)) visit(next);
  };
  for (const t of byTime) if (!t.after?.some((a) => ids.has(a))) visit(t);
  for (const t of byTime) if (!seen.has(t.id)) out.push(t); // 成环等异常数据往后放
  return out;
}

/** 执行流：同一依赖层并排，串行向下；子步骤的出口接后继，不再从容器跨过整段连线。 */
function layoutTree(top: Todo | undefined, nodes: Todo[]) {
  type Block = { placed: Placed[]; links: FlowLink[]; exits: string[]; width: number; height: number };
  const seen = new Set<string>();
  const build = (t: Todo): Block => {
    seen.add(t.id);
    const children = sortSiblings(nodes.filter((k) => k.parent === t.id || (!k.parent && k.origin === t.id)));
    const blocks = new Map<string, Block>();
    const ranks = new Map<string, number>();
    for (const child of children) {
      if (seen.has(child.id)) continue;
      ranks.set(child.id, Math.max(-1, ...(child.after ?? []).map((id) => ranks.get(id) ?? -1)) + 1);
      blocks.set(child.id, build(child));
    }
    const rows: Todo[][] = [];
    for (const child of children) if (blocks.has(child.id)) (rows[ranks.get(child.id)!] ??= []).push(child);
    const widths = rows.map((row) => row.reduce((w, child) => w + blocks.get(child.id)!.width + GAP_X, -GAP_X));
    const width = Math.max(NODE_W, ...widths);
    const placed: Placed[] = [{ t, x: (width - NODE_W) / 2, y: 0 }];
    const links: FlowLink[] = [];
    let height = NODE_H;
    rows.forEach((row, rank) => {
      let x = (width - widths[rank]) / 2;
      const y = height + GAP_Y;
      for (const child of row) {
        const block = blocks.get(child.id)!;
        placed.push(...block.placed.map((p) => ({ ...p, x: p.x + x, y: p.y + y })));
        links.push(...block.links);
        const detached = !child.parent && !!child.origin;
        const deps = (child.after ?? []).filter((id) => blocks.has(id) && ranks.get(id)! < rank);
        const from = deps.length ? deps.flatMap((id) => blocks.get(id)!.exits) : [t.id];
        links.push(...from.map((id) => ({ from: id, to: child.id, detached })));
        x += block.width + GAP_X;
      }
      height = y + Math.max(...row.map((child) => blocks.get(child.id)!.height));
    });
    const leaves = children.filter((c) => c.parent === t.id && blocks.has(c.id)
      && !children.some((other) => blocks.has(other.id) && ranks.get(other.id)! > ranks.get(c.id)! && other.after?.includes(c.id)));
    return { placed, links, exits: leaves.length ? leaves.flatMap((c) => blocks.get(c.id)!.exits) : [t.id], width, height };
  };
  if (!top) return { placed: [] as Placed[], links: [] as FlowLink[] };
  const flow = build(top);
  return { placed: flow.placed.map((p) => ({ ...p, x: p.x + PAD, y: p.y + PAD })), links: flow.links };
}

/** 线型只表达是否走到该步骤；依赖关系本身不是“虚线”。 */
function flowDash(link: FlowLink, target: Todo) {
  if (link.detached) return [2, 4];
  return target.threadId || target.status === "done" || target.status === "failed" ? [] : [5, 4];
}

/** origin 是独立序列，停止范围只沿 parent 上溯。 */
function sequenceRoot(id: string, nodes: Todo[]) {
  const byId = new Map(nodes.map((t) => [t.id, t]));
  let root = byId.get(id);
  const seen = new Set<string>();
  while (root?.parent && !seen.has(root.id)) {
    seen.add(root.id);
    const parent = byId.get(root.parent);
    if (!parent) break;
    root = parent;
  }
  return root;
}

/** 列表标题、画布节点等单行处：去掉 Markdown 标记，压成一行纯文字。 */
const flat = (text: string) => text.replace(/^\s*(?:#{1,6}|>|[-*+]|\d+\.)\s+/gm, "").replace(/[*`]+/g, "").replace(/\s+/g, " ").trim();

/** 按宽度折成最多 `max` 行，超出加省略号（中文没有空格，逐字量）。 */
function wrap(ctx: CanvasRenderingContext2D, text: string, width: number, max: number) {
  const lines: string[] = [];
  let line = "";
  for (const ch of text.replace(/\s+/g, " ")) {
    if (ctx.measureText(line + ch).width > width) {
      lines.push(line);
      line = ch;
      if (lines.length === max) break;
    } else line += ch;
  }
  if (lines.length < max) lines.push(line);
  else lines[max - 1] = lines[max - 1].slice(0, -1) + "…";
  return lines;
}

/** 任务树详情：canvas 画出整棵树（含衍生出的独立任务树）——已完成、执行中、出错的实线，还没开始的虚线；
 *  点节点看详情、改或删没开始的。`readonly`：职责记住的序列预览，只看不改。 */
function TaskTree(props: { id: string; nodes: Todo[]; tasks: Task[]; now: () => number; open: Open; act: Act; run: RunFn; local: boolean; readonly?: boolean }) {
  const [selected, setSelected] = createSignal(props.id);
  const [draft, setDraft] = createSignal<string | null>(null);
  const [stopping, setStopping] = createSignal(false);
  const byId = () => new Map(props.nodes.map((t) => [t.id, t]));
  const taskOf = (id: string) => props.tasks.find((x) => x.itemId === id);
  const state = (t: Todo) => nodeState(t, props.nodes, props.tasks, props.now());
  const top = () => familyTop(props.id, props.nodes);
  let canvas!: HTMLCanvasElement;
  let placed: Placed[] = [];
  const draw = () => {
    const flow = layoutTree(top(), props.nodes);
    placed = flow.placed;
    if (!canvas || !placed.length) return;
    const width = Math.max(...placed.map((p) => p.x)) + NODE_W + PAD;
    const height = Math.max(...placed.map((p) => p.y)) + NODE_H + PAD;
    // 画布边长有上限：树很大时降低清晰度保证能画出来。
    const dpr = Math.min(window.devicePixelRatio || 1, 16000 / Math.max(width, height));
    canvas.width = Math.ceil(width * dpr);
    canvas.height = Math.ceil(height * dpr);
    canvas.style.width = `${width}px`;
    canvas.style.height = `${height}px`;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.scale(dpr, dpr);
    // 主题色取自 CSS 变量（canvas 不认 var()）。
    const colors = new Map<string, string>();
    const color = (name: string) => {
      if (!colors.has(name)) {
        const probe = document.createElement("span");
        probe.style.color = `var(${name})`;
        canvas.parentElement!.appendChild(probe);
        colors.set(name, getComputedStyle(probe).color);
        probe.remove();
      }
      return colors.get(name)!;
    };
    const font = getComputedStyle(canvas).fontFamily;
    const at = new Map(placed.map((p) => [p.t.id, p]));
    for (const link of flow.links) {
      const from = at.get(link.from), to = at.get(link.to);
      if (!from || !to) continue;
      const x1 = from.x + NODE_W / 2, y1 = from.y + NODE_H;
      const x2 = to.x + NODE_W / 2, y2 = to.y;
      ctx.beginPath();
      ctx.setLineDash(flowDash(link, to.t));
      ctx.strokeStyle = color(link.detached ? "--seal" : "--text-dim");
      ctx.lineWidth = 1.5;
      ctx.moveTo(x1, y1);
      // 相邻层直接折线分叉/汇合；跨层边走画布外缘，不能穿过居中的后续卡片。
      if (y2 - y1 > GAP_Y + 1) {
        const lane = width - PAD / 2;
        ctx.lineTo(x1, y1 + GAP_Y / 2);
        ctx.lineTo(lane, y1 + GAP_Y / 2);
        ctx.lineTo(lane, y2 - GAP_Y / 2);
        ctx.lineTo(x2, y2 - GAP_Y / 2);
      } else {
        ctx.lineTo(x1, (y1 + y2) / 2);
        ctx.lineTo(x2, (y1 + y2) / 2);
      }
      ctx.lineTo(x2, y2 - 2);
      ctx.stroke();
      ctx.setLineDash([]);
      ctx.fillStyle = ctx.strokeStyle;
      ctx.beginPath();
      ctx.moveTo(x2, y2);
      ctx.lineTo(x2 - 4, y2 - 8);
      ctx.lineTo(x2 + 4, y2 - 8);
      ctx.fill();
    }
    for (const p of placed) {
      const s = state(p.t);
      const look = NODE[s];
      const tint = color(look.color);
      if (p.t.id === selected()) {
        ctx.setLineDash([]);
        ctx.strokeStyle = color("--accent");
        ctx.lineWidth = 2;
        ctx.beginPath();
        ctx.roundRect(p.x - 4, p.y - 4, NODE_W + 8, NODE_H + 8, 11);
        ctx.stroke();
      }
      ctx.beginPath();
      ctx.roundRect(p.x, p.y, NODE_W, NODE_H, 8);
      ctx.globalAlpha = look.virtual ? 0.55 : 1;
      ctx.fillStyle = color("--bg-panel");
      ctx.fill();
      ctx.globalAlpha = 1;
      ctx.setLineDash(look.virtual ? [5, 4] : []);
      ctx.strokeStyle = tint;
      ctx.lineWidth = s === "running" ? 2.5 : 1.2;
      ctx.stroke();
      ctx.setLineDash([]);
      ctx.textBaseline = "alphabetic";
      ctx.font = `600 11px ${font}`;
      ctx.fillStyle = tint;
      ctx.fillText(wrap(ctx, `${s === "running" ? "● " : ""}${look.label}${p.t.dutyId ? " · 开始" : byId().get(p.t.parent ?? "")?.relayed ? " · 接力" : ""}${p.t.branch ? ` · ${p.t.branch}` : ""}`, NODE_W - 20, 1)[0], p.x + 10, p.y + 17);
      ctx.font = `13px ${font}`;
      ctx.fillStyle = color(look.virtual ? "--text-dim" : "--text");
      wrap(ctx, flat(p.t.text), NODE_W - 20, 2).forEach((line, i) => ctx.fillText(line, p.x + 10, p.y + 36 + i * 18));
    }
  };
  createEffect(draw);
  const pick = (ev: MouseEvent) => {
    const hit = placed.find((p) => ev.offsetX >= p.x && ev.offsetX <= p.x + NODE_W && ev.offsetY >= p.y && ev.offsetY <= p.y + NODE_H);
    if (hit) { setSelected(hit.t.id); setDraft(null); }
  };
  const members = () => { const t = top(); return t ? familyNodes(t, props.nodes) : []; };
  const counts = () => {
    const by = new Map<string, number>();
    for (const t of members()) by.set(state(t), (by.get(state(t)) ?? 0) + 1);
    return [...by].map(([s, n]) => `${NODE[s].label} ${n}`).join("，");
  };
  const sel = () => byId().get(selected()) ?? top();
  const sequence = () => sequenceRoot(sel()?.id ?? props.id, props.nodes);
  const canStop = () => !props.readonly && props.nodes.some((t) => !t.status && sequenceRoot(t.id, props.nodes)?.id === sequence()?.id);
  const stopSequence = async () => {
    const root = sequence();
    if (!root || stopping()) return;
    setStopping(true);
    try { await props.act("sequence_stop", root.id); } finally { setStopping(false); }
  };
  return (
    <div class="employee-tree">
      <div class="employee-detail-head">
        <h2>{flat(top()?.text ?? "")}</h2>
        <small>{counts()}</small>
        <Show when={canStop()}><button type="button" class="employee-sequence-stop" disabled={stopping()}
          title="停止选中节点所属的本轮序列及所有子步骤；保留已完成成果，不影响其它轮次和职责下次触发。已提交的外部作业不会自动取消。"
          onClick={() => void stopSequence()}>{stopping() ? "正在停止…" : "停止本轮序列"}</button></Show>
      </div>
      <div class="employee-tree-legend" aria-hidden="true">
        <span>实线：已开始的流程 · 虚线：未执行 · 点线：独立衍生序列 · 并排：可并行</span>
        <For each={Object.values(NODE)}>{(look) => <span><i classList={{ virtual: !!look.virtual }} style={{ "--c": `var(${look.color})` }} />{look.label}</span>}</For>
      </div>
      <div class="employee-tree-canvas">
        <canvas ref={canvas} role="img" aria-label={`任务树：${counts()}。点节点查看详情`} onClick={pick} />
      </div>
      <Show when={sel()}>{(t) => {
        const s = () => state(t());
        const task = () => taskOf(t().id);
        const thread = () => task()?.threadId ?? t().threadId;
        const pending = () => !props.readonly && !t().status && !task();
        const save = async () => {
          const text = draft()?.trim();
          if (text && text !== t().text) await props.run("employee_do", { action: "node_edit", id: t().id, text });
          setDraft(null);
        };
        return (
          <div class="employee-tree-detail">
            <div class="employee-task-tags">
              <span class="employee-tag" style={{ color: `var(${NODE[s()].color})` }}>{NODE[s()].label}</span>
              <Show when={from(t().owner)}>{(n) => <span class="employee-tag">来自 {n()}</span>}</Show>
              <Show when={t().desktop && !t().status}><span class="employee-tag desktop" title="你一空闲就续做；一直忙则到时再问你">等你空闲 · {t().askAfter && t().askAfter! > props.now() ? `${time(t().askAfter!)} 再问` : "可以问你"}</span></Show>
              <Show when={byId().get(t().parent ?? "")}>{(p) => <button type="button" class="employee-text-button" onClick={() => setSelected(p().id)}>上级：{flat(p().text).slice(0, 30)}</button>}</Show>
              <Show when={!props.readonly}><small>{t().finishedAt ? `收尾于 ${time(t().finishedAt!)}` : `创建于 ${time(t().createdAt)}`}</small></Show>
            </div>
            <Show when={draft() !== null} fallback={<div class="employee-duty-body"><Markdown text={t().text} /></div>}>
              <textarea rows={3} maxLength={8000} aria-label="编辑任务内容" value={draft() ?? ""} ref={(el) => queueMicrotask(() => el.focus())}
                onInput={(ev) => setDraft(ev.currentTarget.value)}
                onKeyDown={(ev) => { if (ev.key === "Enter" && !ev.shiftKey && !ev.isComposing) { ev.preventDefault(); void save(); } }} />
            </Show>
            <Show when={t().after?.length}>
              <small>先完成：{t().after!.map((id) => (byId().get(id) ? flat(byId().get(id)!.text).slice(0, 20) : id)).join("、")}</small>
            </Show>
            <Show when={t().waiting}><small class="employee-note">在等补充：{t().waiting}</small></Show>
            <Show when={t().nextCheckAt && t().nextCheckAt! > props.now() && !t().status}>
              <small>{t().failures ? `上次出错，${time(t().nextCheckAt!)} 续做（第 ${t().failures} 次）` : t().checkpoint ? `${time(t().nextCheckAt!)} 再检查；本轮结束后让出电脑与执行名额` : `计划 ${time(t().nextCheckAt!)}`}</small>
            </Show>
            <Show when={t().checkpoint}><div class="employee-note"><strong>恢复点</strong><Markdown text={t().checkpoint!} /></div></Show>
            <Show when={props.local}><Shot path={t().shot} /></Show>
            <Show when={t().replyTo && props.local && !t().status}><ReplyBox id={t().id} run={props.run} label="也可以直接回复，结果回传给对方员工" /></Show>
            <Show when={t().result}><div class="employee-transcript-result"><Markdown text={t().result!} /></div></Show>
            <Show when={!props.readonly}>
              <div class="employee-item-actions">
                <Show when={thread()}>{(id) => <button type="button" onClick={() => props.open(id())}>看会话</button>}</Show>
                <Show when={task()}>{(x) => <button type="button" onClick={() => void props.act("stop", x().threadId)}>停止此步骤</button>}</Show>
                <Show when={pending() && !t().threadId && !t().dutyId}>
                  <Show when={draft() !== null} fallback={<button type="button" onClick={() => setDraft(t().text)}>编辑</button>}>
                    <button type="button" class="employee-approve" disabled={!draft()?.trim()} onClick={() => void save()}>保存</button>
                    <button type="button" onClick={() => setDraft(null)}>取消</button>
                  </Show>
                </Show>
                <Show when={pending()}>
                  <button type="button" title="连同它没开始的子任务一起撤掉；等它的兄弟任务不再等它" onClick={() => void props.act("dismiss", t().id)}>撤回</button>
                </Show>
              </div>
            </Show>
          </div>
        );
      }}</Show>
    </div>
  );
}

/** 新增 / 编辑职责的弹窗：职责可能是很长的流程说明，用大编辑框并可预览 Markdown。
 *  触发规则只在新增时填，之后在卡片「⋯」里改。 */
function DutyForm(props: { run: RunFn; onClose: () => void; duty?: Duty }) {
  const d = props.duty;
  const [text, setText] = createSignal(d?.text ?? "");
  const [note, setNote] = createSignal(d?.note ?? "");
  const [preview, setPreview] = createSignal(false);
  const [mode, setMode] = createSignal<"times" | "every" | "events">("times");
  const [times, setTimes] = createSignal("09:00");
  const [every, setEvery] = createSignal(60);
  const [events, setEvents] = createSignal<string[]>([]);
  const noteBytes = () => new TextEncoder().encode(note().trim()).length;
  const dirty = () => text() !== (d?.text ?? "") || note() !== (d?.note ?? "");
  const ok = () => !!text().trim() && noteBytes() <= 4096;
  const submit = async () => {
    if (!ok()) return;
    const t = text().trim(), n = note().trim();
    if (!d) {
      const rule = mode() === "times" ? { times: times().split(/[,，、\s]+/).filter(Boolean) }
        : mode() === "every" ? { everyMinutes: every() } : { events: events() };
      if (await props.run("employee_do", { action: "duty_add", id: null, text: JSON.stringify({ text: t, note: n, ...rule }) })) props.onClose();
      return;
    }
    if (t !== d.text && !await props.run("employee_do", { action: "duty_edit", id: d.id, text: t })) return;
    if (n !== (d.note ?? "") && !await props.run("employee_do", { action: "duty_note", id: d.id, text: n })) return;
    props.onClose();
  };
  return (
    <Portal>
      <dialog class="modal-backdrop employee-duty-dialog" aria-labelledby="employee-duty-dialog-title" aria-modal="true"
        ref={(dialog) => {
          queueMicrotask(() => { if (dialog.isConnected) { dialog.showModal(); dialog.querySelector("textarea")?.focus(); } });
          onCleanup(() => dialog.close());
        }}
        // 内容可能很长：Esc 只在没改动时关闭，点背景不关闭，免得误丢。
        onCancel={(ev) => { ev.preventDefault(); if (!dirty()) props.onClose(); }}
        onKeyDown={(ev) => {
          if (ev.key === "Enter" && (ev.ctrlKey || ev.metaKey) && !ev.isComposing) { ev.preventDefault(); void submit(); }
        }}>
        <form class="modal" onSubmit={(ev) => { ev.preventDefault(); void submit(); }}>
          <div class="modal-head">
            <span id="employee-duty-dialog-title">{d ? "编辑职责" : "新增职责"}</span>
            <button type="button" class="employee-text-button" aria-pressed={preview()} onClick={() => setPreview(!preview())}>{preview() ? "编辑" : "预览"}</button>
          </div>
          <div class="modal-body">
            <Show when={preview()} fallback={
              <textarea class="employee-duty-text" maxLength={8000} required aria-label="职责内容"
                placeholder={"职责内容，支持 Markdown。可以写完整流程，如：\n1. 查询 TAPD 里待开发的单子\n2. 逐个开发并提交"}
                value={text()} onInput={(ev) => setText(ev.currentTarget.value)} />
            }>
              <div class="employee-duty-preview"><Markdown text={text() || "（空）"} /></div>
            </Show>
            <small class="employee-duty-count">{text().length} / 8000 · Ctrl+Enter 保存</small>
            <Show when={!d}>
              <div class="employee-duty-rule">
                <select aria-label="触发方式" value={mode()} onChange={(ev) => setMode(ev.currentTarget.value as "times" | "every" | "events")}>
                  <option value="times">每天定时</option>
                  <option value="every">固定间隔</option>
                  <option value="events">星轨事件</option>
                </select>
                <Show when={mode() === "times"}><input aria-label="每天触发时刻" placeholder="09:00,14:30" value={times()} onInput={(ev) => setTimes(ev.currentTarget.value)} /></Show>
                <Show when={mode() === "every"}><span class="employee-idle-input">每<input type="number" min="1" max="10080" aria-label="间隔分钟" value={every()} onInput={(ev) => setEvery(Number(ev.currentTarget.value) || 0)} />分钟</span></Show>
              </div>
              <Show when={mode() === "events"}>
                <fieldset class="employee-events"><legend>触发事件</legend>
                  <For each={EVENTS}>{([key, label]) => (
                    <label><input type="checkbox" checked={events().includes(key)}
                      onChange={(ev) => setEvents(ev.currentTarget.checked ? [...events(), key] : events().filter((k) => k !== key))} /> {label}</label>
                  )}</For>
                </fieldset>
              </Show>
            </Show>
            <textarea class="employee-duty-note" rows={4} aria-label="补充要求" placeholder="补充要求（可选），员工每次执行都会看到；留空即清除"
              value={note()} onInput={(ev) => setNote(ev.currentTarget.value)} />
            <Show when={noteBytes() > 4096}><small class="employee-unscheduled">补充要求太长（{noteBytes()} / 4096 字节）</small></Show>
          </div>
          <div class="modal-foot">
            <button type="button" class="btn" onClick={() => props.onClose()}>取消</button>
            <button type="submit" class="btn primary" disabled={!ok()}>{d ? "保存" : "添加"}</button>
          </div>
        </form>
      </dialog>
    </Portal>
  );
}

function ConfirmCard(props: { item: Todo; me: string; run: RunFn; act: Act; open: Open }) {
  // 补充说明放本地信号，轮询 reconcile 不会冲掉正在输入的内容。
  const [extra, setExtra] = createSignal("");
  // 下指令的人来批；主机用户（本机看板，me 为空）可以替任何人批。
  const mine = () => !props.me || (props.item.owner?.token ?? "") === props.me;
  return (
    <article class="employee-task needs-confirm">
      <div class="employee-task-tags">
        <span class="employee-tag confirm">待确认</span>
        <Show when={from(props.item.owner)}>{(n) => <span class="employee-tag">来自 {n()}</span>}</Show>
      </div>
      <div class="employee-item-title">{props.item.text}</div>
      <small>{time(props.item.createdAt)}</small>
      <details class="employee-approval-note">
        <summary>补充说明（可选）</summary>
        <textarea rows={2} placeholder="批准时一并交给员工" aria-label="批准时的补充说明"
          value={extra()} onInput={(ev) => setExtra(ev.currentTarget.value)} />
      </details>
      <div class="employee-item-actions">
        <button type="button" class="employee-approve" disabled={!mine()} title={mine() ? undefined : "由下指令的人或员工主人批准"}
          onClick={() => void props.run("employee_do", { action: "approve", id: props.item.id, text: extra() })}>{mine() ? "批准执行" : `等 ${from(props.item.owner) || "本机用户"} 批准`}</button>
        <button type="button" onClick={() => void props.act("dismiss", props.item.id)}>驳回</button>
        <Show when={props.item.threadId}>{(id) => <button type="button" onClick={() => props.open(id())}>看会话</button>}</Show>
      </div>
    </article>
  );
}

function DutyCard(props: { duty: Duty; due: number; task: boolean; now: number; run: RunFn; act: Act }) {
  const duty = props.duty;
  const [editing, setEditing] = createSignal(false);
  const times = () => duty.times ?? [];
  const events = () => duty.events ?? [];
  const dynamic = () => duty.everyMinutes === 0 && times().length === 0 && events().length === 0;
  const rule = () => [
    duty.everyMinutes > 0 && `每 ${duty.everyMinutes} 分钟`,
    times().length > 0 && `每天 ${times().join("、")}`,
    events().length > 0 && `事件：${events().map((ev) => EVENTS.find(([k]) => k === ev)?.[1] ?? ev).join("、")}`,
  ].filter(Boolean).join("；") || "按安排触发";
  const next = () => !duty.enabled ? "—"
    : props.due ? (props.due > props.now ? time(props.due) : "已到点，将新建一轮序列")
    : props.task && duty.everyMinutes > 0 ? "本轮结束后按间隔新建下一轮"
    : events().length ? "等事件触发新一轮"
    : dynamic() ? "未安排（不会自动执行）" : "已到点";
  return (
    <article class="employee-task employee-duty" classList={{ "is-paused": !duty.enabled }}>
      <div class="employee-task-tags">
        <input type="checkbox" checked={duty.enabled} aria-label={`启用职责：${flat(duty.text).slice(0, 40)}`}
          onChange={() => void props.act("duty_toggle", duty.id)} />
        <span class="employee-tag">{!duty.enabled ? "已停用" : `职责 · ${rule()}`}</span>
        <Show when={from(duty.owner)}>{(n) => <span class="employee-tag">来自 {n()}</span>}</Show>
        <button type="button" class="employee-text-button employee-duty-edit" onClick={() => setEditing(true)}>编辑</button>
        <details class="employee-more">
          <summary aria-label={`更多操作：${flat(duty.text).slice(0, 40)}`}>⋯</summary>
          <div>
            <label class="employee-interval" title="每天固定时刻，多个用逗号分隔，如 09:00,14:30；留空取消">每天几点<input type="text" placeholder="如 09:00"
              value={times().join(",")} aria-label="每天触发时刻"
              onChange={(ev) => void props.run("employee_do", { action: "duty_times", id: duty.id, text: ev.currentTarget.value })} /></label>
            <label class="employee-interval" title="留空或 0：不按间隔；填入数字：每次任务结束后等待固定分钟数（与每天时刻二选一）">执行间隔（分钟）<input type="number" min="0" max="10080" placeholder="自动"
              value={duty.everyMinutes || ""} aria-label="执行间隔（分钟）"
              onChange={(ev) => void props.run("employee_do", { action: "duty_every", id: duty.id, text: ev.currentTarget.value })} /></label>
            <fieldset class="employee-events" title="订阅的事件一发生就触发一次，可与定时并存">
              <legend>事件触发</legend>
              <For each={EVENTS}>{([key, label]) => (
                <label><input type="checkbox" checked={events().includes(key)}
                  onChange={(ev) => void props.run("employee_do", { action: "duty_events", id: duty.id,
                    text: (ev.currentTarget.checked ? [...events(), key] : events().filter((k) => k !== key)).join(",") })} /> {label}</label>
              )}</For>
            </fieldset>
            <button type="button" onClick={() => void props.act("duty_run", duty.id)}>立即执行</button>
            <button type="button" onClick={() => void props.act("duty_delete", duty.id)}>删除</button>
          </div>
        </details>
      </div>
      <div class="employee-duty-body"><Markdown text={duty.text} /></div>
      <small classList={{ "employee-unscheduled": duty.enabled && !props.task && dynamic() && !duty.nextCheckAt }}>下次执行 {next()}</small>
      <small>上次 {time(duty.lastRunAt)}{duty.lastResult ? ` · ${duty.lastResult}` : ""}</small>
      <Show when={duty.note}>
        <div class="employee-note employee-duty-body"><small>补充要求</small><Markdown text={duty.note} /></div>
      </Show>
      <Show when={editing()}><DutyForm run={props.run} duty={duty} onClose={() => setEditing(false)} /></Show>
    </article>
  );
}
