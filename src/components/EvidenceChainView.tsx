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
  setClueSpace,
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

function excerpt(text: string, max = 220) {
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

type TimelineEntry =
  | { kind: "comment"; at: number; comment: ClueComment }
  | { kind: "version"; at: number; versionIndex: number };

export function EvidenceChainView() {
  const [selectedCardId, setSelectedCardId] = createSignal<string | null>(null);
  const [capture, setCapture] = createSignal<{
    placement: Placement;
    targetCardId: string | null;
  } | null>(null);
  const [deletingCardId, setDeletingCardId] = createSignal<string | null>(null);
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

  // 微博流：按最近更新倒序，新线索在最上面。
  const cards = createMemo(() =>
    state.clueGroups
      .flatMap((group) => group.cards)
      .sort((left, right) => right.updatedAt - left.updatedAt || right.createdAt - left.createdAt),
  );

  const successorsOf = (cardId: string): ClueCard[] =>
    state.clueGroups
      .filter((group) => group.parentCardIds.includes(cardId))
      .flatMap((group) => group.cards);

  const mentionPeers = createMemo(clueMentionPeers);
  const replyTarget = createMemo(() => {
    const replyId = replyToCommentId();
    const card = clueCardById(selectedCardId());
    return replyId ? (card?.comments ?? []).find((comment) => comment.id === replyId) : undefined;
  });

  const selectOnly = (cardId: string | null) => {
    if (cardId) markClueMentionRead(cardId);
    setSelectedCardId(cardId);
  };

  // 微博式：点卡片展开/收起；不存在多选——线索关系本来就是一棵回复树。
  const selectCard = (cardId: string, _event: MouseEvent) => {
    selectOnly(selectedCardId() === cardId ? null : cardId);
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
      if (selectedCardId() === card.id) selectOnly(null);
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

  const submitComment = async (cardId: string) => {
    const content = commentText().trim();
    if (!content || commentBusy()) return;
    setCommentBusy(true);
    try {
      await addClueComment(cardId, content, replyToCommentId(), commentMentions());
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
      clearClueOpenRequest(request);
      return;
    }
    const selected = selectedCardId();
    if (selected && available.some((card) => card.id === selected)) return;
    if (selected && !available.some((card) => card.id === selected)) {
      setSelectedCardId(null);
      return;
    }
    if (!selected) {
      const preferred = state.pendingClueCard?.id;
      if (preferred && available.some((card) => card.id === preferred)) {
        setSelectedCardId(preferred);
      }
    }
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
            像刷微博一样浏览线索：点卡片原地展开全文与回帖；线索之间的连接以回帖的形式呈现。
          </p>
        </div>
        <div class="clue-head-actions">
          <div class="clue-space-switch" role="tablist" aria-label="证据链空间">
            <button
              classList={{ active: state.clueSpace === "personal" }}
              disabled={state.clueSpace === "personal"}
              onClick={() => void setClueSpace("personal")}
            >
              个人空间
            </button>
            <button
              classList={{ active: state.clueSpace === "team" }}
              disabled={state.clueSpace === "team" || !state.relay.enabled}
              title={state.relay.enabled ? "团队共享的证据链" : "请先配置团队中转站"}
              onClick={() => void setClueSpace("team")}
            >
              团队空间
            </button>
          </div>
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
        <div class="clue-feed" classList={{ connecting: !!connectFromId() }}>
          <For each={cards()}>
            {(card) => {
              const version = () => clueCurrentVersion(card);
              const group = () => cardToGroup().get(card.id);
              // 证据链连接 = 回帖：前置线索是本条“回复/转发”的对象，后续线索是回复本条的人。
              const preds = () =>
                (group()?.parentCardIds ?? [])
                  .map((id) => clueCardById(id))
                  .filter((item): item is ClueCard => !!item);
              const successors = () => successorsOf(card.id);
              const commentCount = () => (card.comments ?? []).length;
              const attachmentCount = () => (version()?.attachments ?? []).length;
              const mentionCount = () => (version()?.mentions ?? []).length;
              const expanded = () => selectedCardId() === card.id;
              const comments = () => card.comments ?? [];
              const commentById = (id?: string | null) =>
                id ? comments().find((comment) => comment.id === id) : undefined;
              const timeline = (): TimelineEntry[] => {
                if (!expanded()) return [];
                const entries: TimelineEntry[] = comments().map((comment) => ({
                  kind: "comment",
                  at: comment.createdAt,
                  comment,
                }));
                card.versions.forEach((item, index) => {
                  if (index === 0 || item.id === card.currentVersionId) return;
                  entries.push({ kind: "version", at: item.createdAt, versionIndex: index });
                });
                entries.sort((left, right) => left.at - right.at);
                return entries;
              };
              return (
                <article
                  class="clue-feed-card"
                  classList={{
                    expanded: expanded(),
                    "connect-source": connectFromId() === card.id,
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
                    <div
                      class="clue-feed-click"
                      role="button"
                      tabIndex={0}
                      onClick={(event) => onCardClick(card.id, event)}
                      onKeyDown={(event) => {
                        if (event.key === "Enter" || event.key === " ") {
                          event.preventDefault();
                          if (!connectFromId()) selectOnly(expanded() ? null : card.id);
                        }
                      }}
                    >
                      <div class="clue-feed-head">
                        <div class="clue-feed-author">
                          <strong>{authorName(version()?.authorName)}</strong>
                          <span class="clue-feed-time">{fmtTime(card.updatedAt)}</span>
                        </div>
                        <span class="clue-version-tag">v{card.versions.length}</span>
                      </div>

                      {/* 连接即回帖：这条线索是在“回复/接力”前置线索 */}
                      <Show when={preds().length > 0}>
                        <div class="clue-reply-context">
                          <For each={preds()}>
                            {(pred) => (
                              <span class="clue-reply-line">
                                回复
                                <button
                                  type="button"
                                  class="clue-reply-link"
                                  title={clueCurrentVersion(pred)?.title || "未命名线索"}
                                  onClick={(event) => {
                                    event.stopPropagation();
                                    selectOnly(pred.id);
                                  }}
                                >
                                  @{authorName(clueCurrentVersion(pred)?.authorName)}
                                </button>
                                ：{clueCurrentVersion(pred)?.title || "未命名线索"}
                                <button
                                  type="button"
                                  class="clue-reply-unlink"
                                  title="删除这条连接"
                                  disabled={removingEdgeKey() === `${pred.id}->${card.id}`}
                                  onClick={(event) => void removeConnection(pred.id, card.id, event)}
                                >
                                  ×
                                </button>
                              </span>
                            )}
                          </For>
                        </div>
                      </Show>

                      <h2 class="clue-feed-title">{version()?.title || "未命名线索"}</h2>
                      <Show
                        when={expanded()}
                        fallback={<p class="clue-feed-content">{excerpt(version()?.content ?? "")}</p>}
                      >
                        <pre class="clue-feed-full">{version()?.content}</pre>
                      </Show>
                    </div>

                    <Show when={(version()?.attachments ?? []).length > 0}>
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
                                onClick={(event) => {
                                  event.stopPropagation();
                                  void api.openClueAttachment(attachment).catch((error) =>
                                    message(String(error), { kind: "error" }),
                                  );
                                }}
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
                                onClick={(event) => {
                                  event.stopPropagation();
                                  void downloadAttachment(attachment);
                                }}
                              >
                                <IconDownload size={13} />
                              </button>
                            </div>
                          )}
                        </For>
                      </div>
                    </Show>

                    <Show when={(version()?.mentions ?? []).length > 0}>
                      <div class="clue-mention-summary">
                        <For each={version()?.mentions ?? []}>
                          {(mention) => <strong>@{mention.name}</strong>}
                        </For>
                      </div>
                    </Show>

                    {/* 微博式操作行：转发(接力) | 回帖 | 更多操作，灰字横排 */}
                    <div class="clue-feed-bar">
                      <button
                        type="button"
                        class="clue-bar-item"
                        classList={{ active: expanded() }}
                        title="回帖"
                        onClick={(event) => {
                          event.stopPropagation();
                          if (!connectFromId()) selectOnly(expanded() ? null : card.id);
                        }}
                      >
                        {commentCount() + timeline().filter((entry) => entry.kind === "version").length || "回帖"}
                      </button>
                      <Show when={successors().length > 0}>
                        <span class="clue-bar-item static" title="接力这条线索的回帖">
                          {successors().length} 接力
                        </span>
                      </Show>
                      <Show when={attachmentCount() > 0}>
                        <span class="clue-bar-item static">{attachmentCount()} 附件</span>
                      </Show>
                      <Show when={mentionCount() > 0}>
                        <span class="clue-bar-item static">{mentionCount()} 提醒</span>
                      </Show>
                      <Show when={state.unreadClueMentions.includes(card.id)}>
                        <span class="clue-bar-item mention">新提醒</span>
                      </Show>
                      <span class="clue-bar-spacer" />
                      <Show when={!connectFromId()}>
                        <button
                          type="button"
                          class="clue-bar-item"
                          disabled={connectionBusy()}
                          onClick={(event) => {
                            event.stopPropagation();
                            setConnectFromId(card.id);
                          }}
                        >
                          连接
                        </button>
                      </Show>
                      <button
                        type="button"
                        class="clue-bar-item"
                        onClick={(event) => {
                          event.stopPropagation();
                          setCapture({ placement: "new", targetCardId: card.id });
                        }}
                      >
                        回复线索
                      </button>
                      <button
                        type="button"
                        class="clue-bar-item"
                        onClick={(event) => {
                          event.stopPropagation();
                          startSessionFromClue(card);
                        }}
                      >
                        发起会话
                      </button>
                      <button
                        type="button"
                        class="clue-bar-item"
                        onClick={(event) => {
                          event.stopPropagation();
                          setCapture({ placement: "update", targetCardId: card.id });
                        }}
                      >
                        更新
                      </button>
                      <button
                        type="button"
                        class="clue-bar-item danger"
                        disabled={deletingCardId() === card.id}
                        onClick={(event) => {
                          event.stopPropagation();
                          void removeCard(card);
                        }}
                      >
                        {deletingCardId() === card.id ? "删除中…" : "删除"}
                      </button>
                    </div>

                    {/* 展开态：回帖楼层（评论 + 版本更新 + 接力进来的下一条线索） */}
                    <Show when={expanded()}>
                      <div class="clue-thread">
                        <Show when={successors().length > 0}>
                          <div class="clue-relay-block">
                            <div class="clue-section-title">接力这条线索的回帖</div>
                            <For each={successors()}>
                              {(next) => (
                                <button
                                  type="button"
                                  class="clue-relay-item"
                                  onClick={(event) => {
                                    event.stopPropagation();
                                    selectOnly(next.id);
                                  }}
                                >
                                  <span class="clue-author-avatar small">
                                    {authorBadge(clueCurrentVersion(next)?.authorName)}
                                  </span>
                                  <span class="clue-relay-text">
                                    <strong>{authorName(clueCurrentVersion(next)?.authorName)}</strong>
                                    {clueCurrentVersion(next)?.title || "未命名线索"}
                                  </span>
                                  <time>{fmtTime(next.updatedAt)}</time>
                                </button>
                              )}
                            </For>
                          </div>
                        </Show>

                        <div class="clue-comments-head">
                          <div class="clue-section-title">全部回帖</div>
                          <span>{timeline().length}</span>
                        </div>
                        <Show
                          when={timeline().length > 0}
                          fallback={<div class="clue-comments-empty">还没有回帖，来抢沙发</div>}
                        >
                          <div class="clue-timeline">
                            <For each={timeline()}>
                              {(entry) => (
                                <Show
                                  when={entry.kind === "comment" ? entry : undefined}
                                  fallback={
                                    <Show when={entry.kind === "version" ? entry : undefined}>
                                      {(item) => {
                                        const itemVersion = () => card.versions[item().versionIndex];
                                        const isCurrent = () =>
                                          itemVersion()?.id === card.currentVersionId;
                                        return (
                                          <article class="clue-floor version">
                                            <span class="clue-author-avatar small">
                                              {authorBadge(itemVersion()?.authorName)}
                                            </span>
                                            <div class="clue-floor-body">
                                              <div class="clue-comment-head">
                                                <strong>{authorName(itemVersion()?.authorName)}</strong>
                                                <span class="clue-post-tag">更新</span>
                                                <time>{fmtTime(item().at)}</time>
                                              </div>
                                              <div class="clue-floor-version">
                                                <strong>
                                                  更新到 v{item().versionIndex + 1}
                                                  {isCurrent() ? "（当前）" : ""}：
                                                  {itemVersion()?.title || "未命名线索"}
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
                                        <span class="clue-author-avatar small">
                                          {authorBadge(comment().authorName)}
                                        </span>
                                        <div class="clue-floor-body">
                                          <div class="clue-comment-head">
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
                                            onClick={(event) => {
                                              event.stopPropagation();
                                              beginReply(comment());
                                            }}
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
                            placeholder={replyTarget() ? "写下回复…" : "写下回帖…"}
                            onInput={(event) => setCommentText(event.currentTarget.value)}
                            onClick={(event) => event.stopPropagation()}
                          />
                          <MentionPicker
                            peers={mentionPeers()}
                            selectedTokens={commentMentions()}
                            disabled={commentBusy() || mentionPeers().length === 0}
                            placeholder={
                              mentionPeers().length > 0 ? "@ 提醒团队成员" : "暂无可提醒的团队成员"
                            }
                            onChange={setCommentMentions}
                          />
                          <div class="clue-comment-submit-row">
                            <span class="field-hint">回复会自动 @ 原评论作者。</span>
                            <button
                              type="button"
                              class="btn primary small"
                              disabled={commentBusy() || !commentText().trim()}
                              onClick={(event) => {
                                event.stopPropagation();
                                void submitComment(card.id);
                              }}
                            >
                              {commentBusy() ? "发送中…" : replyTarget() ? "发送回复" : "发表回帖"}
                            </button>
                          </div>
                        </div>
                      </div>
                    </Show>
                  </div>
                </article>
              );
            }}
          </For>
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
