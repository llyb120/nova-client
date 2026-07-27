// markdown.ts — marked lexer 驱动的 Markdown → canvas 节点树构建器。
// 与 DOM 版（Markdown.tsx：marked(gfm+breaks) → DOMPurify → withCopyButtons/withFileReferences）
// 共用同一解析器，块级/行内结构天然一致；样式值逐条转抄自 app.css 的 .markdown 规则。

import { Lexer, type Token, type Tokens } from "marked";
import { iconNode, IconCheck, IconCopy, IconFile } from "../icons.js";
import { Node, h, type CanvasHitEvent, type Style } from "../node.js";
import { theme } from "../theme.js";

const MARKED_OPTS = { gfm: true, breaks: true } as const;

// —— 与 Markdown.tsx 完全相同的文件引用判定 ——
const FILE_EXTENSIONS =
  "7z|avif|avi|bmp|c|cc|cfg|conf|cpp|cs|css|csv|docx?|env|fig|gif|go|gz|h|hpp|html?|ico|ini|java|jpe?g|js|json|jsx|lock|log|md|mjs|mov|mp3|mp4|pdf|php|png|pptx?|ps1|psd|py|rar|rb|rs|scss|sh|sql|svg|tar|toml|ts|tsx|txt|vue|wav|webm|webp|xlsx?|xml|ya?ml|zip";
const FILE_REFERENCE_RE = new RegExp(
  String.raw`(?:[A-Za-z]:[\\/]|(?:\.{1,2})?[\\/])?[^\s<>"'\x60()[\]{}，。；：！？、]+(?:[\\/][^\s<>"'\x60()[\]{}，。；：！？、]+)*\.(?:${FILE_EXTENSIONS})(?::\d+)?(?![\w./\\:-])`,
  "gi",
);
const WHOLE_FILE_REFERENCE_RE = new RegExp(String.raw`\.(?:${FILE_EXTENSIONS})(?::\d+)?$`, "i");
const FILE_REFERENCE_CANDIDATE_RE = new RegExp(String.raw`\.(?:${FILE_EXTENSIONS})(?::\d+)?`, "i");
const IMAGE_FILE_RE = /\.(?:avif|bmp|gif|ico|jpe?g|png|svg|webp)$/i;

export interface MdCallbacks {
  onOpenFile: (path: string, line?: number) => void;
  onOpenUrl: (url: string) => void;
  onFileMenu: (e: CanvasHitEvent, path: string) => void;
  /** 代码块复制：组件侧写剪贴板并把 key 置为「已复制」1.2s */
  onCopyCode: (key: string, text: string) => void;
}

export interface MdBuildOptions {
  /** 段落基准字号/行高/颜色（assistant: 14/1.7/text；thought: 13/默认/dim） */
  fontSize: number;
  lineHeight: number;
  color: string;
  markFiles: boolean;
  /** 处于「已复制」态的代码块 key（组件维护） */
  copiedCodeKey?: string | null;
  cb: MdCallbacks;
}

interface Ctx extends MdBuildOptions {
  copiedCodeKey?: string | null;
}

// HTML 实体反转义（marked 渲染 HTML 时才会转义；直接画文本需要自己还原）
const ENTITY_MAP: Record<string, string> = {
  amp: "&",
  lt: "<",
  gt: ">",
  quot: '"',
  "#39": "'",
  nbsp: " ",
};
export function unescapeEntities(s: string): string {
  return s.replace(/&(#x?[0-9a-f]+|[a-z]+);/gi, (m, name: string) => {
    const lower = name.toLowerCase();
    if (lower in ENTITY_MAP) return ENTITY_MAP[lower];
    if (lower.startsWith("#x")) {
      const cp = parseInt(lower.slice(2), 16);
      return Number.isFinite(cp) ? String.fromCodePoint(cp) : m;
    }
    if (lower.startsWith("#")) {
      const cp = parseInt(lower.slice(1), 10);
      return Number.isFinite(cp) ? String.fromCodePoint(cp) : m;
    }
    return m;
  });
}

