import { createEffect, createMemo, createSignal, For, on, onCleanup, Show } from "solid-js";
import { rememberPromptDraft, takePromptDraft } from "../promptDraft";
import {
  cancelTurn,
  enabledAgentKinds,
  ensureModelOptions,
  ensurePeerModels,
  pickThreadModel,
  sendPrompt,
  setThreadMode,
  state,
} from "../store";
import type { AgentKind, PromptImage } from "../types";
import { agentLabel } from "../utils";
import { ConfigSelects } from "./ConfigSelects";
import { ExclusiveChatMark } from "./ExclusiveChatMark";
import { IconFile, IconSend, IconStop, IconUsers } from "./icons";
import { createImageAttachments, ImageAttachmentStrip } from "./ImageAttachmentStrip";
import { createNoteFlow } from "./NoteFlow";
import { getSlashSuggestions, type SlashSuggestion } from "./slashSuggestions";

type PromptHistoryItem = {
  id: string;
  text: string;
  ts: number;
  images: PromptImage[];
};

export function Composer() {
  const [text, setText] = createSignal("");
  const [cursor, setCursor] = createSignal(0);
  const [slashStart, setSlashStart] = createSignal<number | null>(null);
  const [activeSlashIndex, setActiveSlashIndex] = createSignal(0);
  const [sentHistory, setSentHistory] = createSignal<PromptHistoryItem[]>([]);
  const [historyOpen, setHistoryOpen] = createSignal(false);
  const [activeHistoryIndex, setActiveHistoryIndex] = createSignal(0);
  let textareaRef: HTMLTextAreaElement | undefined;
  let slashMenuRef: HTMLDivElement | undefined;
  let historyMenuRef: HTMLDivElement | undefined;
  let resizeFrame: number | undefined;
  let maxInputHeight: number | undefined;

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

  onCleanup(() => {
    if (resizeFrame !== undefined) cancelAnimationFrame(resizeFrame);
  });

  const attach = createImageAttachments({ enableFileDrop: true });

  const running = () => !!(state.currentId && state.running[state.currentId]);
  const noteFlow = createNoteFlow(running);
  const empty = () => !text().trim() && attach.images().length === 0;
  const providerName = () => agentLabel(state.agentKind);
  const [stopDialogOpen, setStopDialogOpen] = createSignal(false);
  const [stopReason, setStopReason] = createSignal("");
  const [employeeMenuOpen, setEmployeeMenuOpen] = createSignal(false);
  const [selectedEmployeeId, setSelectedEmployeeId] = createSignal<string | null>(null);
  const selectedEmployee = createMemo(() =>
    state.employees.find((employee) => employee.id === selectedEmployeeId()) ?? null,
  );
  const isOrdinaryThread = () => {
    const thread = state.threads.find((item) => item.id === state.currentId);
    return !!thread && !thread.employeeId && !thread.roamingRole && !thread.quotaPeerName;
  };
  const requestStop = () => {
    const thread = state.threads.find((item) => item.id === state.currentId);
    if (thread?.employeeId && !thread.mindThread) {
      setStopReason("");
      setStopDialogOpen(true);
      return;
    }
    void cancelTurn();
  };
  const confirmEmployeeStop = () => {
    const reason = stopReason().trim();
    setStopDialogOpen(false);
    void cancelTurn(reason);
  };
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
  const currentHistoryPrefix = () => `${state.currentId ?? ""}:`;

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

  const promptHistory = createMemo(() => {
    const currentId = state.currentId;
    if (!currentId) return [];
    const transcriptItems = state.items
      .filter((item): item is Extract<(typeof state.items)[number], { type: "user" }> => item.type === "user")
      .map((item) => ({
        id: `${currentId}:item:${item.id}`,
        text: item.text.trim(),
        ts: item.ts,
        images: item.images ?? [],
      }));
    const all = [
      ...sentHistory().filter((item) => item.id.startsWith(currentHistoryPrefix())),
      ...transcriptItems,
    ]
      .filter((item) => item.text)
      .sort((a, b) => b.ts - a.ts);
    const seen = new Set<string>();
    return all.filter((item) => {
      if (seen.has(item.text)) return false;
      seen.add(item.text);
      return true;
    }).slice(0, 20);
  });

  createEffect(() => {
    const count = slashSuggestions().length;
    if (activeSlashIndex() >= count) setActiveSlashIndex(Math.max(0, count - 1));
  });

  createEffect(() => {
    const count = promptHistory().length;
    if (activeHistoryIndex() >= count) setActiveHistoryIndex(Math.max(0, count - 1));
    if (count === 0 && historyOpen()) setHistoryOpen(false);
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

  // 运行中也可发送：消息会注入当前轮次实时引导 agent
  const submit = () => {
    const value = text().trim();
    if (empty()) return;
    const images = attach.images();
    const currentId = state.currentId;
    if (currentId && value) {
      const now = Date.now();
      const snapshot = images.map((img) => ({ ...img }));
      setSentHistory((items) => [
        { id: `${currentId}:sent:${now}`, text: value, ts: now, images: snapshot },
        ...items.filter((item) => item.text !== value || !item.id.startsWith(`${currentId}:`)),
      ].slice(0, 80));
    }
    setText("");
    setHistoryOpen(false);
    attach.clear();
    if (textareaRef) textareaRef.style.height = "auto";
    const employeeId = isOrdinaryThread() ? selectedEmployeeId() : null;
    setSelectedEmployeeId(null);
    setEmployeeMenuOpen(false);
    void sendPrompt(value, images, employeeId);
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

  const onKeyDown = (e: KeyboardEvent) => {
    const suggestions = slashSuggestions();
    const history = promptHistory();
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
      submit();
    }
  };

  const onInput = (e: InputEvent) => {
    const el = e.currentTarget as HTMLTextAreaElement;
    const typedSlash = e.inputType === "insertText" && e.data === "/";
    const trackingSlash = slashStart() !== null;
    setText(el.value);
    noteFlow.bump();
    if (historyOpen()) setHistoryOpen(false);
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
          <For each={promptHistory()}>
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
      <textarea
        ref={textareaRef}
        class="composer-input"
        placeholder={
          running()
            ? `${providerName()} 正在工作…输入并回车可实时引导`
            : `给 ${providerName()} 下达任务，Enter 发送，Shift+Enter 换行，可粘贴或拖入文件`
        }
        value={text()}
        onInput={onInput}
        onKeyDown={onKeyDown}
        onClick={(e) => updateSlashState(e.currentTarget)}
        onKeyUp={(e) => updateSlashState(e.currentTarget)}
        onPaste={attach.onPaste}
        rows={3}
      />
      <div class="composer-bar">
        <Show
          when={!isQuotaBorrowed()}
          fallback={<span class="pill">模型：{state.model || "默认"}（额度会话已锁定）</span>}
        >
          <ConfigSelects
            agentKind={state.agentKind}
            agentKinds={agentKinds()}
            model={state.model}
            mode={state.mode}
            modelSource={usesPeerModels() ? guestModelSource : undefined}
            onPickModel={(k, m) => void pickThreadModel(k, m)}
            onMode={(v) => void setThreadMode(v)}
            anchorTo=".composer"
          />
        </Show>
        <span class="bar-spacer" />
        <Show when={isOrdinaryThread() && state.employees.length > 0}>
          <div class="composer-employee-picker">
            <Show when={employeeMenuOpen()}>
              <div class="composer-employee-menu">
                <div class="composer-employee-head">本次工作交给</div>
                <button
                  type="button"
                  classList={{ "composer-employee-item": true, active: !selectedEmployeeId() }}
                  onClick={() => {
                    setSelectedEmployeeId(null);
                    setEmployeeMenuOpen(false);
                  }}
                >
                  <span>普通会话</span>
                  <small>直接由当前模型执行</small>
                </button>
                <For each={state.employees.filter((employee) => employee.enabled)}>
                  {(employee) => (
                    <button
                      type="button"
                      classList={{
                        "composer-employee-item": true,
                        active: selectedEmployeeId() === employee.id,
                      }}
                      onClick={() => {
                        setSelectedEmployeeId(employee.id);
                        setEmployeeMenuOpen(false);
                      }}
                    >
                      <span>{employee.name}</span>
                      <small>Wake → Do · Dream 生效</small>
                    </button>
                  )}
                </For>
              </div>
            </Show>
            <button
              type="button"
              class="composer-btn employee"
              classList={{ active: !!selectedEmployee() }}
              onClick={() => setEmployeeMenuOpen((open) => !open)}
              title={selectedEmployee() ? `本次工作：${selectedEmployee()!.name}` : "选择本次工作的数字员工"}
            >
              <IconUsers size={16} />
            </button>
          </div>
        </Show>
        <span class="composer-stop-slot" classList={{ hidden: !running() }}>
          <button
            class="composer-btn stop"
            onClick={requestStop}
            title="停止"
            disabled={!running()}
          >
            <IconStop size={16} />
          </button>
        </span>
        <button
          class="composer-btn send"
          disabled={empty()}
          onClick={submit}
          title={running() ? "发送并引导当前任务" : "发送"}
        >
          <IconSend size={16} />
        </button>
      </div>
      <Show when={stopDialogOpen()}>
        <div class="modal-backdrop" onClick={() => setStopDialogOpen(false)}>
          <div class="modal stop-reason-dialog" onClick={(event) => event.stopPropagation()}>
            <div class="modal-head">停止数字员工</div>
            <div class="modal-body">
              <p class="stop-reason-hint">
                可以告诉 Dream 为什么停止；停止后会话保留，账本项会保留为失败记录。
              </p>
              <textarea
                class="field-input"
                rows={4}
                autofocus
                placeholder="可选：方向做偏了、范围太大、应该先请示……"
                value={stopReason()}
                onInput={(event) => setStopReason(event.currentTarget.value)}
              />
              <div class="stop-reason-actions">
                <button class="btn secondary" onClick={() => setStopDialogOpen(false)}>
                  取消
                </button>
                <button class="btn danger" onClick={confirmEmployeeStop}>
                  {stopReason().trim() ? "提交原因并停止" : "直接停止"}
                </button>
              </div>
            </div>
          </div>
        </div>
      </Show>
    </div>
  );
}
