import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createEffect, createMemo, createSignal, For, onSettled, Show } from "solid-js";
import { createStore, reconcile } from "solid-js";
import "./OrbitTodo.css";

type Item = { id: string; text: string; done: boolean; star: boolean; createdAt: number; doneAt: number };
type Filter = "all" | "todo" | "done";
const FILTERS: [Filter, string][] = [["all", "全部"], ["todo", "待办"], ["done", "已完成"]];

const svg = (d: string, size = 15) => <svg viewBox="0 0 24 24" width={size} height={size} fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d={d} /></svg>;
const PLUS = "M12 5v14M5 12h14";
const TRASH = "M4 7h16M10 7V4h4v3M6 7l1 13h10l1-13M10 11v6M14 11v6";
const STAR = "M12 3.8l2.5 5.1 5.6.8-4 4 .9 5.6-5-2.6-5 2.6.9-5.6-4-4 5.6-.8z";

const ago = (ms: number) => {
  const m = Math.floor((Date.now() - ms) / 60000);
  if (m < 1) return "刚刚";
  if (m < 60) return `${m} 分钟前`;
  if (m < 1440) return `${Math.floor(m / 60)} 小时前`;
  return new Date(ms).toLocaleDateString(undefined, { month: "numeric", day: "numeric" });
};

