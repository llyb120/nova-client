import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { enabledAgentKinds, lastUsed, openThread, showToast } from "../store";
import type { AgentKind } from "../types";
import { ModelPicker } from "./ConfigSelects";
import "./EmployeeView.css";

type Duty = { id: string; text: string; enabled: boolean; note: string; everyMinutes: number; nextCheckAt: number; lastRunAt: number; lastResult: string };
type Todo = { id: string; text: string; confirm: boolean; profile?: string; createdAt: number; threadId?: string | null };
type PoolModel = { name: string; when: string; agentKind: string; model: string };
type Run = { at: number; dutyId: string; result: string; threadId: string };
type Snapshot = {
  employee: {
    enabled: boolean; workStart: string; workEnd: string; idleMinutes: number;
    agentKind: string; model: string; pool: PoolModel[]; duties: Duty[]; inbox: Todo[]; runs: Run[];
  };
  status: "working" | "yielded" | "duty" | "rest";
  threadId: string | null;
  nextCheckAt: number;
  idleSupported: boolean;
};

const STATUS = { working: "工作中", yielded: "已让出", duty: "值班中", rest: "休息中" } as const;
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
    onCleanup(() => { void unlisten.then((off) => off()); clearInterval(timer); });
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
  const useCurrentModel = () => {
    const agentKind = lastUsed.agentKind();
    return set({ agentKind, model: lastUsed.model(agentKind) });
  };
  const send = async () => {
    const text = say().trim();
    if (!text || busy()) return;
    setBusy(true);
    if (!data()?.employee.agentKind) await useCurrentModel();
    try { await invoke("employee_say", { text }); setSay(""); } catch (error) { showToast(String(error)); }
    setBusy(false);
    void refetch();
  };

  return (
    <div class="employee-page">
      <Show when={data()} fallback={<p class="employee-empty">加载中…</p>}>
        {(snap) => {
          const e = () => snap().employee;
          return (
            <div class="employee-content">
              <header class="employee-header">
                <div>
                  <div class="employee-title"><h1>数字员工</h1><span class={`employee-badge ${snap().status}`}>{STATUS[snap().status]}</span></div>
                  <p class="employee-hint">把日常事务交给员工，专注更重要的事。</p>
                </div>
                <div class="employee-header-actions">
                  <label class="employee-toggle"><input type="checkbox" role="switch" checked={e().enabled}
                    onChange={(ev) => void (async () => {
                      if (ev.currentTarget.checked && !e().agentKind) await useCurrentModel();
                      await set({ enabled: ev.currentTarget.checked });
                    })()} /> 自动值班</label>
                  <button type="button" onClick={() => void act("check")}>立即检查</button>
                </div>
              </header>
              <div class="employee-status" role="group" aria-label="员工状态">
                <span class="employee-hint">下次检查 <strong>{time(snap().nextCheckAt)}</strong></span>
                <Show when={snap().threadId}>
                  {(id) => <button type="button" class="employee-text-button" onClick={() => void openThread(id())}>查看当前会话 ↗</button>}
                </Show>
                <Show when={!snap().idleSupported}>
                  <span class="employee-hint">当前平台无法检测空闲，不会自动接管，只能手动触发。</span>
                </Show>
              </div>

              <div class="employee-layout">
                <div class="employee-workspace">
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

                  <section class="employee-card employee-inbox" aria-labelledby="employee-inbox">
                    <div class="employee-section-heading"><h2 id="employee-inbox">待处理 <span class="employee-count">{e().inbox.length}</span></h2><span class="employee-hint">待办与需要你确认的事项</span></div>
                    <Show when={e().inbox.length} fallback={<p class="employee-empty employee-empty-inline">暂无待处理事项，交给员工的工作会显示在这里。</p>}>
                      <ul class="employee-list">
                        <For each={e().inbox}>{(item) => {
                          // 补充说明放本地信号，轮询 reconcile 不会冲掉正在输入的内容。
                          const [extra, setExtra] = createSignal("");
                          return (
                          <li class="employee-inbox-item" classList={{ "needs-confirm": item.confirm }}>
                            <span class={`employee-tag ${item.confirm ? "confirm" : ""}`}>{item.confirm ? "待确认" : "待办"}</span>
                            <div class="employee-main">
                              <div class="employee-item-title">{item.text}</div>
                              <small>{time(item.createdAt)}{item.profile ? ` · 模型：${item.profile}` : ""}</small>
                              <Show when={item.confirm}>
                                <details class="employee-approval-note">
                                  <summary>补充说明（可选）</summary>
                                  <textarea rows={2} placeholder="批准时一并交给员工" aria-label="批准时的补充说明"
                                    value={extra()} onInput={(ev) => setExtra(ev.currentTarget.value)} />
                                </details>
                              </Show>
                            </div>
                            <div class="employee-item-actions">
                              <Show when={item.confirm} fallback={<button type="button" onClick={() => void act("dismiss", item.id)}>撤回</button>}>
                                <button type="button" class="employee-approve" onClick={() => void run("employee_do", { action: "approve", id: item.id, text: extra() })}>批准执行</button>
                                <button type="button" onClick={() => void act("dismiss", item.id)}>驳回</button>
                              </Show>
                              <Show when={item.threadId}>
                                {(id) => <button type="button" onClick={() => void openThread(id())}>看会话</button>}
                              </Show>
                            </div>
                          </li>
                          );
                        }}</For>
                      </ul>
                    </Show>
                  </section>

                  <section class="employee-card" aria-labelledby="employee-duties">
                    <div class="employee-section-heading">
                      <h2 id="employee-duties">日常职责 <span class="employee-count">{e().duties.length}</span></h2>
                      <button type="button" class="employee-text-button" onClick={() => sayInput?.focus()}>＋ 添加职责</button>
                    </div>
                    <Show when={e().duties.length} fallback={<div class="employee-empty"><strong>从一件日常小事开始</strong><p>在上方告诉员工需要定期处理什么，它会为你安排。</p></div>}>
                      <ul class="employee-list">
                        <For each={e().duties}>{(duty) => {
                          // 编辑草稿放本地信号，5s 轮询 reconcile 不会冲掉正在输入的内容。
                          const [draft, setDraft] = createSignal<string | null>(null);
                          const save = async () => {
                            const text = draft()?.trim();
                            if (!text) return;
                            if (text !== duty.text) await run("employee_do", { action: "duty_edit", id: duty.id, text });
                            setDraft(null);
                          };
                          const [noteDraft, setNoteDraft] = createSignal<string | null>(null);
                          const saveNote = async () => {
                            const note = noteDraft()?.trim() ?? "";
                            if (note !== duty.note) await run("employee_do", { action: "duty_note", id: duty.id, text: note });
                            setNoteDraft(null);
                          };
                          const nextCheckAt = () => duty.everyMinutes > 0
                            ? (duty.lastRunAt ? duty.lastRunAt + duty.everyMinutes * 60_000 : 0)
                            : duty.nextCheckAt;
                          return (
                          <li class="employee-duty" classList={{ "is-paused": !duty.enabled }}>
                            <input type="checkbox" checked={duty.enabled} aria-label={`启用职责：${duty.text}`}
                              onChange={() => void act("duty_toggle", duty.id)} />
                            <div class="employee-main">
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
                              <div class="employee-meta">
                                <span class="employee-tag">{!duty.enabled ? "已停用" : duty.everyMinutes > 0 ? `每 ${duty.everyMinutes} 分钟` : "动态调度"}</span>
                                <span>下次检查 {duty.enabled ? (nextCheckAt() > now() ? time(nextCheckAt()) : "待检查") : "—"}</span>
                              </div>
                              <small>上次 {time(duty.lastRunAt)}{duty.lastResult ? ` · ${duty.lastResult}` : ""}</small>
                              <Show when={noteDraft() !== null} fallback={<Show when={duty.note}>
                                <small class="employee-note">备注：{duty.note}</small>
                              </Show>}>
                                <textarea rows={3} aria-label="编辑备注" placeholder="员工的备忘，留空即清除" value={noteDraft() ?? ""}
                                  ref={(el) => queueMicrotask(() => el.focus())}
                                  onInput={(ev) => setNoteDraft(ev.currentTarget.value)}
                                  onKeyDown={(ev) => {
                                    if (ev.key === "Escape") setNoteDraft(null);
                                    else if (ev.key === "Enter" && !ev.shiftKey && !ev.isComposing) { ev.preventDefault(); void saveNote(); }
                                  }} />
                                <div class="employee-edit-actions">
                                  <button type="button" onClick={() => void saveNote()}>保存备注</button>
                                  <button type="button" onClick={() => setNoteDraft(null)}>取消</button>
                                </div>
                              </Show>
                            </div>
                            <details class="employee-more">
                              <summary aria-label={`更多操作：${duty.text}`}>⋯</summary>
                              <div>
                                <label class="employee-interval" title="留空或 0：员工根据职责和执行结果安排下次检查；填入数字：每次结束后等待固定分钟数">执行间隔（分钟）<input type="number" min="0" max="10080" placeholder="自动"
                                  value={duty.everyMinutes || ""} aria-label="执行间隔（分钟）"
                                  onChange={(ev) => void run("employee_do", { action: "duty_every", id: duty.id, text: ev.currentTarget.value })} /></label>
                                <button type="button" onClick={(ev) => { ev.currentTarget.closest("details")!.open = false; setDraft(duty.text); }}>编辑</button>
                                <button type="button" onClick={(ev) => { ev.currentTarget.closest("details")!.open = false; setNoteDraft(duty.note); }}>编辑备注</button>
                                <button type="button" onClick={() => void act("duty_run", duty.id)}>立即执行</button>
                                <button type="button" onClick={() => void act("duty_delete", duty.id)}>删除</button>
                              </div>
                            </details>
                          </li>
                          );
                        }}</For>
                      </ul>
                    </Show>
                  </section>

                  <details class="employee-card employee-history">
                    <summary class="employee-section-heading"><h2>最近运行 <span class="employee-count">{e().runs.length}</span></h2><span class="employee-disclosure" aria-hidden="true">⌄</span></summary>
                    <Show when={e().runs.length} fallback={<p class="employee-empty">还没有运行记录。</p>}>
                      <ul class="employee-list">
                        <For each={[...e().runs].reverse()}>{(r) => (
                          <li>
                            <button type="button" class="employee-run" onClick={() => void openThread(r.threadId)}>
                              <small>{time(r.at)} · {e().duties.find((d) => d.id === r.dutyId)?.text ?? { inbox: "待办", say: "对员工说", approve: "批准执行" }[r.dutyId] ?? r.dutyId}</small>
                              <div>{r.result}</div>
                            </button>
                          </li>
                        )}</For>
                      </ul>
                    </Show>
                  </details>
                </div>

                <aside class="employee-sidebar" aria-label="员工配置">
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
                    <label class="employee-field">空闲后接管
                      <span class="employee-idle-input"><input type="number" min="1" max="1440" value={e().idleMinutes}
                        onChange={(ev) => void set({ idleMinutes: Number(ev.currentTarget.value) || 10 })} /> 分钟</span>
                    </label>
                    <p class="employee-hint">在工作时段内，电脑空闲后自动开始工作；你回来时，员工会让出。</p>
                    <div class="employee-field employee-default-model">
                      <span>默认模型</span>
                      <ModelPicker agentKind={(e().agentKind || lastUsed.agentKind()) as AgentKind} agentKinds={enabledAgentKinds()}
                        model={e().model} onPickModel={(agentKind, model) => void set({ agentKind, model })}
                        title="默认模型：值班检查、职责执行，以及没有匹配模型池条件的事项" portal />
                    </div>
                  </section>

                  <details class="employee-card employee-pool">
                    <summary class="employee-section-heading"><h2>模型池 <span class="employee-count">{e().pool.length}</span></h2><span class="employee-disclosure" aria-hidden="true">⌄</span></summary>
                    <p class="employee-hint">按任务匹配专用模型，其余使用默认模型。</p>
                    <ul class="employee-list">
                      <For each={e().pool}>{(m, i) => {
                        const edit = (patch: Partial<PoolModel>) => set({ pool: e().pool.map((p, j) => (j === i() ? { ...p, ...patch } : p)) });
                        return (
                          <li>
                            <input type="text" maxLength={40} placeholder="名字，如 编码" aria-label="模型名字" value={m.name}
                              onChange={(ev) => void edit({ name: ev.currentTarget.value })} />
                            <input type="text" maxLength={500} placeholder="适用条件，如 需要写代码时" aria-label="适用条件" value={m.when}
                              onChange={(ev) => void edit({ when: ev.currentTarget.value })} />
                            <div class="employee-pool-actions">
                              <ModelPicker agentKind={m.agentKind as AgentKind} agentKinds={enabledAgentKinds()} model={m.model}
                                onPickModel={(agentKind, model) => void edit({ agentKind, model })} title="该条件下使用的模型" portal />
                              <button type="button" onClick={() => void set({ pool: e().pool.filter((_, j) => j !== i()) })}>删除</button>
                            </div>
                          </li>
                        );
                      }}</For>
                    </ul>
                    <button type="button" class="employee-add-model" onClick={() => {
                      const agentKind = lastUsed.agentKind();
                      void set({ pool: [...e().pool, { name: `模型${e().pool.length + 1}`, when: "", agentKind, model: lastUsed.model(agentKind) }] });
                    }}>＋ 添加模型</button>
                  </details>
                </aside>
              </div>
            </div>
          );
        }}
      </Show>
    </div>
  );
}
