// builders.ts — transcript 各元素的 canvas 节点构建器。
// 逐组件复刻 DOM 版（TranscriptItem/ToolCallCard/EditedFilesCard/PermissionCard/TurnGroup）
// 的结构与 app.css 样式值；所有交互经 ctx 回调回到组件层。

import { convertFileSrc } from "@tauri-apps/api/core";
import { diffLines } from "diff";
import type {
  Item,
  PermissionRequest,
  PromptImage,
  RevertChange,
  SystemItem,
  ThoughtItem,
  ToolContent,
  ToolItem,
  UserItem,
} from "../../types.js";
import { displayToolTitle, stripAnsi } from "../../utils.js";
import {
  chevronNode,
  iconNode,
  IconCheck,
  IconCopy,
  IconFile,
  IconPencil,
  IconUndo,
  spinnerNode,
  toolIconShapes,
} from "../icons.js";
import { Node, h, type CanvasHitEvent, type Style } from "../node.js";
import { mix, theme } from "../theme.js";
import { innerScrollPositions } from "../canvasdom.js";
import { buildMarkdown, hasFileRefCandidate, type MdCallbacks } from "./markdown.js";
import {
  activeBodyIdOf,
  bodyExpandedOf,
  foldKeyOf,
  foldLabelOf,
  showLiveTailOf,
  splitGroupBody,
  tokenTitleOf,
  type Group,
} from "./groups.js";

export interface BuilderCtx {
  md: MdCallbacks;
  copiedCodeKey: string | null;
  /** 正在编辑的用户消息 id（该消息渲染为占位节点，DOM 覆盖层浮于其上） */
  editingUserId: number | null;
  /** 编辑覆盖层的当前高度（占位节点同步） */
  editingHeight: number;
  running: boolean;
  isExpanded: (key: string, def?: boolean) => boolean;
  toggleExpanded: (key: string, value: boolean) => void;
  onUserEdit: (item: UserItem) => void;
  onUndoEdits: (changes: RevertChange[], undoneKey: string) => void;
  onRespondPermission: (requestKey: string, optionId: string) => void;
  onOpenInEditor: (path: string, line?: number) => void;
  /** 撤销按钮 busy 态 */
  undoBusy: (undoneKey: string) => boolean;
  /** 权限卡 question 的选择状态（组件侧按 requestKey 保存） */
  permAnswers: (requestKey: string) => { answers: string[][]; custom: string[] };
  onPermSelect: (requestKey: string, index: number, label: string, multiple: boolean) => void;
}

// ---------- 通用小件 ----------

function basename(p: string): string {
  return p.split(/[\\/]/).pop() || p;
}

