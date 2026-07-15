import { confirm, message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import {
  clueCardById,
  clueCurrentVersion,
  deleteClue,
  refreshClueGroups,
  startSessionFromClue,
  state,
} from "../store";
import type { ClueCard, ClueNodeGroup } from "../types";
import { ClueAssociateModal } from "./ClueAssociateModal";
import { ClueCaptureModal } from "./ClueCaptureModal";
import { IconClue, IconPlus } from "./icons";

type Placement = "update" | "parallel" | "new";

function fmtTime(ts: number) {
  return new Date(ts).toLocaleString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function excerpt(text: string) {
  const compact = text.replace(/\s+/g, " ").trim();
  return compact.length > 110 ? `${compact.slice(0, 110)}…` : compact;
}

function authorName(name?: string) {
  return name?.trim() || "未知";
}

function authorBadge(name?: string) {
  const value = authorName(name);
  const characters = [...value];
  if (characters.length <= 3) return value;
  const words = value.split(/\s+/).filter(Boolean);
  if (words.length > 1) return words.slice(0, 2).map((word) => word[0]).join("").toUpperCase();
  return characters.slice(0, 2).join("");
}

type ClueEdge = {
  from: string;
  to: string;
  path: string;
};

export function EvidenceChainView() {
  const [selectedCardId, setSelectedCardId] = createSignal<string | null>(null);
  const [capture, setCapture] = createSignal<{
    placement: Placement;
    targetCardId: string | null;
  } | null>(null);
  const [showAssociate, setShowAssociate] = createSignal(false);
  const [deletingCardId, setDeletingCardId] = createSignal<string | null>(null);
  const [edgeCanvas, setEdgeCanvas] = createSignal({ width: 0, height: 0, edges: [] as ClueEdge[] });
  const cardElements = new Map<string, HTMLButtonElement>();
  let boardElement: HTMLDivElement | undefined;
  let resizeObserver: ResizeObserver | undefined;
  let edgeFrame: number | undefined;

  const updateEdges = () => {
    const board = boardElement;
    if (!board) return;
    const boardRect = board.getBoundingClientRect();
    const edges: ClueEdge[] = [];
    for (const group of state.clueGroups) {
      for (const parentId of group.parentCardIds) {
        const parent = cardElements.get(parentId);
        if (!parent) continue;
        const parentRect = parent.getBoundingClientRect();
        for (const card of group.cards) {
          const child = cardElements.get(card.id);
          if (!child) continue;
          const childRect = child.getBoundingClientRect();
          const startX = parentRect.right - boardRect.left + board.scrollLeft + 5;
          const startY = parentRect.top - boardRect.top + board.scrollTop + parentRect.height / 2;
          const endX = childRect.left - boardRect.left + board.scrollLeft - 10;
          const endY = childRect.top - boardRect.top + board.scrollTop + childRect.height / 2;
          const curve = Math.max(18, (endX - startX) / 2);
          edges.push({
            from: parentId,
            to: card.id,
            path: `M ${startX} ${startY} C ${startX + curve} ${startY}, ${endX - curve} ${endY}, ${endX} ${endY}`,
          });
        }
      }
    }
    setEdgeCanvas({ width: board.scrollWidth, height: board.scrollHeight, edges });
  };

  const scheduleEdgeUpdate = () => {
    if (edgeFrame !== undefined) cancelAnimationFrame(edgeFrame);
    edgeFrame = requestAnimationFrame(() => {
      edgeFrame = undefined;
      updateEdges();
    });
  };

  const registerCard = (cardId: string, element: HTMLButtonElement) => {
    cardElements.set(cardId, element);
    resizeObserver?.observe(element);
    scheduleEdgeUpdate();
  };

  onMount(() => {
    void refreshClueGroups();
    resizeObserver = new ResizeObserver(scheduleEdgeUpdate);
    if (boardElement) resizeObserver.observe(boardElement);
    for (const element of cardElements.values()) resizeObserver.observe(element);
    window.addEventListener("resize", scheduleEdgeUpdate);
    scheduleEdgeUpdate();
  });

  onCleanup(() => {
    if (edgeFrame !== undefined) cancelAnimationFrame(edgeFrame);
    resizeObserver?.disconnect();
    window.removeEventListener("resize", scheduleEdgeUpdate);
  });

  const cardToGroup = createMemo(() => {
    const map = new Map<string, ClueNodeGroup>();
    for (const group of state.clueGroups) {
      for (const card of group.cards) map.set(card.id, group);
    }
    return map;
  });

  const cards = createMemo(() => state.clueGroups.flatMap((group) => group.cards));

  createEffect(() => {
    const available = cards();
    const selected = selectedCardId();
    if (selected && available.some((card) => card.id === selected)) return;
    const preferred = state.pendingClueCard?.id;
    setSelectedCardId(
      (preferred && available.some((card) => card.id === preferred) ? preferred : available[0]?.id) ?? null,
    );
  });

  const stages = createMemo(() => {
    const byCard = cardToGroup();
    const memo = new Map<string, number>();
    const depth = (group: ClueNodeGroup, visiting = new Set<string>()): number => {
      const cached = memo.get(group.id);
      if (cached !== undefined) return cached;
      if (visiting.has(group.id)) return 0;
      const nextVisiting = new Set(visiting);
      nextVisiting.add(group.id);
      const value = group.parentCardIds.length
        ? Math.max(
            0,
            ...group.parentCardIds.map((parentId) => {
              const parentGroup = byCard.get(parentId);
              return parentGroup ? depth(parentGroup, nextVisiting) + 1 : 0;
            }),
          )
        : 0;
      memo.set(group.id, value);
      return value;
    };
    const map = new Map<number, ClueNodeGroup[]>();
    for (const group of state.clueGroups) {
      const value = depth(group);
      map.set(value, [...(map.get(value) ?? []), group]);
    }
    return [...map.entries()]
      .sort(([left], [right]) => left - right)
      .map(([value, groups]) => ({
        depth: value,
        groups: groups.sort((left, right) => left.createdAt - right.createdAt),
      }));
  });

  createEffect(() => {
    const activeIds = new Set(cards().map((card) => card.id));
    stages();
    for (const [cardId, element] of cardElements) {
      if (!activeIds.has(cardId)) {
        resizeObserver?.unobserve(element);
        cardElements.delete(cardId);
      }
    }
    scheduleEdgeUpdate();
  });

  const selectedCard = createMemo(() => clueCardById(selectedCardId()));
  const selectedGroup = createMemo(() => {
    const id = selectedCardId();
    return id ? cardToGroup().get(id) : undefined;
  });
  const predecessors = createMemo(() =>
    (selectedGroup()?.parentCardIds ?? [])
      .map((id) => clueCardById(id))
      .filter((card): card is ClueCard => !!card),
  );
  const successors = createMemo(() => {
    const id = selectedCardId();
    if (!id) return [];
    return state.clueGroups
      .filter((group) => group.parentCardIds.includes(id))
      .flatMap((group) => group.cards);
  });

  const removeCard = async (card: ClueCard) => {
    const title = clueCurrentVersion(card)?.title || "未命名线索";
    const accepted = await confirm(`删除线索「${title}」？下游线索会保留，但不再以它作为前置。`, {
      title: "删除线索",
      kind: "warning",
    });
    if (!accepted) return;
    setDeletingCardId(card.id);
    try {
      await deleteClue(card.id);
    } catch (error) {
      await message(String(error), { title: "删除失败", kind: "error" });
    } finally {
      setDeletingCardId(null);
    }
  };

  return (
    <main class="clue-view">
      <header class="clue-head">
        <div>
          <h1 class="clue-title">证据链</h1>
          <p class="clue-sub">把会话结论沉淀成线索，并沿前后顺序继续发起更精确的会话。</p>
        </div>
        <div class="clue-head-actions">
          <Show when={cards().length > 1}>
            <button class="btn secondary" onClick={() => setShowAssociate(true)}>
              关联线索
            </button>
          </Show>
          <button class="btn primary" onClick={() => setCapture({ placement: "new", targetCardId: null })}>
            <IconPlus size={14} />
            新建线索
          </button>
        </div>
      </header>

      <Show
        when={cards().length > 0}
        fallback={
          <div class="clue-empty">
            <IconClue size={34} />
            <p>还没有线索。</p>
            <span>完成一轮普通会话后点击“生成线索”，或在这里新建第一条线索。</span>
          </div>
        }
      >
        <div class="clue-layout">
          <div class="clue-board" ref={(element) => (boardElement = element)}>
            <svg
              class="clue-edges"
              width={edgeCanvas().width}
              height={edgeCanvas().height}
              aria-hidden="true"
            >
              <defs>
                <marker
                  id="clue-arrow"
                  viewBox="0 0 10 10"
                  refX="8"
                  refY="5"
                  markerWidth="7"
                  markerHeight="7"
                  orient="auto-start-reverse"
                >
                  <path d="M 0 0 L 10 5 L 0 10 z" />
                </marker>
                <marker
                  id="clue-arrow-active"
                  viewBox="0 0 10 10"
                  refX="8"
                  refY="5"
                  markerWidth="7"
                  markerHeight="7"
                  orient="auto-start-reverse"
                >
                  <path d="M 0 0 L 10 5 L 0 10 z" />
                </marker>
              </defs>
              <For each={edgeCanvas().edges}>
                {(edge) => {
                  const active = () => selectedCardId() === edge.from || selectedCardId() === edge.to;
                  return (
                    <path
                      classList={{ "clue-edge": true, active: active() }}
                      d={edge.path}
                      marker-end={active() ? "url(#clue-arrow-active)" : "url(#clue-arrow)"}
                    />
                  );
                }}
              </For>
            </svg>
            <For each={stages()}>
              {(stage) => (
                <section class="clue-stage">
                  <div class="clue-stage-title">{stage.depth === 0 ? "起点" : `第 ${stage.depth + 1} 步`}</div>
                  <For each={stage.groups}>
                    {(group) => (
                      <div class="clue-parallel-set">
                        <For each={group.cards}>
                          {(card) => {
                            const version = () => clueCurrentVersion(card);
                            return (
                              <button
                                type="button"
                                classList={{ "clue-card": true, active: selectedCardId() === card.id }}
                                ref={(element) => registerCard(card.id, element)}
                                onClick={() => setSelectedCardId(card.id)}
                              >
                                <div class="clue-card-head">
                                  <span
                                    class="clue-author-avatar"
                                    title={`作者：${authorName(version()?.authorName)}`}
                                    aria-label={`作者：${authorName(version()?.authorName)}`}
                                  >
                                    {authorBadge(version()?.authorName)}
                                  </span>
                                  <span class="clue-card-heading">
                                    <span class="clue-card-title">{version()?.title || "未命名线索"}</span>
                                    <Show when={card.versions.length > 1}>
                                      <span class="clue-version-badge">v{card.versions.length}</span>
                                    </Show>
                                  </span>
                                </div>
                                <div class="clue-card-body">{excerpt(version()?.content ?? "")}</div>
                                <div class="clue-card-meta">{fmtTime(card.updatedAt)}</div>
                              </button>
                            );
                          }}
                        </For>
                      </div>
                    )}
                  </For>
                </section>
              )}
            </For>
          </div>

          <Show when={selectedCard()}>
            {(card) => {
              const version = () => clueCurrentVersion(card());
              return (
                <aside class="clue-detail">
                  <div class="clue-detail-head">
                    <span class="clue-detail-kicker">ClueCard</span>
                    <h2>{version()?.title || "未命名线索"}</h2>
                    <div class="clue-detail-author">
                      <span class="clue-author-avatar" title={`作者：${authorName(version()?.authorName)}`}>
                        {authorBadge(version()?.authorName)}
                      </span>
                      <span>{authorName(version()?.authorName)}</span>
                    </div>
                    <span class="clue-detail-meta">{fmtTime(card().updatedAt)} · {card().versions.length} 个版本</span>
                  </div>
                  <pre class="clue-detail-content">{version()?.content}</pre>

                  <div class="clue-detail-actions">
                    <button class="btn primary" onClick={() => startSessionFromClue(card())}>
                      沿此线索发起会话
                    </button>
                    <button
                      class="btn secondary"
                      onClick={() => setCapture({ placement: "update", targetCardId: card().id })}
                    >
                      更新
                    </button>
                    <button
                      class="btn secondary"
                      onClick={() => setCapture({ placement: "parallel", targetCardId: card().id })}
                    >
                      平行后续
                    </button>
                    <button
                      class="btn secondary"
                      onClick={() => setCapture({ placement: "new", targetCardId: card().id })}
                    >
                      开启下一条
                    </button>
                    <button
                      class="btn danger"
                      disabled={deletingCardId() === card().id}
                      onClick={() => void removeCard(card())}
                    >
                      {deletingCardId() === card().id ? "删除中…" : "删除线索"}
                    </button>
                  </div>

                  <Show when={predecessors().length > 0}>
                    <div class="clue-links">
                      <div class="clue-section-title">前置线索</div>
                      <For each={predecessors()}>
                        {(item) => (
                          <button onClick={() => setSelectedCardId(item.id)}>
                            {clueCurrentVersion(item)?.title || "未命名线索"}
                          </button>
                        )}
                      </For>
                    </div>
                  </Show>
                  <Show when={successors().length > 0}>
                    <div class="clue-links">
                      <div class="clue-section-title">后续线索</div>
                      <For each={successors()}>
                        {(item) => (
                          <button onClick={() => setSelectedCardId(item.id)}>
                            {clueCurrentVersion(item)?.title || "未命名线索"}
                          </button>
                        )}
                      </For>
                    </div>
                  </Show>

                  <Show when={card().versions.length > 1}>
                    <div class="clue-history">
                      <div class="clue-section-title">版本记录</div>
                      <For each={[...card().versions].reverse()}>
                        {(item, index) => (
                          <div class="clue-history-item">
                            <span>v{card().versions.length - index()}</span>
                            <strong>{item.title}</strong>
                            <time>{fmtTime(item.createdAt)}</time>
                          </div>
                        )}
                      </For>
                    </div>
                  </Show>
                </aside>
              );
            }}
          </Show>
        </div>
      </Show>

      <Show when={capture()}>
        {(value) => (
          <ClueCaptureModal
            initialPlacement={value().placement}
            initialTargetCardId={value().targetCardId}
            onClose={() => setCapture(null)}
          />
        )}
      </Show>
      <Show when={showAssociate()}>
        <ClueAssociateModal
          initialBeforeCardId={selectedCardId()}
          onClose={() => setShowAssociate(false)}
        />
      </Show>
    </main>
  );
}
