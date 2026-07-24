import { createMemo, For, Show } from "solid-js";
import { state, toggleExpanded } from "../store";
import type { Item, TurnItem, UserItem } from "../types";
import { EditedFilesCard } from "./EditedFilesCard";
import { IconChevron } from "./icons";
import { TranscriptItem } from "./TranscriptItem";

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
 *
 * 增量：流式期间每次结构变化（新增一条 item）只影响尾部分组，前面的分组已闭合、不再变动。
 * 因此复用与 items 前缀逐条引用相同的旧分组（prev），只从最后一个稳定分组之后重建——把
 * 单次开销从 O(全会话 item 数) 降到 O(尾部)，消除长会话流式时反复全量分配分组对象带来的 GC 抖动。
 * prev 的最后一组可能仍在增长，一律排除、从它的起点重算。
 */
export function groupItems(items: Item[], prev: Group[] = []): Group[] {
  let itemIdx = 0;
  let reuse = 0;
  // 排除 prev 末组（可能仍在增长）：只复用「后面还有别的分组、因而必定已闭合」的前缀
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
      // 每条用户消息都开新组：运行中补充/引导提示词不能塞进上一轮 body，
      // 否则会埋进过程区；末项变成 user 后忙碌态消失，界面像已停止。
      cur = { user: item, body: [] };
      result.push(cur);
    } else if (item.type === "turn") {
      if (cur) cur.turn = item;
      else result.push({ body: [], turn: item });
      // turn 闭合本轮，后续输出归下一组（通常由下一条 user 开启）
      cur = null;
    } else {
      if (!cur) {
        cur = { body: [] };
        result.push(cur);
      }
      cur.body.push(item);
    }
  }

  // 重建出的尾部分组若内容与旧对象一致，复用旧对象身份，避免下游 <For>/VirtualGroup
  // 无谓重挂载（保留展开态、DOM、滚动位置）。
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

function isBusyItem(item: Item): boolean {
  return (
    (item.type === "tool" && (item.status === "pending" || item.status === "in_progress")) ||
    (item.type === "thought" && item.text === "思考中…")
  );
}

/**
 * codex 风格轮次渲染：进行中过程实时展开；
 * 完成后过程折叠为「已处理 Xs · N tokens」行，与结论区分开
 */
