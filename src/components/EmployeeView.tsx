import { createSignal, For, JSX, onCleanup, onMount, Show } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openThread, showToast } from "../store";
import "./EmployeeView.css";

type Duty = { id: string; text: string; enabled: boolean; note: string; everyMinutes: number; times?: string[]; nextCheckAt: number; lastRunAt: number; lastResult: string };
type Todo = { id: string; text: string; confirm: boolean; dutyId?: string; failures?: number; profile?: string; createdAt: number; threadId?: string | null; nextCheckAt?: number; desktop?: boolean; askAfter?: number };
type Run = { at: number; dutyId: string; label?: string; status?: string; result: string; threadId: string };
type Memory = { id: string; text: string; at: number; importance?: number; strength?: number; usedAt?: number };
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
    enabled: boolean; workStart: string; workEnd: string; idleMinutes: number;
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

export default function EmployeeView() {
  // 不用 createResource：页面在 <Suspense> 里，轮询 refetch 会让整页切到 fallback 闪烁并丢失输入焦点；
  // reconcile 保持行对象稳定，列表不重建，展开的菜单也不会被关掉。
  const [store, setStore] = createStore<{ snap?: Snapshot }>({});
  const [now, setNow] = createSignal(Date.now());
  const data = () => store.snap;
  const refetch = () => invoke<Snapshot>("employee_get").then(
    (snap) => { setStore("snap", reconcile(snap)); setNow(Date.now()); },
    (error) => showToast(`读取员工数据失败：${String(error)}`),
  );
  void refetch();
  const [say, setSay] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  let sayInput: HTMLTextAreaElement | undefined;

  onMount(() => {
    const unlisten = listen("employee:changed", () => void refetch());
    const timer = setInterval(() => void refetch(), 5000);
    // 「⋯」菜单是 <details>：点到菜单外部时关掉所有已展开的菜单。
    const closeMenus = (ev: Event) => document.querySelectorAll<HTMLDetailsElement>(".employee-more[open]").forEach((d) => { if (!ev.target || !d.contains(ev.target as Node)) d.open = false; });
    const onKey = (ev: KeyboardEvent) => { if (ev.key === "Escape") closeMenus(ev); };
    document.addEventListener("click", closeMenus);
    document.addEventListener("keydown", onKey);
    onCleanup(() => { void unlisten.then((off) => off()); clearInterval(timer); document.removeEventListener("click", closeMenus); document.removeEventListener("keydown", onKey); });
  });

  const run = async (command: string, args: Record<string, unknown>) => {
    try {
      const out = await invoke(command, args);
      if (typeof out === "string" && out) showToast(out);
    } catch (error) { showToast(String(error)); }
    void refetch();
  };
  const act = (action: string, id?: string) => run("employee_do", { action, id: id ?? null });
  const set = (patch: Record<string, unknown>) => run("employee_set", { patch });
  const send = async () => {
    const text = say().trim();
    if (!text || busy()) return;
    setBusy(true);
    try { await invoke("employee_say", { text }); setSay(""); } catch (error) { showToast(String(error)); }
    setBusy(false);
    void refetch();
  };

  return (
    <div class="employee-page">
      <Show when={data()} fallback={<p class="employee-empty">加载中…</p>}>
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
          const confirms = () => e().inbox.filter((t) => t.confirm);
          const todos = () => e().inbox.filter((t) => !t.confirm && !busyItems().has(t.id));
          // 职责是触发依据：它触发出的任务在跑时只显示任务卡片。
          const waitingDuties = () => e().duties.filter((d) => d.enabled && !e().inbox.some((t) => t.dutyId === d.id && busyItems().has(t.id)));
          const done = () => latest().filter((r) => !live(r) && (!r.status || r.status === "done")).slice(0, SHOWN);
          // 重启前未收尾的运行没有任务承接，也算已停止。
          const stopped = () => latest().filter((r) => !live(r) && r.status && r.status !== "done").slice(0, SHOWN);
          const pausedDuties = () => e().duties.filter((d) => !d.enabled);
          const pendingCount = () => confirms().length + queued().length + todos().length + waitingDuties().length;

          const columns: { key: string; title: string; hint: string; count: () => number; body: () => JSX.Element }[] = [
            {
              key: "pending", title: "待执行", hint: "待确认、排队等电脑、待办与日常职责", count: pendingCount,
              body: () => (
                <>
                  <For each={confirms()}>{(item) => <ConfirmCard item={item} run={run} act={act} />}</For>
                  <For each={queued()}>{(task) => (
                    <article class="employee-task">
                      <div class="employee-task-tags"><span class="employee-tag desktop">等电脑空出</span></div>
                      <div class="employee-item-title">{runOf(task.threadId) ? label(runOf(task.threadId)!) : "任务"}</div>
                      <small>其他任务正在用电脑，空出后它的电脑操作立即开始</small>
                      <div class="employee-item-actions">
                        <button type="button" onClick={() => void openThread(task.threadId)}>看会话</button>
                        <button type="button" onClick={() => void act("stop", task.threadId)}>停止</button>
                      </div>
                    </article>
                  )}</For>
                  <For each={todos()}>{(item) => (
                    <article class="employee-task">
                      <div class="employee-task-tags">
                        <span class="employee-tag">{item.dutyId ? "职责任务" : "待办"}</span>
                        <Show when={item.desktop}><span class="employee-tag desktop" title="你一空闲就续做；一直忙则到时再问你">等你空闲 · {item.askAfter && item.askAfter > now() ? `${time(item.askAfter)} 再问` : "可以问你"}</span></Show>
                      </div>
                      <div class="employee-item-title">{item.text}</div>
                      <small>{time(item.createdAt)}{item.profile ? ` · ${item.profile}` : ""}{item.nextCheckAt && item.nextCheckAt > now() ? ` · ${item.failures ? `上次出错，${time(item.nextCheckAt)} 续做（第 ${item.failures} 次）` : `计划 ${time(item.nextCheckAt)}`}` : item.threadId ? " · 在原会话续做" : ""}</small>
                      <div class="employee-item-actions">
                        <button type="button" onClick={() => void act("dismiss", item.id)}>撤回</button>
                        <Show when={item.threadId}>{(id) => <button type="button" onClick={() => void openThread(id())}>看会话</button>}</Show>
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
                      <small>开始于 {time(r()?.at ?? 0)}</small>
                      <div class="employee-item-actions">
                        <button type="button" onClick={() => void openThread(task.threadId)}>看会话</button>
                        <button type="button" onClick={() => void act("stop", task.threadId)}>停止</button>
                      </div>
                    </article>
                  );
                }}</For>
              ),
            },
            {
              key: "done", title: "已完成", hint: `最近 ${SHOWN} 项`, count: () => done().length,
              body: () => <For each={done()}>{(r) => <RunCard run={r} label={label(r)} />}</For>,
            },
            {
              key: "stopped", title: "已停止", hint: "被你停止、让出或中断的任务，以及停用的职责", count: () => stopped().length + pausedDuties().length,
              body: () => (
                <>
                  <For each={stopped()}>{(r) => <RunCard run={r} label={label(r)} />}</For>
                  <For each={pausedDuties()}>{(duty) => <DutyCard duty={duty} due={0} task={false} now={now()} run={run} act={act} />}</For>
                </>
              ),
            },
          ];

          return (
            <div class="employee-content">
              <header class="employee-header">
                <div>
                  <div class="employee-title"><h1>数字员工</h1><span class={`employee-badge ${snap().status}`}>{STATUS[snap().status]}</span></div>
                  <p class="employee-hint">把日常事务交给员工，专注更重要的事。</p>
                </div>
                <div class="employee-header-actions">
                  <label class="employee-toggle"><input type="checkbox" role="switch" checked={e().enabled}
                    onChange={(ev) => void set({ enabled: ev.currentTarget.checked })} /> 自动值班</label>
                  <button type="button" title="不等触发时间，立即开始一件已到点的事项" onClick={() => void act("check")}>执行到点事项</button>
                </div>
              </header>
              <div class="employee-status" role="group" aria-label="员工状态">
                <span class="employee-chip">下次触发<strong>{time(snap().nextCheckAt)}</strong></span>
                <span class="employee-chip">进行中<strong>{running().length} / {snap().parallel}</strong></span>
                <Show when={!snap().idleSupported}>
                  <span class="employee-hint">当前平台无法检测空闲：操作电脑前总会先询问你。</span>
                </Show>
              </div>

              <div class="employee-top">
                <form class="employee-say" onSubmit={(ev) => { ev.preventDefault(); void send(); }}>
                  <label for="employee-message">交给员工一件事</label>
                  <textarea id="employee-message" ref={sayInput} rows={3} placeholder="例如：每天 9 点检查未读邮件并汇总" aria-label="对员工说" title="Enter 发送，Shift+Enter 换行"
                    value={say()} onInput={(ev) => setSay(ev.currentTarget.value)}
                    onKeyDown={(ev) => { if (ev.key === "Enter" && !ev.shiftKey && !ev.isComposing) { ev.preventDefault(); void send(); } }} />
                  <div class="employee-say-footer">
                    <span class="employee-hint">新增职责、修改安排，或直接派活 <span class="employee-shortcut">· Enter 发送</span></span>
                    <button type="submit" class="employee-primary" disabled={busy() || !say().trim()}>{busy() ? "发送中…" : "发送指令 ↑"}</button>
                  </div>
                </form>

                <section class="employee-card employee-settings" aria-labelledby="employee-settings-title">
                  <div class="employee-section-heading"><h2 id="employee-settings-title">值班设置</h2><span class="employee-hint">自动保存</span></div>
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
                  <details class="employee-help"><summary>工作方式说明</summary><p class="employee-hint">每件事只在安排好的时间触发，触发就直接干活，不会空跑会话去检查。多件事并行处理；操作电脑是单独排队的一步，每次开始前都会在 Butterfly 上提示你，15 秒内不选择即开始。员工操作电脑时（屏幕边缘呼吸灯亮着）按 Esc 即可叫停；你拒绝或叫停后，员工会在你空闲时、或过一阵再问你，择机补做。</p>
                  <p class="employee-hint">员工从「设置 → 场景模型 → 轻量化」开局，编码交给编码场景，操作电脑交给剑来场景的模型。</p></details>
                </section>
              </div>

              <section class="employee-board" aria-label="任务看板">
                <div class="employee-columns">
                  <For each={columns}>{(c) => (
                    <section class={`employee-column is-${c.key}`} aria-labelledby={`employee-col-${c.key}`}>
                      <div class="employee-column-heading">
                        <h2 id={`employee-col-${c.key}`}>{c.title} <span class="employee-count">{c.count()}</span></h2>
                        <span class="employee-hint">{c.hint}</span>
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

              <details class="employee-card employee-memory">
                <summary class="employee-section-heading">
                  <h2>员工的记忆 <span class="employee-count">{e().memories?.length ?? 0}</span></h2>
                  <span class="employee-hint">跨任务共享的偏好和经验，用得越多越牢，对员工说“长期记住…”的不会忘。<span class="employee-disclosure"> ▾</span></span>
                </summary>
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
              </details>
            </div>
          );
        }}
      </Show>
    </div>
  );
}

