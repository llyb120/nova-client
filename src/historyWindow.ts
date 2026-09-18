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

function trim(items: Item[], meta: HistoryWindow, keep: 'start' | 'end'): { items: Item[]; history: HistoryWindow } {
  let start = 0, end = items.length, bytes = windowBytes(items);
  while (end - start > 1 && (end - start > HISTORY_WINDOW_ITEMS || bytes > HISTORY_WINDOW_BYTES)) {
    const removed = keep === 'end' ? items[start++] : items[--end];
    bytes -= displayBytes(removed);
    if (keep === 'end' && removed.type === 'user' && removed.id >= 0) meta.turnOffset++;
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
export function mergeHistoryPage(current: Thread, page: HistoryPage, direction: 'before' | 'after'): Thread {
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
  return { ...current, ...trim(items, meta, direction === 'before' ? 'start' : 'end') };
}

/** IDs are authoritative replacements, not deltas: a snapshot handover cannot double-append text. */
export function mergeHistoryUpdate(current: Thread, update: HistoryDisplayUpdate, followTail = true): { thread: Thread; gap: boolean } {
  const old = current.history;
  if (!old || old.generation !== update.generation) throw new Error('HISTORY_CHANGED');
  const atTail = followTail && !old.afterCursor;
  const lookup = new Map(update.items.map(item => [item.id, item]));
  const incomingUser = update.items.some(item => item.type === 'user' && index(item) >= old.totalItems);
  const items = current.items.filter(item => !(incomingUser && item.id < 0)).map(item => lookup.get(item.id) ?? item);
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

/** A latest-page navigation may happen before send_prompt acknowledges the user's
 * optimistic bubble. Old users newly entering the viewport are NOT that ack. */
export function preserveOptimistic(current: Thread | undefined, next: Thread): Thread {
  if (!current || (next.history?.stats.users ?? 0) > (current.history?.stats.users ?? 0)) return next;
  const optimistic = current.items.filter(i => i.id < 0 && !next.items.some(n => n.id === i.id));
  return optimistic.length ? { ...next, items: [...next.items, ...optimistic] } : next;
}
