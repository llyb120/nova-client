import { message } from "@tauri-apps/plugin-dialog";
import { diffLines } from "diff";
import { createEffect, createMemo, createSignal, For, Index, Match, Show, Switch } from "solid-js";
import { api } from "../ipc";
import { isExpanded, state, toggleExpanded } from "../store";
import type { ToolContent, ToolItem } from "../types";
import { displayToolTitle, stripAnsi } from "../utils";
import { createFileContextMenu } from "./FileContextMenu";
import { relPath } from "./EditedFilesCard";
import { IconCheck, IconChevron, IconCopy, toolIcon } from "./icons";

/** 在配置的编辑器中打开文件（带可选行号），失败弹错误 */
function openInEditor(path: string, line?: number) {
  const id = state.currentId;
  if (!id || !path) return;
  void api.openInEditor(id, path, line).catch((e) => void message(String(e), { kind: "error" }));
}

const toolScrollPositions = new Map<string, number>();

/** 带悬停复制按钮的 pre 块 */
function CopyablePre(props: { class: string; text: string; scrollKey: string }) {
  const [copied, setCopied] = createSignal(false);
  let pre: HTMLPreElement | undefined;

  const restoreScrollPosition = () => {
    const element = pre;
    if (!element) return;
    queueMicrotask(() => {
      if (pre !== element) return;
      const scrollTop = toolScrollPositions.get(props.scrollKey);
      if (scrollTop === undefined) return;
      const maxScrollTop = Math.max(0, element.scrollHeight - element.clientHeight);
      const nextScrollTop = Math.min(scrollTop, maxScrollTop);
      element.scrollTop = nextScrollTop;
      toolScrollPositions.set(props.scrollKey, nextScrollTop);
    });
  };

  // 流式快照会替换文本节点，必要时也可能重挂载该 pre；两种情况都恢复用户的位置。
  createEffect(() => {
    props.text;
    restoreScrollPosition();
  });

  const copy = (e: MouseEvent) => {
    e.stopPropagation();
    void navigator.clipboard.writeText(props.text);
    setCopied(true);
    setTimeout(() => setCopied(false), 1200);
  };
  return (
    <div class="codeblock">
      <button
        type="button"
        class={`code-copy ${copied() ? "copied" : ""}`}
        title="复制"
        onClick={copy}
      >
        {copied() ? <IconCheck size={13} /> : <IconCopy size={13} />}
      </button>
      <pre
        ref={(element) => {
          pre = element;
          restoreScrollPosition();
        }}
        class={props.class}
        onScroll={(event) => toolScrollPositions.set(props.scrollKey, event.currentTarget.scrollTop)}
      >{props.text}</pre>
    </div>
  );
}

