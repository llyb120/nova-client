import type { ThreadMeta } from "./types";

function isWakeThread(thread: ThreadMeta): boolean {
  return !!thread.employeeId && /\]\s*Wake(?:\s|$|·)/.test(thread.title);
}

function isDoThread(thread: ThreadMeta): boolean {
  return !!thread.employeeId && /\]\s*Do(?:\s|$|·)/.test(thread.title);
}

export function firstWakeDoChild(
  threads: readonly ThreadMeta[],
  wake: ThreadMeta,
): ThreadMeta | undefined {
  if (wake.parentThreadId || !isWakeThread(wake)) return undefined;
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
  const wake = current.parentThreadId
    ? threads.find((thread) => thread.id === current.parentThreadId)
    : current;
  if (!wake || wake.parentThreadId || !isWakeThread(wake)) return null;
  const doThread = firstWakeDoChild(threads, wake);
  if (current.id !== wake.id && current.id !== doThread?.id) return null;
  return { wake, doThread };
}