let codeBlockSeq = 0;

/** 构建整段 markdown 为一个块容器（对应 div.markdown） */
export function buildMarkdown(text: string, opts: MdBuildOptions): Node {
  const t = theme();
  const root = h("div", {
    width: "100%",
    fontSize: opts.fontSize,
    lineHeight: opts.lineHeight,
    color: opts.color,
    fontFamily: t.sans,
    wordBreak: "normal", // .markdown 的 break-word 由 layout 的超长硬断覆盖
  });
  const ctx: Ctx = { ...opts };
  const tokens = Lexer.lex(text, MARKED_OPTS);
  const blocks = buildBlocks(tokens, ctx);
  root.replaceChildren(blocks);
  // .markdown p:last-child { margin-bottom: 0 }
  const last = blocks[blocks.length - 1];
  if (last && last.data?.mdBlock === "p") {
    const m = last.style.margin;
    if (Array.isArray(m)) last.setStyle({ margin: [m[0], m[1], 0, m[3]] });
  }
  return root;
}

function base(ctx: Ctx): Partial<Style> {
  const t = theme();
  return {
    fontSize: ctx.fontSize,
    lineHeight: ctx.lineHeight,
    color: ctx.color,
    fontFamily: t.sans,
  };
}

function buildBlocks(tokens: Token[], ctx: Ctx, depth = 0): Node[] {
  const out: Node[] = [];
  for (const token of tokens) {
    const node = buildBlock(token, ctx, depth);
    if (node) out.push(node);
  }
  // .markdown p:last-child { margin-bottom: 0 }（每个容器的末段都适用：blockquote/li 同理）
  const last = out[out.length - 1];
  if (last && last.data?.mdBlock === "p") {
    const m = last.style.margin;
    if (Array.isArray(m)) last.setStyle({ margin: [m[0], m[1], 0, m[3]] });
  }
  return out;
}

function buildBlock(token: Token, ctx: Ctx, depth = 0): Node | null {
  const t = theme();
  switch (token.type) {
    case "space":
      return null;
    case "paragraph": {
      const tok = token as Tokens.Paragraph;
      const p = h("p", { ...base(ctx), margin: [0, 0, 10, 0] });
      p.data = { mdBlock: "p" };
      buildInline(Lexer.lexInline(tok.text, MARKED_OPTS), ctx, p);
      return p;
    }
    case "text": {
      // breaks 模式下顶层 text（无空行分段）
      const tok = token as Tokens.Text;
      const p = h("p", { ...base(ctx), margin: [0, 0, 10, 0] });
      p.data = { mdBlock: "p" };
      if (tok.tokens) buildInline(tok.tokens, ctx, p);
      else appendTextWithFileRefs(tok.text, ctx, p);
      return p;
    }
    case "heading": {
      const tok = token as Tokens.Heading;
      const scale = [1.25, 1.15, 1.05, 1.05, 0.83, 0.67][tok.depth - 1] ?? 1;
      const fs = ctx.fontSize * scale;
      const node = h("h" + tok.depth, {
        ...base(ctx),
        fontSize: fs,
        fontWeight: 600,
        letterSpacing: -0.01 * fs,
        margin: [16, 0, 8, 0],
        lineHeight: ctx.lineHeight,
      });
      node.data = { mdBlock: "heading" };
      buildInline(Lexer.lexInline(tok.text, MARKED_OPTS), ctx, node, { fontWeight: 600, fontSize: fs });
      return node;
    }
    case "code": {
      const tok = token as Tokens.Code;
      return codeBlock(tok, ctx);
    }
    case "blockquote": {
      const tok = token as Tokens.Blockquote;
      const q = h("blockquote", {
        ...base(ctx),
        color: t.textDim,
        padding: [2, 0, 2, 12],
        border: [0, 0, 0, 3],
        borderColor: ["", "", "", t.borderLight],
        margin: [0, 0, 10, 0],
      });
      q.data = { mdBlock: "blockquote" };
      q.replaceChildren(buildBlocks(tok.tokens, ctx, depth));
      return q;
    }
    case "list": {
      const tok = token as Tokens.List;
      const list = h(tok.ordered ? "ol" : "ul", {
        ...base(ctx),
        padding: [0, 0, 0, 22],
        margin: [0, 0, 10, 0],
      });
      list.data = { mdBlock: "list" };
      let index = typeof tok.start === "number" && Number.isFinite(tok.start) ? tok.start : 1;
      for (const item of tok.items) {
        list.appendChild(listItem(item, index++, tok.ordered, ctx, depth));
      }
      return list;
    }
    case "hr": {
      const node = h("hr", { height: 1, background: t.borderLight, margin: [8, 0] });
      node.data = { mdBlock: "hr" };
      return node;
    }
    case "table": {
      const tok = token as Tokens.Table;
      return tableBlock(tok, ctx);
    }
    case "html": {
      // DOMPurify 会保留安全标签的结构；canvas 上退化为纯文本段落
      const tok = token as Tokens.HTML | Tokens.Paragraph;
      const raw = "text" in tok ? tok.text : "";
      const text = unescapeEntities(raw.replace(/<[^>]*>/g, " ").replace(/\s+/g, " ").trim());
      if (!text) return null;
      const p = h("p", { ...base(ctx), margin: [0, 0, 10, 0] }, text);
      p.data = { mdBlock: "p" };
      return p;
    }
    default:
      return null;
  }
}

