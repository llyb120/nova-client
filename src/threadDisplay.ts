import type { ThreadMeta } from "./types";

function isFireThread(thread: ThreadMeta): boolean {
  // 同时覆盖内置 Fire 与通用工作流（/run）产生的接力链。
  return /^\[(?:Fire|WF)\]/.test(thread.title);
}

/** 普通 /stage 会话同样是链上的阶段节点；标题可能被自动改名，以 stageSourceThreadId 为准。 */
function isStageThread(thread: ThreadMeta): boolean {
  return isFireThread(thread) || !!thread.stageSourceThreadId;
}

/**
 * 任务链分组锚点：多级 stage 链里各环节的 cwd 可能已被 agent 中途切换
 *（change_working_directory），侧栏分组若只向上一级找父会话，会把孙子会话
 * 分进另一组，组内找不到父节点而被当作链根，普通列表与室女座都裂成两条会话。
 * 返回该会话在 threads 内可见的最高祖先（父级不在列表或成环时停在当前会话）。
 */
export function chainGroupAnchor(
  threads: readonly ThreadMeta[],
  thread: ThreadMeta,
): ThreadMeta {
  const byId = new Map(threads.map((t) => [t.id, t]));
  let cur = thread;
  const seen = new Set<string>([cur.id]);
  while (cur.parentThreadId) {
    const parent = byId.get(cur.parentThreadId);
    if (!parent || seen.has(parent.id)) break;
    cur = parent;
    seen.add(cur.id);
  }
  return cur;
}

/**
 * Fire 任务链与 /stage 链在侧栏只显示根会话，但用户真正关心的是当前进行到的阶段：
 * 点击时优先直达链上有未读的阶段会话（多个取最早创建的），其次正在运行的
 * （多个取最新创建的），都没有时回退到最新创建的阶段会话，
 * 而不是回到最初的目标会话。
 */
export function latestFireStage(
  threads: readonly ThreadMeta[],
  root: ThreadMeta,
  isRunning?: (id: string) => boolean,
  unreadOf?: (id: string) => number,
): ThreadMeta | undefined {
  let latest = root;
  let running: ThreadMeta | undefined;
  let unread: ThreadMeta | undefined;
  let hasStage = false;
  const pending = [root.id];
  const seen = new Set(pending);
  while (pending.length > 0) {
    const parentId = pending.shift()!;
    for (const thread of threads) {
      if (thread.parentThreadId !== parentId || seen.has(thread.id)) continue;
      seen.add(thread.id);
      pending.push(thread.id);
      if (!isStageThread(thread)) continue;
      hasStage = true;
      if (thread.createdAt > latest.createdAt) latest = thread;
      if (isRunning?.(thread.id) && (!running || thread.createdAt > running.createdAt)) {
        running = thread;
      }
      if ((unreadOf?.(thread.id) ?? 0) > 0 && (!unread || thread.createdAt < unread.createdAt)) {
        unread = thread;
      }
    }
  }
  // 链上没有任何 stage 节点（如预检→开发子会话）时保持原行为：打开被点击的会话本身。
  if (!isFireThread(root) && !hasStage) return undefined;
  const target = unread ?? running ?? latest;
  return target === root ? undefined : target;
}
