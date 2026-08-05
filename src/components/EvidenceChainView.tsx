import { confirm, message, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, onMount, Show } from "solid-js";
import { computeClueStructure, authorBadge, authorName, fmtTime } from "../clueGraph";
import { api } from "../ipc";
import {
  addClueComment,
  clearClueOpenRequest,
  clueCardById,
  clueCurrentVersion,
  clueMentionPeers,
  deleteClue,
  markClueMentionRead,
  refreshClueGroups,
  splitClue,
  stackClues,
  startSessionFromClue,
  state,
} from "../store";
import type { ClueAttachment, ClueCard, ClueComment } from "../types";
import { ClueCaptureModal } from "./ClueCaptureModal";
import { EvidenceCanvasView } from "./EvidenceCanvasView";
import { EvidenceListView } from "./EvidenceListView";
import { attachmentPreviewSrc } from "./ImageAttachmentStrip";
import { IconClue, IconDownload, IconFile, IconPlus } from "./icons";
import { MentionPicker } from "./MentionPicker";

type Placement = "update" | "parallel" | "new";
type ViewMode = "list" | "canvas";

const VIEW_MODE_KEY = "nova.clueViewMode";

function initialViewMode(): ViewMode {
  try {
    return localStorage.getItem(VIEW_MODE_KEY) === "canvas" ? "canvas" : "list";
  } catch {
    return "list";
  }
}

