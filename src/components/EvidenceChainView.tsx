import { confirm, message, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { api } from "../ipc";
import {
  addClueComment,
  associateClues,
  clearClueOpenRequest,
  clueCardById,
  clueCurrentVersion,
  clueMentionPeers,
  deleteClue,
  disassociateClues,
  markClueMentionRead,
  refreshClueGroups,
  splitClue,
  stackClues,
  startSessionFromClue,
  state,
} from "../store";
import type { ClueAttachment, ClueCard, ClueComment, ClueNodeGroup } from "../types";
import { ClueCaptureModal } from "./ClueCaptureModal";
import { attachmentPreviewSrc } from "./ImageAttachmentStrip";
import { IconClue, IconDownload, IconFile, IconPlus } from "./icons";
import { MentionPicker } from "./MentionPicker";

type Placement = "update" | "parallel" | "new";

function fmtTime(ts: number) {
  return new Date(ts).toLocaleString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function excerpt(text: string, max = 170) {
  const compact = text.replace(/\s+/g, " ").trim();
  return compact.length > max ? `${compact.slice(0, max)}…` : compact;
}

function authorName(name?: string) {
  return name?.trim() || "历史";
}

function authorBadge(name?: string) {
  const value = authorName(name);
  const characters = [...value];
  if (characters.length <= 3) return value;
  const words = value.split(/\s+/).filter(Boolean);
  if (words.length > 1) return words.slice(0, 2).map((word) => word[0]).join("").toUpperCase();
  return characters.slice(0, 2).join("");
}

export function EvidenceChainView() {
  const [selectedCardId, setSelectedCardId] = createSignal<string | null>(null);
  const [capture, setCapture] = createSignal<{
    placement: Placement;
    targetCardId: string | null;
  } | null>(null);
  const [selectedCardIds, setSelectedCardIds] = createSignal<Set<string>>(new Set());
  const [previewCardId, setPreviewCardId] = createSignal<string | null>(null);
  const [deletingCardId, setDeletingCardId] = createSignal<string | null>(null);
  const [splittingCardId, setSplittingCardId] = createSignal<string | null>(null);
  const [stacking, setStacking] = createSignal(false);
  const [connectFromId, setConnectFromId] = createSignal<string | null>(null);
  const [connectionBusy, setConnectionBusy] = createSignal(false);
  const [removingEdgeKey, setRemovingEdgeKey] = createSignal<string | null>(null);
  const [commentText, setCommentText] = createSignal("");
  const [commentMentions, setCommentMentions] = createSignal<string[]>([]);
  const [replyToCommentId, setReplyToCommentId] = createSignal<string | null>(null);
  const [commentBusy, setCommentBusy] = createSignal(false);
  let commentInputElement: HTMLTextAreaElement | undefined;
  let composingCardId: string | null | undefined;

  const cardToGroup = createMemo(() => {
    const map = new Map<string, ClueNodeGroup>();
    for (const group of state.clueGroups) {
      for (const card of group.cards) map.set(card.id, group);
    }
    return map;
  });

  const cards = createMemo(() =>
    state.clueGroups
      .flatMap((group) => group.cards)
      .sort((left, right) => right.updatedAt - left.updatedAt || right.createdAt - left.createdAt),
  );

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

  const selectCard = (cardId: string, event: MouseEvent) => {
    if (!event.ctrlKey && !event.metaKey) {
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

  const completeConnect = async (fromCardId: string, targetCardId: string) => {
    if (connectionBusy()) return;
    setConnectFromId(null);
    setConnectionBusy(true);
    try {
      await associateClues(fromCardId, targetCardId);
      selectOnly(targetCardId);
    } catch (error) {
      await message(String(error), { title: "连接失败", kind: "error" });
    } finally {
      setConnectionBusy(false);
    }
  };

  const onCardClick = (cardId: string, event: MouseEvent) => {
    const from = connectFromId();
    if (from) {
      if (from === cardId) setConnectFromId(null);
      else void completeConnect(from, cardId);
      return;
    }
    selectCard(cardId, event);
  };

  const removeConnection = async (fromCardId: string, toCardId: string, event: MouseEvent) => {
    event.stopPropagation();
    const key = `${fromCardId}->${toCardId}`;
    if (removingEdgeKey() === key) return;
    setRemovingEdgeKey(key);
    try {
      await disassociateClues(fromCardId, toCardId);
    } catch (error) {
      await message(String(error), { title: "删除连接失败", kind: "error" });
    } finally {
      setRemovingEdgeKey(null);
    }
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

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape") setConnectFromId(null);
  };

  onMount(() => {
    void refreshClueGroups();
    window.addEventListener("keydown", onKeyDown);
  });

  onCleanup(() => {
    window.removeEventListener("keydown", onKeyDown);
  });

  return (
    <main class="clue-view">
      <header class="clue-head">
        <div>
          <h1 class="clue-title">证据链</h1>
          <p class="clue-sub">
            像刷微博一样浏览线索：新线索在最上面，点卡片查看回帖、版本更新和上下文。
          </p>
        </div>
        <div class="clue-head-actions">
          <Show when={connectFromId()}>
            <button class="btn secondary" onClick={() => setConnectFromId(null)}>
              取消连接
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
        <div class="clue-feed-layout">
          <section class="clue-feed" classList={{ connecting: !!connectFromId() }}>
            <For each={cards()}>
              {(card) => {
                const version = () => clueCurrentVersion(card);
                const group = () => cardToGroup().get(card.id);
                const groupPreds = () =>
                  (group()?.parentCardIds ?? [])
                    .map((id) => clueCardById(id))
                    .filter((item): item is ClueCard => !!item);
                const commentCount = () => (card.comments ?? []).length;
                const attachmentCount = () => (version()?.attachments ?? []).length;
                const mentionCount = () => (version()?.mentions ?? []).length;
                const isActive = () => selectedCardId() === card.id;
                return (
                  <article
                    class="clue-feed-card"
                    classList={{
                      active: isActive(),
                      selected: selectedCardIds().has(card.id),
                      "connect-source": connectFromId() === card.id,
                    }}
                    role="button"
                    tabIndex={0}
                    onClick={(event) => onCardClick(card.id, event)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter" || event.key === " ") {
                        event.preventDefault();
                        if (!connectFromId()) selectOnly(card.id);
                      }
                    }}
                  >
                    <div class="clue-feed-side">
                      <span
                        class="clue-author-avatar"
                        title={`作者：${authorName(version()?.authorName)}`}
                        aria-label={`作者：${authorName(version()?.authorName)}`}
                      >
                        {authorBadge(version()?.authorName)}
                      </span>
                    </div>
                    <div class="clue-feed-main">
                      <div class="clue-feed-head">
                        <div class="clue-feed-author">
                          <strong>{authorName(version()?.authorName)}</strong>
                          <span class="clue-feed-time">{fmtTime(card.updatedAt)}</span>
                        </div>
                        <span class="clue-version-tag">v{card.versions.length}</span>
                      </div>

                      <h2 class="clue-feed-title">{version()?.title || "未命名线索"}</h2>
                      <p class="clue-feed-content">{excerpt(version()?.content ?? "", 220)}</p>

                      <Show when={groupPreds().length > 0}>
                        <div class="clue-quote">
                          <div class="clue-quote-title">前置线索</div>
                          <For each={groupPreds()}>
                            {(pred) => (
                              <button
                                type="button"
                                class="clue-quote-item"
                                onClick={(event) => {
                                  event.stopPropagation();
                                  selectOnly(pred.id);
                                }}
                              >
                                <span>{clueCurrentVersion(pred)?.title || "未命名线索"}</span>
                                <small>{authorName(clueCurrentVersion(pred)?.authorName)}</small>
                              </button>
                            )}
                          </For>
                        </div>
                      </Show>

                      <Show when={previewCardId() === card.id}>
                        <div class="clue-feed-preview">{version()?.content}</div>
                      </Show>

                      <div class="clue-feed-stats">
                        <span>{commentCount()} 评论</span>
                        <span>{attachmentCount()} 附件</span>
                        <span>{mentionCount()} 提醒</span>
                        <Show when={state.unreadClueMentions.includes(card.id)}>
                          <span class="mention">新提醒</span>
                        </Show>
                      </div>

                      <div class="clue-feed-actions">
                        <button
                          type="button"
                          class="btn secondary small"
                          onClick={(event) => {
                            event.stopPropagation();
                            setPreviewCardId(previewCardId() === card.id ? null : card.id);
                          }}
                        >
                          {previewCardId() === card.id ? "收起" : "预览"}
                        </button>
                        <Show when={!connectFromId()}>
                          <button
                            type="button"
                            class="btn secondary small"
                            disabled={connectionBusy()}
                            onClick={(event) => {
                              event.stopPropagation();
                              setConnectFromId(card.id);
                            }}
                          >
                            连接
                          </button>
                        </Show>
                      </div>
                    </div>
                  </article>
                );
              }}
            </For>
          </section>

          <Show when={selectedCard()}>
            {(card) => {
              const version = () => clueCurrentVersion(card());
              const comments = () => card().comments ?? [];
              const commentById = (id?: string | null) =>
                id ? comments().find((comment) => comment.id === id) : undefined;
              type TimelineEntry =
                | { kind: "comment"; at: number; floor: number; comment: ClueComment }
                | { kind: "version"; at: number; floor: number; versionIndex: number };
              const timeline = createMemo<TimelineEntry[]>(() => {
                const entries: TimelineEntry[] = comments().map((comment) => ({
                  kind: "comment",
                  at: comment.createdAt,
                  floor: 0,
                  comment,
                }));
                card().versions.forEach((item, index) => {
                  if (index === 0 || item.id === card().currentVersionId) return;
                  entries.push({ kind: "version", at: item.createdAt, floor: 0, versionIndex: index });
                });
                entries.sort((left, right) => left.at - right.at);
                entries.forEach((entry, index) => {
                  entry.floor = index + 2;
                });
                return entries;
              });
              return (
                <aside class="clue-detail">
                  <header class="clue-post-head">
                    <span class="clue-author-avatar" title={`作者：${authorName(version()?.authorName)}`}>
                      {authorBadge(version()?.authorName)}
                    </span>
                    <div class="clue-post-byline">
                      <strong>{authorName(version()?.authorName)}</strong>
                      <span class="clue-post-tag">楼主</span>
                    </div>
                    <div class="clue-post-head-main">
                      <h2>{version()?.title || "未命名线索"}</h2>
                      <span class="clue-detail-meta">
                        1 楼 · {fmtTime(card().updatedAt)} · v{card().versions.length} · {comments().length} 条评论
                      </span>
                    </div>
                  </header>
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
                      <div class="clue-section-title">评论</div>
                      <span>{timeline().length}</span>
                    </div>
                    <Show
                      when={timeline().length > 0}
                      fallback={<div class="clue-comments-empty">还没有评论，来抢沙发</div>}
                    >
                      <div class="clue-timeline">
                        <For each={timeline()}>
                          {(entry) => (
                            <Show
                              when={entry.kind === "comment" ? entry : undefined}
                              fallback={
                                <Show when={entry.kind === "version" ? entry : undefined}>
                                  {(item) => {
                                    const itemVersion = () => card().versions[item().versionIndex];
                                    const isCurrent = () => itemVersion()?.id === card().currentVersionId;
                                    return (
                                      <article class="clue-floor version">
                                        <span class="clue-floor-no">{item().floor} 楼</span>
                                        <div class="clue-floor-body">
                                          <div class="clue-comment-head">
                                            <span
                                              class="clue-author-avatar"
                                              title={`作者：${authorName(itemVersion()?.authorName)}`}
                                            >
                                              {authorBadge(itemVersion()?.authorName)}
                                            </span>
                                            <strong>{authorName(itemVersion()?.authorName)}</strong>
                                            <span class="clue-post-tag">更新</span>
                                            <time>{fmtTime(item().at)}</time>
                                          </div>
                                          <div class="clue-floor-version">
                                            <strong>
                                              更新到 v{item().versionIndex + 1}
                                              {isCurrent() ? "（当前）" : ""}：{itemVersion()?.title || "未命名线索"}
                                            </strong>
                                            <span>{excerpt(itemVersion()?.content ?? "", 120)}</span>
                                          </div>
                                        </div>
                                      </article>
                                    );
                                  }}
                                </Show>
                              }
                            >
                              {(item) => {
                                const comment = () => item().comment;
                                const parent = () => commentById(comment().parentCommentId);
                                return (
                                  <article
                                    class="clue-floor"
                                    classList={{ reply: !!comment().parentCommentId }}
                                  >
                                    <span class="clue-floor-no">{item().floor} 楼</span>
                                    <div class="clue-floor-body">
                                      <div class="clue-comment-head">
                                        <span
                                          class="clue-author-avatar"
                                          title={`作者：${authorName(comment().authorName)}`}
                                        >
                                          {authorBadge(comment().authorName)}
                                        </span>
                                        <strong>{authorName(comment().authorName)}</strong>
                                        <time>{fmtTime(comment().createdAt)}</time>
                                      </div>
                                      <Show when={parent()}>
                                        {(target) => (
                                          <blockquote class="clue-comment-quote">
                                            <strong>@{authorName(target().authorName)}</strong>
                                            <span>{target().content}</span>
                                          </blockquote>
                                        )}
                                      </Show>
                                      <Show when={(comment().mentions ?? []).length > 0}>
                                        <div class="clue-comment-mentions">
                                          <For each={comment().mentions ?? []}>
                                            {(mention) => <span>@{mention.name}</span>}
                                          </For>
                                        </div>
                                      </Show>
                                      <p>{comment().content}</p>
                                      <button
                                        type="button"
                                        class="clue-comment-reply"
                                        onClick={() => beginReply(comment())}
                                      >
                                        回复
                                      </button>
                                    </div>
                                  </article>
                                );
                              }}
                            </Show>
                          )}
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
                          {commentBusy() ? "发送中…" : replyTarget() ? "发送��复" : "发表评论"}
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
