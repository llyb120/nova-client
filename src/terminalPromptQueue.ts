import { createEffect, createRoot, createSignal, untrack } from "solid-js";
import { formatTerminalPromptWithPaths } from "./agyTerminalPrompt";
import { api } from "./ipc";
import { state } from "./store";
import { getTerminalGroup, sendTerminalInput } from "./terminalSessions";
import type { PromptImage } from "./types";

export type TerminalQueuedPrompt = {
  id: string;
  threadId: string;
  text: string;
  ts: number;
  images: PromptImage[];
};

export type TerminalQueueDispatcher = (
  item: TerminalQueuedPrompt,
  steerNow: boolean,
) => Promise<boolean>;

const [terminalQueuedPrompts, setTerminalQueuedPrompts] = createSignal<TerminalQueuedPrompt[]>([]);
const [terminalQueueDispatchingIds, setTerminalQueueDispatchingIds] = createSignal<Set<string>>(new Set());
const [terminalQueueFailedIds, setTerminalQueueFailedIds] = createSignal<Set<string>>(new Set());
/** 用户主动停止后挂起自动投递，队列保留供手动发送、引导或撤回。 */
const [terminalQueueHeldThreadIds, setTerminalQueueHeldThreadIds] = createSignal<Set<string>>(new Set());
/** 前台视图刚乐观发送或正在运行中的线程集合，防止后端running未同步前连发下一条。 */
const [terminalQueueBusyThreads, setTerminalQueueBusyThreads] = createSignal<Set<string>>(new Set());

export {
  terminalQueuedPrompts,
  terminalQueueDispatchingIds,
  terminalQueueFailedIds,
  terminalQueueHeldThreadIds,
};

const viewDispatchers = new Map<string, TerminalQueueDispatcher>();
const inFlight = new Set<string>();

export function registerTerminalQueueDispatcher(
  threadId: string,
  dispatcher: TerminalQueueDispatcher,
): () => void {
  viewDispatchers.set(threadId, dispatcher);
  return () => {
    if (viewDispatchers.get(threadId) === dispatcher) {
      viewDispatchers.delete(threadId);
    }
  };
}

export function setTerminalQueueViewBusy(threadId: string | null | undefined, busy: boolean): void {
  if (!threadId) return;
  setTerminalQueueBusyThreads((ids) => {
    const has = ids.has(threadId);
    if (has === busy) return ids;
    const next = new Set(ids);
    if (busy) next.add(threadId);
    else next.delete(threadId);
    return next;
  });
}

export function holdTerminalPromptQueue(threadId: string | null | undefined): void {
  if (!threadId) return;
  setTerminalQueueHeldThreadIds((ids) => {
    if (ids.has(threadId)) return ids;
    const next = new Set(ids);
    next.add(threadId);
    return next;
  });
}

export function releaseTerminalPromptQueue(threadId: string | null | undefined): void {
  if (!threadId) return;
  setTerminalQueueHeldThreadIds((ids) => {
    if (!ids.has(threadId)) return ids;
    const next = new Set(ids);
    next.delete(threadId);
    return next;
  });
}

export function enqueueTerminalPrompt(
  threadId: string,
  text: string,
  images: PromptImage[],
  id?: string,
): void {
  const now = Date.now();
  const itemId = id?.trim() || `${threadId}:term-queued:${now}:${Math.random().toString(36).slice(2, 8)}`;
  setTerminalQueuedPrompts((items) => {
    if (items.some((queued) => queued.id === itemId)) return items;
    return [
      ...items,
      {
        id: itemId,
        threadId,
        text,
        ts: now,
        images: images.map((image) => ({ ...image })),
      },
    ];
  });
  void api.setPromptQueuePending(threadId, true);
}

export function removeTerminalQueuedPrompt(itemId: string): TerminalQueuedPrompt | null {
  let removed: TerminalQueuedPrompt | null = null;
  let releaseThreadId: string | null = null;
  setTerminalQueuedPrompts((items) => {
    const next = items.filter((queued) => {
      if (queued.id !== itemId) return true;
      removed = queued;
      return false;
    });
    if (removed) {
      const pending = next.some((queued) => queued.threadId === removed!.threadId);
      void api.setPromptQueuePending(removed.threadId, pending);
      if (!pending) releaseThreadId = removed.threadId;
    }
    return next;
  });
  if (releaseThreadId) releaseTerminalPromptQueue(releaseThreadId);
  setTerminalQueueFailedIds((ids) => {
    if (!ids.has(itemId)) return ids;
    const next = new Set(ids);
    next.delete(itemId);
    return next;
  });
  return removed;
}

export function clearTerminalPromptQueue(threadId: string): void {
  const removedIds: string[] = [];
  setTerminalQueuedPrompts((items) => {
    const next = items.filter((item) => {
      if (item.threadId !== threadId) return true;
      removedIds.push(item.id);
      return false;
    });
    return next;
  });
  if (removedIds.length > 0) {
    void api.setPromptQueuePending(threadId, false);
    setTerminalQueueFailedIds((ids) => {
      let changed = false;
      const next = new Set(ids);
      for (const id of removedIds) {
        if (next.delete(id)) changed = true;
      }
      return changed ? next : ids;
    });
  }
  releaseTerminalPromptQueue(threadId);
}