function basename(p: string) {
  return p.split(/[\\/]/).pop() || p;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function stringifyJson(value: unknown) {
  try {
    return stripAnsi(JSON.stringify(value, null, 2));
  } catch {
    return stripAnsi(String(value));
  }
}

function isUsefulText(text: string | undefined): text is string {
  if (!text) return false;
  const clean = stripAnsi(text).trim();
  return clean !== "" && clean !== "null" && clean !== "undefined";
}

function compactValue(value: unknown): string {
  if (value == null) return "";
  if (typeof value === "string") return stripAnsi(value).trim();
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  const text = stringifyJson(value).replace(/\s+/g, " ").trim();
  return text.length > 180 ? `${text.slice(0, 180)}...` : text;
}

function rawPreview(raw: unknown): string {
  if (!isRecord(raw)) return compactValue(raw);
  const preferred = [
    "command",
    "cmd",
    "query",
    "q",
    "path",
    "file_path",
    "url",
    "symbol",
    "task",
    "prompt",
  ];
  for (const key of preferred) {
    const text = compactValue(raw[key]);
    if (text) return `${key}: ${text}`;
  }
  const parts = Object.entries(raw)
    .filter(([, value]) => value != null)
    .slice(0, 3)
    .map(([key, value]) => {
      const text = compactValue(value);
      return text ? `${key}: ${text}` : "";
    })
    .filter(Boolean);
  return parts.join("\n");
}

function toolSummary(item: ToolItem): string {
  return rawPreview(item.rawInput) || rawPreview(item.rawOutput);
}

function DiffView(props: {
  path: string;
  oldText?: string | null;
  newText: string;
  onFileContextMenu: (event: MouseEvent, path: string) => void;
}) {
  const rows = createMemo(() => {
    const parts = diffLines(props.oldText ?? "", props.newText ?? "");
    const out: { sign: string; text: string; cls: string }[] = [];
    for (const part of parts) {
      const cls = part.added ? "diff-add" : part.removed ? "diff-del" : "diff-ctx";
      const sign = part.added ? "+" : part.removed ? "-" : " ";
      const lines = part.value.replace(/\n$/, "").split("\n");
      // 未变更的大段上下文只保留首尾
      if (!part.added && !part.removed && lines.length > 8) {
        for (const l of lines.slice(0, 3)) out.push({ sign, text: l, cls });
        out.push({ sign: " ", text: `… 省略 ${lines.length - 6} 行 …`, cls: "diff-skip" });
        for (const l of lines.slice(-3)) out.push({ sign, text: l, cls });
      } else {
        for (const l of lines) out.push({ sign, text: l, cls });
      }
    }
    return out;
  });
  return (
    <div class="diff-view">
      <button
        class="diff-path clickable"
        onClick={() => openInEditor(props.path)}
        onContextMenu={(event) => props.onFileContextMenu(event, props.path)}
        title={`在编辑器中打开 ${props.path}`}
      >
        {relPath(props.path)}
      </button>
      <pre class="diff-body">
        <For each={rows()}>
          {(r) => (
            <div class={`diff-line ${r.cls}`}>
              <span class="diff-sign">{r.sign}</span>
              <span>{r.text}</span>
            </div>
          )}
        </For>
      </pre>
    </div>
  );
}

function ContentBlock(props: {
  block: ToolContent;
  scrollKey: string;
  onFileContextMenu: (event: MouseEvent, path: string) => void;
}) {
  return (
    <Switch>
      <Match when={props.block.type === "diff"}>
        {(() => {
          const b = props.block as Extract<ToolContent, { type: "diff" }>;
          return (
            <DiffView
              path={b.path}
              oldText={b.oldText}
              newText={b.newText}
              onFileContextMenu={props.onFileContextMenu}
            />
          );
        })()}
      </Match>
      <Match when={props.block.type === "content"}>
        {(() => {
          const inner = (props.block as { content: { type: string; text?: string } }).content;
          const text = inner?.type === "text" && isUsefulText(inner.text) ? stripAnsi(inner.text) : "";
          return (
            <Show when={text}>
              <CopyablePre class="tool-output" text={text} scrollKey={props.scrollKey} />
            </Show>
          );
        })()}
      </Match>
    </Switch>
  );
}

const KIND_LABEL: Record<string, string> = {
  read: "读取",
  edit: "编辑",
  delete: "删除",
  move: "移动",
  search: "搜索",
  execute: "执行",
  think: "思考",
  fetch: "抓取",
};

/** codex 风格紧凑工具行：图标 + 标题 + diff 统计 + 状态点，点击展开详情 */
export function ToolCallCard(props: { item: ToolItem; active?: boolean }) {
  const fileMenu = createFileContextMenu();
  // 展开状态放 store（按 item id）：流式更新重挂载组件时不丢失
  const key = () => `tool-${props.item.id}`;
  const rawKey = () => `tool-raw-${props.item.id}`;
  const scrollKey = (part: string) => `${state.currentId ?? ""}-${props.item.id}-${part}`;
  const defaultOpen = () =>
    !!props.active || props.item.status === "pending" || props.item.status === "in_progress";
  const open = () => isExpanded(key(), defaultOpen());
  const showRaw = () => isExpanded(rawKey());
  const hasBody = createMemo(
    () =>
      props.item.content.length > 0 ||
      props.item.locations.length > 0 ||
      props.item.rawInput !== undefined ||
      props.item.rawOutput !== undefined,
  );
  const visibleContent = createMemo(() =>
    props.item.content.some((block) => {
      if (block.type !== "content") return true;
      const inner = (block as { content?: { type?: string; text?: string } }).content;
      return inner?.type !== "text" || isUsefulText(inner.text);
    }),
  );
  const summary = createMemo(() => toolSummary(props.item));

  // 文件编辑统计 +N -N（codex 风格）
  const stats = createMemo(() => {
    let add = 0;
    let del = 0;
    for (const b of props.item.content) {
      if (b.type !== "diff") continue;
      const d = b as Extract<ToolContent, { type: "diff" }>;
      for (const part of diffLines(d.oldText ?? "", d.newText ?? "")) {
        const n = part.count ?? 0;
        if (part.added) add += n;
        else if (part.removed) del += n;
      }
    }
    return { add, del };
  });

  const label = () => {
    const t = (props.item.title || "").trim();
    if (t) return displayToolTitle(stripAnsi(t));
    return KIND_LABEL[props.item.kind] ?? props.item.kind;
  };

  return (
    <div class={`tool-row status-${props.item.status}`}>
      <button
        type="button"
        class="tool-line"
        onClick={() => hasBody() && toggleExpanded(key(), !open())}
      >
        <span class="tool-icon">{toolIcon(props.item.kind)}</span>
        <span class="tool-title" title={label()}>
          {label()}
        </span>
        <Show when={stats().add + stats().del > 0}>
          <span class="tool-stats">
            <span class="stat-add">+{stats().add}</span>
            <span class="stat-del">-{stats().del}</span>
          </span>
        </Show>
        <Switch>
          <Match when={props.item.status === "in_progress" || props.item.status === "pending"}>
            <span class="spinner" />
          </Match>
          <Match when={props.item.status === "failed"}>
            <span class="tool-dot failed" title="失败" />
          </Match>
        </Switch>
        <Show when={hasBody()}>
          <span class="tool-chevron">
            <IconChevron size={12} open={open()} />
          </span>
        </Show>
      </button>
      <Show when={open()}>
        <div class="tool-body">
          <Show when={props.item.locations.length > 0}>
            <div class="tool-locations">
              <For each={props.item.locations}>
                {(loc) => (
                  <button
                    class="loc-chip clickable"
                    title={`在编辑器中打开 ${loc.path ?? ""}`}
                    onClick={() => openInEditor(loc.path ?? "", loc.line ?? undefined)}
                    onContextMenu={(event) => loc.path && fileMenu.open(event, loc.path)}
                  >
                    {basename(loc.path ?? "")}
                    {loc.line != null ? `:${loc.line}` : ""}
                  </button>
                )}
              </For>
            </div>
          </Show>
          <Index each={props.item.content}>
            {(block, index) => (
              <ContentBlock
                block={block()}
                scrollKey={scrollKey(`output-${index}`)}
                onFileContextMenu={fileMenu.open}
              />
            )}
          </Index>
          <Show when={!visibleContent() && summary()}>
            <div class="tool-summary">{summary()}</div>
          </Show>
          <Show when={props.item.rawInput !== undefined || props.item.rawOutput !== undefined}>
            <button
              type="button"
              class="raw-toggle"
              onClick={(e) => {
                e.stopPropagation();
                toggleExpanded(rawKey());
              }}
            >
              {showRaw() ? "隐藏原始数据" : "原始数据"}
            </button>
            <Show when={showRaw()}>
              <Show when={props.item.rawInput !== undefined}>
                <CopyablePre
                  class="tool-raw"
                  text={`输入: ${stringifyJson(props.item.rawInput)}`}
                  scrollKey={scrollKey("raw-input")}
                />
              </Show>
              <Show when={props.item.rawOutput !== undefined}>
                <CopyablePre
                  class="tool-raw"
                  text={`输出: ${stringifyJson(props.item.rawOutput)}`}
                  scrollKey={scrollKey("raw-output")}
                />
              </Show>
            </Show>
          </Show>
        </div>
      </Show>
      <fileMenu.Menu />
    </div>
  );
}
