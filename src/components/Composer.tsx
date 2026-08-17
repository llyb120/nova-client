import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show, untrack } from "solid-js";
import { rememberPromptDraft, takePromptDraft } from "../promptDraft";
import {
  promptHistory as globalPromptHistory,
  rememberPromptHistory,
  type PromptHistoryItem,
} from "../promptHistory";
import {
  dispatchQueuedPrompt,
  dispatchingQueueIds,
  enqueuePrompt,
  failedQueueIds,
  holdPromptQueue,
  queueHeldThreadIds,
  queuedPrompts,
  type QueuedPrompt,
  releasePromptQueue,
  removeQueuedPrompt,
} from "../promptQueue";
import { mountSessionShortcuts } from "../sessionShortcuts";
import { api } from "../ipc";
import { isPasteFilePathsShortcut, resolveClipboardFilePaths } from "../pasteFilePaths";
import {
  cancelTurn,
  clueCardById,
  clueCurrentVersion,
  closeThread,
  enabledAgentKinds,
  ensureModelOptions,
  ensurePeerModels,
  openClueCard,
  pickThreadModel,
  refreshSlashCommands,
  sendPrompt,
  setView,
  state,
} from "../store";
import type { AgentKind, PromptImage } from "../types";
import { agentLabel } from "../utils";
import {
  chooseManualWorkflowTransition,
  manualWorkflowReview,
  workflowReviewRevision,
} from "../workflow/runtime";
import { ConfigSelects } from "./ConfigSelects";
import { ExclusiveChatMark } from "./ExclusiveChatMark";
import { IconClue, IconFile, IconSend, IconStop, IconUndo } from "./icons";
import { createImageAttachments, ImageAttachmentStrip } from "./ImageAttachmentStrip";
import { createNoteFlow } from "./NoteFlow";
import { fitSlashMenuHeight } from "./slashMenuLayout";
import { getSlashSuggestions, type SlashSuggestion } from "./slashSuggestions";
import { fmtTokens } from "./TurnGroup";

