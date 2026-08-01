import type { ThreadMeta } from "./types";

function isWakeThread(thread: ThreadMeta): boolean {
  return !!thread.employeeId && /\]\s*Wake(?:\s|$|·)/.test(thread.title);
}

function isDoThread(thread: ThreadMeta): boolean {
  return !!thread.employeeId && /\]\s*Do(?:\s|$|·)/.test(thread.title);
}

function isFireThread(thread: ThreadMeta): boolean {
  // 同时覆盖内置 Fire 与通用工作流（/run）产生的接力链。
  return /^\[(?:Fire|WF)\]/.test(thread.title);
}

/**
 * Fire 任务链在侧栏只显示根会话，但用户真正关心的是当前进行到的阶段：
 * 点击时直达链上最新创建的阶段会话，而不是回到最初的目标会话。
 */
export function latestFireStage(
  threads: readonly ThreadMeta[],
  root: ThreadMeta,
): ThreadMeta | undefined {
  if (!isFireThread(root)) return undefined;
  let latest = root;
  const pending = [root.id];
  const seen = new Set(pending);
  while (pending.length > 0) {
    const parentId = pending.shift()!;
    for (const thread of threads) {
      if (thread.parentThreadId !== parentId || seen.has(thread.id)) continue;
      seen.add(thread.id);
      pending.push(thread.id);
      if (isFireThread(thread) && thread.createdAt > latest.createdAt) latest = thread;
    }
  }
  return latest === root ? undefined : latest;
}

export function firstWakeDoChild(
  threads: readonly ThreadMeta[],
  wake: ThreadMeta,
): ThreadMeta | undefined {
  if (!isWakeThread(wake)) return undefined;
  return threads
    .filter((thread) => thread.parentThreadId === wake.id && isDoThread(thread))
    .sort((a, b) => a.createdAt - b.createdAt)[0];
}

export function firstWakeDoPairForThread(
  threads: readonly ThreadMeta[],
  threadId: string | null,
): { wake: ThreadMeta; doThread?: ThreadMeta } | null {
  if (!threadId) return null;
  const current = threads.find((thread) => thread.id === threadId);
  if (!current) return null;
  const wake = isWakeThread(current)
    ? current
    : current.parentThreadId && isDoThread(current)
      ? threads.find((thread) => thread.id === current.parentThreadId)
      : undefined;
  if (!wake || !isWakeThread(wake)) return null;
  const doThread = firstWakeDoChild(threads, wake);
  if (current.id !== wake.id && current.id !== doThread?.id) return null;
  return { wake, doThread };
}
