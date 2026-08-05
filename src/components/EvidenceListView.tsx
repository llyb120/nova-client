import { message } from "@tauri-apps/plugin-dialog";
import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import type { ClueStageGroup, ClueStructure } from "../clueGraph";
import { authorBadge, authorName, excerpt, fmtTime } from "../clueGraph";
import { associateClues, clueCardById, clueCurrentVersion, disassociateClues, state } from "../store";
import type { ClueCard, ClueNodeGroup } from "../types";

type Props = {
  structure: ClueStructure;
  selectedCardId: string | null;
  selectedCardIds: Set<string>;
  onSelectCard: (cardId: string, event: MouseEvent) => void;
  onFocusCard: (cardId: string) => void;
};

const INDENT = 26;
const BASE_PAD = 12;
const MAX_INDENT_DEPTH = 6;

const indentOf = (depth: number) => BASE_PAD + Math.min(depth, MAX_INDENT_DEPTH) * INDENT;

/** 证据链大纲视图：用层级缩进表达前置 → 后续，信息密度远高于画布。 */
export function EvidenceListView(props: Props) {
  const [connectingFrom, setConnectingFrom] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);

  const sections = createMemo(() => {
    const main: Array<{ depth: number; groups: ClueStageGroup[] }> = [];
    const isolated: ClueStageGroup[] = [];
    for (const stage of props.structure.stages) {
      const kept = stage.groups.filter((item) => item.role !== "isolated");
      if (kept.length > 0) main.push({ depth: stage.depth, groups: kept });
      for (const item of stage.groups) {
        if (item.role === "isolated") isolated.push(item);
      }
    }
    return { main, isolated };
  });

  const connectingCard = createMemo(() => clueCardById(connectingFrom()));

  onMount(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setConnectingFrom(null);
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  const completeConnection = async (fromCardId: string, target: ClueCard) => {
    setConnectingFrom(null);
    setBusy(true);
    try {
      await associateClues(fromCardId, target.id);
      props.onFocusCard(target.id);
    } catch (error) {
      await message(String(error), { title: "连接失败", kind: "error" });
    } finally {
      setBusy(false);
    }
  };

  const removeLink = async (event: MouseEvent, parentCardId: string, group: ClueNodeGroup) => {
    event.stopPropagation();
    if (busy()) return;
    setBusy(true);
    try {
      await disassociateClues(parentCardId, group.cards[0].id);
    } catch (error) {
      await message(String(error), { title: "断开连接失败", kind: "error" });
    } finally {
      setBusy(false);
    }
  };

  const onRowClick = (card: ClueCard, event: MouseEvent) => {
    const from = connectingFrom();
    if (from) {
      event.stopPropagation();
      if (from !== card.id) void completeConnection(from, card);
      return;
    }
    props.onSelectCard(card.id, event);
  };

  const predecessorCards = (group: ClueNodeGroup) =>
    group.parentCardIds
      .map((id) => clueCardById(id))
      .filter((card): card is ClueCard => !!card);

  // 多前置或跨层长边时，缩进无法完整表达归属，用前置 chips 补齐
  const needsChips = (item: ClueStageGroup) => {
    if (item.group.parentCardIds.length > 1) return true;
    return item.group.parentCardIds.some((parentId) => {
      const parent = props.structure.cardToGroup.get(parentId);
      if (!parent) return false;
      return item.depth - (props.structure.depthByGroup.get(parent.id) ?? 0) > 1;
    });
  };

  const renderGroup = (item: ClueStageGroup) => (
    <div class="clue-list-group" classList={{ parallel: item.group.cards.length > 1 }}>
      <Show when={item.group.cards.length > 1}>
        <div class="clue-list-parallel" style={{ "padding-left": `${indentOf(item.depth)}px` }}>
          <span>平行线索 ×{item.group.cards.length}</span>
        </div>
      </Show>
      <Show when={needsChips(item)}>
        <div class="clue-list-chips" style={{ "padding-left": `${indentOf(item.depth)}px` }}>
          <span class="clue-list-chips-label">前置</span>
          <For each={predecessorCards(item.group)}>
            {(parent) => (
              <span class="clue-list-chip">
                <button
                  type="button"
                  class="clue-list-chip-title"
                  title="定位到该前置线索"
                  onClick={(event) => {
                    event.stopPropagation();
                    props.onFocusCard(parent.id);
                  }}
                >
                  {clueCurrentVersion(parent)?.title || "未命名线索"}
                </button>
                <button
                  type="button"
                  class="clue-list-chip-remove"
                  title="断开与该前置的连接"
                  disabled={busy()}
                  onClick={(event) => void removeLink(event, parent.id, item.group)}
                >
                  ×
                </button>
              </span>
            )}
          </For>
        </div>
      </Show>
      <For each={item.group.cards}>
        {(card) => {
          const version = () => clueCurrentVersion(card);
          return (
            <article
              class={`clue-list-row role-${item.role}`}
              classList={{
                active: props.selectedCardId === card.id,
                selected: props.selectedCardIds.has(card.id),
                mentioned: state.unreadClueMentions.includes(card.id),
                "is-source": connectingFrom() === card.id,
              }}
              style={{ "padding-left": `${indentOf(item.depth) + 10}px` }}
              onClick={(event) => onRowClick(card, event)}
            >
              <For each={Array.from({ length: Math.min(item.depth, MAX_INDENT_DEPTH) }, (_, index) => index + 1)}>
                {(level) => (
                  <span
                    class="clue-list-guide"
                    style={{ left: `${BASE_PAD + level * INDENT - 14}px` }}
                    aria-hidden="true"
                  />
                )}
              </For>
              <span class="clue-list-role" aria-hidden="true" />
              <span class="clue-list-avatar" title={`作者：${authorName(version()?.authorName)}`}>
                {authorBadge(version()?.authorName)}
              </span>
              <div class="clue-list-main">
                <div class="clue-list-titleline">
                  <strong>{version()?.title || "未命名线索"}</strong>
                  <Show when={state.unreadClueMentions.includes(card.id)}>
                    <span class="clue-list-mention" title="有 @ 你的新提醒" />
                  </Show>
                  <Show when={(card.comments ?? []).length > 0}>
                    <span class="clue-list-comments">💬{(card.comments ?? []).length}</span>
                  </Show>
                </div>
                <p class="clue-list-excerpt">{excerpt(version()?.content ?? "", 120) || "（暂无内容）"}</p>
              </div>
              <span class="clue-list-meta">
                v{card.versions.length} · {authorName(version()?.authorName)} · {fmtTime(card.updatedAt)}
              </span>
              <button
                type="button"
                class="clue-list-connect"
                disabled={busy()}
                title="以此线索为前置，点击另一条线索完成连接"
                onClick={(event) => {
                  event.stopPropagation();
                  setConnectingFrom(card.id);
                }}
              >
                {connectingFrom() === card.id ? "连接中…" : "连接"}
              </button>
            </article>
          );
        }}
      </For>
    </div>
  );

  return (
    <div class="clue-list-wrap" classList={{ connecting: !!connectingFrom() }}>
      <Show when={connectingCard()}>
        {(card) => (
          <div class="clue-list-banner">
            <span>
              正在连接：<strong>{clueCurrentVersion(card())?.title || "未命名线索"}</strong>
              <span class="clue-list-banner-hint">→ 点击目标线索完成连接</span>
            </span>
            <button type="button" onClick={() => setConnectingFrom(null)}>
              取消（Esc）
            </button>
          </div>
        )}
      </Show>
      <div class="clue-list">
        <For each={sections().main}>
          {(stage) => (
            <>
              <div class="clue-list-stage">
                <span>{stage.depth === 0 ? "起点" : `第 ${stage.depth + 1} 步`}</span>
                <span class="clue-list-stage-count">
                  {stage.groups.reduce((sum, item) => sum + item.group.cards.length, 0)} 条
                </span>
              </div>
              <For each={stage.groups}>{(item) => renderGroup(item)}</For>
            </>
          )}
        </For>
        <Show when={sections().isolated.length > 0}>
          <div class="clue-list-stage isolated">
            <span>孤立 · 未建立前后关系</span>
            <span class="clue-list-stage-count">{sections().isolated.length} 组</span>
          </div>
          <For each={sections().isolated}>{(item) => renderGroup(item)}</For>
        </Show>
      </div>
      <div class="clue-list-hint">点击「连接」再点目标线索建立前置 → 后续 · 前置 chips 上 × 断开 · Ctrl + 点击多选</div>
    </div>
  );
}