export function TurnGroup(props: { group: Group; active: boolean }) {
  // 折叠状态放 store（按轮次内稳定的 item id），流式更新重建分组时不丢失
  const foldKey = () =>
    `turn-${props.group.turn?.id ?? props.group.user?.id ?? props.group.body[0]?.id ?? 0}`;
  // 运行中用户手动展开过本轮的某个详情（工具/思考）时，结束后该轮保持展开；
  // 未显式点过折叠行（undefined）才走这个自动判断，点过的以用户操作为准
  const bodyExpanded = () =>
    props.group.body.some((it) => state.expanded[String(it.id)]);
  const open = () => state.expanded[foldKey()] ?? bodyExpanded();
  const foldable = () => !!props.group.turn && !props.active;

  // 仅在轮次真正结束（有 turn 标记）后才拆结论区。运行中即使本分组不是
  // active（后面又跟了引导提示开了新组），也不能按 !active 抽结论，否则上一截
  // 会像已收束，看起来会话停了。
  const split = createMemo(() => {
    const body = props.group.body;
    if (!props.group.turn) return { process: body, conclusion: [] };
    const lastConclusion = body.findLastIndex(
      (item) => item.type === "assistant" || item.type === "system",
    );
    if (lastConclusion < 0) return { process: body, conclusion: [] };
    let firstConclusion = lastConclusion;
    while (
      firstConclusion > 0 &&
      (body[firstConclusion - 1].type === "assistant" ||
        body[firstConclusion - 1].type === "system")
    ) {
      firstConclusion--;
    }
    return {
      process: [...body.slice(0, firstConclusion), ...body.slice(lastConclusion + 1)],
      conclusion: body.slice(firstConclusion, lastConclusion + 1),
    };
  });

  // 仅「正在流式输出的那一项」随组活跃而自动展开。Codex 有时会先吐一句
  // assistant 进度说明，然后继续在其上方的工具卡片里刷新输出；此时真正活跃的
  // 是前面的 pending/in_progress 工具或“思考中…”，不能只看 body 末项。
  const activeBodyId = () => {
    const b = props.group.body;
    if (!props.active) return -1;
    for (let i = b.length - 1; i >= 0; i--) {
      if (isBusyItem(b[i])) return b[i].id;
    }
    return b.length ? b[b.length - 1].id : -1;
  };

  // 当最后一行只是上一句 assistant 进度说明、实际仍在上方工具卡片里跑时，
  // 在底部补一个轻量活动尾标，避免用户看到“最后一句不动”误以为卡死。
  const showLiveTail = () => {
    if (!props.active) return false;
    const b = props.group.body;
    if (b.length === 0 || !b.some(isBusyItem)) return false;
    const last = b[b.length - 1];
    return last.type === "assistant" || last.type === "system";
  };

  const foldLabel = () => {
    const t = props.group.turn;
    const dur = t ? fmtDuration(t.durationMs) : "";
    const tok = t?.totalTokens ? `${fmtTokens(t.totalTokens)} tokens` : "";
    return ["已处理", dur, tok ? `· ${tok}` : ""].filter(Boolean).join(" ");
  };

  const tokenTitle = () => {
    const t = props.group.turn;
    if (!t?.totalTokens) return undefined;

    // inputTokens 是总输入量，包含缓存命中和缓存写入。悬浮明细拆成
    // 四个互斥类别，避免把缓存 token 同时算进“读取”和缓存项。
    const cacheRead = t.cacheReadTokens ?? 0;
    const cacheWrite = t.cacheWriteTokens ?? 0;
    const read = Math.max(0, (t.inputTokens ?? 0) - cacheRead - cacheWrite);
    const parts = [
      `读取 ${fmtTokens(read)}`,
      `写入 ${fmtTokens(t.outputTokens ?? 0)}`,
    ];
    if (t.cacheReadTokens != null) parts.push(`缓存读取 ${fmtTokens(cacheRead)}`);
    if (t.cacheWriteTokens != null) parts.push(`缓存写入 ${fmtTokens(cacheWrite)}`);
    return `${parts.join(" / ")} tokens`;
  };

  return (
    <div class="turn-group">
      <Show when={props.group.user}>
        <TranscriptItem item={props.group.user!} />
      </Show>
      <Show when={props.group.turn?.actualModel}>
        <div class="turn-actual-model">实际模型：{props.group.turn!.actualModel}</div>
      </Show>
      <Show when={split().process.length > 0}>
        <Show
          when={foldable()}
          fallback={
            <For each={split().process}>
              {(item) => (
                <TranscriptItem
                  item={item}
                  active={props.active && item.id === activeBodyId()}
                />
              )}
            </For>
          }
        >
          <button
            class="turn-fold"
            onClick={() => toggleExpanded(foldKey(), !open())}
            title={tokenTitle()}
          >
            {foldLabel()}
            <IconChevron size={12} open={open()} />
          </button>
          <Show when={open()}>
            <div class="turn-process">
              <For each={split().process}>{(item) => <TranscriptItem item={item} />}</For>
            </div>
          </Show>
        </Show>
      </Show>
      <Show when={showLiveTail()}>
        <div class="turn-live-tail">
          <span class="spinner small" />
          继续处理中…
        </div>
      </Show>
      <For each={split().conclusion}>{(item) => <TranscriptItem item={item} />}</For>
      <Show when={foldable()}>
        <EditedFilesCard body={props.group.body} undoneKey={`undone-${foldKey()}`} />
      </Show>
    </div>
  );
}
