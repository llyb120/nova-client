import { convertFileSrc } from "@tauri-apps/api/core";
import { message } from "@tauri-apps/plugin-dialog";
import { diffLines } from "diff";
import { createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { api } from "../ipc";
import { editUserMessage, isExpanded, respondPermission, state, toggleExpanded } from "../store";
import type { Item, PermissionRequest, ToolItem, UserItem } from "../types";
import { displayToolTitle, stripAnsi } from "../utils";
import { CanvasDOM, h, measureText, Node, parseMarkdown } from "../canvas-dom/CanvasDOM.js";
import { relPath } from "./EditedFilesCard";
import { createImageAttachments, ImageAttachmentStrip } from "./ImageAttachmentStrip";
import type { Group } from "./TurnGroup";
import { fmtDuration, fmtTokens } from "./TurnGroup";

export interface CanvasTranscriptHandle {
  scrollToBottom(): void;
  scrollToGroup(index: number): void;
  scrollBy(delta: number): void;
  isAtBottom(): boolean;
  scrollTop(): number;
  maxScrollTop(): number;
  activeGroup(): number;
  hasFocusedInput(): boolean;
}

interface CanvasTranscriptProps {
  groups: Group[];
  permissions: PermissionRequest[];
  running: boolean;
  emptyHint: string;
  preview: boolean;
  onReturnToCurrent: () => void;
  onScroll?: (top: number, max: number, user: boolean) => void;
  ref?: (handle: CanvasTranscriptHandle) => void;
}

type Palette = {
  bg: string;
  panel: string;
  sidebar: string;
  hover: string;
  border: string;
  borderLight: string;
  text: string;
  dim: string;
  muted: string;
  faint: string;
  accent: string;
  accentDim: string;
  red: string;
  yellow: string;
  green: string;
  blue: string;
  scroll: string;
  mono: string;
  sans: string;
  columnWidth: number;
};

function palette(): Palette {
  // 一次读取全部 CSS 变量；此前每个字段各触发一次 getComputedStyle，会放大流式重绘成本。
  const style = getComputedStyle(document.documentElement);
  const value = (name: string, fallback: string) => style.getPropertyValue(name).trim() || fallback;
  return {
    bg: value("--bg", "#0e1014"), panel: value("--bg-panel", "#171b23"),
    sidebar: value("--bg-sidebar", "#0a0c10"), hover: value("--bg-hover", "#1e232d"),
    border: value("--border", "#252a33"), borderLight: value("--border-light", "#313844"),
    text: value("--text", "#e4e7ec"), dim: value("--text-dim", "#a9b0bd"),
    muted: value("--text-muted", "#7d8593"), faint: value("--text-faint", "#5d6470"),
    accent: value("--accent", "#6e93f8"), accentDim: value("--accent-dim", "rgba(110,147,248,.14)"),
    red: value("--red", "#e07d76"), yellow: value("--yellow", "#d4b26e"),
    green: value("--green", "#8ec489"), blue: value("--blue", "#7aa2f2"),
    scroll: value("--scroll", "#2e333d"),
    mono: value("--mono", "monospace"),
    sans: value("--sans", "sans-serif"), columnWidth: 980,
  };
}

// DOM div 的默认 width 是 auto；强塞 100% 会在存在左右 margin 时比 CSS 多占一截。
const block = (style: Record<string, unknown> = {}, text = "", children: Node[] = []) =>
  h("div", style, text, children);
const button = (text: string, style: Record<string, unknown>, action: () => void) => {
  const node = h("button", { cursor: "pointer", userSelect: "none", ...style }, text, []);
  node._action = action;
  return node;
};

function restyleMarkdown(
  node: Node,
  p: Palette,
  inherited: { size: number; lineHeight: number; weight: string | number; color: string } =
    { size: 14, lineHeight: 1.7, weight: "normal", color: p.text },
  parentTag = "",
): void {
  let next = inherited;
  node.style.fontFamily = p.sans;
  node.style.fontSize = inherited.size;
  node.style.lineHeight = inherited.lineHeight;
  node.style.fontWeight = inherited.weight;
  node.style.color = inherited.color;

  if (node.tag === "p") {
    node.style.margin = [0, 0, 10, 0];
  } else if (node.tag === "ul" || node.tag === "ol") {
    node.style.margin = [0, 0, 10, 0];
    node.style.padding = [0, 0, 0, 22];
  } else if (node.tag === "li") {
    node.style.margin = [3, 0];
  } else if (/^h[1-6]$/.test(node.tag)) {
    const level = Number(node.tag.slice(1));
    const size = level === 1 ? 17.5 : level === 2 ? 16.1 : level <= 4 ? 14.7 : 14;
    next = { ...inherited, size, weight: 600 };
    node.style.fontSize = size;
    node.style.fontWeight = 600;
    node.style.lineHeight = 1.7;
    node.style.margin = [16, 0, 8, 0];
  } else if (node.tag === "pre") {
    next = { ...inherited, size: 12.5 };
    node.style.fontFamily = p.mono;
    node.style.fontSize = 12.5;
    node.style.background = p.panel;
    node.style.border = 1;
    node.style.borderColor = p.border;
    node.style.borderRadius = 10;
    node.style.padding = [12, 14];
    node.style.margin = [0, 0, 10, 0];
    node.style.whiteSpace = "normal";
    node.style.color = p.text;
  } else if (node.tag === "code") {
    next = { ...inherited, size: 12.5 };
    node.style.fontFamily = p.mono;
    node.style.fontSize = 12.5;
    node.style.color = p.text;
    if (parentTag === "pre") {
      // .markdown pre code 会清掉行内 code 的盒子，只保留代码块自身的 padding / border。
      node.style.background = "transparent";
      node.style.border = 0;
      node.style.borderRadius = 0;
      node.style.padding = 0;
    } else {
      node.style.background = p.panel;
      node.style.border = 1;
      node.style.borderColor = p.border;
      node.style.borderRadius = 5;
      node.style.padding = [1, 6];
    }
  } else if (node.tag === "blockquote") {
    next = { ...inherited, color: p.dim };
    node.style.background = "transparent";
    node.style.border = [0, 0, 0, 3];
    node.style.borderColor = `color-mix(in srgb, ${p.accent} 40%, transparent)`;
    node.style.padding = [2, 0, 2, 14];
    node.style.margin = [0, 0, 10, 0];
    node.style.color = p.dim;
  } else if (node.tag === "a") {
    next = { ...inherited, color: p.blue };
    node.style.color = p.blue;
  } else if (node.tag === "strong") {
    next = { ...inherited, weight: 700 };
    node.style.fontWeight = 700;
  } else if (node.tag === "hr") {
    node.style.background = p.border;
    node.style.margin = [12, 0];
  }
  for (const child of node.children) restyleMarkdown(child, p, next, node.tag);
}

function markdown(text: string, p: Palette): Node {
  const node = parseMarkdown(text, {
    width: "100%", color: p.text, fontFamily: p.sans, fontSize: 14, lineHeight: 1.7,
  });
  restyleMarkdown(node, p);
  const last = node.children.at(-1);
  if (last?.tag === "p") last.style.margin = 0;
  return node;
}

function textPreview(value: unknown): string {
  if (value == null) return "";
  if (typeof value === "string") return stripAnsi(value);
  try { return stripAnsi(JSON.stringify(value, null, 2)); } catch { return String(value); }
}

function openFile(path: string, line?: number): void {
  const threadId = state.currentId;
  if (!threadId || !path) return;
  void api.openInEditor(threadId, path, line).catch((error) => void message(String(error), { kind: "error" }));
}

const nestedScrollPositions = new Map<string, number>();

function outputBlock(
  text: string,
  p: Palette,
  scrollKey: string,
  maxHeight = 320,
  raw = false,
): Node {
  // 与原 DOM 一致：tool-body 自身不滚动，每个 output/raw 框独立限制高度。
  const node = block({ margin: 0, padding: raw ? 8 : 10, maxHeight, overflow: "auto",
    background: p.sidebar, borderRadius: 7, border: 1, borderColor: p.border,
    color: raw ? p.faint : p.dim, fontFamily: p.mono, fontSize: raw ? 11.5 : 12,
    lineHeight: 1.55, whiteSpace: "pre-wrap", scrollbarTrack: "transparent",
    scrollbarThumb: p.scroll, scrollbarWidth: 4 }, text);
  node._scrollKey = scrollKey;
  return node;
}

function diffBlock(item: ToolItem, contentIndex: number,
  diff: Extract<ToolItem["content"][number], { type: "diff" }>, p: Palette): Node {
  const rows: Node[] = [];
  for (const part of diffLines(diff.oldText ?? "", diff.newText)) {
    const sign = part.added ? "+" : part.removed ? "-" : " ";
    const lines = part.value.replace(/\n$/, "").split("\n");
    const visible = !part.added && !part.removed && lines.length > 8
      ? [...lines.slice(0, 3), `… 省略 ${lines.length - 6} 行 …`, ...lines.slice(-3)] : lines;
    for (const line of visible) {
      const skipped = line.startsWith("… 省略 ");
      const mix = part.added ? p.accent : part.removed ? p.red : "";
      rows.push(h("div", { width: "100%", display: "flex", flexDirection: "row",
        padding: [0, 10], background: mix ? `color-mix(in srgb, ${mix} 13%, transparent)` : "transparent",
        color: skipped ? p.faint : mix
          ? `color-mix(in srgb, ${mix} ${part.added ? 60 : 58}%, ${p.text})` : p.dim,
        fontStyle: skipped ? "italic" : "normal", fontFamily: p.mono,
        fontSize: 12, lineHeight: 1.5 }, "", [
        block({ width: 16, flex: "0 0 auto", color: p.faint, fontFamily: p.mono,
          fontSize: 12, lineHeight: 1.5, whiteSpace: "pre" }, skipped ? " " : sign),
        block({ flex: "1 1 auto", minWidth: 0, fontFamily: p.mono, fontSize: 12,
          lineHeight: 1.5, whiteSpace: "pre-wrap" }, line),
      ]));
    }
  }
  const body = block({ width: "100%", maxHeight: 360, overflow: "auto", background: p.sidebar,
    scrollbarTrack: "transparent", scrollbarThumb: p.scroll, scrollbarWidth: 4 }, "", rows);
  body._scrollKey = `tool-${item.id}-diff-${contentIndex}`;
  return block({ margin: 0, background: p.sidebar, borderRadius: 7,
    border: 1, borderColor: p.border, color: p.dim, overflow: "hidden" }, "", [
    button(relPath(diff.path), { width: "100%", padding: [6, 10], background: p.panel,
      hoverBackground: p.hover, border: [0, 0, 1, 0], borderColor: p.border, color: p.blue,
      fontFamily: p.mono, fontSize: 11.5, textAlign: "left", whiteSpace: "nowrap",
      textOverflow: "ellipsis" }, () => openFile(diff.path)),
    body,
  ]);
}

function toolBody(item: ToolItem, p: Palette): Node[] {
  const result: Node[] = [];
  let hasVisibleContent = false;
  if (item.locations.length) {
    const locations = h("div", { width: "100%", display: "flex", flexDirection: "row",
      flexWrap: "wrap", gap: 6 }, "", []);
    for (const location of item.locations) {
      if (!location.path) continue;
      const path = location.path;
      locations.appendChild(button(`${path.split(/[\\\\/]/).pop() ?? path}${location.line == null ? "" : `:${location.line}`}`,
        { width: "auto", padding: [2, 9], borderRadius: 20,
          background: p.panel, hoverBackground: p.hover, hoverDecoration: "underline", color: p.blue,
          fontFamily: p.mono, fontSize: 11.5 }, () => openFile(path, location.line)));
    }
    if (locations.children.length) result.push(locations);
  }
  for (const [contentIndex, content] of item.content.entries()) {
    if (content.type === "diff") {
      const diff = content as Extract<ToolItem["content"][number], { type: "diff" }>;
      hasVisibleContent = true;
      result.push(diffBlock(item, contentIndex, diff, p));
    } else if (content.type === "content") {
      const inner = content.content as { type?: string; text?: string };
      if (inner?.type === "text" && inner.text && stripAnsi(inner.text).trim()) {
        hasVisibleContent = true;
        result.push(outputBlock(stripAnsi(inner.text), p, `tool-${item.id}-output-${contentIndex}`, 320));
      }
    }
  }
  if (!hasVisibleContent) {
    const preview = textPreview(item.rawInput) || textPreview(item.rawOutput);
    if (preview) result.push(block({ width: "auto", maxWidth: "100%", padding: [7, 10],
      background: p.panel, border: 1, borderColor: p.border, borderRadius: 7,
      color: p.dim, fontFamily: p.mono, fontSize: 12, lineHeight: 1.55,
      whiteSpace: "pre-wrap" }, preview));
  }
  return result;
}

function tool(item: ToolItem, active: boolean, p: Palette, rebuild: () => void): Node {
  const key = `tool-${item.id}`;
  const busy = item.status === "pending" || item.status === "in_progress";
  // 仅执行期间自动展开；完成后没有明确操作记录就自动闭合。用户点过后始终尊重显式状态。
  const defaultOpen = active || busy;
  const open = isExpanded(key, defaultOpen);
  const hasBody = item.content.length > 0 || item.locations.length > 0 || item.rawInput !== undefined || item.rawOutput !== undefined;
  let add = 0;
  let del = 0;
  for (const content of item.content) {
    if (content.type !== "diff") continue;
    const diff = content as Extract<ToolItem["content"][number], { type: "diff" }>;
    for (const part of diffLines(diff.oldText ?? "", diff.newText)) {
      if (part.added) add += part.count ?? 0;
      else if (part.removed) del += part.count ?? 0;
    }
  }
  const label = displayToolTitle(stripAnsi(item.title || item.kind));
  const children: Node[] = [button(label,
    { width: "100%", minHeight: 26, padding: [3, 8], borderRadius: 7,
      color: item.status === "failed" ? p.red : p.dim, fontFamily: p.mono,
      fontSize: 12, lineHeight: 1.55, whiteSpace: "nowrap", textOverflow: "ellipsis", textAlign: "left",
      background: "transparent", hoverBackground: p.hover, activeBackground: p.hover,
      leadingIcon: item.kind, iconColor: p.faint,
      trailingStats: add + del > 0 ? { add, del, addColor: p.accent, delColor: p.red } : null,
      trailingStatus: busy ? "busy" : item.status === "failed" ? "failed" : null,
      statusColor: busy ? p.blue : p.red, trailingChevron: hasBody ? open : null,
      trailingInline: true, chevronColor: p.faint }, () => {
      if (hasBody) toggleExpanded(key, !open);
      rebuild();
    })];
  if (open) {
    const details = block({ display: "flex", flexDirection: "column", gap: 8,
      margin: [4, 0, 8, 14], padding: [4, 0, 4, 14],
      border: [0, 0, 0, 2], borderColor: p.border }, "", toolBody(item, p));
    if (item.rawInput !== undefined || item.rawOutput !== undefined) {
      const rawKey = `tool-raw-${item.id}`;
      const rawOpen = isExpanded(rawKey);
      details.appendChild(button(rawOpen ? "隐藏原始数据" : "原始数据", { width: "auto",
        padding: [2, 0], color: p.blue, fontSize: 12, borderRadius: 5, textAlign: "left" }, () => {
        toggleExpanded(rawKey, !rawOpen); rebuild();
      }));
      if (rawOpen) {
        if (item.rawInput !== undefined) {
          details.appendChild(outputBlock(`输入: ${textPreview(item.rawInput)}`, p,
            `tool-${item.id}-raw-input`, 200, true));
        }
        if (item.rawOutput !== undefined) {
          details.appendChild(outputBlock(`输出: ${textPreview(item.rawOutput)}`, p,
            `tool-${item.id}-raw-output`, 200, true));
        }
      }
    }
    children.push(details);
  }
  return block({ margin: [1, 0] }, "", children);
}

function itemNode(item: Item, active: boolean, p: Palette, rebuild: () => void,
  edit: (item: UserItem, bubble: Node) => void, editingId?: number): Node | null {
  if (item.type === "turn") return null;
  if (item.type === "user") {
    const bubbleChildren: Node[] = [];
    if (item.images?.length) {
      const strip = h("div", { width: "100%", display: "flex", flexDirection: "row", flexWrap: "wrap", gap: 7, margin: [0, 0, 8] }, "", []);
      for (const image of item.images) {
        if (image.mimeType.startsWith("image/")) {
          const src = image.data ? `data:${image.mimeType};base64,${image.data}`
            : convertFileSrc(decodeURI((image.uri ?? "").replace(/^file:\/\/+/, "")));
          strip.appendChild(h("img", { width: 150, height: 96, borderRadius: 7, src, alt: image.name }, "", []));
        } else {
          strip.appendChild(block({ width: "auto", padding: [4, 7], border: 1, borderColor: p.borderLight,
            borderRadius: 6, color: p.dim, fontSize: 11.5 }, `▧ ${image.name}`));
        }
      }
      bubbleChildren.push(strip);
    }
    if (item.text) bubbleChildren.push(block({ color: p.text, fontSize: 14, lineHeight: 1.6,
      whiteSpace: "pre-wrap", textAlign: "left" }, item.text));
    const textWidth = Math.max(0, ...item.text.split("\n").map((line) =>
      measureText(line, 14, p.sans, "normal", "normal")));
    const attachmentWidth = item.images?.length ? Math.min(640, item.images.length * 157) : 0;
    // 内容宽度 + 32px padding + 2px border；按整数像素扩到可容纳宽度，避免短提示词临界换行。
    const bubbleWidth = Math.min(p.columnWidth * .85,
      Math.max(72, Math.ceil(textWidth) + 35, attachmentWidth + 34));
    const bubble = block({ width: bubbleWidth, padding: [10, 16], background: p.accentDim,
      border: 1, borderColor: `color-mix(in srgb, ${p.accent} 26%, transparent)`,
      borderRadius: [14, 14, 6, 14],
      color: p.text, fontSize: 14, lineHeight: 1.6, textAlign: "left" }, "", bubbleChildren);
    if (item.id === editingId) return block({ height: 150, margin: [20, 0, 16] });
    const children = state.currentId && !state.running[state.currentId]
      ? [button("", { width: 28, height: 26, padding: [6, 7], color: p.faint,
          margin: [0, 5], borderRadius: 6, leadingIcon: "edit", iconColor: p.faint,
          opacity: 0, hoverOpacity: 1, hoverBackground: p.hover, hoverColor: p.text },
          () => edit(item, bubble)), bubble]
      : [bubble];
    const row = block({ display: "flex", flexDirection: "row", justifyContent: "flex-end",
      alignItems: "center", margin: [20, 0, 16] }, "", children);
    row._editHoverRoot = true;
    return row;
  }
  if (item.type === "assistant") {
    if (item.text.trim() === "None") return null;
    return block({ margin: [14, 0] }, "", [markdown(item.text, p)]);
  }
  if (item.type === "thought") {
    if (item.text === "思考中…") return block({ margin: [6, 0], color: p.faint, fontSize: 13 }, "◌  思考中…");
    const key = `thought-${item.id}`;
    const open = isExpanded(key, active);
    return block({ margin: [6, 0] }, "", [
      button("思考过程", { color: p.faint, fontSize: 12, padding: [3, 6],
        borderRadius: 6, leadingChevron: open, chevronColor: p.faint,
        hoverBackground: p.hover, hoverColor: p.dim }, () => {
        toggleExpanded(key, !open); rebuild();
      }),
      ...(open ? [block({ margin: [6, 0, 0], padding: [8, 14], border: [0, 0, 0, 2],
        borderColor: p.borderLight, color: p.dim, fontSize: 13 }, "", [markdown(item.text, p)])] : []),
    ]);
  }
  if (item.type === "tool") return tool(item, active, p, rebuild);
  if (item.level === "compacting" || item.level === "compacted") {
    const lineColor = `color-mix(in srgb, ${p.accent} 28%, transparent)`;
    return h("div", { width: "100%", display: "flex", flexDirection: "row", alignItems: "center",
      gap: 10, margin: [14, 2], userSelect: "none" }, "", [
      block({ height: 1, background: lineColor, flex: "1 1 auto" }),
      block({ width: "auto", color: item.level === "compacting" ? p.dim
        : `color-mix(in srgb, ${p.accent} 85%, ${p.text})`, fontSize: 11.5 }, item.text),
      block({ height: 1, background: lineColor, flex: "1 1 auto" }),
    ]);
  }
  const important = item.level === "error" || item.level === "warn" || item.level === "info";
  const color = item.level === "error" ? p.red : item.level === "warn" ? p.yellow
    : item.level === "info" ? `color-mix(in srgb, ${p.accent} 80%, ${p.text})` : p.faint;
  const mix = item.level === "error" ? p.red : item.level === "warn" ? p.yellow : p.accent;
  return block({ margin: [10, 0], padding: [8, 13], borderRadius: 8,
    border: important ? 1 : 0, borderColor: `color-mix(in srgb, ${mix} 30%, transparent)`,
    background: important ? `color-mix(in srgb, ${mix} 8%, transparent)` : "transparent",
    color, fontSize: 12.5, textAlign: important ? "left" : "center" }, item.text);
}

interface CanvasFileEdit {
  path: string;
  oldText: string | null;
  newText: string;
  add: number;
  del: number;
}

function collectFileEdits(body: Item[]): CanvasFileEdit[] {
  const files = new Map<string, { first: string | null; last: string }>();
  for (const item of body) {
    if (item.type !== "tool") continue;
    for (const content of item.content) {
      if (content.type !== "diff") continue;
      const diff = content as Extract<ToolItem["content"][number], { type: "diff" }>;
      if (!diff.path) continue;
      const previous = files.get(diff.path);
      if (previous) previous.last = diff.newText ?? "";
      else files.set(diff.path, { first: diff.oldText ?? null, last: diff.newText ?? "" });
    }
  }
  return [...files.entries()].map(([path, value]) => {
    let add = 0;
    let del = 0;
    for (const part of diffLines(value.first ?? "", value.last)) {
      if (part.added) add += part.count ?? 0;
      else if (part.removed) del += part.count ?? 0;
    }
    return { path, oldText: value.first, newText: value.last, add, del };
  }).filter((edit) => edit.add + edit.del > 0);
}

const revertingFileGroups = new Set<string>();

function editedFilesNode(body: Item[], undoneKey: string, p: Palette, rebuild: () => void): Node | null {
  const edits = collectFileEdits(body);
  if (!edits.length) return null;
  const add = edits.reduce((sum, edit) => sum + edit.add, 0);
  const del = edits.reduce((sum, edit) => sum + edit.del, 0);
  const undone = isExpanded(undoneKey);
  const busy = revertingFileGroups.has(undoneKey);
  const stats = block({ width: "auto", display: "flex", flexDirection: "row", gap: 6,
    fontFamily: p.mono, fontSize: 11.5 }, "", [
    block({ width: "auto", color: p.accent }, `+${add}`),
    block({ width: "auto", color: p.red }, `-${del}`),
  ]);
  const header = h("div", { width: "100%", display: "flex", flexDirection: "row",
    alignItems: "center", gap: 10, padding: [9, 12], border: [0, 0, 1, 0],
    borderColor: p.border }, "", [
    block({ width: "auto", color: p.text, fontSize: 13, fontWeight: 600 },
      `已编辑 ${edits.length} 个文件`),
    stats,
    block({ flex: "1 1 auto" }),
    button(undone ? "已撤销" : busy ? "撤销中…" : "撤销", {
      width: "auto", padding: [4, 9], border: 1, borderColor: p.borderLight,
      borderRadius: 7, color: p.dim, fontSize: 12, leadingIcon: "undo", iconColor: p.dim,
      hoverBackground: p.hover, hoverColor: p.text,
    }, () => {
      if (busy || undone || !state.currentId) return;
      revertingFileGroups.add(undoneKey);
      rebuild();
      void api.revertFileChanges(state.currentId, edits.map((edit) => ({
        path: edit.path, oldText: edit.oldText, newText: edit.newText,
      }))).then((result) => {
        if (result.conflicts.length === 0 && result.errors.length === 0) toggleExpanded(undoneKey, true);
      }).catch((error) => void message(String(error), { kind: "error" })).finally(() => {
        revertingFileGroups.delete(undoneKey);
        rebuild();
      });
    }),
  ]);
  const card = block({ margin: [12, 0, 4], background: p.sidebar, border: 1,
    borderColor: p.border, borderRadius: 10, overflow: "hidden" }, "", [header]);
  edits.forEach((edit, index) => {
    card.appendChild(button(relPath(edit.path), { width: "100%", padding: [7, 12],
      border: index === edits.length - 1 ? 0 : [0, 0, 1, 0], borderColor: p.border,
      color: p.blue, fontFamily: p.mono, fontSize: 12, textAlign: "left",
      trailingStats: { add: edit.add, del: edit.del, addColor: p.accent, delColor: p.red },
      hoverBackground: p.hover,
    }, () => openFile(edit.path)));
  });
  return card;
}

function groupNode(group: Group, index: number, running: boolean, p: Palette, rebuild: () => void,
  edit: (item: UserItem, bubble: Node) => void, editingId?: number): Node {
  const result = block();
  result._groupIndex = index;
  if (group.user) result.appendChild(itemNode(group.user, false, p, rebuild, edit, editingId)!);
  if (group.turn?.actualModel) {
    result.appendChild(h("div", { width: "100%", display: "flex", flexDirection: "row",
      justifyContent: "flex-end", margin: [-8, 0, 8] }, "", [
      button(`实际模型：${group.turn.actualModel}`, { width: "auto", padding: [2, 7],
        cursor: "default", userSelect: "text", color: p.faint, background: p.panel,
        border: 1, borderColor: p.borderLight, borderRadius: 10, fontFamily: p.mono,
        fontSize: 11, textAlign: "center" }, () => {}),
    ]));
  }
  const active = running && !group.turn;
  let firstConclusion = -1;
  let lastConclusion = -1;
  if (group.turn) {
    lastConclusion = group.body.findLastIndex((it) => it.type === "assistant" || it.type === "system");
    if (lastConclusion >= 0) {
      firstConclusion = lastConclusion;
      while (firstConclusion > 0 && (group.body[firstConclusion - 1].type === "assistant" ||
        group.body[firstConclusion - 1].type === "system")) firstConclusion--;
    }
  }
  const process = firstConclusion < 0 ? group.body
    : [...group.body.slice(0, firstConclusion), ...group.body.slice(lastConclusion + 1)];
  const conclusion = firstConclusion < 0 ? [] : group.body.slice(firstConclusion, lastConclusion + 1);
  let activeBodyId = -1;
  if (active) {
    for (let i = group.body.length - 1; i >= 0; i--) {
      const item = group.body[i];
      if ((item.type === "tool" && (item.status === "pending" || item.status === "in_progress")) ||
        (item.type === "thought" && item.text === "思考中…")) {
        activeBodyId = item.id;
        break;
      }
    }
  }
  if (group.turn && process.length) {
    const key = `turn-${group.turn.id ?? group.user?.id ?? process[0]?.id ?? 0}`;
    const open = state.expanded[key] ?? process.some((it) => state.expanded[String(it.id)]);
    const label = ["已处理", fmtDuration(group.turn.durationMs), group.turn.totalTokens ? `· ${fmtTokens(group.turn.totalTokens)} tokens` : ""].filter(Boolean).join(" ");
    result.appendChild(button(label, { color: p.dim, fontSize: 13, padding: [4, 8],
      margin: [12, 0, 2, -8], borderRadius: 7, trailingChevron: open,
      chevronColor: p.faint, hoverBackground: p.hover }, () => {
      toggleExpanded(key, !open); rebuild();
    }));
    if (open) {
      const processBody = block({ margin: [4, 0, 6], padding: [2, 0, 2, 12],
        border: [0, 0, 0, 2], borderColor: p.border });
      for (const value of process) {
        const node = itemNode(value, false, p, rebuild, edit, editingId);
        if (node) processBody.appendChild(node);
      }
      result.appendChild(processBody);
    }
  } else {
    for (const value of process) { const node = itemNode(value, active && value.id === activeBodyId, p, rebuild, edit, editingId); if (node) result.appendChild(node); }
  }
  for (const value of conclusion) { const node = itemNode(value, false, p, rebuild, edit, editingId); if (node) result.appendChild(node); }
  if (group.turn && process.length) {
    const foldKey = `turn-${group.turn.id ?? group.user?.id ?? process[0]?.id ?? 0}`;
    const files = editedFilesNode(group.body, `undone-${foldKey}`, p, rebuild);
    if (files) result.appendChild(files);
  }
  return result;
}

const permissionAnswers = new Map<string, string[][]>();
const permissionCustom = new Map<string, string[]>();

function permissionNode(req: PermissionRequest, p: Palette, rebuild: () => void): Node {
  const preview = textPreview(req.toolCall?.rawInput);
  const body: Node[] = [block({ color: p.text, fontSize: 13, fontWeight: "bold", margin: [0, 0, 7] },
    `${req.questions ? "需要确认" : "权限请求"}：${req.toolCall?.title ?? "执行工具"}`)];
  if (req.questions) {
    const selected = permissionAnswers.get(req.requestKey) ?? req.questions.map(() => []);
    const custom = permissionCustom.get(req.requestKey) ?? req.questions.map(() => "");
    permissionAnswers.set(req.requestKey, selected);
    permissionCustom.set(req.requestKey, custom);
    req.questions.forEach((question, index) => {
      body.push(block({ margin: [8, 0, 4], color: p.muted, fontSize: 11.5 }, question.header));
      body.push(block({ margin: [0, 0, 6], color: p.text, fontSize: 13 }, question.question));
      for (const option of question.options) {
        const active = selected[index].includes(option.label);
        body.push(button(`${active ? "●" : "○"}  ${option.label}${option.description ? ` — ${option.description}` : ""}`,
          { width: "100%", padding: [6, 8], margin: [2, 0], border: 1,
            borderColor: active ? p.accent : p.borderLight, borderRadius: 6,
            background: active ? p.accentDim : p.bg, color: active ? p.accent : p.dim,
            textAlign: "left", fontSize: 12 }, () => {
            selected[index] = question.multiple
              ? active ? selected[index].filter((value) => value !== option.label) : [...selected[index], option.label]
              : [option.label];
            if (!question.multiple) custom[index] = "";
            rebuild();
          }));
      }
      if (question.custom) {
        const input = h("input", { width: "100%", height: 34, margin: [4, 0], padding: [6, 9],
          border: 1, borderColor: p.borderLight, borderRadius: 6, background: p.bg,
          color: p.text, value: custom[index], placeholder: "输入其他答案", fontSize: 12 }, "", []);
        input._input = (value) => {
          custom[index] = value;
          if (!question.multiple && value.trim()) selected[index] = [];
        };
        body.push(input);
      }
    });
    const actions = h("div", { width: "100%", display: "flex", flexDirection: "row", gap: 7, margin: [9, 0, 0] }, "", []);
    actions.appendChild(button("提交回答", { padding: [6, 10], border: 1, borderColor: p.green,
      borderRadius: 6, color: p.green, fontSize: 12 }, () => {
      const answers = selected.map((values, index) => custom[index]?.trim() ? [...values, custom[index].trim()] : values);
      if (answers.every((answer) => answer.length)) void respondPermission(req.requestKey, JSON.stringify(answers));
    }));
    actions.appendChild(button("拒绝回答", { padding: [6, 10], border: 1, borderColor: p.red,
      borderRadius: 6, color: p.red, fontSize: 12 }, () => void respondPermission(req.requestKey, "")));
    body.push(actions);
  } else {
    if (preview) body.push(block({ padding: 9, background: p.bg, color: p.dim, fontFamily: p.mono,
      fontSize: 11.5, borderRadius: 5, whiteSpace: "pre" }, preview));
    const actions = h("div", { width: "100%", display: "flex", flexDirection: "row", gap: 7, margin: [9, 0, 0] }, "", []);
    for (const option of req.options) actions.appendChild(button(option.name, { padding: [6, 10], border: 1,
      borderColor: option.kind.startsWith("reject") ? p.red : p.borderLight, borderRadius: 6,
      color: option.kind.startsWith("allow") ? p.green : option.kind.startsWith("reject") ? p.red : p.text,
      fontSize: 12 }, () => { void respondPermission(req.requestKey, option.optionId); rebuild(); }));
    body.push(actions);
  }
  return block({ margin: [14, 0], padding: 13, background: p.panel, border: 1,
    borderColor: p.borderLight, borderRadius: 9 }, "", body);
}

export function CanvasTranscript(props: CanvasTranscriptProps) {
  let canvas: HTMLCanvasElement | undefined;
  let renderer: CanvasDOM | undefined;
  let root: Node | undefined;
  let groupNodes: Node[] = [];
  const groupCache = new WeakMap<Group, { key: string; node: Node }>();
  let resizeObserver: ResizeObserver | undefined;
  let themeObserver: MutationObserver | undefined;
  let rebuildFrame = 0;
  let fontEpoch = 0;
  let fontLoadGeneration = 0;
  let fontLoadHandler: (() => void) | undefined;
  let keepBottom = true;
  const [editing, setEditing] = createSignal<UserItem | null>(null);
  const [editorStyle, setEditorStyle] = createSignal<Record<string, string>>({});
  const [draft, setDraft] = createSignal("");
  const editAttachments = createImageAttachments();

  const emitScroll = (user: boolean) => props.onScroll?.(root?._scrollY ?? 0, root?._maxScrollY ?? 0, user);
  const rememberNestedScroll = (node: Node | undefined) => {
    if (!node) return;
    if (node._scrollKey) nestedScrollPositions.set(node._scrollKey, node._scrollY);
    for (const child of node.children) rememberNestedScroll(child);
  };
  const restoreNestedScroll = (node: Node | undefined) => {
    if (!node) return;
    if (node._scrollKey) node._scrollY = nestedScrollPositions.get(node._scrollKey) ?? 0;
    for (const child of node.children) restoreNestedScroll(child);
  };
  const rebuild = () => {
    if (!canvas || !renderer) return;
    rememberNestedScroll(root);
    const oldTop = root?._scrollY ?? 0;
    const wasBottom = keepBottom || !root || root._maxScrollY - oldTop <= 2;
    const width = Math.max(1, canvas.clientWidth);
    const height = Math.max(1, canvas.clientHeight);
    // 精确复刻 .transcript-inner：max-width: clamp(720px, 78vw, 980px)，
    // padding-inline: clamp(14px, 3vw, 28px)，且全局 box-sizing 为 border-box。
    const horizontal = Math.max(14, Math.min(28, window.innerWidth * .03));
    const outerMax = Math.max(720, Math.min(980, window.innerWidth * .78));
    const outerWidth = Math.min(width, outerMax);
    const contentWidth = Math.max(1, outerWidth - horizontal * 2);
    const p = { ...palette(), columnWidth: contentWidth };
    const side = Math.max(0, (width - outerWidth) / 2) + horizontal;
    const renderKey = `${contentWidth}:${fontEpoch}:${document.documentElement.dataset.theme}:${props.running}:${editing()?.id ?? ""}:${JSON.stringify(state.expanded)}`;
    root = h("div", { width, height, overflow: "auto", display: "flex", flexDirection: "column",
      background: p.bg, padding: [24, side, 16], color: p.text, fontFamily: p.sans,
      fontSize: 14, scrollbarTrack: "transparent",
      scrollbarThumb: p.scroll, scrollbarWidth: 4 }, "", []);
    if (props.preview) root.appendChild(button("回到当前时间线", { width: "auto", padding: [5, 10], margin: [0, 0, 8],
      color: p.accent, border: 1, borderColor: p.accent, borderRadius: 12, fontSize: 10 }, props.onReturnToCurrent));
    groupNodes = [];
    if (!props.groups.length) root.appendChild(block({ padding: [40, 0], color: p.faint, fontSize: 13, textAlign: "center", lineHeight: 1.8 }, props.emptyHint));
    props.groups.forEach((group, index) => {
      // 已闭合轮次不可再流式变化：复用其 Canvas 节点树，避免每个 delta 都重解析全部历史 Markdown。
      const cached = group.turn ? groupCache.get(group) : undefined;
      let node = cached?.key === renderKey ? cached.node : undefined;
      if (!node) {
        node = groupNode(group, index, props.running, p, rebuild, (item, bubble) => {
          setDraft(item.text);
          editAttachments.set(item.images ?? []);
          const visibleTop = bubble._y - (root?._scrollY ?? 0);
          const editorWidth = Math.min(width - 28, contentWidth);
          const left = Math.max(14, Math.min(width - editorWidth - 14, side));
          const top = Math.max(8, Math.min(height - Math.max(150, bubble._height) - 8, visibleTop));
          setEditorStyle({ left: `${left}px`, top: `${top}px`, width: `${editorWidth}px` });
          setEditing(item);
        }, editing()?.id);
        if (group.turn) {
          node._layoutStable = true;
          groupCache.set(group, { key: renderKey, node });
        }
      }
      node._groupIndex = index;
      groupNodes.push(node);
      root!.appendChild(node);
    });
    for (const req of props.permissions) root.appendChild(permissionNode(req, p, rebuild));
    root._scrollY = oldTop;
    root._stickScrollBottom = wasBottom;
    restoreNestedScroll(root);
    renderer.setRoot(root);
    // setRoot 已同步布局、夹取滚动位置；此处上报的几何可直接用于吸底和时光机定位。
    emitScroll(false);
  };

  // 流式 delta、ResizeObserver 和主题变化可能在同一帧连续到达；合并为一次建树/布局/绘制。
  const scheduleRebuild = () => {
    if (rebuildFrame) return;
    rebuildFrame = requestAnimationFrame(() => {
      rebuildFrame = 0;
      rebuild();
    });
  };

  const handle: CanvasTranscriptHandle = {
    scrollToBottom() { if (!root) return; keepBottom = true; root._scrollY = root._maxScrollY; renderer?.invalidate(false); emitScroll(false); },
    scrollToGroup(index) { const node = groupNodes[index]; if (!root || !node) return; keepBottom = false; root._scrollY = Math.max(0, Math.min(root._maxScrollY, node._y - 20)); renderer?.invalidate(false); emitScroll(false); },
    scrollBy(delta) { if (!root) return; keepBottom = false; root._scrollY = Math.max(0, Math.min(root._maxScrollY, root._scrollY + delta)); renderer?.invalidate(false); emitScroll(true); },
    isAtBottom() { return !root || root._maxScrollY - root._scrollY <= 2; },
    scrollTop() { return root?._scrollY ?? 0; }, maxScrollTop() { return root?._maxScrollY ?? 0; },
    activeGroup() { if (!root) return -1; const y = root._scrollY + 32; let active = -1; groupNodes.forEach((node, index) => { if (node._y <= y) active = index; }); return active; },
    hasFocusedInput() { return renderer?.hasFocusedInput() ?? false; },
  };

  onMount(() => {
    if (!canvas) return;
    renderer = new CanvasDOM(canvas, { dpr: Math.min(devicePixelRatio || 1, 2) });
    renderer.on("click", (node) => node._action?.());
    renderer.on("input", (node) => node._input?.(String(node.style.value ?? "")));
    renderer.on("scroll", (node) => {
      if (node !== root || !root) return;
      keepBottom = root._maxScrollY - root._scrollY <= 2;
      emitScroll(true);
    });
    renderer.resize(canvas.clientWidth, canvas.clientHeight);
    props.ref?.(handle);
    resizeObserver = new ResizeObserver(() => {
      renderer?.resize(canvas!.clientWidth, canvas!.clientHeight);
      scheduleRebuild();
    });
    resizeObserver.observe(canvas);
    themeObserver = new MutationObserver(scheduleRebuild);
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    fontLoadHandler = () => {
      // Canvas 首次 measureText 本身才可能触发按字形分片加载字体。旧逻辑在首次绘制前
      // 就读取 document.fonts.ready；当时它会立即 resolved，真正字体随后加载完成却没有
      // 重新布局，导致绘制字体与 fallback 测量结果不一致，表现为内容稍后突然挤在一起。
      fontEpoch++;
      scheduleRebuild();
    };
    document.fonts.addEventListener("loadingdone", fontLoadHandler);
    document.fonts.addEventListener("loadingerror", fontLoadHandler);
    fontLoadGeneration++;
    rebuild();
    const generation = fontLoadGeneration;
    if (document.fonts.status !== "loaded") {
      void document.fonts.ready.then(() => {
        if (generation === fontLoadGeneration) fontLoadHandler?.();
      });
    }
  });

  createEffect(() => {
    // Solid store 会原位更新流式 item；显式读取可变字段，确保每个 delta 都直接重绘 Canvas，
    // 而不是依赖数组或分组对象的引用发生变化。
    for (const group of props.groups) {
      if (group.user) void `${group.user.text}:${group.user.images?.length ?? 0}`;
      for (const item of group.body) {
        if ("text" in item) void item.text;
        if (item.type === "tool") {
          void `${item.status}:${item.title}:${item.content.length}:${item.locations.length}`;
          for (const content of item.content) {
            if (content.type === "diff") void `${content.path}:${content.oldText ?? ""}:${content.newText}`;
            else if (content.type === "content") void (content.content as { text?: string }).text;
          }
          void item.rawOutput;
        }
      }
      if (group.turn) void `${group.turn.durationMs}:${group.turn.totalTokens ?? ""}:${group.turn.actualModel ?? ""}`;
    }
    for (const permission of props.permissions) void `${permission.requestKey}:${permission.options.length}:${permission.questions?.length ?? 0}`;
    props.running;
    props.preview;
    JSON.stringify(state.expanded);
    void editing()?.id;
    scheduleRebuild();
  });

  onCleanup(() => {
    fontLoadGeneration++;
    if (fontLoadHandler) {
      document.fonts.removeEventListener("loadingdone", fontLoadHandler);
      document.fonts.removeEventListener("loadingerror", fontLoadHandler);
    }
    resizeObserver?.disconnect();
    themeObserver?.disconnect();
    if (rebuildFrame) cancelAnimationFrame(rebuildFrame);
    renderer?.destroy();
  });

  const saveEdit = () => {
    const item = editing();
    const text = draft().trim();
    const images = editAttachments.images();
    if (!item || (!text && !images.length)) return;
    setEditing(null);
    void editUserMessage(item.id, text, images);
  };

  return (
    <div class="canvas-transcript-host">
      <canvas ref={canvas} class="transcript-canvas-only" tabindex="0" aria-label="会话记录" />
      {editing() && (
        <div class="canvas-prompt-editor" style={editorStyle()}>
          <ImageAttachmentStrip images={editAttachments.images()} onRemove={editAttachments.remove} />
          <textarea value={draft()} onPaste={editAttachments.onPaste} onInput={(event) => setDraft(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.shiftKey && !event.isComposing) { event.preventDefault(); saveEdit(); }
              if (event.key === "Escape") setEditing(null);
            }} ref={(element) => queueMicrotask(() => { element.focus(); element.setSelectionRange(element.value.length, element.value.length); })} />
          <div><span>发送后将从此处重新开始会话</span><button class="btn secondary small" onClick={() => setEditing(null)}>取消</button><button class="btn primary small" onClick={saveEdit}>发送</button></div>
        </div>
      )}
    </div>
  );
}
