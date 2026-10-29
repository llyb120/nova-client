import { getCurrentWindow } from "@tauri-apps/api/window";
import { createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { sidebarLayout } from "../sidebarLayout";
import { IconX } from "./icons";

const appWindow = getCurrentWindow();

// Portal 挂载点用 ref 信号而非 getElementById：首次渲染时标题栏尚未接入文档，按 id 查询会落空。
const [titlebarSide, setTitlebarSide] = createSignal<HTMLElement>();
const [titlebarView, setTitlebarView] = createSignal<HTMLElement>();
let titlebarHost: HTMLDivElement | undefined;
let titlebarRevision = 0;
export { titlebarSide, titlebarView };

/** 各页面只迁移原有顶栏；独立应用窗口没有主标题栏时不挂载。 */
export function TitleBarPortal(props: { children: JSX.Element }) {
  const revision = ++titlebarRevision;
  let staging: HTMLDivElement | undefined;
  onMount(() => {
    const host = titlebarHost;
    if (!host) return;
    // 新页头先在下屏暂存区渲染，等到真实 DOM 存在才替换旧页头，切页不再闪空白。
    const prepared = staging;
    if (!prepared?.firstElementChild) return;
    let moved: ChildNode[] = [];
    const commit = () => {
      if (revision !== titlebarRevision || !prepared.firstElementChild) return;
      moved = Array.from(prepared.childNodes);
      host.replaceChildren(...moved);
    };
    // 页面卸载后页头已被搬进标题栏，Portal 清理管不到；不清掉会残留到没有页头的页面（如新建会话），连带展开的弹层。
    onCleanup(() => moved.forEach(node => { if (node.parentNode === host) node.remove(); }));
    if (document.fonts.status === "loaded") commit();
    else void document.fonts.ready.then(commit);
  });
  onCleanup(() => { staging = undefined; });
  const view = titlebarView();
  return view
    ? <Portal mount={view} ref={element => { staging = element; }}>{props.children}</Portal>
    : props.children;
}

/** 自定义标题栏：左列承接侧栏品牌行（Sidebar Portal），右侧承接当前会话头部（ChatView Portal）。 */
export function TitleBar() {
  const [maximized, setMaximized] = createSignal(false);

  onMount(() => {
    let active = true;
    const syncMaximized = () => {
      void appWindow.isMaximized().then(value => {
        if (active) setMaximized(value);
      }).catch(() => {});
    };
    syncMaximized();
    const unlisten = appWindow.onResized(syncMaximized);
    onCleanup(() => {
      active = false;
      void unlisten.then(remove => remove());
    });
  });

  const toggleMaximize = async () => {
    await appWindow.toggleMaximize();
    setMaximized(await appWindow.isMaximized());
  };
  const isInteractive = (target: EventTarget | null) =>
    target instanceof Element && !!target.closest('button, input, textarea, select, a, summary, label, [role="button"], [contenteditable="true"]');
  // 按下后真正移动才开始原生拖动：按下就拖会进入系统移动循环、吞掉鼠标抬起，双击就识别不到了。
  let pressed = false;
  const handleDown = (event: MouseEvent) => {
    if (event.button !== 0 || isInteractive(event.target)) return;
    // 捕获阶段统一接管标题/目录/空白，避免文字选择与局部处理器抢先；
    // 同时拦下 Tauri 注入的 data-tauri-drag-region 脚本，否则它会再切换一次最大化。
    event.preventDefault();
    event.stopPropagation();
    pressed = true;
  };
  const handleMove = (event: MouseEvent) => {
    if (!pressed) return;
    pressed = false;
    if (event.buttons & 1) void appWindow.startDragging();
  };
  const handleUp = () => { pressed = false; };
  const handleDoubleClick = (event: MouseEvent) => {
    if (event.button !== 0 || isInteractive(event.target)) return;
    if (event.target instanceof Element && event.target.closest(".chat-title")) return;
    void toggleMaximize();
  };
  let titlebarElement: HTMLElement | undefined;
  onMount(() => {
    titlebarElement?.addEventListener("mousedown", handleDown, true);
    titlebarElement?.addEventListener("mousemove", handleMove, true);
    titlebarElement?.addEventListener("dblclick", handleDoubleClick);
    window.addEventListener("mouseup", handleUp, true);
    onCleanup(() => {
      titlebarElement?.removeEventListener("mousedown", handleDown, true);
      titlebarElement?.removeEventListener("mousemove", handleMove, true);
      titlebarElement?.removeEventListener("dblclick", handleDoubleClick);
      window.removeEventListener("mouseup", handleUp, true);
    });
  });

  return (
    <header
      ref={titlebarElement}
      class="window-titlebar"
    >
      <div ref={setTitlebarSide} class="titlebar-side" classList={{ collapsed: sidebarLayout.collapsed }} />
      <div class="titlebar-main">
        <div ref={titlebarHost} class="titlebar-view" data-tauri-drag-region />
        <div ref={setTitlebarView} class="titlebar-staging" aria-hidden="true" inert />
        <div class="titlebar-window-controls">
          <button type="button" class="titlebar-window-btn" title="最小化" aria-label="最小化" onClick={() => void appWindow.minimize()}>
            <svg viewBox="0 0 12 12" width="11" height="11" aria-hidden="true"><path d="M1.5 6h9" /></svg>
          </button>
          <button
            type="button"
            class="titlebar-window-btn"
            title={maximized() ? "还原" : "最大化"}
            aria-label={maximized() ? "还原" : "最大化"}
            onClick={() => void toggleMaximize()}
          >
            <svg viewBox="0 0 12 12" width="11" height="11" aria-hidden="true">
              <Show when={maximized()} fallback={<rect x="1.5" y="1.5" width="9" height="9" />}>
                <path d="M3.5 3.5V1.5h7v7h-2M1.5 3.5h7v7h-7z" />
              </Show>
            </svg>
          </button>
          <button type="button" class="titlebar-window-btn close" title="关闭" aria-label="关闭" onClick={() => void appWindow.close()}>
            <IconX size={12} />
          </button>
        </div>
      </div>
    </header>
  );
}
