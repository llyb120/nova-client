import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { confirm } from "@tauri-apps/plugin-dialog";
import {
  designMode, layoutApi, layoutState, layoutSrcdoc, loadLayouts, setDesignTarget, toggleDesignMode,
} from "../layoutStore";
import "./LayoutPanel.css";

/** 会话里的呈现布局：沙箱 iframe + 布局切换 + 预设。主会话只通过 layout_update 推数据。 */
export function LayoutPanel(props: { threadId: string }) {
  const [collapsed, setCollapsed] = createSignal(false);
  const [menu, setMenu] = createSignal(false);
  const [renaming, setRenaming] = createSignal<string | null>(null);
  let frame: HTMLIFrameElement | undefined;

  const info = () => layoutState.byThread[props.threadId];
  const layouts = () => info()?.layouts ?? [];
  const active = createMemo(() => layouts().find((l) => l.id === info()?.active) ?? layouts()[0]);
  const visible = () => layouts().length > 0 || designMode(props.threadId);

  createEffect(() => void loadLayouts(props.threadId));

  const post = () => {
    const layout = active();
    frame?.contentWindow?.postMessage({ type: "nova-layout-data", data: layout ? JSON.parse(JSON.stringify(layout.data)) : {} }, "*");
  };
  // 数据变化只推数据，不重载 iframe；HTML 变化（srcdoc 变）才重载，重载后靠 ready / load 补发。
  createEffect(() => {
    JSON.stringify(active()?.data);
    post();
  });
  const onMessage = (event: MessageEvent) => {
    if (event.source === frame?.contentWindow && event.data?.type === "nova-layout-ready") post();
  };
  window.addEventListener("message", onMessage);
  onCleanup(() => window.removeEventListener("message", onMessage));

  const act = (run: () => Promise<unknown>) => void run().catch(() => {});
  const newLayout = () => {
    setDesignTarget(props.threadId, "new");
    if (!designMode(props.threadId)) toggleDesignMode(props.threadId);
    setMenu(false);
  };
  const remove = async () => {
    const layout = active();
    if (layout && (await confirm(`删除布局「${layout.name}」？`, { title: "删除布局", kind: "warning" }))) {
      act(() => layoutApi.remove(props.threadId, layout.id));
    }
  };

  return (
    <Show when={visible()}>
      <section class="layout-panel" classList={{ collapsed: collapsed() }} aria-label="呈现布局">
        <div class="layout-bar">
          <div class="layout-tabs" role="tablist">
            <For each={layouts()}>
              {(layout) => (
                <Show
                  when={renaming() === layout.id}
                  fallback={
                    <button type="button" role="tab" class="layout-tab" classList={{ active: layout.id === active()?.id }}
                      aria-selected={layout.id === active()?.id}
                      title="单击切换，双击重命名"
                      onClick={() => act(() => layoutApi.switch(props.threadId, layout.id))}
                      onDblClick={() => setRenaming(layout.id)}>
                      {layout.name}
                      <Show when={layout.designing}><span class="layout-tab-busy">设计中…</span></Show>
                    </button>
                  }
                >
                  <input class="layout-rename" value={layout.name} ref={(el) => queueMicrotask(() => el.select())}
                    onBlur={(e) => {
                      const name = e.currentTarget.value.trim();
                      setRenaming(null);
                      if (name && name !== layout.name) act(() => layoutApi.rename(props.threadId, layout.id, name));
                    }}
                    onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); if (e.key === "Escape") setRenaming(null); }} />
                </Show>
              )}
            </For>
            <div class="layout-add">
              <button type="button" class="layout-icon-btn" title="新建布局 / 使用预设" aria-label="新建布局" aria-expanded={menu()} onClick={() => setMenu(!menu())}>＋</button>
              <Show when={menu()}>
                <div class="layout-menu" onMouseLeave={() => setMenu(false)}>
                  <button type="button" onClick={newLayout}>设计新布局…</button>
                  <Show when={layoutState.presets.some((p) => p.html)}>
                    <div class="layout-menu-head">预设布局</div>
                    <For each={layoutState.presets.filter((p) => p.html)}>
                      {(preset) => (
                        <button type="button" onClick={() => { setMenu(false); act(() => layoutApi.usePreset(props.threadId, preset.id)); }}>{preset.name}</button>
                      )}
                    </For>
                  </Show>
                </div>
              </Show>
            </div>
          </div>
          <div class="layout-actions">
            <Show when={active()?.html}>
              <button type="button" class="layout-icon-btn" title="存为预设布局" onClick={() => act(() => layoutApi.saveAsPreset(props.threadId, active()!.id))}>存为预设</button>
              <button type="button" class="layout-icon-btn" title="清空布局数据" onClick={() => act(() => layoutApi.clearData(props.threadId, active()!.id))}>清空</button>
            </Show>
            <Show when={active()}>
              <button type="button" class="layout-icon-btn" title="删除布局" onClick={() => void remove()}>删除</button>
            </Show>
            <button type="button" class="layout-icon-btn" classList={{ on: designMode(props.threadId) }} title="设计模式（输入框内按 Tab 切换）：提示词用于设计或调整布局"
              onClick={() => toggleDesignMode(props.threadId)}>设计模式</button>
            <button type="button" class="layout-icon-btn" aria-label={collapsed() ? "展开布局" : "收起布局"} onClick={() => setCollapsed(!collapsed())}>{collapsed() ? "▾" : "▴"}</button>
          </div>
        </div>
        <Show when={!collapsed()}>
          <div class="layout-stage">
            <Show
              when={active()?.html}
              fallback={
                <div class="layout-empty">
                  {active()?.designing ? "设计会话正在设计布局…" : "还没有布局。按 Tab 进入设计模式，描述你想要的呈现方式；或点「＋」使用预设。"}
                </div>
              }
            >
              {/* sandbox 不含 allow-same-origin：页面拿不到应用上下文，只能通过 postMessage 收数据 */}
              <iframe ref={frame} title={active()!.name} sandbox="allow-scripts" referrerpolicy="no-referrer"
                srcdoc={layoutSrcdoc(active()!.html)} onLoad={post} />
            </Show>
          </div>
        </Show>
      </section>
    </Show>
  );
}
