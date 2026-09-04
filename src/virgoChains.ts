/**
 * 室女座（减少焦虑）任务链归属计算：纯函数，便于脱离 Solid/Tauri 单测。
 * 这里判定的两套口径必须分开：
 * - hidden：整条链是否留在室女座（工作流没走到终点就一直在，哪怕这一瞬没人 running）。
 * - busy：链上是否有推进中的阶段（回合在跑或阶段接力空档），refreshThreads 据此
 *   决定「后端快照 running=false」能不能冲掉前端已有的忙碌态。
 * 只按 running 判定会让未完成的工作流在室女座与普通列表之间来回闪。
 */
export type VirgoChainInput = {
  threads: { id: string; parentThreadId?: string | null }[];
  /** 单会话忙碌态（acp:turn 事件、乐观置位的结果）。 */
  isRunning: (threadId: string) => boolean;
  /** 工作流仍在推进的 root（活动阶段或阶段接力在途）。 */
  advancingRoots: Iterable<string>;
  /** 尚未走到终点的工作流 root（含暂停待补充、等待人工审核）。 */
  unfinishedRoots: Iterable<string>;
};

export function virgoChains(input: VirgoChainInput): {
  hidden: Set<string>;
  busy: Set<string>;
  rootCount: number;
} {
  const byId = new Map(input.threads.map((t) => [t.id, t]));
  // 会话所属任务链的根：工作流阶段接力、/stage 都是父子会话，整条链算一个任务。
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

  const advancing = new Set<string>();
  for (const root of input.advancingRoots) advancing.add(resolve(root));
  const unfinished = new Set<string>(advancing);
  for (const root of input.unfinishedRoots) unfinished.add(resolve(root));

  // 运行中任务数：本回合在跑的链 + 工作流推进中的链（阶段接力空档也算在跑）。
  const runningRoots = new Set<string>(advancing);
  for (const t of input.threads) {
    if (input.isRunning(t.id)) runningRoots.add(resolve(t.id));
  }
  const hidden = new Set<string>();
  const busy = new Set<string>();
  for (const t of input.threads) {
    const root = resolve(t.id);
    if (unfinished.has(root) || runningRoots.has(root)) hidden.add(t.id);
    // busy 只认工作流自己的推进态：普通会话漏收一个 acp:turn(false) 时，
    // 后端快照仍有机会把它自愈回 idle，不能被前端假忙碌态永久锁住。
    if (advancing.has(root)) busy.add(t.id);
  }
  return { hidden, busy, rootCount: runningRoots.size };
}
