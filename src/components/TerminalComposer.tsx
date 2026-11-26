import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { createEffect, createSignal, onCleanup, onSettled, Show, untrack } from "solid-js";
import { formatTerminalPromptWithPaths } from "../agyTerminalPrompt";
import { api } from "../ipc";
import { clipboardFiles, MAX_ATTACHMENT_BYTES } from "../pasteAttachments";
import { isPasteFilePathsShortcut, resolveClipboardFilePaths } from "../pasteFilePaths";
import { saveSessionDraft, takeSessionDraft } from "../promptDraft";
import { showToast } from "../store";
import { createImageAttachments, fileToAttachment, ImageAttachmentStrip } from "./ImageAttachmentStrip";
import { isRunningAgyPhase, type AgyAgentPhase } from "./TerminalAgyConversation";
import { IconPaperclip, IconSend, IconStop } from "./icons";

const draftKey = (threadId: string) => `term:${threadId}`;

export function TerminalComposer(props: {
  threadId: string;
  agyPhase: AgyAgentPhase | null;
  terminalReady: boolean;
  canStop: boolean;
  onStop: () => void;
  onSend: (payload: string, submit: boolean) => boolean;
}) {
  let draftThreadId = untrack(() => props.threadId);
  const initialDraft = takeSessionDraft(draftKey(draftThreadId));
  const [text, setText] = createSignal(initialDraft?.text ?? "");
  const [sending, setSending] = createSignal(false);
  let textareaRef: HTMLTextAreaElement | undefined;
  let resizeFrame: number | undefined;
  let pasteAsPaths = false;
  let pastePathsSeq = 0;

  const attach = createImageAttachments({ enableFileDrop: true });
  onSettled(() => {
    attach.set(initialDraft?.images ?? []);
  });

  const flushInputResize = () => {
    resizeFrame = undefined;
    if (!textareaRef) return;
    textareaRef.style.height = "auto";
    const maxHeight = 180;
    textareaRef.style.height = `${Math.min(textareaRef.scrollHeight, maxHeight)}px`;
  };

  const resizeInput = () => {
    if (resizeFrame === undefined) resizeFrame = requestAnimationFrame(flushInputResize);
  };

  createEffect(text, resizeInput);

  createEffect(
    () => props.threadId,
    (nextId, prevId) => {
      if (!nextId || prevId === undefined || nextId === prevId) return;
      saveSessionDraft(draftKey(draftThreadId), text(), attach.images());
      draftThreadId = nextId;
      const draft = takeSessionDraft(draftKey(nextId));
      setText(draft?.text ?? "");
      attach.set(draft?.images ?? []);
      queueMicrotask(resizeInput);
    },
  );

  onCleanup(() => {
    if (resizeFrame !== undefined) cancelAnimationFrame(resizeFrame);
    saveSessionDraft(draftKey(draftThreadId), text(), attach.images());
  });

  const empty = () => !text().trim() && attach.images().length === 0;
  const disabled = () => empty() || !props.terminalReady || sending();

  const insertTextAtCursor = (snippet: string) => {
    if (!textareaRef || !snippet) return;
    const focused = document.activeElement === textareaRef;
    const start = focused ? (textareaRef.selectionStart ?? text().length) : text().length;
    const end = focused ? (textareaRef.selectionEnd ?? start) : text().length;
    const next = `${text().slice(0, start)}${snippet}${text().slice(end)}`;
    const nextCursor = start + snippet.length;
    setText(next);
    queueMicrotask(() => {
      if (!textareaRef) return;
      textareaRef.focus();
      textareaRef.setSelectionRange(nextCursor, nextCursor);
      resizeInput();
    });
  };

  const insertClipboardFilePaths = (data?: DataTransfer | null) => {
    const seq = ++pastePathsSeq;
    void resolveClipboardFilePaths(data).then((paths) => {
      if (seq !== pastePathsSeq || paths.length === 0) return;
      insertTextAtCursor(paths.join("\n"));
    });
  };

  const pickAttachments = async () => {
    try {
      const picked = await openDialog({ multiple: true, title: "选择要附加的文件或图片" });
      const paths = (Array.isArray(picked) ? picked : picked ? [picked] : []).filter(
        (item): item is string => typeof item === "string" && item.length > 0,
      );
      if (paths.length > 0) attach.addPaths(paths);
    } catch (error) {
      showToast(`选择附件失败：${error}`);
    }
  };

  const clearInput = () => {
    setText("");
    attach.clear();
    if (textareaRef) textareaRef.style.height = "auto";
  };

  const submit = async (autoSubmit: boolean) => {
    if (disabled()) return;
    const currentText = text();
    const currentImages = attach.images().map((img) => ({ ...img }));
    setSending(true);
    try {
      const paths = currentImages.length > 0
        ? await api.saveTerminalAttachments(currentImages)
        : [];
      const payload = formatTerminalPromptWithPaths(currentText, paths);
      if (!payload) return;
      const accepted = props.onSend(payload, autoSubmit);
      if (!accepted) {
        showToast("终端尚未就绪，未能写入输入");
        return;
      }
      clearInput();
      saveSessionDraft(draftKey(props.threadId), "", []);
    } catch (error) {
      showToast(`发送失败：${error}`);
    } finally {
      setSending(false);
    }
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (isPasteFilePathsShortcut(e)) {
      e.preventDefault();
      e.stopPropagation();
      pasteAsPaths = true;
      insertClipboardFilePaths();
      queueMicrotask(() => {
        pasteAsPaths = false;
      });
      return;
    }
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing && e.keyCode !== 229) {
      e.preventDefault();
      void submit(true);
    }
  };

  const onPaste = (e: ClipboardEvent) => {
    const asPaths = pasteAsPaths;
    pasteAsPaths = false;
    if (asPaths) {
      e.preventDefault();
      insertClipboardFilePaths(e.clipboardData);
      return;
    }

    const items = e.clipboardData?.items ?? [];
    const { files, oversized } = clipboardFiles(items);
    if (files.length > 0) {
      e.preventDefault();
      void Promise.all(files.map(fileToAttachment)).then(attach.add);
      if (oversized > 0) {
        void api.clipboardFilePaths()
          .then((paths) => {
            if (paths.length > 0) attach.addPaths(paths);
            else showToast(`附件过大：单个文件不能超过 ${MAX_ATTACHMENT_BYTES / 1024 / 1024}MB`);
          })
          .catch(() => {
            showToast(`附件过大：单个文件不能超过 ${MAX_ATTACHMENT_BYTES / 1024 / 1024}MB`);
          });
      }
      return;
    }
    if (oversized > 0) {
      e.preventDefault();
      void api.clipboardFilePaths()
        .then((paths) => {
          if (paths.length > 0) attach.addPaths(paths);
          else showToast(`附件过大：单个文件不能超过 ${MAX_ATTACHMENT_BYTES / 1024 / 1024}MB`);
        })
        .catch(() => {
          showToast(`附件过大：单个文件不能超过 ${MAX_ATTACHMENT_BYTES / 1024 / 1024}MB`);
        });
      return;
    }

    let plainText = "";
    try {
      plainText = e.clipboardData?.getData("text/plain") ?? "";
    } catch {
      plainText = "";
    }
    if (!plainText) {
      void (async () => {
        try {
          const paths = await api.clipboardFilePaths();
          if (paths.length > 0) {
            attach.addPaths(paths);
            return;
          }
        } catch {
          // 忽略原生剪贴板路径探测异常，继续尝试位图截图
        }
        try {
          const img = await api.clipboardImage();
          if (img) attach.add([img]);
        } catch {
          // 忽略原生剪贴板图片探测异常
        }
      })();
    }
  };

  const hintText = () => {
    if (!props.terminalReady) return "终端未运行，启动就绪后可输入";
    if (props.canStop) {
      return "CLI 执行中 · 可点右侧停止按钮中止当前轮次，或继续输入";
    }
    return "Enter 发送 · Shift+Enter 换行 · 支持 Ctrl+V 粘贴截图/文件与拖入附件";
  };

  return (
    <div class={["composer", "terminal-composer", { "is-dragging": attach.dragging() }]}>
      <ImageAttachmentStrip images={attach.images()} onRemove={attach.remove} />
      <div class="composer-input-wrap">
        <textarea
          ref={textareaRef}
          class="composer-input terminal-composer-input"
          placeholder="输入长文本，支持 Ctrl+V 粘贴截图或文件、拖入附件后发送给反重力 CLI（Enter 发送，Shift+Enter 换行）"
          value={text()}
          onInput={(e) => setText(e.currentTarget.value)}
          onKeyDown={onKeyDown}
          onPaste={onPaste}
          rows={2}
        />
      </div>
      <div class="composer-bar terminal-composer-bar">
        <button
          type="button"
          class="terminal-composer-attach"
          title="选择本地文件或图片作为附件"
          onClick={() => void pickAttachments()}
        >
          <IconPaperclip size={14} />
          <span>附件</span>
        </button>
        <span class="terminal-composer-hint" title={hintText()}>
          {hintText()}
        </span>
        <div class="composer-actions">
          <Show when={props.canStop}>
            <button
              type="button"
              class="composer-btn stop"
              disabled={!props.terminalReady}
              title="停止当前任务（向 CLI 发送 Esc）"
              aria-label="停止当前任务"
              onClick={() => props.onStop()}
            >
              <IconStop size={15} />
            </button>
          </Show>
          <button
            type="button"
            class="terminal-composer-fill"
            disabled={disabled()}
            title="仅将内容粘贴到左侧 CLI 输入框，不自动回车"
            onClick={() => void submit(false)}
          >
            仅填入
          </button>
          <button
            type="button"
            class="composer-btn send"
            disabled={disabled()}
            title="发送到反重力 CLI（Enter）"
            aria-label="发送到反重力 CLI"
            onClick={() => void submit(true)}
          >
            <IconSend size={16} />
          </button>
        </div>
      </div>
    </div>
  );
}
