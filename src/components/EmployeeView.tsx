import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { enabledAgentKinds, lastUsed, openThread, showToast } from "../store";
import type { AgentKind } from "../types";
import { ModelPicker } from "./ConfigSelects";
import "./EmployeeView.css";

type Duty = { id: string; text: string; enabled: boolean; note: string; lastRunAt: number; lastResult: string };
type Todo = { id: string; text: string; confirm: boolean; createdAt: number; threadId?: string | null };
type Run = { at: number; dutyId: string; result: string; threadId: string };
type Snapshot = {
  employee: {
    enabled: boolean; workStart: string; workEnd: string; idleMinutes: number;
    agentKind: string; model: string; duties: Duty[]; inbox: Todo[]; runs: Run[];
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
  const data = () => store.snap;
  const refetch = () => invoke<Snapshot>("employee_get").then(
    (snap) => setStore("snap", reconcile(snap)),
    (error) => showToast(`读取员工数据失败：${String(error)}`),
  );
  void refetch();
  const [say, setSay] = createSignal("");
  const [busy, setBusy] = createSignal(false);

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
            <>
              <section class="employee-status" aria-label="员工状态">
                <strong class={`employee-badge ${snap().status}`}>{STATUS[snap().status]}</strong>
                <label><input type="checkbox" checked={e().enabled}
                  onChange={(ev) => void (async () => {
                    if (ev.currentTarget.checked && !e().agentKind) await useCurrentModel();
                    await set({ enabled: ev.currentTarget.checked });
                  })()} /> 自动值班</label>
                <span>下次检查 {time(snap().nextCheckAt)}</span>
                <label>工作时段 <input type="time" value={e().workStart} onChange={(ev) => void set({ workStart: ev.currentTarget.value })} />
                  – <input type="time" value={e().workEnd} onChange={(ev) => void set({ workEnd: ev.currentTarget.value })} /></label>
                <label>空闲 <input type="number" min="1" max="1440" value={e().idleMinutes}
                  onChange={(ev) => void set({ idleMinutes: Number(ev.currentTarget.value) || 10 })} /> 分钟后接管</label>
                <ModelPicker agentKind={(e().agentKind || lastUsed.agentKind()) as AgentKind} agentKinds={enabledAgentKinds()}
                  model={e().model} onPickModel={(agentKind, model) => void set({ agentKind, model })}
                  title="员工会话使用的模型" portal />
                <Show when={snap().threadId}>
                  {(id) => <button type="button" onClick={() => void openThread(id())}>看当前会话</button>}
                </Show>
                <button type="button" onClick={() => void act("check")}>立即检查一次</button>
                <Show when={!snap().idleSupported}>
                  <span class="employee-hint">当前平台无法检测空闲，不会自动接管，只能手动触发。</span>
                </Show>
              </section>

              <section aria-labelledby="employee-duties">
                <h2 id="employee-duties">职责</h2>
                <Show when={e().duties.length} fallback={<p class="employee-empty">还没有职责，在下方对员工说一句，例如“每天 9 点检查未读邮件并汇总”。</p>}>
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
                      return (
                      <li>
                        <input type="checkbox" checked={duty.enabled} aria-label={`启用职责：${duty.text}`}
                          onChange={() => void act("duty_toggle", duty.id)} />
                        <div class="employee-main">
                          <Show when={draft() !== null} fallback={<div>{duty.text}</div>}>
                            <textarea rows={2} maxLength={500} aria-label="编辑职责" value={draft() ?? ""} ref={(el) => queueMicrotask(() => el.focus())}
                              onInput={(ev) => setDraft(ev.currentTarget.value)}
                              onKeyDown={(ev) => {
                                if (ev.key === "Escape") setDraft(null);
                                else if (ev.key === "Enter" && !ev.shiftKey && !ev.isComposing) { ev.preventDefault(); void save(); }
                              }} />
                            <div>
                              <button type="button" disabled={!draft()?.trim()} onClick={() => void save()}>保存</button>
                              <button type="button" onClick={() => setDraft(null)}>取消</button>
                            </div>
                          </Show>
                          <small>上次 {time(duty.lastRunAt)}{duty.lastResult ? ` · ${duty.lastResult}` : ""}</small>
                        </div>
                        <details class="employee-more">
                          <summary aria-label="更多操作">⋯</summary>
                          <div>
                            <button type="button" onClick={(ev) => { ev.currentTarget.closest("details")!.open = false; setDraft(duty.text); }}>编辑</button>
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

              <section aria-labelledby="employee-inbox">
                <h2 id="employee-inbox">待办 / 待确认</h2>
                <Show when={e().inbox.length} fallback={<p class="employee-empty">没有待处理的事。任意会话里说“交给员工：xxx”即可派活。</p>}>
                  <ul class="employee-list">
                    <For each={e().inbox}>{(item) => (
                      <li>
                        <span class={`employee-tag ${item.confirm ? "confirm" : ""}`}>{item.confirm ? "待确认" : "待办"}</span>
                        <div class="employee-main">
                          <div>{item.text}</div>
                          <small>{time(item.createdAt)}</small>
                        </div>
                        <Show when={item.confirm} fallback={<button type="button" onClick={() => void act("dismiss", item.id)}>撤回</button>}>
                          <button type="button" onClick={() => void act("approve", item.id)}>批准</button>
                          <button type="button" onClick={() => void act("dismiss", item.id)}>驳回</button>
                        </Show>
                        <Show when={item.threadId}>
                          {(id) => <button type="button" onClick={() => void openThread(id())}>看会话</button>}
                        </Show>
                      </li>
                    )}</For>
                  </ul>
                </Show>
              </section>

              <section aria-labelledby="employee-runs">
                <h2 id="employee-runs">最近运行</h2>
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
              </section>
            </>
          );
        }}
      </Show>
      <form class="employee-say" onSubmit={(ev) => { ev.preventDefault(); void send(); }}>
        <textarea rows={2} placeholder="对员工说：新增/修改职责，或直接派活…（Enter 发送，Shift+Enter 换行）" aria-label="对员工说"
          value={say()} onInput={(ev) => setSay(ev.currentTarget.value)}
          onKeyDown={(ev) => { if (ev.key === "Enter" && !ev.shiftKey && !ev.isComposing) { ev.preventDefault(); void send(); } }} />
        <button type="submit" disabled={busy() || !say().trim()}>发送</button>
      </form>
    </div>
  );
}
