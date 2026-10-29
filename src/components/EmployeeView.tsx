import { createEffect, createSignal, For, JSX, on, onCleanup, onMount, Show } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openThread, showToast, state } from "../store";
import type { Peer } from "../types";
import { Markdown } from "./Markdown";
import "./EmployeeView.css";
import { TitleBarPortal } from "./TitleBar";
import VaultPop from "./VaultPop";

/** 交代的人；token 为空是员工所在机器的用户。 */
type Owner = { token: string; name: string };
type Duty = { id: string; text: string; enabled: boolean; note: string; everyMinutes: number; times?: string[]; events?: string[]; nextCheckAt: number; lastRunAt: number; lastResult: string; owner?: Owner };
type Todo = { id: string; text: string; confirm: boolean; dutyId?: string; failures?: number; profile?: string; createdAt: number; threadId?: string | null; nextCheckAt?: number; desktop?: boolean; askAfter?: number;
  owner?: Owner; waiting?: string; replyTo?: { token: string; name: string; id: string } | null; shot?: string };
const from = (o?: Owner) => (o?.token ? o.name || o.token : "");
/** chain：同一任务接力出的各段会话共用的首段会话 id，旧记录为空（自成一组）。 */
type Run = { at: number; dutyId: string; label?: string; status?: string; result: string; threadId: string; chain?: string; scene?: string };
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
    duties: Duty[]; inbox: Todo[]; runs: Run[]; memories?: Memory[];
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
  const [transcript, setTranscript] = createSignal<Transcript | null>(null);
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
    const closeMenus = (ev: Event) => document.querySelectorAll<HTMLDetailsElement>(".employee-more[open], .employee-pop[open]").forEach((d) => { if (!ev.target || !d.contains(ev.target as Node)) d.open = false; });
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
          const label = (r: Run) => r.label || e().duties.find((d) => d.id === r.dutyId)?.text || LEGACY[r.dutyId] || r.dutyId;
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
          const todos = () => e().inbox.filter((t) => !t.confirm && !t.waiting && !busyItems().has(t.id));
          // 职责是触发依据：它触发出的任务在跑时只显示任务卡片。
          const waitingDuties = () => e().duties.filter((d) => d.enabled && !e().inbox.some((t) => t.dutyId === d.id && busyItems().has(t.id)));
          const done = () => groups().filter((g) => !live(last(g)) && (!last(g).status || last(g).status === "done")).slice(0, SHOWN);
          // 重启前未收尾的运行没有任务承接，也算已停止。
          const stopped = () => groups().filter((g) => !live(last(g)) && last(g).status && last(g).status !== "done").slice(0, SHOWN);
          const pausedDuties = () => e().duties.filter((d) => !d.enabled);
          const pendingCount = () => confirms().length + waits().length + queued().length + todos().length + waitingDuties().length;

          const columns: { key: string; title: string; hint: string; count: () => number; body: () => JSX.Element }[] = [
            {
              key: "pending", title: "待执行", hint: "待确认、等补充、排队等电脑、待办与日常职责", count: pendingCount,
              body: () => (
                <>
                  <For each={confirms()}>{(item) => <ConfirmCard item={item} me={me} run={run} act={act} open={open} />}</For>
                  <For each={waits()}>{(item) => <HelpCard item={item} me={me} run={run} open={open} local={!peer} />}</For>
                  <For each={queued()}>{(task) => (
                    <article class="employee-task">
                      <div class="employee-task-tags"><span class="employee-tag desktop">等电脑空出</span></div>
                      <div class="employee-item-title">{runOf(task.threadId) ? label(runOf(task.threadId)!) : "任务"}</div>
                      <small>其他任务正在用电脑，空出后它的电脑操作立即开始</small>
                      <Segments runs={segmentsOf(task.threadId)} current={task.threadId} open={open} />
                      <div class="employee-item-actions">
                        <button type="button" onClick={() => open(task.threadId)}>看会话</button>
                        <button type="button" onClick={() => void act("stop", task.threadId)}>停止</button>
                      </div>
                    </article>
                  )}</For>
                  <For each={todos()}>{(item) => (
                    <article class="employee-task">
                      <div class="employee-task-tags">
                        <span class="employee-tag">{item.dutyId ? "职责任务" : item.replyTo ? `替 ${item.replyTo.name || "团队成员"} 的员工补充` : "待办"}</span>
                        <Show when={from(item.owner)}>{(n) => <span class="employee-tag">来自 {n()}</span>}</Show>
                        <Show when={item.desktop}><span class="employee-tag desktop" title="你一空闲就续做；一直忙则到时再问你">等你空闲 · {item.askAfter && item.askAfter > now() ? `${time(item.askAfter)} 再问` : "可以问你"}</span></Show>
                      </div>
                      <div class="employee-item-title">{item.text}</div>
                      <small>{time(item.createdAt)}{item.profile ? ` · ${item.profile}` : ""}{item.nextCheckAt && item.nextCheckAt > now() ? ` · ${item.failures ? `上次出错，${time(item.nextCheckAt)} 续做（第 ${item.failures} 次）` : `计划 ${time(item.nextCheckAt)}`}` : item.threadId ? " · 在原会话续做" : ""}</small>
                      <Show when={!peer}><Shot path={item.shot} /></Show>
                      <Show when={item.replyTo && !peer}><ReplyBox id={item.id} run={run} label="也可以直接回复，结果回传给对方员工" /></Show>
                      <div class="employee-item-actions">
                        <button type="button" onClick={() => void act("dismiss", item.id)}>撤回</button>
                        <Show when={item.threadId}>{(id) => <button type="button" onClick={() => open(id())}>看会话</button>}</Show>
                      </div>
                    </article>
                  )}</For>
                  <For each={waitingDuties()}>{(duty) => <DutyCard duty={duty} due={snap().dues?.[duty.id] ?? 0} task={e().inbox.some((t) => t.dutyId === duty.id)} now={now()} run={run} act={act} />}</For>
                </>
              ),
            },
            {
              key: "running", title: "正在执行", hint: `最多同时 ${snap().parallel} 项，只有操作电脑时排队`, count: () => running().length,
              body: () => (
                <For each={running()}>{(task) => {
                  const r = () => runOf(task.threadId);
                  return (
                    <article class="employee-task is-running">
                      <div class="employee-task-tags">
                        <span class="employee-tag running">执行中</span>
                        <Show when={task.desktop}>{(d) => <span class="employee-tag desktop">{DESKTOP[d()]}</span>}</Show>
                      </div>
                      <div class="employee-item-title">{r() ? label(r()!) : "任务"}</div>
                      <small>开始于 {time(segmentsOf(task.threadId)[0]?.at ?? r()?.at ?? 0)}</small>
                      <Segments runs={segmentsOf(task.threadId)} current={task.threadId} open={open} />
                      <div class="employee-item-actions">
                        <button type="button" onClick={() => open(task.threadId)}>看会话</button>
                        <button type="button" onClick={() => void act("stop", task.threadId)}>停止</button>
                      </div>
                    </article>
                  );
                }}</For>
              ),
            },
            {
              key: "done", title: "已完成", hint: `最近 ${SHOWN} 项`, count: () => done().length,
              body: () => <For each={done()}>{(g) => <ChainCard runs={g} label={label(last(g))} open={open} />}</For>,
            },
            {
              key: "stopped", title: "已停止", hint: "被你停止、让出或中断的任务，以及停用的职责", count: () => stopped().length + pausedDuties().length,
              body: () => (
                <>
                  <For each={stopped()}>{(g) => <ChainCard runs={g} label={label(last(g))} open={open} />}</For>
                  <For each={pausedDuties()}>{(duty) => <DutyCard duty={duty} due={0} task={false} now={now()} run={run} act={act} />}</For>
                </>
              ),
            },
          ];

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
                  <details class="employee-help"><summary>工作方式说明</summary><p class="employee-hint">每件事只在安排好的时间触发，触发就直接干活，不会空跑会话去检查。多件事并行处理；操作电脑是单独排队的一步，每次开始前都会在 Butterfly 上提示你，{e().takeoverSeconds ?? 15} 秒内不选择即开始。员工操作电脑时（屏幕边缘呼吸灯亮着）按 Esc 即可叫停；你拒绝或叫停后，员工会在你空闲时、或过一阵再问你，择机补做。</p>
                  <p class="employee-hint">员工从「设置 → 场景模型 → 轻量化」开局，编码交给编码场景，操作电脑交给剑来场景的模型。</p></details>
                  <label class="employee-field employee-public" title="经团队中转站公开：团队成员都能查看并下达指令；谁交代的事缺信息时找谁补充，待确认由谁批准">
                    <input type="checkbox" role="switch" checked={!!e().public} onChange={(ev) => void set({ public: ev.currentTarget.checked })} /> 公开到团队（StarLink）
                  </label>
                    </div>
                  </details>
                  </Show>
                </div>
              </header></TitleBarPortal>

              <form class="employee-say" onSubmit={(ev) => { ev.preventDefault(); void send(); }}>
                <textarea id="employee-message" ref={sayInput} rows={2} aria-label={peer ? `交给 ${peer.name} 的员工一件事` : "交给员工一件事"} title="Enter 发送，Shift+Enter 换行"
                  placeholder={`${peer ? `交给 ${peer.name} 的员工一件事` : "交给员工一件事"}：新增职责、修改安排或直接派活，例如“每天 9 点检查未读邮件并汇总”。Enter 发送`}
                  value={say()} onInput={(ev) => setSay(ev.currentTarget.value)}
                  onKeyDown={(ev) => { if (ev.key === "Enter" && !ev.shiftKey && !ev.isComposing) { ev.preventDefault(); void send(); } }} />
                <button type="submit" class="employee-primary" disabled={busy() || !say().trim()}>{busy() ? "发送中…" : "发送 ↑"}</button>
              </form>

              <section class="employee-board" aria-label="任务看板">
                <div class="employee-columns">
                  <For each={columns}>{(c) => (
                    <section class={`employee-column is-${c.key}`} aria-labelledby={`employee-col-${c.key}`}>
                      <div class="employee-column-heading" title={c.hint}>
                        <h2 id={`employee-col-${c.key}`}>{c.title} <span class="employee-count">{c.count()}</span></h2>
                      </div>
                      <div class="employee-column-body">
                        <Show when={c.count()} fallback={<p class="employee-empty">
                          {c.key === "pending" ? <>暂无。<button type="button" class="employee-text-button" onClick={() => sayInput?.focus()}>交给员工一件事</button></> : "暂无"}
                        </p>}>
                          {c.body()}
                        </Show>
                      </div>
                    </section>
                  )}</For>
                </div>
              </section>
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
const SEGMENT = { relayed: "已接力", running: "进行中", queued: "等电脑", stopped: "已停止", done: "完成" } as Record<string, string>;

