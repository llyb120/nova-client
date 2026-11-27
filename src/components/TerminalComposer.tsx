import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { createEffect, createSignal, onCleanup, onSettled, Show, untrack } from "solid-js";
import { formatTerminalPromptWithPaths } from "../agyTerminalPrompt";
import { api } from "../ipc";
import { clipboardFiles, MAX_ATTACHMENT_BYTES } from "../pasteAttachments";
import { isPasteFilePathsShortcut, resolveClipboardFilePaths } from "../pasteFilePaths";
import { saveSessionDraft, takeSessionDraft, terminalDraftKey } from "../promptDraft";
import { showToast } from "../store";
import { createImageAttachments, fileToAttachment, ImageAttachmentStrip } from "./ImageAttachmentStrip";
import { IconPaperclip, IconSend, IconStop } from "./icons";

export function TerminalComposer(props: {
  threadId: string;
  agy: boolean;
  autoSendState: "waiting" | "ready" | "cancelled";
  terminalReady: boolean;
  canStop: boolean;
  onStop: () => void;
  onSend: (payload: string, submit: boolean) => Promise<boolean>;
}) {
  let draftThreadId = untrack(() => props.threadId);
  const initialDraft = takeSessionDraft(terminalDraftKey(draftThreadId));
  const [text, setText] = createSignal(initialDraft?.text ?? "");
  const [sending, setSending] = createSignal(false);
  const [autoPending, setAutoPending] = createSignal(false);
  let disposed = false;
  let textareaRef: HTMLTextAreaElement | undefined;
  let resizeFrame: number | undefined;
  let pasteAsPaths = false;
  let pastePathsSeq = 0;

  const attach = createImageAttachments({ enableFileDrop: true });
  onSettled(() => {
    attach.set(initialDraft?.images ?? []);
    if (!initialDraft?.autoSend) return;
    setAutoPending(true);
    const timer = window.setTimeout(() => setAutoPending(false), 15_000);
    return () => window.clearTimeout(timer);
  });

  const flushInputResize = () => {
    resizeFrame = undefined;
    if (!textareaRef) return;
    textareaRef.style.height = "auto";
    const nextHeight = Math.max(44, Math.min(textareaRef.scrollHeight, 180));
    textareaRef.style.height = `${nextHeight}px`;
  };

  const resizeInput = () => {
    if (resizeFrame === undefined) resizeFrame = requestAnimationFrame(flushInputResize);
  };

  createEffect(text, resizeInput);

  createEffect(
    () => props.threadId,
    (nextId, prevId) => {
      if (!nextId || prevId === undefined || nextId === prevId) return;
      setAutoPending(false);
      saveSessionDraft(terminalDraftKey(draftThreadId), text(), attach.images());
      draftThreadId = nextId;
      const draft = takeSessionDraft(terminalDraftKey(nextId));
      setText(draft?.text ?? "");
      attach.set(draft?.images ?? []);
      queueMicrotask(resizeInput);
    },
  );

  onCleanup(() => {
    disposed = true;
    if (resizeFrame !== undefined) cancelAnimationFrame(resizeFrame);
    saveSessionDraft(terminalDraftKey(draftThreadId), text(), attach.images());
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
    setAutoPending(false);
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
    if (textareaRef) textareaRef.style.height = "44px";
  };

  const submit = async (autoSubmit: boolean, fromHome = false) => {
    setAutoPending(false);
    if (disabled()) return;
    const threadId = props.threadId;
    const currentText = text();
    const currentImages = attach.images();
    const sameDraft = () => currentText === text() && currentImages === attach.images();
    setSending(true);
    try {
      const paths = currentImages.length > 0
        ? await api.saveTerminalAttachments(currentImages.map((image) => ({ ...image })))
        : [];
      if (disposed || props.threadId !== threadId) return;
      if (fromHome && (!sameDraft() || props.autoSendState !== "ready")) return;
      if (paths.length !== currentImages.length) throw new Error("部分附件未能写入终端草稿");
      const payload = !props.agy && paths.length === 0 ? currentText : formatTerminalPromptWithPaths(currentText, paths);
      if (!payload || (fromHome && (payload.trimStart().startsWith("/") || /[\x00-\x09\x0b\x0c\x0e-\x1f\x7f]/.test(payload)))) return;
      const accepted = await props.onSend(payload, autoSubmit);
      if (!accepted) {
        if (!disposed) showToast("未确认写入终端，已保留草稿");
        return;
      }
      if (!disposed && props.threadId === threadId && sameDraft()) {
        clearInput();
        saveSessionDraft(terminalDraftKey(threadId), "", []);
      }
    } catch (error) {
      if (!disposed) showToast(`发送失败：${error}`);
    } finally {
      if (!disposed) setSending(false);
    }
  };

  createEffect(() => {
    if (!autoPending() || !initialDraft) return;
    const images = attach.images();
    if (text() !== initialDraft.text || images.length !== initialDraft.images.length || images.some((image, index) => {
      const seed = initialDraft.images[index];
      return image.name !== seed.name || image.mimeType !== seed.mimeType || image.data !== seed.data || image.uri !== seed.uri;
    })) return "cancelled";
    if (props.autoSendState === "cancelled") return "cancelled";
    if (props.autoSendState === "ready" && !disabled()) return "ready";
  }, (decision) => {
    if (decision === "cancelled") setAutoPending(false);
    if (decision === "ready") void submit(true, true);
  });

  const onKeyDown = (e: KeyboardEvent) => {
    if (isPasteFilePathsShortcut(e)) {
      setAutoPending(false);
      e.preventDefault();
      e.stopPropagation();
      pasteAsPaths = true;
      insertClipboardFilePaths();
      queueMicrotask(() => {
        pasteAsPaths = false;
      });
      return;
    }
    if (props.agy && e.key === "Enter" && !e.shiftKey && !e.isComposing && e.keyCode !== 229) {
      e.preventDefault();
      void submit(true);
    }
  };

  const onPaste = (e: ClipboardEvent) => {
    setAutoPending(false);
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

  const target = () => props.agy ? "反重力 CLI" : "终端";
  const hintText = () => {
    if (autoPending()) return "反重力 CLI 就绪后自动发送；编辑草稿或操作终端可取消";
    if (!props.terminalReady) return "终端未运行，启动就绪后可发送";
    if (!props.agy) return "草稿不会自动执行；核对后点击发送，内容可能作为命令执行";
    if (props.canStop) {
      return "CLI 执行中 · 可点右侧停止按钮中止当前轮次，或继续输入";
    }
    return "Enter 发送 · Shift+Enter 换行 · 支持 Ctrl+V 粘贴截图/文件与拖入附件";
  };

  return (
    <div class={["composer", "terminal-composer", { "is-dragging": attach.dragging() }]}>
      <ImageAttachmentStrip images={attach.images()} onRemove={(index) => { setAutoPending(false); attach.remove(index); }} />
      <div class="composer-input-wrap">
        <textarea
          ref={textareaRef}
          class="composer-input terminal-composer-input"
          placeholder={props.agy ? "输入内容发送给反重力 CLI（Enter 发送，Shift+Enter 换行）" : "终端草稿 · 核对后点击发送"}
          value={text()}
          readonly={sending()}
          onInput={(e) => { setAutoPending(false); setText(e.currentTarget.value); }}
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
          <Show when={props.agy}>
            <button
              type="button"
              class="terminal-composer-fill"
              disabled={disabled()}
              title="仅将内容粘贴到 CLI 输入框，不自动回车"
              onClick={() => void submit(false)}
            >
              仅填入
            </button>
          </Show>
          <button
            type="button"
            class="composer-btn send"
            disabled={disabled()}
            title={props.agy ? "发送到反重力 CLI（Enter）" : "发送到终端并回车，可能执行命令"}
            aria-label={`发送到${target()}`}
            onClick={() => void submit(true)}
          >
            <IconSend size={16} />
          </button>
        </div>
      </div>
    </div>
  );
}
