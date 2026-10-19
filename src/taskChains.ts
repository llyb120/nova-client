/**
 * 任务链（父子接力整条链）运行态计算：纯函数，便于脱离 Solid/Tauri 单测。
 * active 包含运行中或提示词队列有待发条目的整条链；rootCount 只统计运行中的链。
 */
export type TaskChainInput = {
  threads: { id: string; parentThreadId?: string | null }[];
  /** 单会话忙碌态（acp:turn 事件、乐观置位的结果）。 */
  isRunning: (threadId: string) => boolean;
  /** 提示词队列仍有待发条目（未挂起）的会话：接力空档仍算未结束，但不算在跑、不锁忙碌态。 */
  queuedThreads: Iterable<string>;
};

export function taskChains(input: TaskChainInput): {
  active: Set<string>;
  rootCount: number;
} {
  const byId = new Map(input.threads.map((t) => [t.id, t]));
  // Fire 接力、/stage 都是父子会话，整条链算一个任务。
  const rootOf = new Map<string, string>();
  for (const t of input.threads) {
    let cur = t;
    const seen = new Set<string>([cur.id]);
    while (cur.parentThreadId) {
      const parent = byId.get(cur.parentThreadId);
      // parentThreadId 指向链外会话或成环时以当前会话为根，避免死循环。
      if (!parent || seen.has(parent.id)) break;
      cur = parent;
      seen.add(cur.id);
    }
    rootOf.set(t.id, cur.id);
  }
  const resolve = (id: string) => rootOf.get(id) ?? id;

  const unfinished = new Set<string>();
  // 队列待发条目只保未结束，不计入运行中任务数。
  for (const id of input.queuedThreads) unfinished.add(resolve(id));

  const runningRoots = new Set<string>();
  for (const t of input.threads) {
    if (input.isRunning(t.id)) runningRoots.add(resolve(t.id));
  }
  const active = new Set<string>();
  for (const t of input.threads) {
    const root = resolve(t.id);
    if (unfinished.has(root) || runningRoots.has(root)) active.add(t.id);
  }
  return { active, rootCount: runningRoots.size };
}
