import type { ThreadMeta } from "./types";

function isFireThread(thread: ThreadMeta): boolean {
  // 同时覆盖内置 Fire 与通用工作流（/run）产生的接力链。
  return /^\[(?:Fire|WF)\]/.test(thread.title);
}

/**
 * Fire 任务链在侧栏只显示根会话，但用户真正关心的是当前进行到的阶段：
 * 点击时直达链上正在运行的阶段会话（多个并行运行时取最新创建的），
 * 都没有运行时回退到最新创建的阶段会话，而不是回到最初的目标会话。
 */
export function latestFireStage(
  threads: readonly ThreadMeta[],
  root: ThreadMeta,
  isRunning?: (id: string) => boolean,
): ThreadMeta | undefined {
  if (!isFireThread(root)) return undefined;
  let latest = root;
  let running: ThreadMeta | undefined;
  const pending = [root.id];
  const seen = new Set(pending);
  while (pending.length > 0) {
    const parentId = pending.shift()!;
    for (const thread of threads) {
      if (thread.parentThreadId !== parentId || seen.has(thread.id)) continue;
      seen.add(thread.id);
      pending.push(thread.id);
      if (!isFireThread(thread)) continue;
      if (thread.createdAt > latest.createdAt) latest = thread;
      if (isRunning?.(thread.id) && (!running || thread.createdAt > running.createdAt)) {
        running = thread;
      }
    }
  }
  const target = running ?? latest;
  return target === root ? undefined : target;
}