/** 一项任务接力出的各段会话（只有一段时不显示）：场景、状态、交接或结果，点击看该段会话。 */
function Segments(props: { runs: Run[]; open: Open; current?: string }) {
  return (
    <Show when={props.runs.length > 1}>
      <ol class="employee-segments" aria-label={`接力的 ${props.runs.length} 段会话`}>
        <For each={props.runs}>{(r, i) => (
          <li>
            <button type="button" classList={{ "is-current": r.threadId === props.current }} title={r.result} onClick={() => props.open(r.threadId)}>
              <span class="employee-segment-index">{i() + 1}</span>
              <strong>{r.scene || "会话"}</strong>
              <span class="employee-segment-status">{r.threadId === props.current ? "进行中" : SEGMENT[r.status || "done"] ?? r.status}</span>
              <Show when={r.threadId !== props.current}><small>{resultOf(r)}</small></Show>
            </button>
          </li>
        )}</For>
      </ol>
    </Show>
  );
}

/** 已收尾的一项任务：接力过的把各段会话框在同一张卡片里。 */
function ChainCard(props: { runs: Run[]; label: string; open: Open }) {
  const run = () => props.runs[props.runs.length - 1];
  return (
    <article class={`employee-task is-${run().status || "done"}`} classList={{ "employee-chain": props.runs.length > 1 }}>
      <button type="button" class="employee-run" onClick={() => props.open(run().threadId)}>
        <small>{time(props.runs[0].at)}{props.runs.length > 1 ? ` · 接力 ${props.runs.length} 段` : ""}</small>
        <div class="employee-item-title">{props.label}</div>
        <div class="employee-run-result">{resultOf(run())}</div>
      </button>
      <Segments runs={props.runs} open={props.open} />
    </article>
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
  // 编辑草稿放本地信号，5s 轮询 reconcile 不会冲掉正在输入的内容。
  const [draft, setDraft] = createSignal<string | null>(null);
  const save = async () => {
    const text = draft()?.trim();
    if (!text) return;
    if (text !== duty.text) await props.run("employee_do", { action: "duty_edit", id: duty.id, text });
    setDraft(null);
  };
  const [noteDraft, setNoteDraft] = createSignal<string | null>(null);
  const saveNote = async () => {
    const note = noteDraft()?.trim() ?? "";
    if (note !== duty.note) await props.run("employee_do", { action: "duty_note", id: duty.id, text: note });
    setNoteDraft(null);
  };
  const times = () => duty.times ?? [];
  const events = () => duty.events ?? [];
  const dynamic = () => duty.everyMinutes === 0 && times().length === 0 && events().length === 0;
  const rule = () => [
    duty.everyMinutes > 0 && `每 ${duty.everyMinutes} 分钟`,
    times().length > 0 && `每天 ${times().join("、")}`,
    events().length > 0 && `事件：${events().map((ev) => EVENTS.find(([k]) => k === ev)?.[1] ?? ev).join("、")}`,
  ].filter(Boolean).join("；") || "按安排触发";
  // 职责只负责触发：本次任务没收尾前不再触发；动态职责没安排下一次就不会再触发，要让用户一眼看到。
  const next = () => !duty.enabled ? "—"
    : props.task ? "本次任务处理中，收尾后按规则触发"
    : dynamic() && !duty.nextCheckAt ? "未安排（不会自动执行）"
    : props.due > props.now ? time(props.due) : "已到点";
  return (
    <article class="employee-task employee-duty" classList={{ "is-paused": !duty.enabled }}>
      <div class="employee-task-tags">
        <input type="checkbox" checked={duty.enabled} aria-label={`启用职责：${duty.text}`}
          onChange={() => void props.act("duty_toggle", duty.id)} />
        <span class="employee-tag">{!duty.enabled ? "已停用" : `职责 · ${rule()}`}</span>
        <Show when={from(duty.owner)}>{(n) => <span class="employee-tag">来自 {n()}</span>}</Show>
        <details class="employee-more">
          <summary aria-label={`更多操作：${duty.text}`}>⋯</summary>
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
            <button type="button" onClick={(ev) => { ev.currentTarget.closest("details")!.open = false; setDraft(duty.text); }}>编辑</button>
            <button type="button" onClick={(ev) => { ev.currentTarget.closest("details")!.open = false; setNoteDraft(duty.note); }}>补充要求</button>
            <button type="button" onClick={() => void props.act("duty_run", duty.id)}>立即执行</button>
            <button type="button" onClick={() => void props.act("duty_delete", duty.id)}>删除</button>
          </div>
        </details>
      </div>
      <Show when={draft() !== null} fallback={<div class="employee-item-title">{duty.text}</div>}>
        <textarea rows={2} maxLength={500} aria-label="编辑职责" value={draft() ?? ""} ref={(el) => queueMicrotask(() => el.focus())}
          onInput={(ev) => setDraft(ev.currentTarget.value)}
          onKeyDown={(ev) => {
            if (ev.key === "Escape") setDraft(null);
            else if (ev.key === "Enter" && !ev.shiftKey && !ev.isComposing) { ev.preventDefault(); void save(); }
          }} />
        <div class="employee-edit-actions">
          <button type="button" disabled={!draft()?.trim()} onClick={() => void save()}>保存</button>
          <button type="button" onClick={() => setDraft(null)}>取消</button>
        </div>
      </Show>
      <small classList={{ "employee-unscheduled": duty.enabled && !props.task && dynamic() && !duty.nextCheckAt }}>下次执行 {next()}</small>
      <small>上次 {time(duty.lastRunAt)}{duty.lastResult ? ` · ${duty.lastResult}` : ""}</small>
      <Show when={noteDraft() !== null} fallback={<Show when={duty.note}>
        <small class="employee-note">补充要求：{duty.note}</small>
      </Show>}>
        <textarea rows={3} aria-label="编辑补充要求" placeholder="你对这条职责的细化要求，员工每次执行都会看到；留空即清除" value={noteDraft() ?? ""}
          ref={(el) => queueMicrotask(() => el.focus())}
          onInput={(ev) => setNoteDraft(ev.currentTarget.value)}
          onKeyDown={(ev) => {
            if (ev.key === "Escape") setNoteDraft(null);
            else if (ev.key === "Enter" && !ev.shiftKey && !ev.isComposing) { ev.preventDefault(); void saveNote(); }
          }} />
        <div class="employee-edit-actions">
          <button type="button" onClick={() => void saveNote()}>保存</button>
          <button type="button" onClick={() => setNoteDraft(null)}>取消</button>
        </div>
      </Show>
    </article>
  );
}
