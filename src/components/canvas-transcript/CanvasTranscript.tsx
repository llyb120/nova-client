import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { CanvasDOM, hitTest, type Node } from "../../lib/canvas-dom";
import { api } from "../../ipc";
import { editUserMessage, state, toggleExpanded } from "../../store";
import type { PermissionRequest, UserItem } from "../../types";
import { agentLabel } from "../../utils";
import { createImageAttachments, ImageAttachmentStrip } from "../ImageAttachmentStrip";
import { PermissionCard } from "../PermissionCard";
import type { Group } from "../TurnGroup";
import {
  buildTranscriptNodes,
  parseCanvasAction,
  updateHeightCache,
} from "./buildNodes";
import { readTranscriptTheme } from "./theme";

export interface CanvasTranscriptHandle {
  scrollToGroup: (index: number) => void;
  scrollTop: () => number;
  scrollHeight: () => number;
  clientHeight: () => number;
  pinBottom: () => void;
  mountGroup: (index: number) => void;
  getGroupOffset: (index: number) => number | null;
}

export interface CanvasTranscriptProps {
  groups: Group[];
  isRunning: boolean;
  permissions: PermissionRequest[];
  previewBanner?: boolean;
  onReturnToTimeline?: () => void;
  emptyHintCwd?: string;
  loadingThread?: boolean;
  stickToBottom: () => boolean;
  setStickToBottom: (v: boolean) => void;
  onScroll?: (scrollTop: number) => void;
  ref?: (handle: CanvasTranscriptHandle | undefined) => void;
}

function actionFromPayload(raw: unknown): string {
  if (typeof raw === "string") return raw;
  if (raw && typeof raw === "object") {
    const obj = raw as { action?: unknown; node?: Node; meta?: { onClick?: unknown } };
    if (typeof obj.action === "string") return obj.action;
    let n: Node | null | undefined = obj.node ?? (obj.meta ? (raw as Node) : null);
    while (n) {
      const oc = n.meta?.onClick;
      if (typeof oc === "string") return oc;
      n = n.parent;
    }
  }
  return "";
}

