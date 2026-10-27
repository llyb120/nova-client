import { createEffect, createMemo, createSignal, For, lazy, onCleanup, Show, Suspense } from "solid-js";
import { confirm } from "@tauri-apps/plugin-dialog";
import {
  layoutApi, layoutRefreshing, layoutState, layoutSrcdoc, layoutTabOpen, loadLayouts, presetsOpen, setPresetsOpen, showLayouts,
  themePayload, watchTheme, type LayoutItem,
} from "../layoutStore";
import "./LayoutPanel.css";

const LayoutPresetsView = lazy(() => import("./LayoutPresetsView"));

/**
 * 有看板但侧栏没显示看板页时，聊天区右上角的「看板」入口；
 * 会话里新增了看板（选择预设）时自动打开侧栏的看板页。
 */
export function LayoutBack(props: { threadId: string }) {
  createEffect(() => void loadLayouts(props.threadId));
  const count = () => layoutState.byThread[props.threadId]?.layouts.length ?? 0;
  let seen: number | undefined;
  createEffect(() => {
    const n = count();
    // 首次载入只记录，不自动打开；之后增加才打开。
    if (seen !== undefined && n > seen) showLayouts();
    if (layoutState.byThread[props.threadId]) seen = n;
  });
  return (
    <Show when={count() > 0 && !layoutTabOpen()}>
      <button type="button" class="layout-back" onClick={() => showLayouts()} title="在侧栏显示看板">▦ 看板</button>
    </Show>
  );
}

/**
 * 侧栏「看板」页：本会话选择的看板并排显示。每个看板是后端持久化在磁盘上的页面（模板 + 数据，
 * 见 layout.rs save_doc），数据由后端按会话内容自动生成后再推送。「配置」切到预设的设计与管理。
 */
export function LayoutPanel(props: { threadId: string }) {
  const [menu, setMenu] = createSignal(false);
  const info = () => layoutState.byThread[props.threadId];
  const layouts = () => info()?.layouts ?? [];
  const presets = () => layoutState.presets.filter((p) => p.html);

  createEffect(() => void loadLayouts(props.threadId));

  const act = (run: () => Promise<unknown>) => void run().catch(() => {});

  // 只有用户真的在看看板（窗口可见、不在配置页）时后端才生成数据；离开时撤销，回来时后端补一次。
  const [pageVisible, setPageVisible] = createSignal(!document.hidden);
  const onVisibility = () => setPageVisible(!document.hidden);
  document.addEventListener("visibilitychange", onVisibility);
  onCleanup(() => document.removeEventListener("visibilitychange", onVisibility));
  createEffect(() => act(() => layoutApi.viewing(pageVisible() && !presetsOpen() ? props.threadId : null)));
  onCleanup(() => act(() => layoutApi.viewing(null)));

  return (
    <section class="layout-pane" aria-label="看板">
        <div class="layout-bar">
          <Show when={!presetsOpen()} fallback={<span class="layout-bar-title">看板配置</span>}>
            <div class="layout-add">
              <button type="button" class="layout-icon-btn" title="添加看板" aria-expanded={menu()} onClick={() => setMenu(!menu())}>＋ 添加看板</button>
              <Show when={menu()}>
                <div class="layout-menu" onMouseLeave={() => setMenu(false)}>
                  <For each={presets()} fallback={<div class="layout-menu-head">还没有预设，先在「配置」里设计一个</div>}>
                    {(preset) => (
                      <button type="button" onClick={() => { setMenu(false); act(() => layoutApi.usePreset(props.threadId, preset.id)); }}>{preset.name}</button>
                    )}
                  </For>
                </div>
              </Show>
            </div>
          </Show>
          <Show when={!presetsOpen() && layoutRefreshing(props.threadId)}>
            <span class="layout-refreshing" role="status"><span class="layout-spinner small" />正在根据会话更新…</span>
          </Show>
          <button type="button" class="layout-icon-btn" classList={{ on: presetsOpen() }} aria-pressed={presetsOpen()}
            title={presetsOpen() ? "返回本会话的看板" : "设计和管理看板预设"} onClick={() => setPresetsOpen(!presetsOpen())}>
            {presetsOpen() ? "← 返回" : "⚙ 配置"}
          </button>
        </div>
        <Show when={presetsOpen()}>
          <Suspense><LayoutPresetsView /></Suspense>
        </Show>
        <div class="layout-cols" style={{ display: presetsOpen() ? "none" : undefined }}>
          <For each={layouts()} fallback={
            <div class="layout-empty">
              <div class="layout-empty-title">还没有看板</div>
              <div>从「＋ 添加看板」选择一个预设，系统会根据会话内容自动填充并持续更新。</div>
            </div>
          }>
            {(layout) => <LayoutColumn threadId={props.threadId} layout={layout} />}
          </For>
        </div>
      </section>
  );
}


