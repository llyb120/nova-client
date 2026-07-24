import { convertFileSrc } from "@tauri-apps/api/core";
import { createSignal, For, Match, Show, Switch } from "solid-js";
import { editUserMessage, isExpanded, state, toggleExpanded } from "../store";
import type { Item, PromptImage } from "../types";
import { createFileContextMenu } from "./FileContextMenu";
import { IconChevron, IconFile, IconPencil } from "./icons";
import { createImageAttachments, ImageAttachmentStrip } from "./ImageAttachmentStrip";
import { Markdown } from "./Markdown";
import { ToolCallCard } from "./ToolCallCard";

function attachmentPath(img: PromptImage): string | undefined {
  if (img.data || !img.uri) return undefined;
  return decodeURI(img.uri.replace(/^file:\/+/, ""));
}

function attachmentSrc(img: PromptImage): string {
  if (img.data) return `data:${img.mimeType};base64,${img.data}`;
  return convertFileSrc(attachmentPath(img) ?? "");
}

function normalizeThoughtMarkdown(text: string): string {
  // Older OpenCode sessions joined adjacent reasoning parts as **A****B**.
  return state.agentKind === "opencode"
    ? text.replace(/(\S)\*{4}(?=\S)/g, "$1**\n\n**")
    : text;
}

function isCodexModelResumeWarning(item: Item): boolean {
  if (item.type !== "system" || item.level !== "error") return false;
  return (
    item.text.startsWith("This session was recorded with model `") &&
    item.text.includes("` but is resuming with `") &&
    item.text.includes("`. Consider switching back to `") &&
    item.text.endsWith("` as it may affect Codex performance.")
  );
}

/** 用户消息气泡：hover 显示编辑按钮，编辑后从该处重新开始会话（codex 风格） */
function UserMessage(props: { item: Extract<Item, { type: "user" }> }) {
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  // 编辑时附件可见可管理：保留原图、可粘贴新增、可移除
  const attach = createImageAttachments();
  const fileMenu = createFileContextMenu();
  const running = () => !!(state.currentId && state.running[state.currentId]);

  const openAttachmentMenu = (event: MouseEvent, image: PromptImage) => {
    const path = attachmentPath(image);
    if (path) fileMenu.open(event, path);
  };

  const startEdit = () => {
    setDraft(props.item.text);
    attach.set(props.item.images ?? []);
    setEditing(true);
  };

  const save = () => {
    const text = draft().trim();
    const images = attach.images();
    if (!text && images.length === 0) return;
    setEditing(false);
    void editUserMessage(props.item.id, text, images);
  };

  return (
    <div class="msg msg-user">
      <Show
        when={!editing()}
        fallback={
          <div class="user-edit">
            <ImageAttachmentStrip images={attach.images()} onRemove={attach.remove} />
            <textarea
              class="user-edit-input"
              value={draft()}
              rows={Math.min(10, Math.max(2, draft().split("\n").length))}
              onPaste={attach.onPaste}
              onInput={(e) => setDraft(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
                  e.preventDefault();
                  save();
                }
                if (e.key === "Escape") setEditing(false);
              }}
              ref={(el) =>
                queueMicrotask(() => {
                  el.focus();
                  el.setSelectionRange(el.value.length, el.value.length);
                })
              }
            />
            <div class="user-edit-actions">
              <span class="user-edit-hint">发送后将从此处重新开始会话</span>
              <button class="btn secondary small" onClick={() => setEditing(false)}>
                取消
              </button>
              <button class="btn primary small" onClick={save}>
                发送
              </button>
            </div>
          </div>
        }
      >
        <Show when={!running()}>
          <button class="user-edit-btn" title="编辑此消息，并从这里重新开始" onClick={startEdit}>
            <IconPencil size={13} />
          </button>
        </Show>
        <div class="user-bubble">
          <Show when={props.item.images?.length}>
            <div class="bubble-images">
              <For each={props.item.images}>
                {(img) => (
                  <Show
                    when={img.mimeType.startsWith("image/")}
                    fallback={
                      <span
                        class="bubble-file"
                        title={img.name}
                        onContextMenu={(event) => openAttachmentMenu(event, img)}
                      >
                        <IconFile size={15} />
                        {img.name}
                      </span>
                    }
                  >
                    <img
                      src={attachmentSrc(img)}
                      alt={img.name}
                      title={img.name}
                      onContextMenu={(event) => openAttachmentMenu(event, img)}
                    />
                  </Show>
                )}
              </For>
            </div>
          </Show>
          {props.item.text}
        </div>
        <fileMenu.Menu />
      </Show>
    </div>
  );
}

/** codex 风格：用户消息右对齐圆角块，assistant 直接正文，工具调用为紧凑行 */
export function TranscriptItem(props: { item: Item; active?: boolean }) {
  const thoughtKey = () => `thought-${props.item.id}`;
  const thoughtOpen = () => isExpanded(thoughtKey(), !!props.active);

  return (
    <Switch>
      <Match when={props.item.type === "user"}>
        <UserMessage item={props.item as Extract<Item, { type: "user" }>} />
      </Match>
      <Match when={props.item.type === "assistant"}>
        {(() => {
          const item = props.item as Extract<Item, { type: "assistant" }>;
          return (
            <Show when={item.text.trim() !== "None"}>
              <div class="msg msg-assistant">
                <Markdown text={item.text} markFiles live={props.active} />
              </div>
            </Show>
          );
        })()}
      </Match>
      <Match when={props.item.type === "thought"}>
        <Show
          when={(props.item as Extract<Item, { type: "thought" }>).text === "思考中…"}
          fallback={
            <div class="msg msg-thought">
              <button
                type="button"
                class="thought-toggle"
                onClick={() => toggleExpanded(thoughtKey(), !thoughtOpen())}
              >
                <IconChevron size={12} open={thoughtOpen()} />
                思考过程
              </button>
              <Show when={thoughtOpen()}>
                <div class="thought-body">
                  <Markdown
                    text={normalizeThoughtMarkdown(
                      (props.item as Extract<Item, { type: "thought" }>).text,
                    )}
                  />
                </div>
              </Show>
            </div>
          }
        >
          <div class="msg msg-thought thinking">思考中…</div>
        </Show>
      </Match>
      <Match when={props.item.type === "tool"}>
        <ToolCallCard item={props.item as Extract<Item, { type: "tool" }>} active={props.active} />
      </Match>
      <Match when={props.item.type === "system" && !isCodexModelResumeWarning(props.item)}>
        {(() => {
          const item = props.item as Extract<Item, { type: "system" }>;
          const isCompaction = item.level === "compacting" || item.level === "compacted";
          return (
            <Show
              when={isCompaction}
              fallback={<div class={`msg msg-system level-${item.level}`}>{item.text}</div>}
            >
              <div class={`compaction-divider ${item.level}`}>
                <span class="compaction-line" />
                <span class="compaction-label">
                  <Show when={item.level === "compacting"}>
                    <span class="spinner small" />
                  </Show>
                  {item.text}
                </span>
                <span class="compaction-line" />
              </div>
            </Show>
          );
        })()}
      </Match>
    </Switch>
  );
}