export function CanvasTranscript(props: CanvasTranscriptProps): JSX.Element {
  let host: HTMLDivElement | undefined;
  let canvasEl: HTMLCanvasElement | undefined;
  let renderer: CanvasDOM | undefined;
  const heightCache = new Map<number, number>();
  const forceMount = new Set<number>();
  let lastScrollTop = 0;
  let lastVirtualRebuildTop = Number.NaN;
  let pointerActive = false;
  let scrollFrame = 0;
  let rebuildQueued = false;
  let themeObserver: MutationObserver | undefined;

  const [editingUser, setEditingUser] = createSignal<UserItem | null>(null);
  const [editDraft, setEditDraft] = createSignal("");
  const [editRect, setEditRect] = createSignal<{
    top: number;
    left: number;
    width: number;
  } | null>(null);
  const attach = createImageAttachments();

  const rebuildNow = (keepForce = false) => {
    if (!renderer || !canvasEl) return;
    if (!keepForce && forceMount.size > 0) {
      const st = renderer.scrollTop;
      const vh = renderer.height || 1;
      for (const idx of [...forceMount]) {
        let y = 0;
        for (let i = 0; i < idx; i++) y += heightCache.get(i) ?? 160;
        const h = heightCache.get(idx) ?? 160;
        if (y + h < st - vh * 3 || y > st + vh * 4) forceMount.delete(idx);
      }
    }

    const theme = readTranscriptTheme();
    renderer.setMarkdownTheme(theme.markdown);
    const content = buildTranscriptNodes(props.groups, {
      theme,
      isRunning: props.isRunning,
      activeGroupIndex: props.groups.findIndex((g) => props.isRunning && !g.turn),
      lastGroupIndex: props.groups.length - 1,
      scrollTop: renderer.scrollTop,
      viewportHeight: renderer.height || host?.clientHeight || 600,
      heightCache,
      forceMount,
    });
    renderer.replaceContent(content);
    renderer.forceLayout();
    if (renderer.root) updateHeightCache(renderer.root, heightCache);
    const max = Math.max(0, renderer.scrollHeight - renderer.height);
    if (renderer.scrollTop > max) renderer.scrollTo(max);
  };

  const scheduleRebuild = () => {
    if (rebuildQueued) return;
    rebuildQueued = true;
    queueMicrotask(() => {
      rebuildQueued = false;
      rebuildNow();
      if (props.stickToBottom()) handle.pinBottom();
    });
  };

  const handle: CanvasTranscriptHandle = {
    scrollToGroup(index: number) {
      forceMount.add(index);
      rebuildNow(true);
      renderer?.forceLayout();
      const y = renderer?.getGroupOffset(index);
      if (y == null || !renderer) return;
      props.setStickToBottom(false);
      renderer.scrollTo(Math.max(0, y - 20));
      props.onScroll?.(renderer.scrollTop);
    },
    scrollTop: () => renderer?.scrollTop ?? 0,
    scrollHeight: () => renderer?.scrollHeight ?? 0,
    clientHeight: () => renderer?.height ?? 0,
    pinBottom() {
      if (!renderer || !props.stickToBottom() || pointerActive) return;
      const max = Math.max(0, renderer.scrollHeight - renderer.height);
      renderer.scrollTo(max);
      lastScrollTop = renderer.scrollTop;
    },
    mountGroup(index: number) {
      forceMount.add(index);
      scheduleRebuild();
    },
    getGroupOffset(index: number) {
      return renderer?.getGroupOffset(index) ?? null;
    },
  };

  const openFile = (path: string, line?: number) => {
    const id = state.currentId;
    if (!id || !path) return;
    void api.openInEditor(id, path, line).catch((e) => void message(String(e), { kind: "error" }));
  };

  const processScroll = () => {
    if (!renderer) return;
    const currentTop = renderer.scrollTop;
    const max = Math.max(0, renderer.scrollHeight - renderer.height);
    const atBottom = max - currentTop <= 1;
    if (props.stickToBottom()) {
      if (pointerActive && !atBottom && currentTop !== lastScrollTop) {
        props.setStickToBottom(false);
      }
    } else if (atBottom && currentTop > lastScrollTop) {
      props.setStickToBottom(true);
    }
    lastScrollTop = currentTop;
    props.onScroll?.(currentTop);
    // Remount far placeholders only after meaningful scroll (same idea as old VirtualGroup).
    const vh = renderer.height || 1;
    if (
      !Number.isFinite(lastVirtualRebuildTop) ||
      Math.abs(currentTop - lastVirtualRebuildTop) >= vh / 3
    ) {
      lastVirtualRebuildTop = currentTop;
      scheduleRebuild();
    }
  };

  const onWheelCancelStick = (event: WheelEvent) => {
    if (!renderer?.root || !canvasEl) return;
    const rect = canvasEl.getBoundingClientRect();
    const hit = hitTest(renderer.root, event.clientX - rect.left, event.clientY - rect.top);
    let node: Node | null = hit;
    while (node && node !== renderer.root) {
      const overflow = node.style.overflow;
      if (
        node.meta?.kind === "tool-output" ||
        node.meta?.nestedScroll ||
        ((overflow === "scroll" || overflow === "auto") &&
          (node._maxScrollY > 0 || node._maxScrollX > 0))
      ) {
        // Nested tool-detail scroll must not cancel outer stick-to-bottom.
        return;
      }
      node = node.parent;
    }
    if (renderer.scrollHeight <= renderer.height + 1) return;
    const max = Math.max(0, renderer.scrollHeight - renderer.height);
    const atBottom = max - renderer.scrollTop <= 1;
    if (event.deltaY > 0 && atBottom) {
      if (!props.stickToBottom()) props.setStickToBottom(true);
      return;
    }
    if (event.deltaY !== 0) props.setStickToBottom(false);
  };

  const handleAction = (raw: unknown) => {
    const action = parseCanvasAction(actionFromPayload(raw));
    if (!action) return;
    switch (action.type) {
      case "toggle-fold":
      case "toggle-thought":
      case "toggle-tool":
      case "toggle-tool-raw":
        toggleExpanded(action.key);
        scheduleRebuild();
        break;
      case "edit-user": {
        const item = props.groups.map((g) => g.user).find((u) => u && u.id === action.itemId);
        if (!item) return;
        const node = renderer?.findNode(
          (n) => n.meta?.kind === "user" && n.meta.itemId === action.itemId,
        );
        const rect = node && renderer ? renderer.getNodeScreenRect(node) : null;
        const hostRect = host?.getBoundingClientRect();
        setEditDraft(item.text);
        attach.set(item.images ?? []);
        setEditRect(
          rect && hostRect
            ? {
                top: Math.max(8, rect.y),
                left: Math.max(8, rect.x),
                width: Math.min(rect.w, (hostRect.width || 480) - 16),
              }
            : { top: 24, left: 24, width: Math.min(560, (host?.clientWidth || 600) - 48) },
        );
        setEditingUser(item);
        break;
      }
      case "open-file":
        openFile(action.path, action.line);
        break;
      case "copy-text":
        void navigator.clipboard.writeText(action.text);
        break;
    }
  };

  onMount(() => {
    if (!canvasEl || !host) return;
    const padX = Math.min(28, Math.max(14, host.clientWidth * 0.03));
    renderer = new CanvasDOM(canvasEl, {
      maxWidth: 980,
      columnPadX: padX,
      onScroll: () => {
        if (scrollFrame) return;
        scrollFrame = requestAnimationFrame(() => {
          scrollFrame = 0;
          processScroll();
        });
      },
    });
    renderer.on("action", handleAction);
    renderer.on("click", handleAction);

    const ro = new ResizeObserver(() => {
      if (!renderer || !host) return;
      renderer.resize(host.clientWidth, host.clientHeight);
      scheduleRebuild();
    });
    ro.observe(host);
    renderer.resize(host.clientWidth, host.clientHeight);
    rebuildNow(true);

    themeObserver = new MutationObserver(() => scheduleRebuild());
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme", "class", "style"],
    });

    const onPointerUp = () => {
      pointerActive = false;
      if (props.stickToBottom()) handle.pinBottom();
    };
    const onKey = (event: KeyboardEvent) => {
      const scrollUpKeys = new Set(["ArrowUp", "PageUp", "Home"]);
      const scrollDownKeys = new Set(["ArrowDown", "PageDown", "End"]);
      const scrollsUp = scrollUpKeys.has(event.key) || (event.key === " " && event.shiftKey);
      const scrollsDown = scrollDownKeys.has(event.key) || (event.key === " " && !event.shiftKey);
      if (event.altKey || event.ctrlKey || event.metaKey || (!scrollsUp && !scrollsDown)) return;
      if (!renderer || renderer.scrollHeight <= renderer.height + 1) return;
      const target = event.target;
      if (
        target instanceof HTMLElement &&
        (target.isContentEditable ||
          target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.tagName === "SELECT")
      ) {
        return;
      }
      if (scrollsDown) {
        const max = Math.max(0, renderer.scrollHeight - renderer.height);
        if (max - renderer.scrollTop <= 1) {
          if (!props.stickToBottom()) props.setStickToBottom(true);
          return;
        }
      }
      props.setStickToBottom(false);
    };

    window.addEventListener("pointerup", onPointerUp, true);
    window.addEventListener("pointercancel", onPointerUp, true);
    window.addEventListener("keydown", onKey, true);
    props.ref?.(handle);

    onCleanup(() => {
      props.ref?.(undefined);
      ro.disconnect();
      themeObserver?.disconnect();
      if (scrollFrame) cancelAnimationFrame(scrollFrame);
      window.removeEventListener("pointerup", onPointerUp, true);
      window.removeEventListener("pointercancel", onPointerUp, true);
      window.removeEventListener("keydown", onKey, true);
      renderer?.destroy();
      renderer = undefined;
    });
  });

  createEffect(() => {
    void props.groups;
    void props.isRunning;
    void state.expanded;
    for (const g of props.groups) {
      for (const item of g.body) {
        if ("text" in item) void (item as { text: string }).text.length;
        if (item.type === "tool") {
          void item.content.length;
          void item.status;
        }
      }
      if (g.user) void g.user.text.length;
    }
    scheduleRebuild();
  });

  createEffect((prev: string | null | undefined) => {
    const id = state.currentId;
    if (id !== prev) {
      heightCache.clear();
      forceMount.clear();
      setEditingUser(null);
    }
    return id;
  }, undefined);

  const saveEdit = () => {
    const item = editingUser();
    if (!item) return;
    const text = editDraft().trim();
    const images = attach.images();
    if (!text && images.length === 0) return;
    setEditingUser(null);
    void editUserMessage(item.id, text, images);
  };

  return (
    <div
      class="transcript transcript-canvas"
      ref={host}
      onPointerDown={() => {
        pointerActive = true;
      }}
      onWheel={onWheelCancelStick}
    >
      <canvas class="transcript-canvas-el" ref={canvasEl} tabindex={0} />
      <div class="transcript-canvas-overlay">
        <Show when={props.previewBanner}>
          <button
            type="button"
            class="checkpoint-preview-banner"
            title="回到当前时间线和最新消息"
            onClick={() => props.onReturnToTimeline?.()}
          >
            回到当前时间线
          </button>
        </Show>
        <Show when={props.groups.length === 0 && !props.loadingThread && !editingUser()}>
          <div class="transcript-hint">
            在下方输入任务，{agentLabel(state.agentKind)} 将在{" "}
            <code>{props.emptyHintCwd || state.cwd}</code> 中工作。
          </div>
        </Show>
        <Show when={editingUser()}>
          <div
            class="user-edit canvas-user-edit"
            style={{
              top: `${editRect()?.top ?? 24}px`,
              left: `${editRect()?.left ?? 24}px`,
              width: `${editRect()?.width ?? 480}px`,
            }}
          >
            <ImageAttachmentStrip images={attach.images()} onRemove={attach.remove} />
            <textarea
              class="user-edit-input"
              value={editDraft()}
              rows={Math.min(10, Math.max(2, editDraft().split("\n").length))}
              onPaste={attach.onPaste}
              onInput={(e) => setEditDraft(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
                  e.preventDefault();
                  saveEdit();
                }
                if (e.key === "Escape") setEditingUser(null);
              }}
              ref={(el) =>
                queueMicrotask(() => {
                  el.focus();
                  el.setSelectionRange(el.value.length, el.value.length);
                })
              }
            />
            <div class="user-edit-actions">
              <span class="user-edit-hint">发送后将从此处重新开始会话</span>
              <button class="btn secondary small" onClick={() => setEditingUser(null)}>
                取消
              </button>
              <button class="btn primary small" onClick={saveEdit}>
                发送
              </button>
            </div>
          </div>
        </Show>
        <div class="transcript-canvas-permissions">
          <For each={props.permissions}>{(req) => <PermissionCard req={req} />}</For>
        </div>
      </div>
    </div>
  );
}
