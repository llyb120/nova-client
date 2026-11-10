import { For, createSignal, onCleanup, onSettled, Show } from "solid-js";
import { api } from "../ipc";
import { showToast } from "../store";
import type { HeadlessBrowserView } from "../types";
import { IconBrowser, IconRefresh } from "./icons";
import "./WorkspaceBrowser.css";

const POLL_MS = 1000;
const MIN_ZOOM = 0.1;
const MAX_ZOOM = 8;

/** 侧栏「浏览器」页：本会话自己的标签 + 自动同步的截图，可拖动平移、滚轮缩放。 */
export default function WorkspaceBrowser(props: { threadId: string }) {
  const [view, setView] = createSignal<HeadlessBrowserView | null>(null);
  // 用户点选的标签；null = 跟随浏览器当前标签，agent 新开的页会直接显示。
  const [picked, setPicked] = createSignal<string | null>(null);
  const [error, setError] = createSignal("");
  const [zoom, setZoom] = createSignal(1);
  const [offset, setOffset] = createSignal({ x: 0, y: 0 });
  const [dragging, setDragging] = createSignal(false);
  const [opening, setOpening] = createSignal(false);
  let stage: HTMLDivElement | undefined;
  let picture: HTMLImageElement | undefined;
  // 已按宽度自适应过的画面（标签 + 像素尺寸）；之后保留用户自己的缩放。
  let fitted = "";
  let tabSignature = "";
  let timer = 0;
  let disposed = false;
  let busy = false;

  const tabs = () => view()?.tabs ?? [];
  const tabTag = () => view()?.tabTag ?? null;

  const poll = async () => {
    if (busy || disposed) return;
    busy = true;
    try {
      const next = await api.headlessBrowserView(props.threadId, picked());
      if (disposed) return;
      setView(next);
      setError(next.error ?? "");
      // 标签被关掉或标签集合变了（agent 开了新页）就回到跟随当前标签，画面不卡在旧页上。
      const list = next.tabs ?? [];
      const signature = list.map(tab => tab.tag).join(",");
      if (signature !== tabSignature || (picked() && !list.some(tab => tab.tag === picked()))) setPicked(null);
      tabSignature = signature;
    } catch (reason) {
      if (!disposed) setError(String(reason));
    } finally {
      busy = false;
      if (!disposed) timer = window.setTimeout(() => void poll(), POLL_MS);
    }
  };
  const refresh = () => {
    // 正在请求时不要清掉定时器：让它自然结束后自己安排下一帧，否则轮询会停。
    if (busy) return;
    window.clearTimeout(timer);
    void poll();
  };
  onSettled(() => void poll());
  onCleanup(() => {
    disposed = true;
    window.clearTimeout(timer);
  });

  const fit = () => {
    if (!picture || !stage) return;
    const key = `${tabTag()}:${picture.naturalWidth}x${picture.naturalHeight}`;
    if (fitted === key) return;
    fitted = key;
    const width = Math.max(1, stage.clientWidth - 8);
    setZoom(picture.naturalWidth > 0 ? Math.min(1, Math.max(MIN_ZOOM, width / picture.naturalWidth)) : 1);
    setOffset({ x: 0, y: 0 });
  };

  // 滚轮缩放以指针位置为锚点；wheel 必须非 passive 才能阻止面板一起滚动。
  const onWheel = (event: WheelEvent) => {
    if (!stage) return;
    event.preventDefault();
    const rect = stage.getBoundingClientRect();
    const px = event.clientX - rect.left;
    const py = event.clientY - rect.top;
    const before = zoom();
    const next = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, before * Math.exp(-event.deltaY / 320)));
    const ratio = next / before;
    const current = offset();
    setOffset({ x: px - (px - current.x) * ratio, y: py - (py - current.y) * ratio });
    setZoom(next);
  };
  onSettled(() => stage?.addEventListener("wheel", onWheel, { passive: false }));
  onCleanup(() => stage?.removeEventListener("wheel", onWheel));

  let drag: { sx: number; sy: number; ox: number; oy: number } | null = null;
  const onPointerDown = (event: PointerEvent) => {
    if (event.button !== 0) return;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    const current = offset();
    drag = { sx: event.clientX, sy: event.clientY, ox: current.x, oy: current.y };
    setDragging(true);
  };
  const onPointerMove = (event: PointerEvent) => {
    if (!drag) return;
    setOffset({ x: drag.ox + event.clientX - drag.sx, y: drag.oy + event.clientY - drag.sy });
  };
  const onPointerUp = (event: PointerEvent) => {
    if (!drag) return;
    (event.currentTarget as HTMLElement).releasePointerCapture?.(event.pointerId);
    drag = null;
    setDragging(false);
  };

  const select = (tag: string) => {
    if (tag === tabTag()) return;
    setPicked(tag);
    fitted = "";
    refresh();
  };

  const openInChrome = async () => {
    const tag = tabTag();
    if (!tag || opening()) return;
    setOpening(true);
    try {
      const result = await api.headlessBrowserOpen(tag);
      const detail = result.warnings.length
        ? result.warnings[0]
        : `同步 ${result.cookies} 个 cookie、${result.localStorage} 条 localStorage`;
      showToast(result.tabTag ? `已在 Chrome 打开：${detail}` : `真实浏览器没有打开新标签：${detail}`);
    } catch (reason) {
      showToast(`打开到真实浏览器失败：${reason}`);
    } finally {
      setOpening(false);
    }
  };

  return (
    <div class="browser-view">
      <div class="workspace-toolbar browser-tabbar">
        <Show when={tabs().length > 1}>
          <div class="browser-tab-list" role="tablist" aria-label="无头浏览器标签">
            <For each={tabs()}>
              {(tab) => (
                <button
                  role="tab"
                  aria-selected={tab.tag === tabTag() ? "true" : "false"}
                  class={{ "browser-tab": true, active: tab.tag === tabTag() }}
                  title={tab.url || tab.tag}
                  onClick={() => select(tab.tag)}
                >
                  {tab.title || tab.url || tab.tag}
                </button>
              )}
            </For>
          </div>
        </Show>
        <button
          aria-label="在真实浏览器打开"
          title="在真实 Chrome 打开并回写登录态"
          disabled={!tabTag() || opening()}
          onClick={() => void openInChrome()}
        >
          <IconBrowser size={14} />
        </button>
        <button aria-label="刷新画面" title="立即刷新画面" onClick={refresh}>
          <IconRefresh size={14} />
        </button>
      </div>
      <div
        ref={stage}
        class={["browser-stage", { "is-dragging": dragging() }]}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
      >
        <Show
          when={view()?.image}
          fallback={
            <div class="browser-hint">
              {view() === null
                ? "正在启动无头浏览器…"
                : error() || view()?.imageError || (tabs().length === 0 ? "本会话还没有打开过页面：让 AI 用 chrome_navigate 打开后这里会同步画面" : "等待画面…")}
            </div>
          }
        >
          {(image) => (
            <img
              ref={picture}
              src={image()}
              alt={view()?.title ?? "无头浏览器"}
              draggable={false}
              style={{ transform: `translate(${offset().x}px, ${offset().y}px) scale(${zoom()})` }}
              onLoad={fit}
            />
          )}
        </Show>
      </div>
      <div class="browser-status" title={view()?.notice ?? ""}>
        {view()?.notice
          ?? (view()?.running === false
            ? `后台未就绪：${error() || "重试中"}`
            : `${view()?.url ?? ""}${view()?.loading ? " · 加载中" : ""}`)}
      </div>
    </div>
  );
}