/** 星轨：Butterfly 里的待办清单。回车连续添加，点圆圈完成，点文字就地修改；数字员工可订阅它的更新。 */
export default function OrbitTodo(props: { active: boolean }) {
  const [items, setItems] = createStore<Item[]>([]);
  const [filter, setFilter] = createSignal<Filter>("all");
  const [draft, setDraft] = createSignal("");
  const [editing, setEditing] = createSignal<{ id: string; text: string } | null>(null);
  const [error, setError] = createSignal("");
  let input: HTMLInputElement | undefined;
  let disposed = false;

  const refresh = () => invoke<Item[]>("orbit_list").then((list) => { if (!disposed) { setItems(reconcile(list, "id")); setError(""); } }, (e) => setError(String(e)));
  const run = (job: Promise<unknown>) => job.then(refresh, (e) => { setError(String(e)); void refresh(); });
  onSettled(() => {
    void refresh();
    const off = listen("orbit:changed", () => void refresh());
    return () => { disposed = true; void off.then((fn) => fn()); };
  });
  createEffect(() => props.active, (active) => { if (active) { void refresh(); queueMicrotask(() => input?.focus()); } });

  const pending = createMemo(() => items.filter((i) => !i.done).sort((a, b) => Number(b.star) - Number(a.star)));
  const finished = createMemo(() => items.filter((i) => i.done).sort((a, b) => b.doneAt - a.doneAt));
  const shown = createMemo(() => filter() === "todo" ? pending() : filter() === "done" ? finished() : [...pending(), ...finished()]);
  const ratio = () => items.length ? finished().length / items.length : 0;

  const add = () => {
    const text = draft().trim();
    if (!text) return;
    setDraft("");
    void run(invoke("orbit_add", { text }));
  };
  const toggle = (item: Item) => {
    // 先在界面上翻转，手感更跟手；失败时 run 会重新同步。
    const done = !item.done; // item 是 store 代理，setItems 后再读会拿到翻转后的值
    setItems(draft => { const target = draft.find(i => i.id === item.id); if (target) Object.assign(target, { done, doneAt: done ? Date.now() : 0 }); });
    void run(invoke("orbit_done", { id: item.id, done }));
  };
  const star = (item: Item) => {
    const next = !item.star;
    setItems(draft => { const target = draft.find(i => i.id === item.id); if (target) target.star = next; });
    void run(invoke("orbit_edit", { id: item.id, patch: { star: next } }));
  };
  const commit = () => {
    const e = editing();
    setEditing(null);
    const text = e?.text.trim();
    if (e && text && text !== items.find((i) => i.id === e.id)?.text) void run(invoke("orbit_edit", { id: e.id, patch: { text } }));
  };

  return (
    <section class="ob" aria-label="星轨">
      <div class="ob-top">
        <div class="ob-progress" role="progressbar" aria-valuemin={0} aria-valuemax={items.length} aria-valuenow={finished().length} aria-label="完成进度">
          <i style={{ width: `${Math.round(ratio() * 100)}%` }} />
        </div>
        <span class="ob-count"><b>{finished().length}</b>/{items.length}</span>
      </div>
      <form class="ob-add" onSubmit={(e) => { e.preventDefault(); add(); }}>
        {svg(PLUS, 16)}
        <input ref={input} value={draft()} maxlength={500} placeholder="添加一件事，回车保存" aria-label="添加待办" autocomplete="off"
          onInput={(e) => setDraft(e.currentTarget.value)} />
        <button type="submit" disabled={!draft().trim()} aria-label="添加">添加</button>
      </form>
      <div class="ob-bar">
        <div class="ob-seg" role="tablist" aria-label="筛选">
          <For each={FILTERS}>{([key, label]) => (
            <button type="button" role="tab" aria-selected={filter() === key ? "true" : "false"} class={{ on: filter() === key }} onClick={() => setFilter(key)}>
              {label}<Show when={key === "todo" && pending().length}><em>{pending().length}</em></Show>
            </button>
          )}</For>
        </div>
        <Show when={finished().length}>
          <button type="button" class="ob-clear" onClick={() => void run(invoke("orbit_clear_done"))}>清除已完成</button>
        </Show>
      </div>
      <ul class="ob-list">
        <For each={shown()} fallback={
          <li class="ob-empty">
            <svg viewBox="0 0 64 64" width="56" height="56" fill="none" stroke="currentColor" stroke-width="1.4" aria-hidden="true">
              <ellipse cx="32" cy="32" rx="26" ry="10" transform="rotate(-24 32 32)" /><circle cx="32" cy="32" r="5" fill="currentColor" stroke="none" /><circle cx="52" cy="22" r="2.4" fill="currentColor" stroke="none" />
            </svg>
            <span>{filter() === "done" ? "还没有完成的事项" : items.length ? "全部完成，星轨清爽" : "星轨空空如也"}</span>
            <small>{items.length ? "" : "在上面写下第一件事，回车即可"}</small>
          </li>
        }>{(item) => (
          <li class={["ob-item", { done: item.done, starred: item.star }]}>
            <button type="button" class="ob-check" role="checkbox" aria-checked={item.done ? "true" : "false"} aria-label={item.done ? `重新打开：${item.text}` : `完成：${item.text}`} onClick={() => toggle(item)}>
              <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true"><circle cx="12" cy="12" r="9" /><path d="M7.8 12.4l3 3 5.4-6" /></svg>
            </button>
            <Show when={editing()?.id === item.id} fallback={
              <span class="ob-text" title="点击修改" onClick={() => setEditing({ id: item.id, text: item.text })}>{item.text}</span>
            }>
              <input class="ob-edit" value={editing()!.text} maxlength={500} aria-label="修改内容" ref={(el) => queueMicrotask(() => { el.focus(); el.select(); })}
                onInput={(e) => setEditing({ id: item.id, text: e.currentTarget.value })} onBlur={commit}
                onKeyDown={(e) => { if (e.key === "Enter" && !e.isComposing) commit(); else if (e.key === "Escape") setEditing(null); }} />
            </Show>
            <span class="ob-time">{ago(item.done ? item.doneAt : item.createdAt)}</span>
            <span class="ob-tools">
              <button type="button" class={["ob-star", { on: item.star }]} aria-pressed={item.star ? "true" : "false"} title={item.star ? "取消标星" : "标星置顶"} aria-label="标星" onClick={() => star(item)}>{svg(STAR)}</button>
              <button type="button" class="ob-del" title="删除" aria-label={`删除：${item.text}`} onClick={() => void run(invoke("orbit_remove", { id: item.id }))}>{svg(TRASH)}</button>
            </span>
          </li>
        )}</For>
      </ul>
      <Show when={error()}><div class="ob-error" role="alert">{error()}</div></Show>
    </section>
  );
}