function codeBlock(tok: Tokens.Code, ctx: Ctx): Node {
  const t = theme();
  const key = `code-${++codeBlockSeq}-${tok.lang ?? ""}`;
  const wrap = h("div", { position: "relative", margin: [0, 0, 10, 0] });
  wrap.data = { mdBlock: "pre" };
  wrap.hoverContainer = true;
  // DOM 中 <pre> 的文本以单个 \n 结尾时不渲染末尾空行
  const text = tok.text.replace(/\n$/, "");
  // DOM 结构 <pre><code>：pre 继承 msg 字号（strut 决定行高），code 才用 mono 12.5
  const pre = h("pre", {
    fontFamily: t.mono,
    fontSize: ctx.fontSize,
    lineHeight: ctx.lineHeight,
    color: t.text,
    background: t.bgPanel,
    border: 1,
    borderColor: t.border,
    borderRadius: t.rMd,
    padding: [12, 14],
    overflow: "auto",
    whiteSpace: "pre",
  });
  const code = h("code", {
    display: "inline",
    fontFamily: t.mono,
    fontSize: 12.5,
    lineHeight: ctx.lineHeight,
    color: t.text,
    whiteSpace: "pre",
  });
  code.setText(text);
  pre.appendChild(code);
  wrap.appendChild(pre);

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
  btn.data = { copyKey: key };
  btn.onClick = () => ctx.cb.onCopyCode(key, text);
  btn.appendChild(iconNode(copied ? IconCheck : IconCopy, 13, { color: copied ? t.accent : t.textFaint }));
  // icon 颜色跟随按钮 hover：图标自身也挂一份 hoverStyle
  btn.children[0].hoverStyle = { color: copied ? t.accent : t.text };
  wrap.appendChild(btn);
  return wrap;
}

