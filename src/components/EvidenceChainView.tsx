import { createEffect, createMemo, createSignal, For, onMount, Show } from "solid-js";
import {
  clueCardById,
  clueCurrentVersion,
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

export function EvidenceChainView() {
  const [selectedCardId, setSelectedCardId] = createSignal<string | null>(null);
  const [capture, setCapture] = createSignal<{
    placement: Placement;
    targetCardId: string | null;
  } | null>(null);
  const [showAssociate, setShowAssociate] = createSignal(false);

  onMount(() => void refreshClueGroups());

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
          <div class="clue-board">
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
                                onClick={() => setSelectedCardId(card.id)}
                              >
                                <div class="clue-card-head">
                                  <span class="clue-card-title">{version()?.title || "未命名线索"}</span>
                                  <Show when={card.versions.length > 1}>
                                    <span class="clue-version-badge">v{card.versions.length}</span>
                                  </Show>
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
                    <span>{fmtTime(card().updatedAt)} · {card().versions.length} 个版本</span>
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
