import { createEffect, createRoot, createSignal } from "solid-js";
import { api } from "./ipc";
import { sendPrompt, sendPromptTo, state } from "./store";
import type { PromptImage } from "./types";

export type QueuedPrompt = {
  id: string;
  threadId: string;
  text: string;
  ts: number;
  images: PromptImage[];
};

const [queuedPrompts, setQueuedPrompts] = createSignal<QueuedPrompt[]>([]);
const [dispatchingQueueIds, setDispatchingQueueIds] = createSignal<Set<string>>(new Set());
const [failedQueueIds, setFailedQueueIds] = createSignal<Set<string>>(new Set());
/** 用户主动停止后挂起自动投递，队列保留供手动发送或撤回。 */
const [queueHeldThreadIds, setQueueHeldThreadIds] = createSignal<Set<string>>(new Set());

export { queuedPrompts, dispatchingQueueIds, failedQueueIds, queueHeldThreadIds };

export function holdPromptQueue(threadId: string | null | undefined) {
  if (!threadId) return;
  setQueueHeldThreadIds((ids) => {
    if (ids.has(threadId)) return ids;
    const next = new Set(ids);
    next.add(threadId);
    return next;
  });
}

export function releasePromptQueue(threadId: string | null | undefined) {
  if (!threadId) return;
  setQueueHeldThreadIds((ids) => {
    if (!ids.has(threadId)) return ids;
    const next = new Set(ids);
    next.delete(threadId);
    return next;
  });
}

export function enqueuePrompt(threadId: string, text: string, images: PromptImage[]) {
  const now = Date.now();
  setQueuedPrompts((items) => [
    ...items,
    {
      id: `${threadId}:queued:${now}:${items.length}`,
      threadId,
      text,
      ts: now,
      images: images.map((image) => ({ ...image })),
    },
  ]);
  void api.setPromptQueuePending(threadId, true);
}

export function removeQueuedPrompt(itemId: string): QueuedPrompt | null {
  let removed: QueuedPrompt | null = null;
  setQueuedPrompts((items) => {
    const next = items.filter((queued) => {
      if (queued.id !== itemId) return true;
      removed = queued;
      return false;
    });
    if (removed) {
      const pending = next.some((queued) => queued.threadId === removed!.threadId);
      void api.setPromptQueuePending(removed.threadId, pending);
      if (!pending) releasePromptQueue(removed.threadId);
    }
    return next;
  });
  setFailedQueueIds((ids) => {
    if (!ids.has(itemId)) return ids;
    const next = new Set(ids);
    next.delete(itemId);
    return next;
  });
  return removed;
}

export async function dispatchQueuedPrompt(item: QueuedPrompt, steerNow = false) {
  if (dispatchingQueueIds().has(item.id)) return;
  // 用户主动发送或恢复队列后，允许后续条目在回合结束后继续自动投递。
  releasePromptQueue(item.threadId);
  setFailedQueueIds((ids) => {
    if (!ids.has(item.id)) return ids;
    const next = new Set(ids);
    next.delete(item.id);
    return next;
  });
  setDispatchingQueueIds((ids) => new Set(ids).add(item.id));
  try {
    const hasMore = queuedPrompts().some(
      (queued) => queued.threadId === item.threadId && queued.id !== item.id,
    );
    // 必须先更新后端标记再发下一轮，避免最后一轮仍被当作队列中间轮次。
    await api.setPromptQueuePending(item.threadId, hasMore);
    if (steerNow && state.currentId === item.threadId) {
      await sendPrompt(item.text, item.images);
    } else {
      await sendPromptTo(item.threadId, item.text, item.images);
    }
    setQueuedPrompts((items) => items.filter((queued) => queued.id !== item.id));
  } catch (error) {
    console.error("发送排队提示词失败", error);
    setFailedQueueIds((ids) => new Set(ids).add(item.id));
  } finally {
    setDispatchingQueueIds((ids) => {
      const next = new Set(ids);
      next.delete(item.id);
      return next;
    });
  }
}

/** 任意会话回合结束后按 FIFO 自动投递；不依赖 Composer 是否挂载或是否为前台会话。 */
function startPromptQueueDispatcher() {
  createRoot(() => {
    createEffect(() => {
      const items = queuedPrompts();
      const held = queueHeldThreadIds();
      const dispatching = dispatchingQueueIds();
      const failed = failedQueueIds();
      const seenThreads = new Set<string>();

      for (const item of items) {
        if (seenThreads.has(item.threadId)) continue;
        seenThreads.add(item.threadId);
        if (held.has(item.threadId)) continue;
        if (state.running[item.threadId]) continue;
        if (dispatching.has(item.id) || failed.has(item.id)) continue;
        void dispatchQueuedPrompt(item);
      }
    });
  });
}

startPromptQueueDispatcher();