function sameImageSet(a: PromptImage[], b: PromptImage[]): boolean {
  if (a.length !== b.length) return false;
  return a.every((image, index) => {
    const other = b[index];
    return (
      image.name === other.name &&
      image.mimeType === other.mimeType &&
      image.size === other.size
    );
  });
}

/** 撤下与即将手动发送内容重复的终端排队条目，避免空闲发送结束后再重复投递一次。 */
export function dropTerminalQueuedPromptsMatching(
  threadId: string,
  text: string,
  images: PromptImage[],
): void {
  const target = text.trim();
  if (!target && images.length === 0) return;
  for (const queued of untrack(terminalQueuedPrompts)) {
    if (queued.threadId !== threadId) continue;
    if (queued.text.trim() !== target) continue;
    if (!sameImageSet(queued.images, images)) continue;
    removeTerminalQueuedPrompt(queued.id);
  }
}

async function fallbackDispatchTerminalItem(
  item: TerminalQueuedPrompt,
  steerNow: boolean,
): Promise<boolean> {
  const tab = untrack(() => getTerminalGroup(`thread:${item.threadId}`).tabs()[0]);
  if (!tab || tab.disposed || tab.status() !== "running") return false;
  const paths = item.images.length > 0
    ? await api.saveTerminalAttachments(item.images.map((img) => ({ ...img })))
    : [];
  if (paths.length !== item.images.length) {
    throw new Error("部分附件未能写入终端草稿");
  }
  const payload = formatTerminalPromptWithPaths(item.text, paths);
  if (!payload) return false;
  const isDevin = untrack(
    () => state.threads.find((t) => t.id === item.threadId)?.terminalConfig?.mode === "devin",
  );
  const submitMode: boolean | "steer" =
    steerNow && !isDevin && untrack(() => !!state.running[item.threadId]) ? "steer" : true;
  return sendTerminalInput(tab, payload, { submit: submitMode, useTerminalPaste: false });
}

export async function dispatchTerminalQueuedPrompt(
  item: TerminalQueuedPrompt,
  steerNow = false,
): Promise<boolean> {
  if (inFlight.has(item.id)) return false;
  inFlight.add(item.id);
  releaseTerminalPromptQueue(item.threadId);
  setTerminalQueueFailedIds((ids) => {
    if (!ids.has(item.id)) return ids;
    const next = new Set(ids);
    next.delete(item.id);
    return next;
  });
  setTerminalQueueDispatchingIds((ids) => new Set(ids).add(item.id));
  try {
    const hasMore = untrack(terminalQueuedPrompts).some(
      (queued) => queued.threadId === item.threadId && queued.id !== item.id,
    );
    void api.setPromptQueuePending(item.threadId, hasMore);
    const dispatcher = viewDispatchers.get(item.threadId);
    const ok = dispatcher
      ? await dispatcher(item, steerNow)
      : await fallbackDispatchTerminalItem(item, steerNow);
    if (!ok) {
      setTerminalQueueFailedIds((ids) => new Set(ids).add(item.id));
      return false;
    }
    setTerminalQueuedPrompts((items) => items.filter((queued) => queued.id !== item.id));
    return true;
  } catch (error) {
    console.error("发送终端排队提示词失败", error);
    setTerminalQueueFailedIds((ids) => new Set(ids).add(item.id));
    return false;
  } finally {
    inFlight.delete(item.id);
    setTerminalQueueDispatchingIds((ids) => {
      const next = new Set(ids);
      next.delete(item.id);
      return next;
    });
  }
}

/** 任意终端 CLI 会话回合结束后按 FIFO 自动投递；支持后台运行中的终端标签。 */
function startTerminalPromptQueueDispatcher() {
  createRoot(() => {
    createEffect(() => {
      const items = terminalQueuedPrompts();
      const held = terminalQueueHeldThreadIds();
      const dispatching = terminalQueueDispatchingIds();
      const failed = terminalQueueFailedIds();
      const busyThreads = terminalQueueBusyThreads();
      const seenThreads = new Set<string>();
      const ready: TerminalQueuedPrompt[] = [];

      for (const item of items) {
        if (seenThreads.has(item.threadId)) continue;
        if (held.has(item.threadId) || state.running[item.threadId] || busyThreads.has(item.threadId)) {
          seenThreads.add(item.threadId);
          continue;
        }
        if (dispatching.has(item.id)) {
          seenThreads.add(item.threadId);
          continue;
        }
        if (failed.has(item.id)) continue;
        const tab = getTerminalGroup(`thread:${item.threadId}`).tabs()[0];
        if (!tab || tab.disposed || tab.status() !== "running") {
          seenThreads.add(item.threadId);
          continue;
        }
        seenThreads.add(item.threadId);
        ready.push(item);
      }
      return ready;
    }, (ready) => {
      untrack(() => {
        for (const item of ready) void dispatchTerminalQueuedPrompt(item, false);
      });
    });
  });
}

startTerminalPromptQueueDispatcher();