export function EvidenceChainView() {
  const [viewMode, setViewMode] = createSignal<ViewMode>(initialViewMode());
  const [selectedCardId, setSelectedCardId] = createSignal<string | null>(null);
  const [capture, setCapture] = createSignal<{
    placement: Placement;
    targetCardId: string | null;
  } | null>(null);
  const [selectedCardIds, setSelectedCardIds] = createSignal<Set<string>>(new Set());
  const [deletingCardId, setDeletingCardId] = createSignal<string | null>(null);
  const [splittingCardId, setSplittingCardId] = createSignal<string | null>(null);
  const [stacking, setStacking] = createSignal(false);
  const [commentText, setCommentText] = createSignal("");
  const [commentMentions, setCommentMentions] = createSignal<string[]>([]);
  const [replyToCommentId, setReplyToCommentId] = createSignal<string | null>(null);
  const [commentBusy, setCommentBusy] = createSignal(false);
  let commentInputElement: HTMLTextAreaElement | undefined;
  let composingCardId: string | null | undefined;

  const switchView = (mode: ViewMode) => {
    setViewMode(mode);
    try {
      localStorage.setItem(VIEW_MODE_KEY, mode);
    } catch {
      // 忽略存储失败，本次会话内仍生效
    }
  };

  // 拓扑结构由共享模块计算，列表与画布两种视图复用同一份结果。
  const structure = createMemo(() => computeClueStructure(state.clueGroups));
  const cards = createMemo(() => state.clueGroups.flatMap((group) => group.cards));

  const selectedCard = createMemo(() => clueCardById(selectedCardId()));
  const selectedGroup = createMemo(() => {
    const id = selectedCardId();
    return id ? structure().cardToGroup.get(id) : undefined;
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
  const mentionPeers = createMemo(clueMentionPeers);
  const replyTarget = createMemo(() => {
    const replyId = replyToCommentId();
    return replyId
      ? (selectedCard()?.comments ?? []).find((comment) => comment.id === replyId)
      : undefined;
  });

  const selectOnly = (cardId: string) => {
    markClueMentionRead(cardId);
    setSelectedCardId(cardId);
    setSelectedCardIds(new Set([cardId]));
  };

  const selectCard = (cardId: string, event?: MouseEvent) => {
    if (!event || (!event.ctrlKey && !event.metaKey)) {
      selectOnly(cardId);
      return;
    }
    const next = new Set(selectedCardIds());
    if (next.has(cardId)) next.delete(cardId);
    else next.add(cardId);
    if (next.size === 0) next.add(cardId);
    setSelectedCardIds(next);
    setSelectedCardId(cardId);
  };

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

  const downloadAttachment = async (attachment: ClueAttachment) => {
    try {
      const target = await saveDialog({
        title: "保存附件",
        defaultPath: attachment.name || "attachment",
      });
      if (!target) return;
      await api.saveClueAttachment(attachment, target);
    } catch (error) {
      await message(String(error), { title: "下载失败", kind: "error" });
    }
  };

  const splitSelectedCard = async (card: ClueCard) => {
    if (splittingCardId()) return;
    setSplittingCardId(card.id);
    try {
      await splitClue(card.id);
      setSelectedCardId(card.id);
      setSelectedCardIds(new Set([card.id]));
    } catch (error) {
      await message(String(error), { title: "拆分失败", kind: "error" });
    } finally {
      setSplittingCardId(null);
    }
  };

  const stackSelectedCards = async () => {
    const cardIds = [...selectedCardIds()];
    if (cardIds.length < 2 || stacking()) return;
    setStacking(true);
    try {
      await stackClues(cardIds);
      const selected = selectedCardId() ?? cardIds[0];
      setSelectedCardIds(new Set([selected]));
    } catch (error) {
      await message(String(error), { title: "堆叠失败", kind: "error" });
    } finally {
      setStacking(false);
    }
  };

  const beginReply = (comment: ClueComment) => {
    setReplyToCommentId(comment.id);
    const myToken = state.settings?.relayToken ?? "";
    setCommentMentions(
      comment.authorToken && comment.authorToken !== myToken ? [comment.authorToken] : [],
    );
    requestAnimationFrame(() => commentInputElement?.focus());
  };

  const cancelReply = () => {
    setReplyToCommentId(null);
    setCommentMentions([]);
  };

  const submitComment = async () => {
    const card = selectedCard();
    const content = commentText().trim();
    if (!card || !content || commentBusy()) return;
    setCommentBusy(true);
    try {
      await addClueComment(card.id, content, replyToCommentId(), commentMentions());
      setCommentText("");
      setCommentMentions([]);
      setReplyToCommentId(null);
    } catch (error) {
      await message(String(error), { title: "评论失败", kind: "error" });
    } finally {
      setCommentBusy(false);
    }
  };

  createEffect(() => {
    const cardId = selectedCardId();
    if (composingCardId !== undefined && composingCardId !== cardId) {
      setCommentText("");
      setCommentMentions([]);
      setReplyToCommentId(null);
    }
    composingCardId = cardId;
  });

  createEffect(() => {
    const available = cards();
    const request = state.clueOpenRequest;
    if (request && available.some((card) => card.id === request)) {
      setSelectedCardId(request);
      setSelectedCardIds(new Set([request]));
      clearClueOpenRequest(request);
      return;
    }
    const selected = selectedCardId();
    if (selected && available.some((card) => card.id === selected)) {
      const availableIds = new Set(available.map((card) => card.id));
      const next = new Set([...selectedCardIds()].filter((cardId) => availableIds.has(cardId)));
      if (next.size !== selectedCardIds().size) setSelectedCardIds(next);
      return;
    }
    const preferred = state.pendingClueCard?.id;
    const next = (preferred && available.some((card) => card.id === preferred) ? preferred : available[0]?.id) ?? null;
    setSelectedCardId(next);
    setSelectedCardIds(new Set(next ? [next] : []));
  });

  onMount(() => {
    void refreshClueGroups();
  });

  return (
    <main class="clue-view">
      <header class="clue-head">
        <div>
          <h1 class="clue-title">证据链</h1>
          <p class="clue-sub">
            {viewMode() === "list"
              ? "层级缩进表达前置 → 后续；点「连接」再点目标线索即可建立关系。"
              : "拖动空白处平移，滚轮缩放；拖动卡牌调整位置，从右侧连接点建立顺序。"}
          </p>
        </div>
        <div class="clue-head-actions">
          <div class="clue-view-switch" role="tablist" aria-label="证据链视图切换">
            <button
              type="button"
              role="tab"
              classList={{ active: viewMode() === "list" }}
              onClick={() => switchView("list")}
            >
              列表
            </button>
            <button
              type="button"
              role="tab"
              classList={{ active: viewMode() === "canvas" }}
              onClick={() => switchView("canvas")}
            >
              关系图
            </button>
          </div>
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
          {viewMode() === "list" ? (
            <EvidenceListView
              structure={structure()}
              selectedCardId={selectedCardId()}
              selectedCardIds={selectedCardIds()}
              onSelectCard={selectCard}
              onFocusCard={selectOnly}
            />
          ) : (
            <EvidenceCanvasView
              structure={structure()}
              selectedCardId={selectedCardId()}
              onSelectCard={selectCard}
            />
          )}

          <Show when={selectedCard()}>
            {(card) => {
              const version = () => clueCurrentVersion(card());
              const comments = () => card().comments ?? [];
              const commentById = (id?: string | null) =>
                id ? comments().find((comment) => comment.id === id) : undefined;
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
                  <Show when={(version()?.attachments ?? []).length > 0}>
                    <div class="clue-attachments">
                      <div class="clue-section-title">附件</div>
                      <div class="clue-attachment-grid">
                        <For each={version()?.attachments ?? []}>
                          {(attachment) => (
                            <div
                              classList={{
                                "clue-attachment": true,
                                image: attachment.mimeType.startsWith("image/"),
                              }}
                            >
                              <button
                                type="button"
                                class="clue-attachment-open"
                                title={`打开 ${attachment.name}`}
                                onClick={() =>
                                  void api.openClueAttachment(attachment).catch((error) =>
                                    message(String(error), { kind: "error" }),
                                  )
                                }
                              >
                                <Show
                                  when={attachment.mimeType.startsWith("image/")}
                                  fallback={<IconFile size={28} />}
                                >
                                  <img
                                    src={attachmentPreviewSrc(attachment)}
                                    alt={attachment.name}
                                    draggable={false}
                                  />
                                </Show>
                                <span>{attachment.name}</span>
                              </button>
                              <button
                                type="button"
                                class="icon-btn clue-attachment-download"
                                title={`下载 ${attachment.name}`}
                                onClick={() => void downloadAttachment(attachment)}
                              >
                                <IconDownload size={13} />
                              </button>
                            </div>
                          )}
                        </For>
                      </div>
                    </div>
                  </Show>
                  <Show when={(version()?.mentions ?? []).length > 0}>
                    <div class="clue-mention-summary">
                      <span>本次提醒</span>
                      <For each={version()?.mentions ?? []}>
                        {(mention) => <strong>@{mention.name}</strong>}
                      </For>
                    </div>
                  </Show>

                  <div class="clue-detail-actions">
                    <Show when={selectedCardIds().size > 1}>
                      <button class="btn primary" disabled={stacking()} onClick={() => void stackSelectedCards()}>
                        {stacking() ? "堆叠中…" : `堆叠所选（${selectedCardIds().size}）`}
                      </button>
                    </Show>
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
                      堆叠线索
                    </button>
                    <Show when={(selectedGroup()?.cards.length ?? 0) > 1}>
                      <button
                        class="btn secondary"
                        disabled={splittingCardId() === card().id}
                        onClick={() => void splitSelectedCard(card())}
                      >
                        {splittingCardId() === card().id ? "拆分中…" : "拆分"}
                      </button>
                    </Show>
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

                  <div class="clue-comments">
                    <div class="clue-comments-head">
                      <div class="clue-section-title">评论与回复</div>
                      <span>{comments().length}</span>
                    </div>
                    <Show
                      when={comments().length > 0}
                      fallback={<div class="clue-comments-empty">还没有评论</div>}
                    >
                      <div class="clue-comment-list">
                        <For each={comments()}>
                          {(item) => {
                            const parent = () => commentById(item.parentCommentId);
                            return (
                              <article
                                class="clue-comment"
                                classList={{ reply: !!item.parentCommentId }}
                              >
                                <div class="clue-comment-head">
                                  <span
                                    class="clue-author-avatar"
                                    title={`作者：${authorName(item.authorName)}`}
                                  >
                                    {authorBadge(item.authorName)}
                                  </span>
                                  <strong>{authorName(item.authorName)}</strong>
                                  <time>{fmtTime(item.createdAt)}</time>
                                </div>
                                <Show when={parent()}>
                                  {(target) => (
                                    <blockquote class="clue-comment-quote">
                                      <strong>@{authorName(target().authorName)}</strong>
                                      <span>{target().content}</span>
                                    </blockquote>
                                  )}
                                </Show>
                                <Show when={(item.mentions ?? []).length > 0}>
                                  <div class="clue-comment-mentions">
                                    <For each={item.mentions ?? []}>
                                      {(mention) => <span>@{mention.name}</span>}
                                    </For>
                                  </div>
                                </Show>
                                <p>{item.content}</p>
                                <button
                                  type="button"
                                  class="clue-comment-reply"
                                  onClick={() => beginReply(item)}
                                >
                                  回复
                                </button>
                              </article>
                            );
                          }}
                        </For>
                      </div>
                    </Show>

                    <div class="clue-comment-composer">
                      <Show when={replyTarget()}>
                        {(target) => (
                          <div class="clue-comment-replying">
                            <span>回复 @{authorName(target().authorName)}</span>
                            <button type="button" onClick={cancelReply}>
                              取消回复
                            </button>
                          </div>
                        )}
                      </Show>
                      <textarea
                        ref={commentInputElement}
                        class="field-input"
                        rows={3}
                        value={commentText()}
                        disabled={commentBusy()}
                        placeholder={replyTarget() ? "写下回复…" : "写下评论…"}
                        onInput={(event) => setCommentText(event.currentTarget.value)}
                      />
                      <MentionPicker
                        peers={mentionPeers()}
                        selectedTokens={commentMentions()}
                        disabled={commentBusy() || mentionPeers().length === 0}
                        placeholder={mentionPeers().length > 0 ? "@ 提醒团队成员" : "暂无可提醒的团队成员"}
                        onChange={setCommentMentions}
                      />
                      <div class="clue-comment-submit-row">
                        <span class="field-hint">回复会自动 @ 原评论作者。</span>
                        <button
                          type="button"
                          class="btn primary small"
                          disabled={commentBusy() || !commentText().trim()}
                          onClick={() => void submitComment()}
                        >
                          {commentBusy() ? "发送中…" : replyTarget() ? "发送回复" : "发表评论"}
                        </button>
                      </div>
                    </div>
                  </div>

                  <Show when={predecessors().length > 0}>
                    <div class="clue-links">
                      <div class="clue-section-title">前置线索</div>
                      <For each={predecessors()}>
                        {(item) => (
                          <button onClick={() => selectOnly(item.id)}>
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
                          <button onClick={() => selectOnly(item.id)}>
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
    </main>
  );
}
