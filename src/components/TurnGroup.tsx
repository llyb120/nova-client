import "./ProcessBlock.css";
import { createMemo, For, Show } from "solid-js";
import { processSegments, processSummary, processLiveLines } from "../processDisplay";
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

/** 轮次 token 悬浮明细（与 DOM turn-fold title 一致） */
export function turnTokenTitle(t: TurnItem | undefined | null): string | undefined {
  if (!t?.totalTokens) return undefined;

  // inputTokens 是总输入量，包含缓存命中和缓存写入。悬浮明细拆成
  // 四个互斥类别，避免把缓存 token 同时算进“读取”和缓存项。
  // 「读取」「写入」只表示未命中缓存的输入 / 模型输出。
  const input = t.inputTokens ?? 0;
  const cacheRead = t.cacheReadTokens ?? 0;
  const cacheWrite = t.cacheWriteTokens ?? 0;
  const read = Math.max(0, input - cacheRead - cacheWrite);
  const parts = [
    `读取 ${fmtTokens(read)}`,
    `写入 ${fmtTokens(t.outputTokens ?? 0)}`,
  ];
  if (t.cacheReadTokens != null) parts.push(`缓存读取 ${fmtTokens(cacheRead)}`);
  if (t.cacheWriteTokens != null) parts.push(`缓存写入 ${fmtTokens(cacheWrite)}`);
  return `${parts.join(" / ")} tokens`;
}

/** 平均输出速度：输出 tokens / 整轮耗时（含工具执行和等待），不是模型纯生成速度。 */
export function turnAvgTokensPerSec(t: TurnItem | undefined | null): number | null {
  const output = t?.outputTokens;
  const ms = t?.durationMs ?? 0;
  if (output == null || !Number.isFinite(output) || output < 0 || !Number.isFinite(ms) || ms < 1_000) return null;
  return Math.round(output / (ms / 1_000));
}

function ProcessBody(props: { items: Item[]; active: boolean }) {
  const segments = createMemo(() => processSegments(props.items));
  return <For each={segments()}>{segment => {
    if (segment.type === "item") return <TranscriptItem item={segment.item} active={props.active && segment.id === props.items.at(-1)?.id} />;
    const live = () => props.active && segment === segments().at(-1);
    const key = () => `process-${segment.id}-${live() ? "live" : "done"}`;
    const open = () => !!state.expanded[key()];
    const lines = () => live() ? processLiveLines(segment.items) : [processSummary(segment.items)];
    return <div class="process-block">
      <button type="button" class="process-toggle" aria-expanded={open()} onClick={() => toggleExpanded(key())}>
        <IconChevron size={12} open={open()} />
        <span class="process-lines" classList={{ "process-lines-live": live() }}><span title={lines().join("\n")}>{lines().join("\n")}</span></span>
      </button>
      <Show when={open()}><div class="turn-process"><For each={segment.items}>{item => <TranscriptItem item={item} />}</For></div></Show>
    </div>;
  }}</For>;
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
  // 与 ToolCallCard / thought 一致：工具用 tool-${id}，思考用 thought-${id}
  const bodyExpanded = () =>
    props.group.body.some((it) => {
      if (it.type === "tool") return !!state.expanded[`tool-${it.id}`];
      if (it.type === "thought") return !!state.expanded[`thought-${it.id}`];
      return !!state.expanded[String(it.id)];
    });
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

  const foldLabel = () => {
    const t = props.group.turn;
    const dur = t ? fmtDuration(t.durationMs) : "";
    const tok = t?.totalTokens ? `${fmtTokens(t.totalTokens)} tokens` : "";
    const avg = turnAvgTokensPerSec(t);
    return ["已处理", dur, tok ? `· ${tok}` : "", avg != null ? `· 平均输出 ${fmtTokens(avg)} tok/s` : ""]
      .filter(Boolean).join(" ");
  };

  const tokenTitle = () => turnTokenTitle(props.group.turn);

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
            <ProcessBody items={split().process} active={props.active} />
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
              <ProcessBody items={split().process} active={false} />
            </div>
          </Show>
        </Show>
      </Show>
      <For each={split().conclusion}>{(item) => <TranscriptItem item={item} />}</For>
      <Show when={foldable()}>
        <EditedFilesCard body={props.group.body} undoneKey={`undone-${foldKey()}`} />
      </Show>
    </div>
  );
}
