import type { ThreadMeta } from "./types";

/** One stop per ordinary conversation chain, at its latest running stage. */
export function nextRunningThread(
  threads: readonly ThreadMeta[], currentId: string | null,
  running: Readonly<Record<string, boolean>>,
): ThreadMeta | undefined {
  const ordinary = threads.filter(t => !t.experienceThread);
  const byId = new Map(ordinary.map(t => [t.id, t]));
  const rootOf = (thread: ThreadMeta) => {
    const seen = new Set([thread.id]);
    let current = thread;
    while (current.parentThreadId) {
      const parent = byId.get(current.parentThreadId);
      if (!parent || seen.has(parent.id)) break;
      seen.add(parent.id); current = parent;
    }
    return current.id;
  };
  const targets = new Map<string, ThreadMeta>();
  for (const thread of ordinary) {
    if (!running[thread.id]) continue;
    const root = rootOf(thread), previous = targets.get(root);
    if (!previous || thread.createdAt > previous.createdAt) targets.set(root, thread);
  }
  if (!targets.size) return undefined;
  const current = byId.get(currentId ?? "");
  const ids = [...targets.keys()];
  return targets.get(ids[(ids.indexOf(current ? rootOf(current) : "") + 1) % ids.length]);
}
