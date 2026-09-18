import type { Item, Thread, TimeMachinePrompt } from './types';

/** UI projections only. These objects must never replace authoritative history. */
export interface HistoryStats {
  items: number; users: number; turns: number;
  estimatedBytes: number; inlineAssetBytes: number;
  totalTokens: number; inputTokens: number; outputTokens: number;
  cacheReadTokens: number; cacheWriteTokens: number; maxId: number;
}
export interface HistoryWindow {
  generation: string; start: number; end: number; totalItems: number;
  turnOffset: number; beforeCursor: string | null; afterCursor: string | null;
  stats: HistoryStats;
}
export interface HistoryPage extends HistoryWindow {
  thread: Thread; prefixStats: HistoryStats; suffixStats: HistoryStats;
  payloadBytes: number; elapsedMs: number;
}
export interface HistoryPageRequest {
  cursor?: string; direction?: 'before' | 'after'; aroundId?: number;
  limit?: number; byteLimit?: number;
}
export interface HistoryDisplayUpdate {
  generation: string; items: Item[]; totalItems: number; stats: HistoryStats;
}
export interface HistoryOutline {
  generation: string; prompts: (TimeMachinePrompt & { index: number })[]; stats: HistoryStats;
}
export interface HistoryAsset {
  attachmentId: string; uri: string; size: number;
  width: number | null; height: number | null; thumbnailUri: string | null;
}
export interface HistoryNotice {
  threadId?: string; resync?: boolean;
  notice?: { ids: number[]; removed: number[]; chars: number; reset: boolean; ops: import('./types').UpdateOp[] };
}
