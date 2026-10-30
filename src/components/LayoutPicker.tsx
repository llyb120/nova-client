import { createSignal, For, onCleanup, Show } from "solid-js";
import { isPendingThreadId } from "../store";
import { layoutApi, layoutState, loadLayouts, openPresets, pendingPreset, setPendingPreset, showLayouts } from "../layoutStore";
import "./LayoutPanel.css";

/**
 * 输入框工具栏里的看板选择器（会话里只能选择预设，设计在侧栏看板页的「配置」里）。
 * - 会话内：查看本会话的看板、添加预设看板；
 * - 新会话页（threadId 为空）：预选一个预设，会话创建后自动套用。
 */
export function LayoutPicker(props: { threadId: string | null; anchorUp?: boolean }) {
  const [open, setOpen] = createSignal(false);
  let root: HTMLDivElement | undefined;

  const tid = () => (props.threadId && !isPendingThreadId(props.threadId) ? props.threadId : null);
  const layouts = () => (tid() ? layoutState.byThread[tid()!]?.layouts ?? [] : []);
  const presets = () => layoutState.presets.filter((p) => p.html);
  const preselected = () => presets().find((p) => p.id === pendingPreset());
  const label = () => (tid() ? (layouts().length > 1 ? `看板 · ${layouts().length}` : layouts()[0]?.name) : preselected()?.name) ?? "看板";
  const on = () => (tid() ? layouts().length > 0 : !!preselected());

  const toggle = () => {
    const next = !open();
    setOpen(next);
    if (next) void loadLayouts(tid() ?? "");
  };
  const close = () => setOpen(false);
  const add = (presetId: string) => {
    close();
    showLayouts();
    void layoutApi.usePreset(tid()!, presetId).catch(() => {});
  };

  const onDocDown = (e: MouseEvent) => {
    if (open() && root && !root.contains(e.target as Node)) close();
  };
  document.addEventListener("mousedown", onDocDown);
  onCleanup(() => document.removeEventListener("mousedown", onDocDown));

  return (
    <div class="layout-picker" ref={root}>
      <button type="button" class={["pill layout-picker-btn", { on: on() }]} aria-expanded={open() ? "true" : "false"} aria-haspopup="menu"
        title={tid() ? "本会话的看板" : "为新会话选择看板"} onClick={toggle}>
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" aria-hidden="true">
          <rect x="1.7" y="2.2" width="12.6" height="11.6" rx="2" /><path d="M1.7 6h12.6M6.2 6v7.8" />
        </svg>
        <span class="pill-text">{label()}</span>
      </button>
      <Show when={open()}>
        <div class={["layout-popover", { down: props.anchorUp === false }]} role="menu">
          <Show when={tid() && layouts().length > 0}>
            <div class="layout-pop-head">本会话的看板</div>
            <For each={layouts()}>
              {(l) => (
                <button type="button" role="menuitem" class="layout-pop-item" onClick={() => { close(); showLayouts(); }}>
                  <span class="layout-pop-check">▦</span>
                  <span class="layout-pop-name">{l.name}</span>
                </button>
              )}
            </For>
            <div class="layout-pop-sep" />
          </Show>
          <div class="layout-pop-head">{tid() ? "添加看板" : "看板（新会话创建后自动添加）"}</div>
          <Show when={!tid()}>
            <button type="button" role="menuitemradio" aria-checked={!pendingPreset() ? "true" : "false"} class={["layout-pop-item", { active: !pendingPreset() }]}
              onClick={() => { setPendingPreset(null); close(); }}>
              <span class="layout-pop-check">{!pendingPreset() ? "✓" : ""}</span>
              <span class="layout-pop-name">不使用看板</span>
            </button>
          </Show>
          <Show when={presets().length > 0} fallback={<div class="layout-pop-empty">还没有看板预设，到会话侧栏「看板 → 配置」设计一个</div>}>
            <For each={presets()}>
              {(p) => (
                <button type="button" role="menuitem" class={["layout-pop-item", { active: !tid() && p.id === pendingPreset() }]}
                  onClick={() => { if (tid()) add(p.id); else { setPendingPreset(p.id); close(); } }}>
                  <span class="layout-pop-check">{tid() ? "＋" : p.id === pendingPreset() ? "✓" : ""}</span>
                  <span class="layout-pop-name">{p.name}</span>
                </button>
              )}
            </For>
          </Show>
          <Show when={tid()}>
            <div class="layout-pop-sep" />
            <button type="button" role="menuitem" class="layout-pop-item" onClick={() => { close(); openPresets(); }}>
              <span class="layout-pop-check">⚙</span><span class="layout-pop-name">配置看板预设…</span>
            </button>
          </Show>
        </div>
      </Show>
    </div>
  );
}
