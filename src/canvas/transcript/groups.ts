// groups.ts — 轮次分组与格式化工具（从 src/components/TurnGroup.tsx 平移，逻辑原样）。
// ChatView（世界线时间轴）与 canvas 渲染层共用。

import { state } from "../../store.js";
import type { Item, TurnItem, UserItem } from "../../types.js";

/** 一轮对话：用户消息 + 过程（思考/工具）+ 结论 + 轮次标记 */
export interface Group {
  user?: UserItem;
  body: Item[];
  turn?: TurnItem;
}

function sameGroup(a: Group | undefined, b: Group | undefined): boolean {
  if (!a || !b || a.user !== b.user || a.turn !== b.turn || a.body.length !== b.body.length) {
    return false;
  }
  return a.body.every((item, idx) => item === b.body[idx]);
}

/** 一个分组消费的原始 item 数（原始顺序为 user? → body… → turn?） */
function groupSize(g: Group): number {
  return (g.user ? 1 : 0) + g.body.length + (g.turn ? 1 : 0);
}

/** items[start..] 的引用是否与分组 g 的 (user, body…, turn) 逐个一致 */
function groupMatchesAt(g: Group, items: Item[], start: number): boolean {
  let i = start;
  if (g.user) {
    if (items[i] !== g.user) return false;
    i++;
  }
  for (const b of g.body) {
    if (items[i] !== b) return false;
    i++;
  }
  if (g.turn && items[i] !== g.turn) return false;
  return true;
}

/**
 * 把 items 折叠成「一轮 = 用户消息 + 过程 + 结论 + 轮次标记」的分组。
 * 增量复用前缀分组（引用相等），只重建尾部——与 DOM 版同一实现。
 */
export function groupItems(items: Item[], prev: Group[] = []): Group[] {
  let itemIdx = 0;
  let reuse = 0;
  const maxReuse = prev.length > 0 ? prev.length - 1 : 0;
  for (let g = 0; g < maxReuse; g++) {
    if (!groupMatchesAt(prev[g], items, itemIdx)) break;
    itemIdx += groupSize(prev[g]);
    reuse = g + 1;
  }

  const result: Group[] = prev.slice(0, reuse);
  const rebuiltStart = result.length;
  let cur: Group | null = null;
  for (let i = itemIdx; i < items.length; i++) {
    const item = items[i];
    if (item.type === "user") {
      cur = { user: item, body: [] };
      result.push(cur);
    } else if (item.type === "turn") {
      if (cur) cur.turn = item;
      else result.push({ body: [], turn: item });
      cur = null;
    } else {
      if (!cur) {
        cur = { body: [] };
        result.push(cur);
      }
      cur.body.push(item);
    }
  }

  for (let j = rebuiltStart; j < result.length; j++) {
    const prevGroup = prev[j];
    if (prevGroup && sameGroup(result[j], prevGroup)) result[j] = prevGroup;
  }
  return result;
}

export function fmtDuration(ms: number): string {
  const s = Math.round(ms / 1000);
  if (s < 1) return "";
  if (s < 60) return `${s}s`;
  return `${Math.floor(s / 60)}m ${s % 60}s`;
}

export function fmtTokens(n: number): string {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + "M";
  if (n >= 1000) return (n / 1000).toFixed(1) + "k";
  return String(n);
}

export function isBusyItem(item: Item): boolean {
  return (
    (item.type === "tool" && (item.status === "pending" || item.status === "in_progress")) ||
    (item.type === "thought" && item.text === "思考中…")
  );
}

/** TurnGroup.split 的纯函数版：过程区 / 结论区划分（仅轮次结束后） */
export function splitGroupBody(body: Item[], hasTurn: boolean): { process: Item[]; conclusion: Item[] } {
  if (!hasTurn) return { process: body, conclusion: [] };
  const lastConclusion = body.findLastIndex(
    (item) => item.type === "assistant" || item.type === "system",
  );
  if (lastConclusion < 0) return { process: body, conclusion: [] };
  let firstConclusion = lastConclusion;
  while (
    firstConclusion > 0 &&
    (body[firstConclusion - 1].type === "assistant" || body[firstConclusion - 1].type === "system")
  ) {
    firstConclusion--;
  }
  return {
    process: [...body.slice(0, firstConclusion), ...body.slice(lastConclusion + 1)],
    conclusion: body.slice(firstConclusion, lastConclusion + 1),
  };
}

/** TurnGroup 的 foldKey（按轮次内稳定的 item id） */
export function foldKeyOf(group: Group): string {
  return `turn-${group.turn?.id ?? group.user?.id ?? group.body[0]?.id ?? 0}`;
}

/** TurnGroup 的 bodyExpanded：运行中用户手动展开过本轮某个详情 */
export function bodyExpandedOf(group: Group): boolean {
  return group.body.some((it) => state.expanded[String(it.id)]);
}

/** TurnGroup 的 activeBodyId：仅「正在流式输出的那一项」随组活跃自动展开 */
export function activeBodyIdOf(group: Group, active: boolean): number {
  if (!active) return -1;
  const b = group.body;
  for (let i = b.length - 1; i >= 0; i--) {
    if (isBusyItem(b[i])) return b[i].id;
  }
  return b.length ? b[b.length - 1].id : -1;
}

/** TurnGroup 的 showLiveTail：末行只是进度说明、实际仍在上方工具里跑时补活动尾标 */
export function showLiveTailOf(group: Group, active: boolean): boolean {
  if (!active) return false;
  const b = group.body;
  if (b.length === 0 || !b.some(isBusyItem)) return false;
  const last = b[b.length - 1];
  return last.type === "assistant" || last.type === "system";
}

/** 「已处理 Xs · N tokens」折叠行文案 */
export function foldLabelOf(turn: TurnItem | undefined): string {
  const dur = turn ? fmtDuration(turn.durationMs) : "";
  const tok = turn?.totalTokens ? `${fmtTokens(turn.totalTokens)} tokens` : "";
  return ["已处理", dur, tok ? `· ${tok}` : ""].filter(Boolean).join(" ");
}

/** 折叠行 tooltip 的 token 明细（与 TurnGroup.tokenTitle 同文案） */
export function tokenTitleOf(turn: TurnItem | undefined): string | undefined {
  const t = turn;
  if (!t?.totalTokens) return undefined;
  const cacheRead = t.cacheReadTokens ?? 0;
  const cacheWrite = t.cacheWriteTokens ?? 0;
  const read = Math.max(0, (t.inputTokens ?? 0) - cacheRead - cacheWrite);
  const parts = [`读取 ${fmtTokens(read)}`, `写入 ${fmtTokens(t.outputTokens ?? 0)}`];
  if (t.cacheReadTokens != null) parts.push(`缓存读取 ${fmtTokens(cacheRead)}`);
  if (t.cacheWriteTokens != null) parts.push(`缓存写入 ${fmtTokens(cacheWrite)}`);
  return `${parts.join(" / ")} tokens`;
}
