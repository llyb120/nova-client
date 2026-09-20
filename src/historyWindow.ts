import type { Item, Thread } from './types';
import type { HistoryDisplayUpdate, HistoryPage, HistoryWindow } from './historyTypes';

export const HISTORY_WINDOW_ITEMS = 240;
export const HISTORY_WINDOW_BYTES = 1536 * 1024;
export const HISTORY_PAGE_ITEMS = 80;
export const HISTORY_PAGE_BYTES = 512 * 1024;

export function pageThread(page: HistoryPage): Thread {
  const { generation, start, end, totalItems, turnOffset, beforeCursor, afterCursor, stats } = page;
  return { ...page.thread, history: { generation, start, end, totalItems, turnOffset, beforeCursor, afterCursor, stats } };
}
function index(item: Item): number { return item.historyIndex ?? -1; }
/** A conservative UTF-8 estimate, bounded by the native display projection. */
export function displayBytes(item: Item): number { return JSON.stringify(item).length * 3; }
export function windowBytes(items: readonly Item[]): number { return items.reduce((n, item) => n + displayBytes(item), 0); }

function trim(items: Item[], meta: HistoryWindow, keep: 'start' | 'end', protectedIds?: readonly number[]): { items: Item[]; history: HistoryWindow } {
  let start = 0, end = items.length, bytes = windowBytes(items);
  const protectedIndices = items.flatMap((item, i) => protectedIds?.includes(item.id) ? [i] : []);
  const protectedStart = Math.min(...protectedIndices), protectedEnd = Math.max(...protectedIndices);
  while (end - start > 1 && (end - start > HISTORY_WINDOW_ITEMS || bytes > HISTORY_WINDOW_BYTES)) {
    let fromStart = keep === 'end';
    const startProtected = start >= protectedStart && start <= protectedEnd;
    const endProtected = end - 1 >= protectedStart && end - 1 <= protectedEnd;
    if (startProtected && endProtected) break;
    if (fromStart && startProtected) fromStart = false;
    else if (!fromStart && endProtected) fromStart = true;
    const removed = fromStart ? items[start++] : items[--end];
    bytes -= displayBytes(removed);
    if (fromStart && removed.type === 'user' && removed.id >= 0) meta.turnOffset++;
  }
  const result = items.slice(start, end);
  const persisted = result.filter(item => item.id >= 0 && index(item) >= 0);
  const first = persisted[0], last = persisted.at(-1);
  if (first) meta.start = index(first);
  if (last) meta.end = index(last) + 1;
  meta.beforeCursor = meta.start > 0 ? `${meta.generation}:${meta.start}` : null;
  meta.afterCursor = meta.end < meta.totalItems ? `${meta.generation}:${meta.end}` : null;
  return { items: result, history: meta };
}

/** Merge adjacent pages, de-duplicate overlap, retain only a bounded moving window. */
export function mergeHistoryPage(current: Thread, page: HistoryPage, direction: 'before' | 'after', protectedIds?: readonly number[]): Thread {
  const old = current.history;
  if (!old || old.generation !== page.generation) throw new Error('HISTORY_CHANGED');
  if (page.end < old.start || page.start > old.end) throw new Error('HISTORY_GAP');
  const merged = new Map(page.thread.items.map(item => [item.id, item]));
  // A page requested earlier must not overwrite newer streamed data in its overlap.
  for (const item of current.items) merged.set(item.id, item);
  const items = [...merged.values()].sort((a, b) => a.id < 0 ? 1 : b.id < 0 ? -1 : index(a) - index(b));
  const meta: HistoryWindow = {
    ...old, totalItems: Math.max(old.totalItems, page.totalItems),
    start: Math.min(old.start, page.start), end: Math.max(old.end, page.end),
    turnOffset: page.start < old.start ? page.turnOffset : old.turnOffset,
    stats: page.totalItems >= old.totalItems ? page.stats : old.stats,
  };
  const bounded = trim(items, meta, direction === 'before' ? 'start' : 'end', protectedIds);
  // A pathological viewport cannot justify unbounded allocation or dropping
  // visible content. Keep the current window; a later user scroll can retry.
  if (bounded.items.length > HISTORY_WINDOW_ITEMS ||
      (bounded.items.length > 1 && windowBytes(bounded.items) > HISTORY_WINDOW_BYTES)) return current;
  return { ...current, ...bounded };
}

/** IDs are authoritative replacements, not deltas: a snapshot handover cannot double-append text. */
export function mergeHistoryUpdate(current: Thread, update: HistoryDisplayUpdate, followTail = true): { thread: Thread; gap: boolean } {
  const old = current.history;
  if (!old || old.generation !== update.generation) throw new Error('HISTORY_CHANGED');
  const atTail = followTail && !old.afterCursor;
  const lookup = new Map(update.items.map(item => [item.id, item]));
  // Authoritative updates only replace persisted IDs. Negative IDs are local
  // optimistic sends and are reconciled exactly once in preserveOptimistic().
  // Removing them here would let one user upsert acknowledge every queued send.
  const items = current.items.map(item => lookup.get(item.id) ?? item);
  const ids = new Set(items.map(item => item.id));
  let nextIndex = old.end, gap = false;
  if (atTail) {
    for (const item of [...update.items].sort((a, b) => index(a) - index(b))) {
      if (ids.has(item.id) || index(item) < old.end) continue;
      if (index(item) !== nextIndex) { gap = true; break; }
      items.push(item); ids.add(item.id); nextIndex++;
    }
    gap ||= nextIndex < update.totalItems;
  }
  const meta = { ...old, end: nextIndex, totalItems: update.totalItems, stats: update.stats };
  // While reading old pages, updates outside the window only advance the tail indicator.
  return { thread: { ...current, ...trim(items, meta, atTail ? 'end' : 'start') }, gap };
}

function isOptimisticUser(item: Item): boolean {
  return item.type === 'user' && item.id < 0;
}
function authoritativeUserCount(thread: Thread): number {
  return thread.history?.stats.users
    ?? thread.items.filter(item => item.type === 'user' && item.id >= 0).length;
}

/** Reconcile local sends against an authoritative snapshot/page.
 *
 * History pages can race the user upsert with a turn/reset notification. A page
 * merge may therefore already contain both the persisted user item and its
 * negative-ID optimistic bubble. Always strip optimistic items from `next`
 * first, then consume exactly the number of sends that the authoritative global
 * user count has acknowledged. This also keeps rapid multi-send correct: one
 * persisted user removes one pending bubble, never all of them.
 *
 * A generation replacement (resend/restore) supersedes every optimistic item
 * from the previous branch even when the persisted user count does not grow.
 */
export function preserveOptimistic(current: Thread | undefined, next: Thread): Thread {
  const canonical = next.items.filter(item => !isOptimisticUser(item));
  if (!current) {
    return canonical.length === next.items.length ? next : { ...next, items: canonical };
  }

  const pending = current.items.filter(isOptimisticUser);
  const generationChanged = !!current.history && !!next.history
    && current.history.generation !== next.history.generation;
  const acknowledged = generationChanged
    ? pending.length
    : Math.min(
        pending.length,
        Math.max(0, authoritativeUserCount(next) - authoritativeUserCount(current)),
      );
  const remaining = pending.slice(acknowledged);

  if (remaining.length === 0 && canonical.length === next.items.length) return next;
  return { ...next, items: [...canonical, ...remaining] };
}