function listItem(item: Tokens.ListItem, index: number, ordered: boolean, ctx: Ctx, depth: number): Node {
  const t = theme();
  const li = h("li", { ...base(ctx), margin: [3, 0, 3, 0], position: "relative" });
  li.data = { mdBlock: "li" };
  // 标记（::marker 在 padding 区）：无序按层级 disc/circle/square，有序 N.，任务列表 checkbox
  if (item.task) {
    const box = h("div", {
      position: "absolute",
      left: -19,
      top: 3,
      width: 13,
      height: 13,
      border: 1,
      borderColor: item.checked ? t.accent : t.borderStrong,
      borderRadius: 3,
      background: item.checked ? t.accent : "transparent",
      display: "flex",
      justifyContent: "center",
      alignItems: "center",
      userSelect: "none",
    });
    if (item.checked) {
      box.appendChild(iconNode(IconCheck, 10, { color: t.onAccent }));
    }
    li.appendChild(box);
  } else if (ordered) {
    const marker = h("span", {
      position: "absolute",
      left: -18,
      top: 0,
      width: 15,
      textAlign: "right",
      color: t.textMuted,
      userSelect: "none",
    });
    marker.setText(`${index}.`);
    li.appendChild(marker);
  } else {
    // disc / circle / square（UA 样式层级）：绝对定位的实心/空心形状
    const kind = depth % 3 === 0 ? "disc" : depth % 3 === 1 ? "circle" : "square";
    const size = 6;
    const marker = h("div", {
      position: "absolute",
      left: -15,
      top: Math.round((ctx.fontSize * ctx.lineHeight - size) / 2) + 1,
      width: size,
      height: size,
      borderRadius: kind === "square" ? 0 : size / 2,
      background: kind === "circle" ? "transparent" : t.textMuted,
      border: kind === "circle" ? 1 : 0,
      borderColor: kind === "circle" ? t.textMuted : "",
      userSelect: "none",
    });
    li.appendChild(marker);
  }
  li.replaceChildren([...li.children, ...buildItemContent(item, ctx, depth)]);
  return li;
}

function buildItemContent(item: Tokens.ListItem, ctx: Ctx, depth: number): Node[] {
  // tight list：item.tokens 是 [text]；loose：含 paragraph 等块
  const out: Node[] = [];
  for (const tok of item.tokens) {
    if (tok.type === "text" && !(tok as Tokens.Text).tokens?.some((tk) => tk.type === "space")) {
      // tight：直接作为 li 的 inline 内容
      const textTok = tok as Tokens.Text;
      const inlineHost = h("div", { ...base(ctx) });
      if (textTok.tokens) buildInline(textTok.tokens, ctx, inlineHost);
      else appendTextWithFileRefs(textTok.text, ctx, inlineHost);
      out.push(inlineHost);
    } else {
      const node = buildBlock(tok, ctx, depth + 1);
      if (node) out.push(node);
    }
  }
  return out;
}

function tableBlock(tok: Tokens.Table, ctx: Ctx): Node {
  const t = theme();
  const fs = 13;
  const table = h("div", {
    width: "fit-content",
    maxWidth: "100%",
    margin: [0, 0, 10, 0],
    border: [1, 0, 0, 1],
    borderColor: [t.border, "", "", t.border],
    fontSize: fs,
    fontFamily: t.sans,
    lineHeight: ctx.lineHeight,
    color: t.text,
  });
  table.data = { mdBlock: "table" };

  const makeCell = (cellText: string, col: number, header: boolean): Node => {
    const align = tok.align[col] ?? (header ? "center" : "left");
    const cell = h("div", {
      flex: "0 1 auto",
      padding: [5, 10],
      border: [0, 1, 1, 0],
      borderColor: ["", t.border, t.border, ""],
      fontSize: fs,
      fontWeight: header ? 600 : "normal",
      textAlign: align === "center" ? "center" : align === "right" ? "right" : "left",
      minHeight: 0,
    });
    buildInline(Lexer.lexInline(cellText, MARKED_OPTS), ctx, cell, {
      fontSize: fs,
      fontWeight: header ? 600 : "normal",
      lineHeight: ctx.lineHeight,
    });
    return cell;
  };

  const headRow = h("div", { display: "flex" });
  headRow.data = { mdBlock: "table-row" };
  tok.header.forEach((cell, col) => headRow.appendChild(makeCell(cell.text, col, true)));
  table.appendChild(headRow);
  for (const row of tok.rows) {
    const rowNode = h("div", { display: "flex" });
    rowNode.data = { mdBlock: "table-row" };
    row.forEach((cell, col) => rowNode.appendChild(makeCell(cell.text, col, false)));
    table.appendChild(rowNode);
  }
  return table;
}