export function Composer() {
  const [text, setText] = createSignal("");
  const [cursor, setCursor] = createSignal(0);
  const [slashStart, setSlashStart] = createSignal<number | null>(null);
  const [activeSlashIndex, setActiveSlashIndex] = createSignal(0);
  const [historyOpen, setHistoryOpen] = createSignal(false);
  const [activeHistoryIndex, setActiveHistoryIndex] = createSignal(0);
  const [choosingWorkflowRoute, setChoosingWorkflowRoute] = createSignal(false);
  let textareaRef: HTMLTextAreaElement | undefined;
  let slashMenuRef: HTMLDivElement | undefined;
  let historyMenuRef: HTMLDivElement | undefined;
  let resizeFrame: number | undefined;
  let maxInputHeight: number | undefined;
  let pasteAsPaths = false;
  let pastePathsSeq = 0;

  const flushInputResize = () => {
    resizeFrame = undefined;
    if (!textareaRef) return;
    textareaRef.style.height = "auto";
    if (maxInputHeight === undefined) {
      const value = Number.parseFloat(getComputedStyle(textareaRef).maxHeight);
      maxInputHeight = Number.isFinite(value) ? value : Number.POSITIVE_INFINITY;
    }
    const next = textareaRef.scrollHeight;
    textareaRef.style.height = Math.min(next, maxInputHeight) + "px";
  };

  const resizeInput = () => {
    if (resizeFrame === undefined) resizeFrame = requestAnimationFrame(flushInputResize);
  };

  createEffect(() => {
    text();
    resizeInput();
  });

  const insertShortcutText = (snippet: string, mayFocus: boolean): boolean => {
    if (!textareaRef) return false;
    const focused = document.activeElement === textareaRef;
    // 焦点不在输入框：允许全局触发时先聚焦并追加到末尾；否则忽略（如其它输入框里的无修饰键输入）。
    if (!focused && !mayFocus) return false;
    const start = focused ? (textareaRef.selectionStart ?? text().length) : text().length;
    const end = focused ? (textareaRef.selectionEnd ?? start) : text().length;
    const next = `${text().slice(0, start)}${snippet}${text().slice(end)}`;
    const nextCursor = start + snippet.length;
    setText(next);
    setCursor(nextCursor);
    setHistoryOpen(false);
    queueMicrotask(() => {
      if (!textareaRef) return;
      textareaRef.focus();
      textareaRef.setSelectionRange(nextCursor, nextCursor);
      updateSlashState(textareaRef, true);
      resizeInput();
    });
    return true;
  };

  onCleanup(() => {
    if (resizeFrame !== undefined) cancelAnimationFrame(resizeFrame);
  });

  const attach = createImageAttachments({ enableFileDrop: true });

  const running = () => !!(state.currentId && state.running[state.currentId]);
  const contextUsedTokens = () => {
    if (state.liveUsage?.contextTokens) return state.liveUsage.contextTokens;
    if (state.liveUsage?.inputTokens) return state.liveUsage.inputTokens;
    for (let index = state.items.length - 1; index >= 0; index--) {
      const item = state.items[index];
      if (item.type === "turn" && item.contextTokens) return item.contextTokens;
      if (item.type === "turn" && item.inputTokens) return item.inputTokens;
    }
    return 0;
  };
  const contextWindow = () => {
    const model = state.modelOptions[state.agentKind]?.configOptions
      ?.find((option) => option.id === "model")
      ?.options?.find((option) => option.value === state.model);
    const value = Number(
      model?._meta?.contextWindow ??
      model?._meta?.context_window ??
      model?._meta?.["codex.ai/contextWindow"],
    );
    return Number.isFinite(value) && value >= 2_000 ? value : null;
  };
  const [runClock, setRunClock] = createSignal(Date.now());
  const [runStartedAt, setRunStartedAt] = createSignal<number | null>(null);
  createEffect(() => {
    const threadId = state.currentId;
    if (!threadId || !state.running[threadId]) {
      setRunStartedAt(null);
      return;
    }
    const startedAt = untrack(() => {
      for (let index = state.items.length - 1; index >= 0; index--) {
        if (state.items[index].type === "turn") break;
        if (state.items[index].type === "user") return state.items[index].ts;
      }
      return Date.now();
    });
    setRunStartedAt(startedAt);
    setRunClock(Date.now());
    const timer = window.setInterval(() => setRunClock(Date.now()), 1_000);
    onCleanup(() => window.clearInterval(timer));
  });
  const runElapsed = () => {
    const startedAt = runStartedAt();
    if (startedAt === null) return "0:00";
    const seconds = Math.max(0, Math.floor((runClock() - startedAt) / 1_000));
    const hours = Math.floor(seconds / 3_600);
    const minutes = Math.floor((seconds % 3_600) / 60);
    const remainder = seconds % 60;
    return hours > 0
      ? `${hours}:${String(minutes).padStart(2, "0")}:${String(remainder).padStart(2, "0")}`
      : `${minutes}:${String(remainder).padStart(2, "0")}`;
  };
  const noteFlow = createNoteFlow(running);
  const empty = () => !text().trim() && attach.images().length === 0;
  const providerName = () => agentLabel(state.agentKind);
  // 原生注入当前轮：Alkaid / Lyra / Codex / Devin。
  const supportsLiveSteer = () =>
    state.agentKind === "alkaid" ||
    state.agentKind === "lyra" ||
    state.agentKind === "codex" ||
    state.agentKind === "devin";
  // 打断当前轮后以新 turn 继续：Cursor（Agent.create + slim memory）、OpenCode。
  const supportsInterruptSteer = () =>
    state.agentKind === "cursor" ||
    state.agentKind === "opencode";
  const supportsSteer = () => supportsLiveSteer() || supportsInterruptSteer();
  const [stopDialogOpen, setStopDialogOpen] = createSignal(false);
  const activeClue = createMemo(() => {
    const cardId = state.threads.find((item) => item.id === state.currentId)?.activeClueCardId;
    if (!cardId) return null;
    const card = clueCardById(cardId);
    const version = card ? clueCurrentVersion(card) : undefined;
    return { id: cardId, title: version?.title || "未命名线索" };
  });
  const openEvidenceChain = () => {
    const clue = activeClue();
    if (clue) {
      openClueCard(clue.id);
      return;
    }
    // ChatView 优先于 view：必须先关闭会话才能进入证据链页（与侧栏一致）。
    closeThread();
    setView("clues");
  };
  const requestStop = () => {
    holdPromptQueue(state.currentId);
    void cancelTurn();
  };

  mountSessionShortcuts({
    allowedActions: ["insertText", "stopSession"],
    onInsertText: insertShortcutText,
    onStopSession: () => {
      if (historyOpen() || slashQuery() !== null || stopDialogOpen()) return false;
      if (!running()) return false;
      requestStop();
      return true;
    },
  });

  const stopShortcutLabel = () => "停止 (Esc)";

  // 进行中 / 漫游会话不开放跨后端切换，退回当前后端单选；否则可在已启用后端间切换
  const isGuest = () =>
    (state.threads.find((t) => t.id === state.currentId)?.roamingRole ?? null) === "guest";
  const isQuotaBorrowed = () =>
    !!state.threads.find((t) => t.id === state.currentId)?.quotaPeerName;
  const usesPeerModels = () => isGuest();
  const agentKinds = (): AgentKind[] =>
    !running() && !usesPeerModels() ? enabledAgentKinds() : [state.agentKind];
  // 漫游 guest：模型选择用对端（host）的列表（本机模型对方可能没有）
  const guestModels = () => {
    const t = state.roamingPeer;
    return usesPeerModels() && t ? state.peerModels[t] : undefined;
  };
  const guestModelSource = (k: AgentKind) => guestModels()?.options[k] ?? null;
  // 只加载当前后端；其他后端在用户打开模型选择器时按需加载。
  createEffect(() => {
    if (!usesPeerModels() && !isQuotaBorrowed()) void ensureModelOptions(state.agentKind);
  });
  // 漫游 guest：确保已拉取对端模型列表
  createEffect(() => {
    if (usesPeerModels() && state.roamingPeer) ensurePeerModels(state.roamingPeer);
  });
  const updateSlashState = (el = textareaRef, allowOpen = false) => {
    if (!el) return;
    const value = el.value;
    const pos = el.selectionStart ?? value.length;
    setCursor(pos);
    if (slashStart() === null && !allowOpen) {
      setSlashStart(null);
      return;
    }
    const prefix = value.slice(0, pos);
    const start = Math.max(prefix.lastIndexOf(" "), prefix.lastIndexOf("\n"), prefix.lastIndexOf("\t")) + 1;
    const token = prefix.slice(start);
    setSlashStart(token.startsWith("/") ? start : null);
  };

  const slashQuery = createMemo(() => {
    const start = slashStart();
    if (start === null) return null;
    return text().slice(start + 1, cursor()).toLowerCase();
  });

  const slashSuggestions = createMemo(() => {
    const query = slashQuery();
    if (query === null) return [];
    return getSlashSuggestions(state.agentKind, state.slashCommands[state.agentKind], query);
  });

  createEffect(() => {
    const count = slashSuggestions().length;
    if (activeSlashIndex() >= count) setActiveSlashIndex(Math.max(0, count - 1));
  });

  createEffect(() => {
    const count = globalPromptHistory().length;
    if (activeHistoryIndex() >= count) setActiveHistoryIndex(Math.max(0, count - 1));
    if (count === 0 && historyOpen()) setHistoryOpen(false);
  });

  // 打开已有会话时，把后端保存的用户输入并入全局历史，供新会话页使用。
  createEffect(() => {
    const currentId = state.currentId;
    if (!currentId) return;
    for (const item of state.items) {
      if (item.type === "user") {
        rememberPromptHistory(item.text, item.images ?? [], item.ts, `${currentId}:item:${item.id}`);
      }
    }
  });

  createEffect(() => {
    activeSlashIndex();
    slashMenuRef
      ?.querySelector(".slash-item.active")
      ?.scrollIntoView({ block: "nearest" });
  });

  createEffect(() => {
    activeHistoryIndex();
    historyMenuRef
      ?.querySelector(".prompt-history-item.active")
      ?.scrollIntoView({ block: "nearest" });
  });

  // Slash / history menus open upward; clamp height to space above the composer.
  createEffect(() => {
    const slashOpen = slashQuery() !== null;
    const historyIsOpen = historyOpen();
    if (!slashOpen && !historyIsOpen) return;
    void slashSuggestions().length;
    void globalPromptHistory().length;
    const sync = () => {
      if (slashOpen) fitSlashMenuHeight(slashMenuRef);
      if (historyIsOpen) fitSlashMenuHeight(historyMenuRef, { maxHeight: 300 });
    };
    const frame = requestAnimationFrame(sync);
    const host = textareaRef?.closest(".composer, .home-composer");
    const ro = host instanceof HTMLElement ? new ResizeObserver(sync) : undefined;
    if (host instanceof HTMLElement) ro?.observe(host);
    window.addEventListener("resize", sync);
    onCleanup(() => {
      cancelAnimationFrame(frame);
      ro?.disconnect();
      window.removeEventListener("resize", sync);
    });
  });

  createEffect(
    on(
      () => state.currentId,
      (currentId, previousId) => {
        if (previousId === undefined || currentId === previousId) return;
        rememberPromptDraft(text(), attach.images());
        setText("");
        setCursor(0);
        setSlashStart(null);
        setHistoryOpen(false);
        attach.clear();
      },
    ),
  );

  onCleanup(() => rememberPromptDraft(text(), attach.images()));

  const currentQueuedPrompts = createMemo(() => {
    const currentId = state.currentId;
    return currentId ? queuedPrompts().filter((item) => item.threadId === currentId) : [];
  });
  const currentQueueHeld = createMemo(() => {
    const currentId = state.currentId;
    return !!(currentId && queueHeldThreadIds().has(currentId));
  });

  const clearInput = () => {
    setText("");
    setHistoryOpen(false);
    attach.clear();
    if (textareaRef) textareaRef.style.height = "auto";
  };

  const sendQueuedPromptNow = (item: QueuedPrompt, steerNow = false) => {
    if (steerNow && running() && !supportsSteer()) return;
    void dispatchQueuedPrompt(item, steerNow);
  };

  const withdrawQueuedPrompt = (item: QueuedPrompt) => {
    removeQueuedPrompt(item.id);
    const existing = text();
    const restored = existing.trim() ? `${item.text}\n${existing}` : item.text;
    setText(restored);
    attach.set([...item.images, ...attach.images()]);
    setHistoryOpen(false);
    setSlashStart(null);
    setCursor(restored.length);
    queueMicrotask(() => {
      textareaRef?.focus();
      textareaRef?.setSelectionRange(restored.length, restored.length);
      resizeInput();
    });
  };

  // 运行中第一次回车只排队；队列可立即引导，或在当前任务结束后自动发送。
  const submit = () => {
    const value = text().trim();
    if (empty()) return;
    const images = attach.images().map((image) => ({ ...image }));
    const currentId = state.currentId;
    if (!currentId) return;
    rememberPromptHistory(value, images);
    clearInput();
    if (running()) {
      enqueuePrompt(currentId, value, images);
      return;
    }
    releasePromptQueue(currentId);
    void sendPrompt(value, images);
  };

  const insertSlashSuggestion = (item: SlashSuggestion) => {
    const start = slashStart();
    if (start === null) return;
    const pos = cursor();
    const insert = item.insertText.endsWith(" ") ? item.insertText : `${item.insertText} `;
    const next = `${text().slice(0, start)}${insert}${text().slice(pos)}`;
    const nextCursor = start + insert.length;
    setText(next);
    setSlashStart(null);
    setCursor(nextCursor);
    queueMicrotask(() => {
      textareaRef?.focus();
      textareaRef?.setSelectionRange(nextCursor, nextCursor);
      resizeInput();
    });
  };

  const insertHistoryItem = (item: PromptHistoryItem) => {
    const nextCursor = item.text.length;
    setText(item.text);
    attach.set(item.images ?? []);
    setHistoryOpen(false);
    setSlashStart(null);
    setCursor(nextCursor);
    queueMicrotask(() => {
      textareaRef?.focus();
      textareaRef?.setSelectionRange(nextCursor, nextCursor);
      resizeInput();
    });
  };

  const restoreDraft = () => {
    const draft = takePromptDraft();
    if (!draft) return false;
    const nextCursor = draft.text.length;
    setText(draft.text);
    attach.set(draft.images);
    setHistoryOpen(false);
    setSlashStart(null);
    setCursor(nextCursor);
    queueMicrotask(() => {
      textareaRef?.focus();
      textareaRef?.setSelectionRange(nextCursor, nextCursor);
      resizeInput();
    });
    return true;
  };

  const insertClipboardFilePaths = (data?: DataTransfer | null) => {
    const seq = ++pastePathsSeq;
    void resolveClipboardFilePaths(data).then((paths) => {
      if (seq !== pastePathsSeq || paths.length === 0) return;
      insertShortcutText(paths.join("\n"), true);
    });
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (isPasteFilePathsShortcut(e)) {
      // WebView2 maps Ctrl+Shift+V to "paste as plain text" and strips file items;
      // preventDefault and read CF_HDROP natively instead of waiting for paste.
      e.preventDefault();
      e.stopPropagation();
      pasteAsPaths = true;
      insertClipboardFilePaths();
      queueMicrotask(() => {
        pasteAsPaths = false;
      });
      return;
    }
    const suggestions = slashSuggestions();
    const history = globalPromptHistory();
    if (historyOpen() && history.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setActiveHistoryIndex((i) => (i + 1) % history.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setActiveHistoryIndex((i) => (i - 1 + history.length) % history.length);
        return;
      }
      if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey && !e.isComposing)) {
        e.preventDefault();
        insertHistoryItem(history[activeHistoryIndex()] ?? history[0]);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setHistoryOpen(false);
        return;
      }
    }
    if (slashQuery() !== null && suggestions.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setActiveSlashIndex((i) => (i + 1) % suggestions.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setActiveSlashIndex((i) => (i - 1 + suggestions.length) % suggestions.length);
        return;
      }
      if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey && !e.isComposing)) {
        e.preventDefault();
        insertSlashSuggestion(suggestions[activeSlashIndex()] ?? suggestions[0]);
        return;
      }
    }
    if (e.key === "Escape" && slashQuery() !== null) {
      e.preventDefault();
      setSlashStart(null);
      return;
    }

    if (e.key === "ArrowDown" && empty() && restoreDraft()) {
      e.preventDefault();
      return;
    }
    if (e.key === "ArrowUp" && empty() && history.length > 0) {
      e.preventDefault();
      setSlashStart(null);
      setActiveHistoryIndex(0);
      setHistoryOpen(true);
      return;
    }
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      const firstQueued = currentQueuedPrompts()[0];
      if (empty() && firstQueued) {
        sendQueuedPromptNow(firstQueued, true);
        return;
      }
      submit();
    }
  };

  const manualReview = createMemo(() => {
    workflowReviewRevision();
    return state.currentId ? manualWorkflowReview(state.currentId) : null;
  });

  const chooseWorkflowRoute = async (transitionId: string) => {
    const threadId = state.currentId;
    if (!threadId || choosingWorkflowRoute()) return;
    setChoosingWorkflowRoute(true);
    try {
      await chooseManualWorkflowTransition(threadId, transitionId);
    } catch (error) {
      console.error("Workflow manual transition failed", error);
    } finally {
      setChoosingWorkflowRoute(false);
    }
  };

  const onPaste = (e: ClipboardEvent) => {
    const asPaths = pasteAsPaths;
    pasteAsPaths = false;
    if (asPaths) {
      // Ctrl+Shift+V is the explicit "paste file paths" gesture; never add attachments.
      e.preventDefault();
      insertClipboardFilePaths(e.clipboardData);
      return;
    }

    attach.onPaste(e);
  };

  const onInput = (e: InputEvent) => {
    const el = e.currentTarget as HTMLTextAreaElement;
    const typedSlash = e.inputType === "insertText" && e.data === "/";
    const trackingSlash = slashStart() !== null;
    setText(el.value);
    noteFlow.bump();
    if (historyOpen()) setHistoryOpen(false);
    if (typedSlash) void refreshSlashCommands(state.agentKind);
    updateSlashState(el, typedSlash || trackingSlash);
  };

  return (
    <div
      class="composer"
      classList={{ "is-dragging": attach.dragging() }}
    >
      <noteFlow.Notes />
      <ExclusiveChatMark token={state.roamingPeer || state.settings?.relayToken || ""} />
      <ImageAttachmentStrip images={attach.images()} onRemove={attach.remove} />
      <Show when={manualReview()}>
        {(review) => (
          <div class="workflow-manual-review">
            <div class="workflow-manual-review-head">
              <strong>等待人工审核</strong>
              <span>选择下一步</span>
            </div>
            <div class="workflow-manual-review-actions">
              <For each={review().transitions}>
                {(transition) => (
                  <button
                    type="button"
                    class="btn secondary small"
                    disabled={choosingWorkflowRoute()}
                    onClick={() => void chooseWorkflowRoute(transition.id)}
                  >
                    {transition.label}
                  </button>
                )}
              </For>
            </div>
          </div>
        )}
      </Show>
      <Show when={activeClue()}>
        {(clue) => (
          <div class="clue-context-chip" title={`本会话引用线索：${clue().title}`}>
            <IconClue size={13} />
            <span class="clue-context-label">证据链</span>
            <span class="clue-context-separator" aria-hidden="true" />
            <span class="clue-context-title">{clue().title}</span>
          </div>
        )}
      </Show>
      <Show when={currentQueuedPrompts().length > 0}>
        <div class="prompt-queue" aria-label="待发送提示词">
          <div class="prompt-queue-head">
            <span>待发送</span>
            <small>{currentQueueHeld() ? "已停止，可手动发送或撤回" : "当前任务结束后自动发送"}</small>
          </div>
          <For each={currentQueuedPrompts()}>
            {(item, index) => (
              <div
                class="prompt-queue-item"
                classList={{ failed: failedQueueIds().has(item.id) }}
              >
                <span class="prompt-queue-index">{index() + 1}</span>
                <span class="prompt-queue-text" title={item.text}>{item.text || "附件"}</span>
                <Show when={item.images.length > 0}>
                  <span class="prompt-history-attach" title={`${item.images.length} 个附件`}>
                    <IconFile size={12} />
                    {item.images.length}
                  </span>
                </Show>
                <button
                  type="button"
                  class="prompt-queue-action"
                  disabled={dispatchingQueueIds().has(item.id)}
                  onClick={() => withdrawQueuedPrompt(item)}
                  title="撤回到输入框"
                >
                  <IconUndo size={13} />
                  撤回
                </button>
                <button
                  type="button"
                  class="prompt-queue-action send-now"
                  disabled={dispatchingQueueIds().has(item.id) || (running() && !supportsSteer())}
                  onClick={() => sendQueuedPromptNow(item, true)}
                  title={failedQueueIds().has(item.id) ? "重试发送" : "立即作为引导发送"}
                >
                  <IconSend size={13} />
                  {dispatchingQueueIds().has(item.id) ? "发送中" : "发送"}
                </button>
              </div>
            )}
          </For>
        </div>
      </Show>
      <Show when={slashQuery() !== null}>
        <div ref={slashMenuRef} class="slash-menu">
          <div class="slash-menu-head">
            {providerName()} {state.agentKind === "codex" ? "skills / commands" : "commands"}
          </div>
          <Show
            when={slashSuggestions().length > 0}
            fallback={<div class="slash-empty">暂无可用项</div>}
          >
            <For each={slashSuggestions()}>
              {(item, index) => (
                <button
                  type="button"
                  classList={{ "slash-item": true, active: index() === activeSlashIndex() }}
                  onMouseEnter={() => setActiveSlashIndex(index())}
                  onMouseDown={(e) => {
                    e.preventDefault();
                    insertSlashSuggestion(item);
                  }}
                >
                  <span class="slash-title">{item.title}</span>
                  <span class="slash-detail">{item.detail}</span>
                  <span class="slash-kind">{item.kind}</span>
                </button>
              )}
            </For>
          </Show>
        </div>
      </Show>
      <Show when={historyOpen()}>
        <div ref={historyMenuRef} class="slash-menu prompt-history-menu">
          <div class="slash-menu-head">历史输入</div>
          <For each={globalPromptHistory()}>
            {(item, index) => (
              <button
                type="button"
                classList={{ "prompt-history-item": true, active: index() === activeHistoryIndex() }}
                onMouseEnter={() => setActiveHistoryIndex(index())}
                onMouseDown={(e) => {
                  e.preventDefault();
                  insertHistoryItem(item);
                }}
              >
                <span class="prompt-history-text">{item.text}</span>
                <Show when={item.images.length > 0}>
                  <span class="prompt-history-attach" title={`${item.images.length} 个附件`}>
                    <IconFile size={12} />
                    {item.images.length}
                  </span>
                </Show>
              </button>
            )}
          </For>
        </div>
      </Show>
      <div class="composer-input-wrap">
        <textarea
          ref={textareaRef}
          class="composer-input"
          placeholder={
            running()
              ? supportsSteer()
                ? `${providerName()} 正在工作…输入并回车加入队列；输入为空时回车可立即引导`
                : `${providerName()} 正在工作…输入并回车加入队列，任务结束后自动发送`
              : `给 ${providerName()} 下达任务，Enter 发送，Shift+Enter 换行，可粘贴或拖入文件`
          }
          value={text()}
          onInput={onInput}
          onKeyDown={onKeyDown}
          onClick={(e) => updateSlashState(e.currentTarget)}
          onKeyUp={(e) => updateSlashState(e.currentTarget)}
          onPaste={onPaste}
          rows={3}
        />
      </div>
      <div class="composer-bar">
        <Show
          when={!isQuotaBorrowed()}
          fallback={<span class="pill">模型：{state.model || "默认"}（额度会话已锁定）</span>}
        >
          <ConfigSelects
            agentKind={state.agentKind}
            agentKinds={agentKinds()}
            model={state.model}
            modelSource={usesPeerModels() ? guestModelSource : undefined}
            onPickModel={(k, m) => void pickThreadModel(k, m)}
            anchorTo=".composer"
            favorites
          />
        </Show>
        <Show when={running()}>
          <span
            class="composer-run-stats"
            title={contextWindow()
              ? `本轮已运行 ${runElapsed()}\n上下文 ${fmtTokens(contextUsedTokens())} / ${fmtTokens(contextWindow()!)} tokens`
              : `本轮已运行 ${runElapsed()}\n当前模型未提供上下文窗口`}
          >
            <span class="composer-run-dot" aria-hidden="true" />
            <span>{runElapsed()}</span>
            <span class="composer-run-sep">·</span>
            <span>
              上下文 {fmtTokens(contextUsedTokens())} / {contextWindow() ? fmtTokens(contextWindow()!) : "--"}
            </span>
          </span>
        </Show>
        <span class="bar-spacer" />
        <button
          type="button"
          class="composer-btn clue"
          classList={{ active: !!activeClue() }}
          title={activeClue() ? "查看本会话证据链" : "打开证据链"}
          onClick={openEvidenceChain}
        >
          <IconClue size={16} />
        </button>
        <span class="composer-stop-slot" classList={{ hidden: !running() }}>
          <button
            class="composer-btn stop"
            onClick={requestStop}
            title={running() ? stopShortcutLabel() : "停止"}
            disabled={!running()}
          >
            <IconStop size={16} />
          </button>
        </span>
        <button
          class="composer-btn send"
          disabled={empty()}
          onClick={submit}
          title={running() ? "加入提示词队列" : "发送"}
        >
          <IconSend size={16} />
        </button>
      </div>
    </div>
  );
}