/** 一个看板：标题 + 沙箱 iframe。 */
function LayoutColumn(props: { threadId: string; layout: LayoutItem }) {
  const [more, setMore] = createSignal(false);
  const [renaming, setRenaming] = createSignal(false);
  let frame: HTMLIFrameElement | undefined;
  const act = (run: () => Promise<unknown>) => void run().catch(() => {});

  // 页面烘焙的数据可能落后于 store（get 与数据事件交错），所以不做去重/就绪门闩：
  // 数据变化时直接推送，页面每次就绪时再无条件推送一次（推给未加载完的文档只会被丢弃）。
  const srcdoc = createMemo(() => layoutSrcdoc(props.layout.html));
  const post = () => frame?.contentWindow?.postMessage({ type: "nova-layout-data", data: JSON.parse(JSON.stringify(props.layout.data)) }, "*");
  createEffect(post);
  const postTheme = () => frame?.contentWindow?.postMessage({ type: "nova-layout-theme", theme: themePayload() }, "*");
  const onMessage = (event: MessageEvent) => {
    if (event.source === frame?.contentWindow && event.data?.type === "nova-layout-ready") { postTheme(); post(); }
  };
  onCleanup(watchTheme(postTheme));
  window.addEventListener("message", onMessage);
  onCleanup(() => window.removeEventListener("message", onMessage));

  const remove = async () => {
    if (await confirm(`从本会话移除看板「${props.layout.name}」？`, { title: "移除看板", kind: "warning" })) act(() => layoutApi.remove(props.threadId, props.layout.id));
  };

  return (
    <div class="layout-col" classList={{ refreshing: layoutRefreshing(props.threadId) }} aria-busy={layoutRefreshing(props.threadId)}>
      <div class="layout-col-head">
        <Show when={renaming()} fallback={
          <span class="layout-col-name" title="双击重命名" onDblClick={() => setRenaming(true)}>{props.layout.name}</span>
        }>
          <input class="layout-rename" value={props.layout.name} ref={(el) => queueMicrotask(() => el.select())}
            onBlur={(e) => {
              const name = e.currentTarget.value.trim();
              setRenaming(false);
              if (name && name !== props.layout.name) act(() => layoutApi.rename(props.threadId, props.layout.id, name));
            }}
            onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); if (e.key === "Escape") setRenaming(false); }} />
        </Show>
        <div class="layout-add">
          <button type="button" class="layout-icon-btn" aria-label="看板操作" aria-expanded={more()} onClick={() => setMore(!more())}>⋯</button>
          <Show when={more()}>
            <div class="layout-menu right" onMouseLeave={() => setMore(false)}>
              <button type="button" onClick={() => { setMore(false); setRenaming(true); }}>重命名</button>
              <button type="button" title="清空后按会话内容重新生成" onClick={() => { setMore(false); act(() => layoutApi.clearData(props.threadId, props.layout.id)); }}>重新生成数据</button>
              <button type="button" class="danger" onClick={() => { setMore(false); void remove(); }}>移除看板</button>
            </div>
          </Show>
        </div>
      </div>
      <div class="layout-stage">
        <Show when={props.layout.html} fallback={<div class="layout-empty">这个看板没有内容</div>}>
          {/* sandbox 不含 allow-same-origin：页面拿不到应用上下文，只能通过 postMessage 收数据 */}
          <iframe ref={frame} title={props.layout.name} sandbox="allow-scripts" referrerpolicy="no-referrer"
            srcdoc={srcdoc()} />
        </Show>
      </div>
    </div>
  );
}