// ---------- 行内 ----------

function buildInline(
  tokens: Token[],
  ctx: Ctx,
  into: Node,
  patch: Partial<Style> = {},
): void {
  const t = theme();
  for (const token of tokens) {
    switch (token.type) {
      case "text": {
        const tok = token as Tokens.Text;
        if (tok.tokens) {
          buildInline(tok.tokens, ctx, into, patch);
        } else {
          appendTextWithFileRefs(tok.text, ctx, into, patch);
        }
        break;
      }
      case "escape": {
        const tok = token as Tokens.Escape;
        into.appendChild(inlineSpan(unescapeEntities(tok.text), ctx, patch));
        break;
      }
      case "strong": {
        const tok = token as Tokens.Strong;
        const host = h("span", { display: "inline" });
        buildInline(tok.tokens, ctx, host, { ...patch, fontWeight: "bold" });
        hoistInline(host, into);
        break;
      }
      case "em": {
        const tok = token as Tokens.Em;
        const host = h("span", { display: "inline" });
        buildInline(tok.tokens, ctx, host, { ...patch, fontStyle: "italic" });
        hoistInline(host, into);
        break;
      }
      case "del": {
        const tok = token as Tokens.Del;
        const host = h("span", { display: "inline" });
        buildInline(tok.tokens, ctx, host, { ...patch, textDecoration: "line-through" });
        hoistInline(host, into);
        break;
      }
      case "codespan": {
        const tok = token as Tokens.Codespan;
        const text = unescapeEntities(tok.text);
        const trimmed = text.trim();
        if (ctx.markFiles && WHOLE_FILE_REFERENCE_RE.test(trimmed)) {
          into.appendChild(fileRefChip(trimmed, ctx, patch));
        } else {
          into.appendChild(
            inlineSpan(text, ctx, {
              ...patch,
              fontFamily: t.mono,
              fontSize: 12.5,
              background: t.bgPanel,
              border: 1,
              borderColor: t.border,
              padding: [1, 6],
              borderRadius: 5,
            }),
          );
        }
        break;
      }
      case "link": {
        const tok = token as Tokens.Link;
        const href = tok.href ?? "";
        if (ctx.markFiles && !/^(?:https?:|mailto:|#)/i.test(href)) {
          const path = decodeURIComponent(href.replace(/^file:\/+/i, ""));
          if (WHOLE_FILE_REFERENCE_RE.test(path)) {
            into.appendChild(fileRefChip(path, ctx, patch));
            break;
          }
        }
        // 片段直接并入父级（拍平）；onClick/href 挂到每个产出的顶层片段上
        const startIdx = into.children.length;
        buildInline(tok.tokens, ctx, into, { ...patch, color: t.blue });
        const isHttp = /^https?:\/\//i.test(href);
        for (let i = startIdx; i < into.children.length; i++) {
          const c = into.children[i];
          c.data = { ...(c.data ?? {}), href };
          if (isHttp) {
            c.style.cursor = "pointer";
            c.onClick = () => ctx.cb.onOpenUrl(href);
          }
        }
        break;
      }
      case "image": {
        const tok = token as Tokens.Image;
        const img = new Node("img", {
          display: "inline-block",
          src: tok.href,
          maxWidth: "100%",
          maxHeight: 320,
          title: tok.title ?? tok.text ?? "",
        });
        into.appendChild(img);
        break;
      }
      case "br": {
        into.appendChild(inlineSpan("\n", ctx, patch));
        break;
      }
      case "html": {
        // 行内 html（<u> 等）：取纯文本
        const tok = token as Tokens.HTML | Tokens.Tag;
        const raw = "text" in tok ? tok.text : "";
        const text = unescapeEntities(raw.replace(/<[^>]*>/g, ""));
        if (text) into.appendChild(inlineSpan(text, ctx, patch));
        break;
      }
      default:
        break;
    }
  }
}

/** strong/em/del 的子片段已带样式 patch 构建，拍平为 inline 兄弟（与 HTML 嵌套渲染等价） */
function hoistInline(host: Node, into: Node): void {
  for (const child of [...host.children]) into.appendChild(child);
}

function inlineSpan(text: string, ctx: Ctx, patch: Partial<Style> = {}): Node {
  const span = h("span", {
    display: "inline",
    fontFamily: theme().sans,
    fontSize: ctx.fontSize,
    lineHeight: ctx.lineHeight,
    color: ctx.color,
    ...patch,
  });
  span.setText(text);
  return span;
}

/** 纯文本追加（含 FILE_REFERENCE_RE 扫描切 chip） */
function appendTextWithFileRefs(
  rawText: string,
  ctx: Ctx,
  into: Node,
  patch: Partial<Style> = {},
): void {
  const text = unescapeEntities(rawText);
  if (!ctx.markFiles) {
    into.appendChild(inlineSpan(text, ctx, patch));
    return;
  }
  FILE_REFERENCE_RE.lastIndex = 0;
  if (!FILE_REFERENCE_RE.test(text)) {
    into.appendChild(inlineSpan(text, ctx, patch));
    return;
  }
  FILE_REFERENCE_RE.lastIndex = 0;
  let end = 0;
  for (const match of text.matchAll(FILE_REFERENCE_RE)) {
    const before = text.slice(end, match.index);
    if (before) into.appendChild(inlineSpan(before, ctx, patch));
    into.appendChild(fileRefChip(match[0], ctx, patch));
    end = match.index + match[0].length;
  }
  const rest = text.slice(end);
  if (rest) into.appendChild(inlineSpan(rest, ctx, patch));
}

/** 文件引用 chip（对应 button.md-file-ref：图标 + 路径，inline-flex 的 inline-block 原子） */
function fileRefChip(pathWithLine: string, ctx: Ctx, patch: Partial<Style> = {}): Node {
  const t = theme();
  const lineMatch = pathWithLine.match(/:(\d+)$/);
  const path = lineMatch ? pathWithLine.slice(0, -lineMatch[0].length) : pathWithLine;
  const line = lineMatch ? Number(lineMatch[1]) : undefined;
  const isImage = IMAGE_FILE_RE.test(path);

  const chip = h("span", {
    display: "inline-block",
    padding: [1, 5],
    borderRadius: 5,
    background: t.bgPanel,
    maxWidth: "100%",
    cursor: "pointer",
    userSelect: "none",
    ...patch,
  });
  chip.style.title = isImage ? `打开图片 ${path}` : `打开文件 ${pathWithLine}`;
  chip.data = { path, line };
  chip.hoverStyle = { background: t.bgHover };
  chip.onClick = () => ctx.cb.onOpenFile(path, line);
  chip.onContextMenu = (n, e) => ctx.cb.onFileMenu(e, path);

  chip.appendChild(iconNode(IconFile, 13, { color: t.blue, display: "inline-block" }));
  const label = h("span", {
    display: "inline-block",
    margin: [0, 0, 0, 4],
    fontFamily: t.mono,
    fontSize: 12.5,
    lineHeight: ctx.lineHeight,
    color: t.blue,
    whiteSpace: "nowrap",
    textOverflow: "ellipsis",
    maxWidth: "100%",
    userSelect: "none",
  });
  label.setText(pathWithLine);
  label.hoverStyle = { textDecoration: "underline" };
  chip.appendChild(label);
  return chip;
}

/** DOM 版的廉价预检：源文本无候选时完全跳过文件引用标记 */
export function hasFileRefCandidate(src: string): boolean {
  return FILE_REFERENCE_CANDIDATE_RE.test(src);
}