type RunFn = (command: string, args: Record<string, unknown>) => Promise<void>;
type Act = (action: string, id?: string) => Promise<void>;

function RunCard(props: { run: Run; label: string }) {
  return (
    <article class={`employee-task is-${props.run.status || "done"}`}>
      <button type="button" class="employee-run" onClick={() => void openThread(props.run.threadId)}>
        <small>{time(props.run.at)}</small>
        <div class="employee-item-title">{props.label}</div>
        <div class="employee-run-result">{props.run.status === "running" || props.run.status === "queued" ? "已中断（Nova 重启）" : props.run.result}</div>
      </button>
    </article>
  );
}

function ConfirmCard(props: { item: Todo; run: RunFn; act: Act }) {
  // 补充说明放本地信号，轮询 reconcile 不会冲掉正在输入的内容。
  const [extra, setExtra] = createSignal("");
  return (
    <article class="employee-task needs-confirm">
      <div class="employee-task-tags"><span class="employee-tag confirm">待确认</span></div>
      <div class="employee-item-title">{props.item.text}</div>
      <small>{time(props.item.createdAt)}</small>
      <details class="employee-approval-note">
        <summary>补充说明（可选）</summary>
        <textarea rows={2} placeholder="批准时一并交给员工" aria-label="批准时的补充说明"
          value={extra()} onInput={(ev) => setExtra(ev.currentTarget.value)} />
      </details>
      <div class="employee-item-actions">
        <button type="button" class="employee-approve" onClick={() => void props.run("employee_do", { action: "approve", id: props.item.id, text: extra() })}>批准执行</button>
        <button type="button" onClick={() => void props.act("dismiss", props.item.id)}>驳回</button>
        <Show when={props.item.threadId}>{(id) => <button type="button" onClick={() => void openThread(id())}>看会话</button>}</Show>
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
  const dynamic = () => duty.everyMinutes === 0 && times().length === 0;
  const rule = () => duty.everyMinutes > 0 ? `每 ${duty.everyMinutes} 分钟` : times().length ? `每天 ${times().join("、")}` : "按安排触发";
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
        <details class="employee-more">
          <summary aria-label={`更多操作：${duty.text}`}>⋯</summary>
          <div>
            <label class="employee-interval" title="每天固定时刻，多个用逗号分隔，如 09:00,14:30；留空取消">每天几点<input type="text" placeholder="如 09:00"
              value={times().join(",")} aria-label="每天触发时刻"
              onChange={(ev) => void props.run("employee_do", { action: "duty_times", id: duty.id, text: ev.currentTarget.value })} /></label>
            <label class="employee-interval" title="留空或 0：不按间隔；填入数字：每次任务结束后等待固定分钟数（与每天时刻二选一）">执行间隔（分钟）<input type="number" min="0" max="10080" placeholder="自动"
              value={duty.everyMinutes || ""} aria-label="执行间隔（分钟）"
              onChange={(ev) => void props.run("employee_do", { action: "duty_every", id: duty.id, text: ev.currentTarget.value })} /></label>
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
