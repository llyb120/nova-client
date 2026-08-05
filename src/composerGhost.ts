import { createSignal, onCleanup } from "solid-js";
import { api } from "./ipc";

const GHOST_IDLE_MS = 200;
const GHOST_MIN_INTERVAL_MS = 500;

export type ComposerGhostOptions = {
  getText: () => string;
  setText: (value: string) => void;
  getTextarea: () => HTMLTextAreaElement | undefined;
  /** 斜杠菜单 / 历史菜单打开时跳过请求 */
  isBlocked?: () => boolean;
  getThreadId?: () => string | null;
  onAfterAccept?: () => void;
};

/** 输入框行内幽灵补全（ChatView Composer 与 HomeView 新会话共用）。 */
export function createComposerGhost(opts: ComposerGhostOptions) {
  const [ghost, setGhost] = createSignal("");
  let ghostRef: HTMLDivElement | undefined;
  let ghostTimer: number | undefined;
  let ghostReqSeq = 0;
  let ghostLastFired = 0;
  let ghostBusy = false;
  // 在飞/限流时若草稿又变了，结束后再拉一次，避免慢请求把后续触发吃掉
  let ghostPending = false;
  let ghostCache: { draft: string; completion: string } | undefined;
  let composing = false;

  const clearGhost = () => {
    if (ghost()) setGhost("");
  };

  const syncGhostScroll = () => {
    const el = opts.getTextarea();
    if (ghostRef && el) ghostRef.scrollTop = el.scrollTop;
  };

  const scheduleGhost = (delay = GHOST_IDLE_MS) => {
    if (ghostTimer !== undefined) window.clearTimeout(ghostTimer);
    ghostTimer = window.setTimeout(() => {
      ghostTimer = undefined;
      void requestGhost();
    }, delay);
  };

  const requestGhost = async () => {
    const el = opts.getTextarea();
    if (!el || composing) return;
    if (opts.isBlocked?.()) return;
    if (document.activeElement !== el) return;
    const draft = opts.getText();
    if (!draft.trim()) return;
    if ((el.selectionEnd ?? draft.length) !== draft.length) return;
    // 只缓存非空结果；空串可能是在飞冲突/静默失败，不能锁死同一草稿
    if (ghostCache && ghostCache.draft === draft && ghostCache.completion) {
      setGhost(ghostCache.completion);
      return;
    }
    if (ghostBusy) {
      ghostPending = true;
      return;
    }
    const now = Date.now();
    const wait = GHOST_MIN_INTERVAL_MS - (now - ghostLastFired);
    if (wait > 0) {
      ghostPending = true;
      scheduleGhost(wait);
      return;
    }
    ghostPending = false;
    ghostLastFired = now;
    ghostBusy = true;
    const reqId = ++ghostReqSeq;
    try {
      const completion = await api.completeComposerDraft(opts.getThreadId?.() ?? null, draft);
      if (reqId !== ghostReqSeq || opts.getText() !== draft || document.activeElement !== el) {
        if (opts.getText().trim() && opts.getText() !== draft) ghostPending = true;
        return;
      }
      const value = (completion ?? "").trim();
      if (value) {
        ghostCache = { draft, completion: value };
        setGhost(value);
      } else {
        clearGhost();
      }
    } catch {
      if (reqId === ghostReqSeq) ghostCache = undefined;
    } finally {
      ghostBusy = false;
      if (ghostPending) {
        ghostPending = false;
        if (opts.getText().trim()) scheduleGhost();
      }
    }
  };

  const acceptGhost = () => {
    const completion = ghost();
    if (!completion) return;
    const next = opts.getText() + completion;
    setGhost("");
    ghostCache = { draft: next, completion: "" };
    opts.setText(next);
    queueMicrotask(() => {
      const el = opts.getTextarea();
      if (!el) return;
      el.focus();
      el.setSelectionRange(next.length, next.length);
      opts.onAfterAccept?.();
    });
  };

  const dismissGhostIfCaretMoved = (el: HTMLTextAreaElement) => {
    if (ghost() && (el.selectionEnd ?? 0) !== opts.getText().length) clearGhost();
  };

  /** @returns true 表示已消费该按键 */
  const handleKeyDown = (e: KeyboardEvent): boolean => {
    if (!ghost()) return false;
    if (e.key === "Tab") {
      e.preventDefault();
      acceptGhost();
      return true;
    }
    if (e.key === "ArrowRight" && (opts.getTextarea()?.selectionEnd ?? 0) >= opts.getText().length) {
      e.preventDefault();
      acceptGhost();
      return true;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      clearGhost();
      return true;
    }
    return false;
  };

  const noteInput = () => {
    clearGhost();
    if (opts.getText().trim()) scheduleGhost();
  };

  onCleanup(() => {
    if (ghostTimer !== undefined) window.clearTimeout(ghostTimer);
  });

  return {
    ghost,
    setGhostRef: (el: HTMLDivElement | undefined) => {
      ghostRef = el;
    },
    clearGhost,
    scheduleGhost,
    acceptGhost,
    dismissGhostIfCaretMoved,
    syncGhostScroll,
    handleKeyDown,
    noteInput,
    onBlur: clearGhost,
    onCompositionStart: () => {
      composing = true;
      clearGhost();
    },
    onCompositionEnd: () => {
      composing = false;
      if (opts.getText().trim()) scheduleGhost();
    },
  };
}