/** 文件路径相对线程工作目录显示（与 EditedFilesCard.relPath 一致） */
export function relPath(p: string, cwd: string): string {
  if (!cwd) return p;
  const norm = (s: string) => s.replace(/\//g, "\\").toLowerCase();
  const base = cwd.endsWith("\\") || cwd.endsWith("/") ? cwd : cwd + "\\";
  if (norm(p).startsWith(norm(base))) return p.slice(base.length);
  return p;
}

function statSpans(add: number, del: number): Node[] {
  const t = theme();
  const out: Node[] = [];
  const wrap = h("span", {
    display: "inline-block",
    fontFamily: t.mono,
    fontSize: 11.5,
    userSelect: "none",
  });
  const addNode = h("span", { display: "inline", fontFamily: t.mono, fontSize: 11.5, color: t.accent }, `+${add}`);
  const delNode = h("span", { display: "inline", fontFamily: t.mono, fontSize: 11.5, color: t.red, margin: [0, 0, 0, 6] }, `-${del}`);
  wrap.appendChild(addNode);
  wrap.appendChild(delNode);
  out.push(wrap);
  return out;
}

/** hover 显现的复制按钮（.codeblock .code-copy） */
function copyButton(text: string, key: string, ctx: BuilderCtx): Node {
  const t = theme();
  const copied = ctx.copiedCodeKey === key;
  const btn = h("div", {
    position: "absolute",
    top: 7,
    right: 7,
    width: 25,
    height: 25,
    display: "flex",
    justifyContent: "center",
    alignItems: "center",
    background: t.bgPanel,
    border: 1,
    borderColor: t.borderLight,
    borderRadius: 6,
    color: copied ? t.accent : t.textFaint,
    cursor: "pointer",
    userSelect: "none",
  });
  btn.revealOnHover = !copied;
  btn.hoverStyle = { color: copied ? t.accent : t.text, background: t.bgHover };
  btn.onClick = () => ctx.md.onCopyCode(key, text);
  btn.appendChild(iconNode(copied ? IconCheck : IconCopy, 13, { color: copied ? t.accent : t.textFaint }));
  btn.children[0].hoverStyle = { color: copied ? t.accent : t.text };
  return btn;
}

// ---------- 用户消息 ----------

function attachmentPath(img: PromptImage): string | undefined {
  if (img.data || !img.uri) return undefined;
  return decodeURI(img.uri.replace(/^file:\/+/, ""));
}

function attachmentSrc(img: PromptImage): string {
  if (img.data) return `data:${img.mimeType};base64,${img.data}`;
  return convertFileSrc(attachmentPath(img) ?? "");
}

export function buildUserMessage(item: UserItem, ctx: BuilderCtx): Node {
  const t = theme();
  const row = h("div", {
    display: "flex",
    justifyContent: "flex-end",
    margin: [20, 0, 16],
    width: "100%",
  });
  row.data = { itemId: item.id, kind: "user" };
  row.hoverContainer = true;

  if (ctx.editingUserId === item.id) {
    // 编辑中：气泡换成等高占位，DOM 覆盖层浮于其上
    row.appendChild(h("div", { width: "100%", height: ctx.editingHeight }));
    return row;
  }

  if (!ctx.running) {
    const btn = h("div", {
      width: 23,
      height: 23,
      margin: [0, 2, 4, 0],
      borderRadius: 6,
      color: t.textFaint,
      cursor: "pointer",
      display: "flex",
      justifyContent: "center",
      alignItems: "center",
      alignSelf: "flex-end",
      userSelect: "none",
    });
    btn.revealOnHover = true;
    btn.hoverStyle = { background: t.bgHover, color: t.text };
    btn.style.title = "编辑此消息，并从这里重新开始";
    btn.onClick = () => ctx.onUserEdit(item);
    btn.data = { clickId: "userEditBtn", itemId: item.id };
    btn.appendChild(iconNode(IconPencil, 13, { color: t.textFaint }));
    btn.children[0].hoverStyle = { color: t.text };
    row.appendChild(btn);
  }

  const bubble = h("div", {
    background: t.accentDim,
    borderRadius: [t.rLg, t.rLg, t.rSm, t.rLg],
    padding: [10, 16],
    maxWidth: "85%",
    lineHeight: 1.6,
    color: t.text,
    fontSize: 14,
    flex: "0 1 auto",
  });
  bubble.data = { clickId: "userBubble", itemId: item.id };

  if (item.images?.length) {
    const strip = h("div", {
      display: "flex",
      flexWrap: "wrap",
      gap: 6,
      margin: [0, 0, 6, 0],
    });
    for (const img of item.images) {
      if (img.mimeType.startsWith("image/")) {
        const node = new Node("img", {
          src: attachmentSrc(img),
          maxWidth: 240,
          maxHeight: 180,
          borderRadius: 10,
          title: img.name,
        });
        const path = attachmentPath(img);
        if (path) {
          node.style.cursor = "context-menu";
          node.onContextMenu = (n, e) => ctx.md.onFileMenu(e, path);
        }
        strip.appendChild(node);
      } else {
        const chip = h("span", {
          display: "inline-block",
          padding: [5, 10],
          borderRadius: 8,
          background: mix(t.text, 0.08),
          fontSize: 12.5,
          maxWidth: 240,
          color: t.text,
          title: img.name,
        });
        chip.appendChild(iconNode(IconFile, 15, { color: t.text, display: "inline-block" }));
        const name = h("span", {
          display: "inline-block",
          margin: [0, 0, 0, 6],
          fontSize: 12.5,
          color: t.text,
          whiteSpace: "nowrap",
          textOverflow: "ellipsis",
          maxWidth: 190,
        });
        name.setText(img.name);
        chip.appendChild(name);
        const path = attachmentPath(img);
        if (path) chip.onContextMenu = (n, e) => ctx.md.onFileMenu(e, path);
        strip.appendChild(chip);
      }
    }
    bubble.appendChild(strip);
  }

  if (item.text) {
    const text = h("div", {
      whiteSpace: "pre-wrap",
      lineHeight: 1.6,
      color: t.text,
      fontSize: 14,
    });
    text.setText(item.text);
    bubble.appendChild(text);
  }
  row.appendChild(bubble);
  return row;
}

// ---------- assistant ----------

export function buildAssistant(item: Item & { type: "assistant" }, ctx: BuilderCtx, live: boolean): Node | null {
  if (item.text.trim() === "None") return null;
  const wrap = h("div", { margin: [14, 0], lineHeight: 1.7, width: "100%" });
  wrap.data = { itemId: item.id, kind: "assistant" };
  const md = buildMarkdown(item.text, {
    fontSize: 14,
    lineHeight: 1.7,
    color: theme().text,
    // 流式尾部的结构随时会变，live 时跳过文件引用标记（与 DOM 版一致）
    markFiles: !live && hasFileRefCandidate(item.text),
    copiedCodeKey: ctx.copiedCodeKey,
    cb: ctx.md,
  });
  wrap.appendChild(md);
  return wrap;
}

// ---------- thought ----------

function normalizeThoughtMarkdown(text: string, agentKind: string): string {
  return agentKind === "opencode" ? text.replace(/(\S)\*{4}(?=\S)/g, "$1**\n\n**") : text;
}

export function buildThought(item: ThoughtItem, active: boolean, ctx: BuilderCtx, agentKind: string): Node {
  const t = theme();
  const wrap = h("div", { margin: [6, 0], fontSize: 13, width: "100%" });
  wrap.data = { itemId: item.id, kind: "thought" };

  if (item.text === "思考中…") {
    const node = h("div", { fontSize: 13, color: t.text }, item.text);
    wrap.appendChild(node);
    return wrap;
  }

  const key = `thought-${item.id}`;
  const open = ctx.isExpanded(key, active);
  const toggle = h("div", {
    display: "flex",
    flexDirection: "row",
    gap: 5,
    color: t.textFaint,
    fontSize: 12,
    padding: [3, 6],
    borderRadius: 6,
    cursor: "pointer",
    width: "fit-content",
    alignItems: "center",
    userSelect: "none",
  });
  toggle.hoverStyle = { color: t.textDim, background: t.bgHover };
  toggle.onClick = () => ctx.toggleExpanded(key, !open);
  toggle.data = { clickId: "thoughtToggle", itemId: item.id };
  toggle.appendChild(chevronNode(open, 12, { color: open ? t.textDim : t.textFaint }));
  const label = h("span", { display: "inline", fontSize: 12, color: t.textFaint }, "思考过程");
  label.hoverStyle = { color: t.textDim };
  toggle.appendChild(label);
  wrap.appendChild(toggle);

  if (open) {
    const body = h("div", {
      margin: [6, 0, 0, 0],
      padding: [8, 14],
      border: [0, 0, 0, 2],
      borderColor: ["", "", "", t.borderLight],
      color: t.textDim,
      fontSize: 13,
    });
    body.appendChild(
      buildMarkdown(normalizeThoughtMarkdown(item.text, agentKind), {
        fontSize: 13,
        lineHeight: 1.45,
        color: t.textDim,
        markFiles: false,
        copiedCodeKey: ctx.copiedCodeKey,
        cb: ctx.md,
      }),
    );
    wrap.appendChild(body);
  }
  return wrap;
}

// ---------- system ----------

function isCodexModelResumeWarning(item: Item): boolean {
  if (item.type !== "system" || item.level !== "error") return false;
  return (
    item.text.startsWith("This session was recorded with model `") &&
    item.text.includes("` but is resuming with `") &&
    item.text.includes("`. Consider switching back to `") &&
    item.text.endsWith("` as it may affect Codex performance.")
  );
}

export function buildSystem(item: SystemItem, ctx: BuilderCtx): Node | null {
  if (isCodexModelResumeWarning(item)) return null;
  const t = theme();
  const isCompaction = item.level === "compacting" || item.level === "compacted";

  if (isCompaction) {
    const row = h("div", {
      display: "flex",
      alignItems: "center",
      gap: 10,
      margin: [14, 2],
      userSelect: "none",
      width: "100%",
    });
    row.data = { itemId: item.id, kind: "system" };
    const line1 = h("div", { flex: 1, height: 1, background: mix(t.accent, 0.22) });
    const labelColor = item.level === "compacting" ? t.textDim : mix(t.accent, 0.85, t.text);
    const label = h("div", {
      display: "flex",
      alignItems: "center",
      gap: 6,
      fontSize: 11.5,
      color: labelColor,
      whiteSpace: "nowrap",
      width: "fit-content",
    });
    if (item.level === "compacting") label.appendChild(spinnerNode(true));
    const txt = h("span", { display: "inline", fontSize: 11.5, color: labelColor }, item.text);
    label.appendChild(txt);
    const line2 = h("div", { flex: 1, height: 1, background: mix(t.accent, 0.22) });
    row.replaceChildren([line1, label, line2]);
    return row;
  }

  let color = t.textFaint;
  let bg = "transparent";
  let border = "transparent";
  let align: Style["textAlign"] = "center";
  if (item.level === "error") {
    color = t.red;
    bg = mix(t.red, 0.08);
    border = mix(t.red, 0.32);
    align = "left";
  } else if (item.level === "warn") {
    color = t.yellow;
    bg = mix(t.yellow, 0.07);
    border = mix(t.yellow, 0.28);
    align = "left";
  } else if (item.level === "info") {
    color = mix(t.accent, 0.8, t.text);
    bg = mix(t.accent, 0.07);
    border = mix(t.accent, 0.26);
    // .msg-system 居中；level-info 只改三色，不改对齐
  }
  const node = h("div", {
    fontSize: 12.5,
    margin: [10, 0],
    padding: [8, 13],
    borderRadius: 8,
    color,
    background: bg,
    border: 1,
    borderColor: border,
    textAlign: align,
    width: "100%",
    lineHeight: 1.4,
  });
  node.data = { itemId: item.id, kind: "system" };
  node.setText(item.text);
  return node;
}

// ---------- 工具卡 ----------

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

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function stringifyJson(value: unknown): string {
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
  const preferred = ["command", "cmd", "query", "q", "path", "file_path", "url", "symbol", "task", "prompt"];
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

/** CopyablePre：codeblock 容器 + 可滚 pre + 悬停复制钮；滚动位置持久化 */
function copyablePre(
  classKind: "tool-output" | "tool-raw",
  text: string,
  scrollKey: string,
  ctx: BuilderCtx,
): Node {
  const t = theme();
  const isRaw = classKind === "tool-raw";
  const wrap = h("div", { position: "relative", width: "100%" });
  wrap.hoverContainer = true;
  const pre = h("pre", {
    fontFamily: t.mono,
    fontSize: isRaw ? 11.5 : 12,
    lineHeight: 1.55,
    background: t.bgSidebar,
    border: 1,
    borderColor: t.border,
    borderRadius: 7,
    padding: isRaw ? 8 : 10,
    maxHeight: isRaw ? 200 : 320,
    overflow: "auto",
    overscrollBehavior: "contain",
    scrollbarGutter: "stable",
    whiteSpace: "pre-wrap",
    wordBreak: "break-all",
    color: isRaw ? t.textFaint : t.textDim,
    width: "100%",
  });
  pre.setText(text);
  pre.data = { scrollKey };
  pre._scrollY = Math.min(
    innerScrollPositions.get(scrollKey) ?? 0,
    Number.MAX_SAFE_INTEGER,
  );
  wrap.appendChild(pre);
  wrap.appendChild(copyButton(text, `${scrollKey}-copy`, ctx));
  return wrap;
}

interface DiffRow {
  sign: string;
  text: string;
  cls: "diff-add" | "diff-del" | "diff-ctx" | "diff-skip";
}

function diffRows(oldText: string, newText: string): DiffRow[] {
  const parts = diffLines(oldText, newText);
  const out: DiffRow[] = [];
  for (const part of parts) {
    const cls = part.added ? "diff-add" : part.removed ? "diff-del" : "diff-ctx";
    const sign = part.added ? "+" : part.removed ? "-" : " ";
    const lines = part.value.replace(/\n$/, "").split("\n");
    if (!part.added && !part.removed && lines.length > 8) {
      for (const l of lines.slice(0, 3)) out.push({ sign, text: l, cls });
      out.push({ sign: " ", text: `… 省略 ${lines.length - 6} 行 …`, cls: "diff-skip" });
      for (const l of lines.slice(-3)) out.push({ sign, text: l, cls });
    } else {
      for (const l of lines) out.push({ sign, text: l, cls });
    }
  }
  return out;
}

function diffView(
  path: string,
  oldText: string | null | undefined,
  newText: string,
  ctx: BuilderCtx,
  cwd: string,
): Node {
  const t = theme();
  const wrap = h("div", {
    border: 1,
    borderColor: t.border,
    borderRadius: 7,
    overflow: "hidden",
    width: "100%",
  });
  const head = h("div", {
    fontFamily: t.mono,
    fontSize: 11.5,
    padding: [6, 10],
    background: t.bgPanel,
    color: t.blue,
    border: [0, 0, 1, 0],
    borderColor: ["", "", t.border, ""],
    textAlign: "left",
    cursor: "pointer",
    width: "100%",
    lineHeight: 1.45,
  });
  head.setText(relPath(path, cwd));
  head.style.title = `在编辑器中打开 ${path}`;
  head.hoverStyle = { textDecoration: "underline", background: t.bgHover };
  head.onClick = () => ctx.onOpenInEditor(path);
  head.onContextMenu = (n, e) => ctx.md.onFileMenu(e, path);
  wrap.appendChild(head);

  const body = h("pre", {
    fontFamily: t.mono,
    fontSize: 12,
    lineHeight: 1.5,
    maxHeight: 360,
    overflow: "auto",
    background: t.bgSidebar,
    width: "100%",
  });
  for (const row of diffRows(oldText ?? "", newText ?? "")) {
    let color = t.textDim;
    let bg = "transparent";
    if (row.cls === "diff-add") {
      bg = mix(t.accent, 0.13);
      color = mix(t.accent, 0.6, t.text);
    } else if (row.cls === "diff-del") {
      bg = mix(t.red, 0.13);
      color = mix(t.red, 0.58, t.text);
    } else if (row.cls === "diff-skip") {
      color = t.textFaint;
    }
    const line = h("div", {
      display: "flex",
      padding: [0, 10],
      background: bg,
      width: "100%",
    });
    const sign = h("span", {
      display: "inline-block",
      width: 16,
      color: t.textFaint,
      fontFamily: t.mono,
      fontSize: 12,
      lineHeight: 1.5,
      userSelect: "none",
    });
    sign.setText(row.sign);
    const text = h("span", {
      display: "inline-block",
      flex: 1,
      fontFamily: t.mono,
      fontSize: 12,
      lineHeight: 1.5,
      color,
      whiteSpace: "pre-wrap",
      wordBreak: "break-all",
      fontStyle: row.cls === "diff-skip" ? "italic" : "normal",
    });
    text.setText(row.text || " ");
    line.replaceChildren([sign, text]);
    body.appendChild(line);
  }
  wrap.appendChild(body);
  return wrap;
}

export function buildTool(item: ToolItem, active: boolean, ctx: BuilderCtx, cwd: string, threadId: string): Node {
  const t = theme();
  const row = h("div", { margin: [1, 0], width: "100%" });
  row.data = { itemId: item.id, kind: "tool" };

  const key = `tool-${item.id}`;
  const rawKey = `tool-raw-${item.id}`;
  const scrollKey = (part: string) => `${threadId}-${item.id}-${part}`;
  const defaultOpen = active || item.status === "pending" || item.status === "in_progress";
  const open = ctx.isExpanded(key, defaultOpen);
  const showRaw = ctx.isExpanded(rawKey);
  const hasBody =
    item.content.length > 0 ||
    item.locations.length > 0 ||
    item.rawInput !== undefined ||
    item.rawOutput !== undefined;

  // diff 统计
  let add = 0;
  let del = 0;
  for (const b of item.content) {
    if (b.type !== "diff") continue;
    const d = b as Extract<ToolContent, { type: "diff" }>;
    for (const part of diffLines(d.oldText ?? "", d.newText ?? "")) {
      const n = part.count ?? 0;
      if (part.added) add += n;
      else if (part.removed) del += n;
    }
  }

  const label = () => {
    const title = (item.title || "").trim();
    if (title) return displayToolTitle(stripAnsi(title));
    return KIND_LABEL[item.kind] ?? item.kind;
  };

  const failed = item.status === "failed";
  const line = h("div", {
    display: "flex",
    alignItems: "center",
    gap: 8,
    width: "100%",
    padding: [3, 8],
    borderRadius: 7,
    minHeight: 26,
    cursor: hasBody ? "pointer" : "default",
    userSelect: "none",
  });
  line.hoverStyle = hasBody ? { background: t.bgHover } : undefined;
  line.onClick = () => {
    if (hasBody) ctx.toggleExpanded(key, !open);
  };
  line.data = { ...(line.data ?? {}), clickId: "toolLine", itemId: item.id };
  line.appendChild(iconNode(toolIconShapes(item.kind), 14, { color: t.textFaint }));

  const titleText = label();
  const title = h("span", {
    display: "inline-block",
    fontFamily: t.mono,
    fontSize: 12,
    color: failed ? t.red : t.textDim,
    whiteSpace: "nowrap",
    textOverflow: "ellipsis",
    flex: "0 1 auto",
    title: titleText,
    userSelect: "none",
  });
  title.setText(titleText);
  line.appendChild(title);

  if (add + del > 0) {
    for (const s of statSpans(add, del)) line.appendChild(s);
  }
  if (item.status === "in_progress" || item.status === "pending") {
    line.appendChild(spinnerNode(false));
  } else if (failed) {
    const dot = h("span", {
      display: "inline-block",
      width: 7,
      height: 7,
      borderRadius: 4,
      background: t.red,
      title: "失败",
    });
    line.appendChild(dot);
  }
  if (hasBody) {
    line.appendChild(chevronNode(open, 12, { color: t.textFaint }));
  }
  row.appendChild(line);

  if (open) {
    const body = h("div", {
      margin: [4, 0, 8, 14],
      padding: [4, 0, 4, 14],
      border: [0, 0, 0, 2],
      borderColor: ["", "", "", t.border],
      display: "flex",
      flexDirection: "column",
      gap: 8,
    });
    if (item.locations.length > 0) {
      const locs = h("div", { display: "flex", flexWrap: "wrap", gap: 6 });
      for (const loc of item.locations) {
        const chip = h("span", {
          display: "inline-block",
          fontFamily: t.mono,
          fontSize: 11.5,
          background: t.bgPanel,
          color: t.blue,
          padding: [2, 9],
          borderRadius: 20,
          cursor: "pointer",
          userSelect: "none",
        });
        chip.setText(`${basename(loc.path ?? "")}${loc.line != null ? `:${loc.line}` : ""}`);
        chip.style.title = `在编辑器中打开 ${loc.path ?? ""}`;
        chip.hoverStyle = { background: t.bgHover, textDecoration: "underline" };
        chip.onClick = () => ctx.onOpenInEditor(loc.path ?? "", loc.line ?? undefined);
        if (loc.path) chip.onContextMenu = (n, e) => ctx.md.onFileMenu(e, loc.path!);
        locs.appendChild(chip);
      }
      body.appendChild(locs);
    }

    let visibleContent = false;
    item.content.forEach((block, index) => {
      if (block.type === "diff") {
        visibleContent = true;
        const b = block as Extract<ToolContent, { type: "diff" }>;
        body.appendChild(diffView(b.path, b.oldText, b.newText, ctx, cwd));
      } else if (block.type === "content") {
        const inner = (block as { content?: { type?: string; text?: string } }).content;
        const text = inner?.type === "text" && isUsefulText(inner.text) ? stripAnsi(inner.text!) : "";
        if (text) {
          visibleContent = true;
          body.appendChild(copyablePre("tool-output", text, scrollKey(`output-${index}`), ctx));
        }
      } else {
        visibleContent = true;
      }
    });

    if (!visibleContent) {
      const summary = toolSummary(item);
      if (summary) {
        const sum = h("div", {
          width: "fit-content",
          maxWidth: "100%",
          fontFamily: t.mono,
          fontSize: 12,
          lineHeight: 1.55,
          color: t.textDim,
          background: t.bgPanel,
          border: 1,
          borderColor: t.border,
          borderRadius: 7,
          padding: [7, 10],
          whiteSpace: "pre-wrap",
          wordBreak: "normal",
        });
        sum.setText(summary);
        body.appendChild(sum);
      }
    }

    if (item.rawInput !== undefined || item.rawOutput !== undefined) {
      const toggle = h("div", {
        alignSelf: "flex-start",
        fontSize: 12,
        color: t.blue,
        padding: [2, 0],
        cursor: "pointer",
        width: "fit-content",
        userSelect: "none",
      });
      toggle.setText(showRaw ? "隐藏原始数据" : "原始数据");
      toggle.hoverStyle = { textDecoration: "underline" };
      toggle.onClick = () => ctx.toggleExpanded(rawKey, !showRaw);
      body.appendChild(toggle);
      if (showRaw) {
        if (item.rawInput !== undefined) {
          body.appendChild(
            copyablePre("tool-raw", `输入: ${stringifyJson(item.rawInput)}`, scrollKey("raw-input"), ctx),
          );
        }
        if (item.rawOutput !== undefined) {
          body.appendChild(
            copyablePre("tool-raw", `输出: ${stringifyJson(item.rawOutput)}`, scrollKey("raw-output"), ctx),
          );
        }
      }
    }
    row.appendChild(body);
  }
  return row;
}

// ---------- 已编辑文件卡 ----------

interface FileEdit {
  path: string;
  oldText: string | null;
  newText: string;
  add: number;
  del: number;
}

function collectEdits(body: Item[]): FileEdit[] {
  const map = new Map<string, { first: string | null; last: string }>();
  for (const item of body) {
    if (item.type !== "tool") continue;
    for (const b of item.content) {
      if (b.type !== "diff") continue;
      const d = b as Extract<ToolContent, { type: "diff" }>;
      if (!d.path) continue;
      const prev = map.get(d.path);
      if (prev) prev.last = d.newText ?? "";
      else map.set(d.path, { first: d.oldText ?? null, last: d.newText ?? "" });
    }
  }
  return [...map.entries()]
    .map(([path, { first, last }]) => {
      let add = 0;
      let del = 0;
      for (const part of diffLines(first ?? "", last)) {
        if (part.added) add += part.count ?? 0;
        else if (part.removed) del += part.count ?? 0;
      }
      return { path, oldText: first, newText: last, add, del };
    })
    .filter((e) => e.add + e.del > 0);
}

export function buildEditedFilesCard(body: Item[], undoneKey: string, ctx: BuilderCtx, cwd: string): Node | null {
  const edits = collectEdits(body);
  if (edits.length === 0) return null;
  const t = theme();
  const undone = ctx.isExpanded(undoneKey);
  const busy = ctx.undoBusy(undoneKey);
  const totals = edits.reduce((a, e) => ({ add: a.add + e.add, del: a.del + e.del }), { add: 0, del: 0 });

  const card = h("div", {
    border: 1,
    borderColor: t.border,
    borderRadius: t.rMd,
    margin: [12, 0, 4],
    overflow: "hidden",
    background: t.bgSidebar,
    width: "100%",
  });
  card.data = { kind: "files" };

  const head = h("div", {
    display: "flex",
    alignItems: "center",
    gap: 10,
    padding: [9, 12],
    border: [0, 0, 1, 0],
    borderColor: ["", "", t.border, ""],
  });
  const title = h("span", { display: "inline", fontSize: 13, fontWeight: 600, color: t.text }, `已编辑 ${edits.length} 个文件`);
  head.appendChild(title);
  for (const s of statSpans(totals.add, totals.del)) head.appendChild(s);
  head.appendChild(h("span", { display: "inline-block", flex: 1 }));
  const undo = h("div", {
    display: "flex",
    alignItems: "center",
    gap: 5,
    padding: [4, 12],
    borderRadius: 7,
    border: 1,
    borderColor: t.borderLight,
    color: t.textDim,
    fontSize: 12,
    cursor: undone || busy ? "default" : "pointer",
    opacity: undone || busy ? 0.5 : 1,
    userSelect: "none",
  });
  if (!undone && !busy) undo.hoverStyle = { background: t.bgHover, color: t.text };
  undo.style.title = undone ? "本轮改动已撤销" : "把这些文件恢复到本轮编辑前的内容（被后续修改过的文件会跳过）";
  undo.onClick = () => {
    if (undone || busy) return;
    ctx.onUndoEdits(
      edits.map((e) => ({ path: e.path, oldText: e.oldText, newText: e.newText })),
      undoneKey,
    );
  };
  undo.appendChild(iconNode(IconUndo, 12, { color: t.textDim }));
  undo.children[0].hoverStyle = { color: t.text };
  const undoLabel = h(
    "span",
    { display: "inline", fontSize: 12, color: t.textDim },
    undone ? "已撤销" : busy ? "撤销中…" : "撤销",
  );
  undoLabel.hoverStyle = { color: t.text };
  undo.appendChild(undoLabel);
  head.appendChild(undo);
  card.appendChild(head);

  edits.forEach((e, i) => {
    const rowNode = h("div", {
      display: "flex",
      alignItems: "center",
      gap: 10,
      width: "100%",
      padding: [7, 12],
      border: i < edits.length - 1 ? [0, 0, 1, 0] : 0,
      borderColor: ["", "", t.border, ""],
      cursor: "pointer",
    });
    rowNode.hoverStyle = { background: t.bgHover };
    rowNode.style.title = `在编辑器中打开 ${e.path}`;
    rowNode.onClick = () => ctx.onOpenInEditor(e.path);
    rowNode.onContextMenu = (n, ev) => ctx.md.onFileMenu(ev, e.path);
    const path = h("span", {
      display: "inline-block",
      flex: 1,
      fontFamily: t.mono,
      fontSize: 12,
      color: t.blue,
      whiteSpace: "nowrap",
      textOverflow: "ellipsis",
    });
    path.setText(relPath(e.path, cwd));
    path.hoverStyle = { textDecoration: "underline" };
    rowNode.appendChild(path);
    for (const s of statSpans(e.add, e.del)) rowNode.appendChild(s);
    card.appendChild(rowNode);
  });
  return card;
}

// ---------- 权限卡 ----------

function inputPreview(raw: unknown): string {
  if (raw == null) return "";
  if (typeof raw === "object") {
    const o = raw as Record<string, unknown>;
    const cmd = o.command ?? o.cmd ?? o.path ?? o.file_path ?? o.url;
    if (typeof cmd === "string") return stripAnsi(cmd);
    try {
      const s = JSON.stringify(raw);
      return stripAnsi(s.length > 200 ? s.slice(0, 200) + "…" : s);
    } catch {
      return "";
    }
  }
  return stripAnsi(String(raw));
}

function permBtn(
  label: string,
  kind: "allow" | "reject" | "plain",
  disabled: boolean,
  onClick: () => void,
): Node {
  const t = theme();
  let bg = t.bgPanel;
  let border = t.borderLight;
  let color = t.textDim;
  let hoverBg = t.bgHover;
  if (kind === "allow") {
    bg = t.accentDim;
    border = mix(t.accent, 0.34);
    color = t.accent;
    hoverBg = mix(t.accent, 0.2);
  } else if (kind === "reject") {
    bg = mix(t.red, 0.08);
    border = mix(t.red, 0.3);
    color = t.red;
    hoverBg = mix(t.red, 0.16);
  }
  const btn = h("div", {
    padding: [6, 14],
    borderRadius: 7,
    fontSize: 12.5,
    fontWeight: 500,
    border: 1,
    borderColor: border,
    background: bg,
    color,
    cursor: disabled ? "default" : "pointer",
    opacity: disabled ? 0.45 : 1,
    width: "fit-content",
    userSelect: "none",
  });
  if (!disabled) {
    btn.hoverStyle = { background: hoverBg };
    btn.onClick = onClick;
  }
  btn.setText(label);
  btn.data = { clickId: `permBtn:${label}` };
  return btn;
}

export function buildPermissionCard(req: PermissionRequest, ctx: BuilderCtx, agentLabelText: string): Node {
  const t = theme();
  const card = h("div", {
    border: 1,
    borderColor: mix(t.yellow, 0.36),
    background: mix(t.yellow, 0.06),
    borderRadius: t.rMd,
    padding: [12, 14],
    display: "flex",
    flexDirection: "column",
    gap: 9,
    margin: [10, 0],
    width: "100%",
  });
  card.data = { kind: "perm", requestKey: req.requestKey };

  const head = h("div", { display: "flex", alignItems: "center", gap: 9 });
  head.appendChild(iconNode(toolIconShapes(req.toolCall?.kind ?? "other"), 14, { color: t.yellow }));
  const title = h("span", {
    display: "inline",
    fontWeight: 600,
    fontSize: 13,
    color: t.text,
  }, `${agentLabelText} ${req.questions ? "需要确认" : "请求权限"}：${req.toolCall?.title ?? "执行工具"}`);
  head.appendChild(title);
  card.appendChild(head);

  if (!req.questions) {
    const preview = inputPreview(req.toolCall?.rawInput);
    if (preview) {
      const pre = h("pre", {
        fontFamily: t.mono,
        fontSize: 12,
        background: t.bgSidebar,
        border: 1,
        borderColor: t.border,
        borderRadius: 7,
        padding: [8, 10],
        whiteSpace: "pre-wrap",
        wordBreak: "break-all",
        maxHeight: 140,
        overflow: "auto",
        overscrollBehavior: "contain",
        color: t.textDim,
        width: "100%",
        lineHeight: 1.5,
      });
      pre.setText(preview);
      card.appendChild(pre);
    }
    const actions = h("div", { display: "flex", gap: 8, flexWrap: "wrap" });
    for (const opt of req.options) {
      const kind = opt.kind.startsWith("allow") ? "allow" : opt.kind.startsWith("reject") ? "reject" : "plain";
      actions.appendChild(
        permBtn(opt.name, kind, false, () => ctx.onRespondPermission(req.requestKey, opt.optionId)),
      );
    }
    card.appendChild(actions);
    return card;
  }

  const stateFor = ctx.permAnswers(req.requestKey);
  const questionAnswers = () =>
    stateFor.answers.map((selected, index) => {
      const custom = stateFor.custom[index]?.trim();
      return custom ? [...selected, custom] : selected;
    });
  const canAnswer = questionAnswers().every((answer) => answer.length > 0);

  req.questions.forEach((question, index) => {
    const block = h("div", { display: "flex", flexDirection: "column", gap: 7 });
    const header = h("div", {
      color: t.textDim,
      fontSize: 11,
      fontWeight: 600,
      letterSpacing: 0.6,
    }, question.header.toUpperCase());
    block.appendChild(header);
    const qtext = h("div", { fontSize: 13, color: t.text }, question.question);
    block.appendChild(qtext);
    const options = h("div", { display: "flex", flexDirection: "column", gap: 6 });
    for (const option of question.options) {
      const selected = stateFor.answers[index]?.includes(option.label);
      const opt = h("div", {
        display: "flex",
        flexDirection: "column",
        gap: 2,
        padding: [8, 10],
        border: 1,
        borderColor: selected ? mix(t.accent, 0.5) : t.borderLight,
        borderRadius: 7,
        background: selected ? t.accentDim : t.bgPanel,
        color: t.text,
        cursor: "pointer",
        userSelect: "none",
      });
      opt.onClick = () => ctx.onPermSelect(req.requestKey, index, option.label, Boolean(question.multiple));
      const label = h("span", { display: "inline", fontSize: 13, color: t.text }, option.label);
      opt.appendChild(label);
      if (option.description) {
        opt.appendChild(h("span", { display: "inline", fontSize: 12, color: t.textDim }, option.description));
      }
      options.appendChild(opt);
    }
    block.appendChild(options);
    if (question.custom) {
      // DOM 覆盖层锚点：真实 <input> 浮于其上（组件定位）
      const anchor = h("div", {
        width: "100%",
        height: 34,
        border: 1,
        borderColor: t.borderLight,
        borderRadius: 7,
        background: t.bgPanel,
      });
      anchor.data = { overlayKey: `perm-custom-${req.requestKey}-${index}` };
      block.appendChild(anchor);
    }
    card.appendChild(block);
  });

  const actions = h("div", { display: "flex", gap: 8, flexWrap: "wrap" });
  actions.appendChild(
    permBtn("提交回答", "allow", !canAnswer, () =>
      ctx.onRespondPermission(req.requestKey, JSON.stringify(questionAnswers())),
    ),
  );
  actions.appendChild(permBtn("拒绝回答", "reject", false, () => ctx.onRespondPermission(req.requestKey, "")));
  card.appendChild(actions);
  return card;
}

// ---------- 轮次折叠 / 尾标 / 模型 pill ----------

export function buildActualModelPill(model: string): Node {
  const t = theme();
  const wrap = h("div", {
    display: "flex",
    justifyContent: "flex-end",
    margin: [-8, 0, 8, 0],
    width: "100%",
  });
  const pill = h("div", {
    width: "fit-content",
    padding: [2, 7],
    border: 1,
    borderColor: t.borderLight,
    borderRadius: 999,
    color: t.textFaint,
    background: t.bgPanel,
    fontFamily: t.mono,
    fontSize: 11,
    userSelect: "none",
  }, `实际模型：${model}`);
  wrap.appendChild(pill);
  return wrap;
}

export function buildFoldButton(group: Group, open: boolean, ctx: BuilderCtx): Node {
  const t = theme();
  const foldKey = foldKeyOf(group);
  const btn = h("div", {
    display: "flex",
    alignItems: "center",
    gap: 6,
    width: "fit-content",
    color: t.textDim,
    fontSize: 13,
    padding: [4, 8],
    margin: [12, 0, 2, -8],
    borderRadius: 7,
    cursor: "pointer",
    userSelect: "none",
  });
  btn.hoverStyle = { background: t.bgHover, color: t.text };
  const tokenTitle = tokenTitleOf(group.turn);
  if (tokenTitle) btn.style.title = tokenTitle;
  btn.onClick = () => ctx.toggleExpanded(foldKey, !open);
  btn.data = { clickId: "turnFold", turnId: group.turn?.id };
  const label = h("span", { display: "inline", fontSize: 13, color: t.textDim }, foldLabelOf(group.turn));
  label.hoverStyle = { color: t.text };
  btn.appendChild(label);
  btn.appendChild(chevronNode(open, 12, { color: open ? t.text : t.textDim }));
  return btn;
}

export function buildLiveTail(): Node {
  const t = theme();
  const row = h("div", {
    display: "flex",
    alignItems: "center",
    gap: 6,
    margin: [4, 0, 10],
    color: t.textFaint,
    fontSize: 12.5,
    width: "fit-content",
    userSelect: "none",
  });
  row.appendChild(spinnerNode(true));
  row.appendChild(h("span", { display: "inline", fontSize: 12.5, color: t.textFaint }, "继续处理中…"));
  return row;
}

export function buildProcessContainer(children: Node[]): Node {
  const t = theme();
  const wrap = h("div", {
    margin: [4, 0, 6],
    padding: [2, 0, 2, 12],
    border: [0, 0, 0, 2],
    borderColor: ["", "", "", t.border],
    width: "100%",
  });
  wrap.replaceChildren(children);
  return wrap;
}

// ---------- 单条 item 分发 ----------

export type ItemBuilder = (
  item: Item,
  active: boolean,
  ctx: BuilderCtx,
  env: { cwd: string; agentKind: string; threadId: string },
) => Node | null;

export const buildItem: ItemBuilder = (item, active, ctx, env) => {
  switch (item.type) {
    case "user":
      return buildUserMessage(item, ctx);
    case "assistant":
      return buildAssistant(item, ctx, active);
    case "thought":
      return buildThought(item, active, ctx, env.agentKind);
    case "tool":
      return buildTool(item, active, ctx, env.cwd, env.threadId);
    case "system":
      return buildSystem(item, ctx);
    case "turn":
      return null;
    default:
      return null;
  }
};

// ---------- 分组 ----------

export function buildGroup(
  group: Group,
  active: boolean,
  ctx: BuilderCtx,
  env: { cwd: string; agentKind: string; threadId: string },
  itemFn: ItemBuilder = buildItem,
): Node {
  const wrap = h("div", { width: "100%" });
  wrap.data = { kind: "group" };

  if (group.user) wrap.appendChild(buildUserMessage(group.user, ctx));
  if (group.turn?.actualModel) wrap.appendChild(buildActualModelPill(group.turn.actualModel));

  const split = splitGroupBody(group.body, !!group.turn);
  const foldKey = foldKeyOf(group);
  const open = ctx.isExpanded(foldKey, bodyExpandedOf(group));
  const foldable = !!group.turn && !active;

  const processNodes: Node[] = [];
  for (const item of split.process) {
    const node = itemFn(item, active && item.id === activeBodyIdOf(group, active), ctx, env);
    if (node) processNodes.push(node);
  }

  if (split.process.length > 0) {
    if (foldable) {
      wrap.appendChild(buildFoldButton(group, open, ctx));
      if (open) wrap.appendChild(buildProcessContainer(processNodes));
    } else {
      for (const n of processNodes) wrap.appendChild(n);
    }
  }

  if (showLiveTailOf(group, active)) wrap.appendChild(buildLiveTail());

  for (const item of split.conclusion) {
    const node = itemFn(item, false, ctx, env);
    if (node) wrap.appendChild(node);
  }

  if (foldable) {
    const files = buildEditedFilesCard(group.body, `undone-${foldKey}`, ctx, env.cwd);
    if (files) wrap.appendChild(files);
  }
  return wrap;
}

// ---------- checkpoint banner / 空态 hint ----------

export function buildCheckpointBanner(onReturn: () => void): Node {
  const t = theme();
  const wrap = h("div", {
    display: "flex",
    justifyContent: "center",
    position: "sticky",
    top: 8,
    margin: [0, 0, 8, 0],
    width: "100%",
  });
  const btn = h("div", {
    padding: [5, 10],
    border: 1,
    borderColor: mix(t.accent, 0.3),
    borderRadius: 999,
    color: t.accent,
    background: t.bgPanel,
    fontSize: 10,
    cursor: "pointer",
    width: "fit-content",
    userSelect: "none",
  }, "回到当前时间线");
  btn.style.title = "回到当前时间线和最新消息";
  btn.hoverStyle = { borderColor: mix(t.accent, 0.58), background: mix(t.accent, 0.12, t.bgPanel) };
  btn.onClick = onReturn;
  wrap.appendChild(btn);
  return wrap;
}

export function buildHint(agentLabelText: string, cwdText: string): Node {
  const t = theme();
  const hint = h("div", {
    color: t.textFaint,
    fontSize: 13,
    textAlign: "center",
    padding: [40, 0],
    lineHeight: 1.8,
    width: "100%",
  });
  const span1 = h("span", { display: "inline", fontSize: 13, color: t.textFaint }, `在下方输入任务，${agentLabelText} 将在 `);
  const code = h("span", {
    display: "inline",
    fontFamily: t.mono,
    background: t.bgPanel,
    padding: [2, 6],
    borderRadius: 4,
    fontSize: 12,
    color: t.textFaint,
  }, cwdText);
  const span2 = h("span", { display: "inline", fontSize: 13, color: t.textFaint }, " 中工作。");
  hint.replaceChildren([span1, code, span2]);
  return hint;
}
