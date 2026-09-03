import { listen } from "@tauri-apps/api/event";
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

export function enqueuePrompt(
  threadId: string,
  text: string,
  images: PromptImage[],
  id?: string,
) {
  const now = Date.now();
  const itemId = id?.trim() || `${threadId}:queued:${now}:${Math.random().toString(36).slice(2, 8)}`;
  setQueuedPrompts((items) => {
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

export function removeQueuedPrompt(itemId: string): QueuedPrompt | null {
  let removed: QueuedPrompt | null = null;
  let releaseThreadId: string | null = null;
  setQueuedPrompts((items) => {
    const next = items.filter((queued) => {
      if (queued.id !== itemId) return true;
      removed = queued;
      return false;
    });
    if (removed) {
      const pending = next.some((queued) => queued.threadId === removed!.threadId);
      void api.setPromptQueuePending(removed.threadId, pending);
      // 必须在 setQueuedPrompts 提交后再 release：更新器内改 hold 会触发
      // dispatcher，此时 queuedPrompts 仍是旧值，撤回会被误当成发送。
      // 在更新器内取出 threadId：TS6 在回调外会把闭包赋值的 removed 收窄成 never。
      if (!pending) releaseThreadId = removed.threadId;
    }
    return next;
  });
  if (releaseThreadId) releasePromptQueue(releaseThreadId);
  setFailedQueueIds((ids) => {
    if (!ids.has(itemId)) return ids;
    const next = new Set(ids);
    next.delete(itemId);
    return next;
  });
  return removed;
}

/** 附件集合比较：重发时的图片是克隆对象，按名称/类型/大小判定是否同一批。 */
function sameImageSet(a: PromptImage[], b: PromptImage[]) {
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

/**
 * 撤下与即将手动发送内容重复的排队条目。
 * 用户停止后重新输入同一条提示词（例如从历史里取回刚排队的那条）时，队列里往往
 * 还留着停止前排入的同一份内容；放任它在手动那轮结束后自动投递，用户就会看到
 * 两条完全相同的消息。
 */
export function dropQueuedPromptsMatching(
  threadId: string,
  text: string,
  images: PromptImage[],
) {
  const target = text.trim();
  if (!target && images.length === 0) return;
  for (const queued of queuedPrompts()) {
    if (queued.threadId !== threadId) continue;
    if (queued.text.trim() !== target) continue;
    if (!sameImageSet(queued.images, images)) continue;
    removeQueuedPrompt(queued.id);
  }
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
        // 挂起/运行中：本会话后面的条目都先别动。
        if (held.has(item.threadId) || state.running[item.threadId]) {
          seenThreads.add(item.threadId);
          continue;
        }
        // 发送中的队首必须占住整个会话；否则 dispatching 状态触发 effect 重跑时，
        // 会在 running 尚未置位前把同一会话的下一条也并发发出。
        if (dispatching.has(item.id)) {
          seenThreads.add(item.threadId);
          continue;
        }
        // 失败的条目允许跳过，避免后续队列永久卡住。
        if (failed.has(item.id)) continue;
        seenThreads.add(item.threadId);
        void dispatchQueuedPrompt(item);
      }
    });
  });
}

/** 远程控制忙碌时入队：与桌面 Composer 共用同一条提示词队列和操作界面。 */
function startRemotePromptQueueBridge() {
  void listen<{ threadId: string; text: string; images?: PromptImage[]; id?: string }>(
    "prompt-queue:enqueue",
    (event) => {
      const threadId = event.payload?.threadId;
      if (!threadId) return;
      enqueuePrompt(
        threadId,
        event.payload.text || "",
        event.payload.images || [],
        event.payload.id,
      );
    },
  );
  void listen<{ threadId?: string; id: string }>("prompt-queue:remove", (event) => {
    const id = event.payload?.id;
    if (!id) return;
    removeQueuedPrompt(id);
  });
  void listen<{ threadId?: string; id: string; steerNow?: boolean }>(
    "prompt-queue:send-now",
    (event) => {
      const id = event.payload?.id;
      if (!id) return;
      const item = queuedPrompts().find((queued) => queued.id === id);
      if (!item) return;
      void dispatchQueuedPrompt(item, !!event.payload?.steerNow);
    },
  );
}

startPromptQueueDispatcher();
startRemotePromptQueueBridge();
