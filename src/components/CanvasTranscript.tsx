import { convertFileSrc } from "@tauri-apps/api/core";
import {
  paintCanvasBackdrop,
  STAR_MAP_UPDATE_MS,
} from "../canvasTranscript/base";
import { createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { clearCanvasChatSelection, setCanvasChatSelection } from "../chatSelection";
import { editUserMessage, expandedRevision, isExpanded, state, toggleExpanded } from "../store";
import { advanceStreamText, latestStreamTextItem, STREAM_PREBUFFER_MS } from "../streamReveal";
import type { Item, PermissionRequest, PromptImage, ToolItem, UserItem } from "../types";
import { displayToolTitle, isTrivialToolOutput, stripAnsi, toolHeadlineDetail } from "../utils";
import { createImageAttachments, ImageAttachmentStrip } from "./ImageAttachmentStrip";
import type { Group } from "./TurnGroup";
import { fmtDuration, fmtTokens, turnTokenTitle } from "./TurnGroup";

// ─── Public interface ────────────────────────────────────────────────────────

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
  threadId: string | null;
  groups: Group[];
  permissions: PermissionRequest[];
  running: boolean;
  loading: boolean;
  emptyHint: string;
  preview: boolean;
  onReturnToCurrent: () => void;
  onScroll?: (top: number, max: number, user: boolean) => void;
  /** 详情开合从 pointerup 吸底流程中退出，避免 click 前滚动导致命中项变化。 */
  onBrowseDetail?: () => void;
  ref?: (handle: CanvasTranscriptHandle) => void;
}

// ─── Theme / palette ─────────────────────────────────────────────────────────

interface Palette {
  bg: string; panel: string; sidebar: string; hover: string;
  border: string; borderLight: string;
  text: string; dim: string; muted: string; faint: string;
  accent: string; accentDim: string;
  red: string; yellow: string; green: string; blue: string;
  scroll: string; wash1: string; wash2: string; gridDot: string;
  glowAccent: string; glowCyan: string; glowCorner: string;
  mono: string; sans: string;
}

function readPalette(): Palette {
  const s = getComputedStyle(document.documentElement);
  const v = (n: string, fb: string) => s.getPropertyValue(n).trim() || fb;
  return {
    bg: v("--bg", "#0e1014"), panel: v("--bg-panel", "#171b23"),
    sidebar: v("--bg-sidebar", "#0a0c10"), hover: v("--bg-hover", "#1e232d"),
    border: v("--border", "#252a33"), borderLight: v("--border-light", "#313844"),
    text: v("--canvas-text", v("--text", "#e4e7ec")),
    dim: v("--canvas-text-dim", v("--text-dim", "#a9b0bd")),
    muted: v("--canvas-text-muted", v("--text-muted", "#7d8593")),
    faint: v("--canvas-text-faint", v("--text-faint", "#5d6470")),
    accent: v("--accent", "#6e93f8"), accentDim: v("--accent-dim", "rgba(110,147,248,.14)"),
    red: v("--red", "#e07d76"), yellow: v("--yellow", "#d4b26e"),
    green: v("--green", "#8ec489"), blue: v("--blue", "#7aa2f2"),
    scroll: v("--scroll", "#2e333d"),
    wash1: v("--wash-1", "rgba(111,151,240,.05)"),
    wash2: v("--wash-2", "rgba(209,154,102,.04)"),
    gridDot: v("--grid-dot", "rgba(228,231,236,.045)"),
    glowAccent: v("--canvas-glow-accent", "rgba(110,147,248,.11)"),
    glowCyan: v("--canvas-glow-cyan", "rgba(63,212,228,.06)"),
    glowCorner: v("--canvas-glow-corner", "rgba(110,147,248,.05)"),
    mono: v("--mono", "monospace"), sans: v("--sans", "sans-serif"),
  };
}

// ─── Text measurement (cached) ──────────────────────────────────────────────

/** 行内代码块 chip 的内边距：文字 → chip 边缘 */
const CODE_PAD = 7;
/** 行内代码块 chip 与相邻文字之间的空隙。chip 绘制时左右各外扩 1px（描边），
 *  所以实际视觉空隙 = CODE_GAP - 1。 */
const CODE_GAP = 5;

let _mCtx: CanvasRenderingContext2D | null = null;
function mCtx() {
  if (!_mCtx) {
    _mCtx = document.createElement("canvas").getContext("2d")!;
    // 与主画布（paint 里的 optimizeLegibility / fontKerning）保持同一套文本整形，
    // 否则 measureText 与 fillText 量出的宽度不一致，chip 尺寸与文字位置都会偏。
    _mCtx.textRendering = "optimizeLegibility";
    _mCtx.fontKerning = "normal";
  }
  return _mCtx;
}
const _mCache = new Map<string, number>();
function measure(text: string, fs: number, ff: string, fw = "400", style = "normal"): number {
  const key = `${style}|${fw}|${fs}|${ff}|${text}`;
  let w = _mCache.get(key);
  if (w != null) return w;
  const ctx = mCtx();
  ctx.font = `${style} ${fw} ${fs}px ${ff}`;
  w = ctx.measureText(text).width;
  // 大会话唯一字符串远超旧上限 8192，整表清空会让热点全部重测；提高上限并只淘汰最旧的一半
  if (_mCache.size > 32768) {
    let n = 0;
    for (const k of _mCache.keys()) {
      _mCache.delete(k);
      if (++n > 16384) break;
    }
  }
  _mCache.set(key, w);
  return w;
}

/** 省略号截断缓存：key = 字体+宽度+原文。工具标题普遍是超长命令，此前每帧逐字收缩
 *  （每行每帧上百次 measure），展开大量工具行后成为滚动卡顿主因。 */
const _ellipsisCache = new Map<string, string>();
function ellipsize(text: string, maxW: number, fs: number, ff: string): string {
  if (measure(text, fs, ff) <= maxW) return text;
  const key = `${fs}|${ff}|${Math.round(maxW)}|${text}`;
  let out = _ellipsisCache.get(key);
  if (out !== undefined) return out;
  // 二分定位最长可显示前缀，把逐字收缩从 O(n) 次 measure 降到 O(log n)
  let lo = 1;
  let hi = text.length;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (measure(text.slice(0, mid) + "…", fs, ff) <= maxW) lo = mid;
    else hi = mid - 1;
  }
  out = text.slice(0, lo) + "…";
  if (_ellipsisCache.size > 4096) _ellipsisCache.clear();
  _ellipsisCache.set(key, out);
  return out;
}

/** Soft-wrapped line plus per-UTF16-unit offsets into the original (normalized) string. */
interface WrappedLine {
  text: string; offsets: number[];
  /** 行尾是源文本中的真实换行（段落边界），复制时应保留 "\n" */
  hardBreak?: boolean;
  /** 软换行发生在空白处（折行时该空白被丢弃），复制时应补一个空格 */
  spaceBreak?: boolean;
}
interface CharStyle { bold?: boolean; italic?: boolean; code?: boolean; link?: string }
type LineMeasurer = (text: string, offsets: number[]) => number;

function pushTrimmedLine(lines: WrappedLine[], text: string, offsets: number[]) {
  let end = text.length;
  while (end > 0 && /\s/.test(text.charAt(end - 1))) end--;
  lines.push({ text: text.slice(0, end), offsets: offsets.slice(0, end) });
}

/**
 * Wrap like wrapText, but keep original-string offsets for each kept character.
 * Critical for styled paint: wrapText drops `\n` / trimmed spaces, while charStyles
 * is indexed by the full plain text — without offsets, inline code/bold runs shift
 * (e.g. first char of each `code` falls outside the pill).
 */
function wrapTextIndexed(
  text: string,
  maxW: number,
  fs: number,
  ff: string,
  fw = "400",
  customMeasure?: LineMeasurer,
): WrappedLine[] {
  const lines: WrappedLine[] = [];
  const safeMax = Math.max(1, maxW);
  const lineWidth: LineMeasurer = customMeasure || ((value) => measure(value, fs, ff, fw));
  const normalized = text.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  const paras = normalized.split("\n");
  let base = 0;
  for (let pi = 0; pi < paras.length; pi++) {
    const para = paras[pi];
    const paraLineStart = lines.length;
    // Keep intentional empty paragraphs as a single blank line.
    if (para === "") {
      lines.push({ text: "", offsets: [], hardBreak: true });
      if (pi < paras.length - 1) base += 1; // consume the separating `\n`
      continue;
    }
    const tokens: { start: number; text: string }[] = [];
    const re = /(\s+)/g;
    let last = 0;
    let m: RegExpExecArray | null;
    while ((m = re.exec(para)) !== null) {
      if (m.index > last) tokens.push({ start: base + last, text: para.slice(last, m.index) });
      tokens.push({ start: base + m.index, text: m[0] });
      last = m.index + m[0].length;
    }
    if (last < para.length) tokens.push({ start: base + last, text: para.slice(last) });
    if (!tokens.length) tokens.push({ start: base, text: para });

    let cur = "", curOffs: number[] = [], curW = 0;
    for (const token of tokens) {
      // Don't start a line with whitespace — matches typical pre-wrap soft-wrap feel.
      if (!cur && /^\s+$/.test(token.text)) continue;
      const tokenOffs = Array.from({ length: token.text.length }, (_, ci) => token.start + ci);
      const joined = cur + token.text;
      const joinedOffs = curOffs.concat(tokenOffs);
      const joinedW = lineWidth(joined, joinedOffs);
      if (joinedW <= safeMax) {
        cur = joined;
        curOffs = joinedOffs;
        curW = joinedW;
        continue;
      }
      // Word won't fit: fill remaining space on this line char-by-char first
      // (CJK soft-wrap), instead of flushing `cur` and starting the word on the next line.
      if (/^\s+$/.test(token.text)) {
        if (cur) {
          pushTrimmedLine(lines, cur, curOffs);
          // 折行发生在空白处：复制时应还原为一个空格，而不是换行或直接拼接
          lines[lines.length - 1].spaceBreak = true;
          cur = ""; curOffs = []; curW = 0;
        }
        continue;
      }
      let ci = 0;
      for (const ch of Array.from(token.text)) {
        const off = token.start + ci;
        const chOffs = Array.from({ length: ch.length }, (_, k) => off + k);
        const candidate = cur + ch;
        const candidateOffs = curOffs.concat(chOffs);
        const candidateW = lineWidth(candidate, candidateOffs);
        if (cur && candidateW > safeMax) {
          lines.push({ text: cur, offsets: curOffs });
          cur = ch;
          curOffs = chOffs;
          curW = lineWidth(ch, chOffs);
        } else {
          cur = candidate;
          curOffs = candidateOffs;
          curW = candidateW;
        }
        ci += ch.length;
      }
    }
    // A trailing whitespace token can overflow after the paragraph's content was
    // already flushed. Do not turn that discarded whitespace into a blank line.
    if (cur || lines.length === paraLineStart) pushTrimmedLine(lines, cur, curOffs);
    // 段落最后一行以源文本中的真实 \n 结尾
    if (lines.length > paraLineStart) lines[lines.length - 1].hardBreak = true;
    base += para.length;
    if (pi < paras.length - 1) base += 1; // skip `\n` — not present in any line
  }
  while (lines.length > 1 && lines[lines.length - 1].text === "") lines.pop();
  while (lines.length > 1 && lines[0].text === "") lines.shift();
  return lines.length ? lines : [{ text: "", offsets: [] }];
}

function wrapText(text: string, maxW: number, fs: number, ff: string, fw = "400"): string[] {
  return wrapTextIndexed(text, maxW, fs, ff, fw).map((l) => l.text);
}

/** 复制时的行尾连接符：硬换行 → "\n"；空白处软换行 → " "；词内软换行 → ""（直接拼接，不截断）。 */
function wrappedLineSeps(wrapped: WrappedLine[]): string[] {
  return wrapped.map((l) => (l.hardBreak ? "\n" : l.spaceBreak ? " " : ""));
}

function wrapTextFull(text: string, maxW: number, fs: number, ff: string, fw = "400"): { lines: string[]; seps: string[] } {
  const wrapped = wrapTextIndexed(text, maxW, fs, ff, fw);
  return { lines: wrapped.map((l) => l.text), seps: wrappedLineSeps(wrapped) };
}

const CODE_TAB_SIZE = 4;

/** CanvasRenderingContext2D does not lay out tab characters like a <pre> does. */
function expandCodeTabs(text: string): string {
  const normalized = text.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  let column = 0;
  let result = "";
  for (const ch of normalized) {
    if (ch === "\n") {
      result += ch;
      column = 0;
    } else if (ch === "\t") {
      const spaces = CODE_TAB_SIZE - (column % CODE_TAB_SIZE);
      result += " ".repeat(spaces);
      column += spaces;
    } else {
      result += ch;
      column++;
    }
  }
  return result;
}

/** Code needs to retain leading whitespace; the prose wrapper intentionally drops it. */
function wrapCodeTextFull(text: string, maxW: number, fs: number, ff: string): { lines: string[]; seps: string[] } {
  const lines: string[] = [];
  const seps: string[] = [];
  const safeMax = Math.max(1, maxW);
  const expanded = expandCodeTabs(text);
  for (const sourceLine of expanded.split("\n")) {
    if (!sourceLine) {
      lines.push("");
      seps.push("\n");
      continue;
    }
    let line = "";
    let lineW = 0;
    for (const ch of Array.from(sourceLine)) {
      const charW = measure(ch, fs, ff);
      if (line && lineW + charW > safeMax) {
        lines.push(line);
        seps.push(""); // 代码行内软换行：复制时直接拼接
        line = "";
        lineW = 0;
      }
      line += ch;
      lineW += charW;
    }
    lines.push(line);
    seps.push("\n");
  }
  while (lines.length > 1 && lines[lines.length - 1] === "") { lines.pop(); seps.pop(); }
  if (!lines.length) { lines.push(""); seps.push("\n"); }
  return { lines, seps };
}

function wrapCodeText(text: string, maxW: number, fs: number, ff: string): string[] {
  return wrapCodeTextFull(text, maxW, fs, ff).lines;
}

function segmentCharStyles(segments: TextSegment[]): CharStyle[] {
  const styles: CharStyle[] = [];
  for (const seg of segments) {
    for (let i = 0; i < seg.text.length; i++) {
      styles.push({ bold: seg.bold, italic: seg.italic, code: seg.code, link: seg.link });
    }
  }
  return styles;
}

function wrapStyledTextIndexed(
  text: string,
  segments: TextSegment[],
  maxW: number,
  fs: number,
  ff: string,
  mono: string,
  baseFw = "400",
  styles = segmentCharStyles(segments),
): WrappedLine[] {
  const measureLine: LineMeasurer = (line, offsets) => {
    let width = 0;
    let start = 0;
    while (start < line.length) {
      const style = styles[offsets[start]] || {};
      let end = start + 1;
      while (end < line.length) {
        const next = styles[offsets[end]] || {};
        if (next.bold !== style.bold || next.italic !== style.italic || next.code !== style.code || next.link !== style.link) break;
        end++;
      }
      width += measure(line.slice(start, end), style.code ? 12.5 : fs, style.code ? mono : ff, style.bold ? "bold" : baseFw, style.italic ? "italic" : "normal");
      // 与 paintStyledText 中 code run 的推进量保持一致
      if (style.code) width += CODE_PAD * 2 + CODE_GAP * 2;
      start = end;
    }
    return width;
  };
  return wrapTextIndexed(text, maxW, fs, ff, baseFw, measureLine);
}

function wrapStyledText(
  text: string,
  segments: TextSegment[],
  maxW: number,
  fs: number,
  ff: string,
  mono: string,
  baseFw = "400",
): string[] {
  return wrapStyledTextIndexed(text, segments, maxW, fs, ff, mono, baseFw).map((line) => line.text);
}
// Match DOM .bubble-images img: max-width 240px; max-height 180px
// Scale uniformly by original aspect ratio to fit within max W or H (never stretch).
const BUBBLE_IMG_MAX_W = 240;
const BUBBLE_IMG_MAX_H = 180;
const BUBBLE_IMG_GAP = 6;
const BUBBLE_IMG_MARGIN_BOTTOM = 6;

function promptImageSrc(img: PromptImage): string {
  return img.data
    ? `data:${img.mimeType};base64,${img.data}`
    : convertFileSrc(decodeURI((img.uri ?? "").replace(/^file:\/\/+/, "")));
}

function bubbleImageSize(el: HTMLImageElement | null | undefined): { w: number; h: number } {
  const nw = el?.naturalWidth ?? 0;
  const nh = el?.naturalHeight ?? 0;
  if (!nw || !nh) return { w: 160, h: 120 };
  const scale = Math.min(1, BUBBLE_IMG_MAX_W / nw, BUBBLE_IMG_MAX_H / nh);
  return { w: Math.max(1, Math.round(nw * scale)), h: Math.max(1, Math.round(nh * scale)) };
}

interface BubbleImageLayout {
  dx: number; dy: number; w: number; h: number; img: PromptImage;
}

function layoutBubbleImages(
  images: PromptImage[] | undefined,
  maxInnerW: number,
  load: (img: PromptImage) => HTMLImageElement | null,
): { layouts: BubbleImageLayout[]; usedW: number; stackH: number } {
  const layouts: BubbleImageLayout[] = [];
  let usedW = 0;
  let stackH = 0;
  if (!images?.length) return { layouts, usedW, stackH };

  let x = 0;
  let y = 0;
  let rowH = 0;
  for (const img of images) {
    if (!img.mimeType.startsWith("image/")) continue;
    const size = bubbleImageSize(load(img));
    if (x > 0 && x + size.w > maxInnerW) {
      usedW = Math.max(usedW, x - BUBBLE_IMG_GAP);
      y += rowH + BUBBLE_IMG_GAP;
      x = 0;
      rowH = 0;
    }
    layouts.push({ dx: 16 + x, dy: 10 + y, w: size.w, h: size.h, img });
    x += size.w + BUBBLE_IMG_GAP;
    rowH = Math.max(rowH, size.h);
  }
  if (!layouts.length) return { layouts, usedW, stackH };
  usedW = Math.max(usedW, x > 0 ? x - BUBBLE_IMG_GAP : 0);
  stackH = y + rowH + BUBBLE_IMG_MARGIN_BOTTOM;
  return { layouts, usedW, stackH };
}

// ─── Markdown parser ─────────────────────────────────────────────────────────

interface TextSegment {
  text: string;
  bold?: boolean;
  italic?: boolean;
  code?: boolean;
  link?: string;
}

interface MdTableCell {
  segments: TextSegment[];
}

interface MdBlock {
  type: "paragraph" | "heading" | "code" | "list-item" | "blockquote" | "hr" | "table";
  segments: TextSegment[];
  level?: number;
  lang?: string;
  ordered?: boolean;
  prefix?: string;
  listDepth?: number;
  raw?: string;
  rows?: MdTableCell[][];
  aligns?: Array<"left" | "center" | "right">;
}

function splitTableRow(line: string): string[] {
  let s = line.trim();
  if (s.startsWith("|")) s = s.slice(1);
  if (s.endsWith("|")) s = s.slice(0, -1);
  return s.split("|").map(c => c.trim());
}

function isTableSeparator(line: string): boolean {
  if (!line.includes("|")) return false;
  const cells = splitTableRow(line);
  return cells.length > 0 && cells.every(c => /^:?-{1,}:?$/.test(c.trim()));
}

function parseTableAlign(cell: string): "left" | "center" | "right" {
  const t = cell.trim();
  const left = t.startsWith(":");
  const right = t.endsWith(":");
  if (left && right) return "center";
  if (right) return "right";
  return "left";
}

function isTableStart(lines: string[], i: number): boolean {
  return i + 1 < lines.length && lines[i].includes("|") && isTableSeparator(lines[i + 1]);
}

function tokenizeInline(text: string): TextSegment[] {
  const tokens: TextSegment[] = [];
  let i = 0, buf = "";
  const flush = () => { if (buf) { tokens.push({ text: buf }); buf = ""; } };
  while (i < text.length) {
    const rest = text.slice(i);
    let m: RegExpMatchArray | null;
    m = rest.match(/^`([^`]+)`/);
    if (m) { flush(); tokens.push({ text: m[1], code: true }); i += m[0].length; continue; }
    // `__` 粗体要求开定界符前是空白/标点/行首（CommonMark flanking），避免把
    // mcp__nova-tools__polaris 中的 __nova-tools__ 误识别为粗体吃掉两侧 __。
    const prevCh = i > 0 ? text[i - 1] : "";
    const preOk = i === 0 || /[\s\p{P}]/u.test(prevCh);
    m = rest.match(/^\*\*([^*\s](?:[^*]*[^*\s])?)\*\*(?![\w])/) ;
    if (m) { flush(); tokens.push({ text: m[1], bold: true }); i += m[0].length; continue; }
    m = preOk && prevCh !== "_" ? rest.match(/^(?!_)__([^_\s](?:[^_]*[^_\s])?)__(?![\w_])/) : null;
    if (m) { flush(); tokens.push({ text: m[1], bold: true }); i += m[0].length; continue; }
    m = preOk ? rest.match(/^\*([^*\s](?:[^*]*[^*\s])?)\*(?![\w])/) : null;
    if (m) { flush(); tokens.push({ text: m[1], italic: true }); i += m[0].length; continue; }
    // 下划线斜体要求前一个字符不是下划线（排除 __ 前缀、标识符内的连续下划线），
    // 且闭合定界符后不接词字符。
    m = preOk && prevCh !== "_" ? rest.match(/^(?!__)_([^_\s](?:[^_]*[^_\s])?)_(?![\w_])/) : null;
    if (m) { flush(); tokens.push({ text: m[1], italic: true }); i += m[0].length; continue; }
    m = rest.match(/^\[([^\]]+)\]\(([^)\s]+)\)/);
    if (m) { flush(); tokens.push({ text: m[1], link: m[2] }); i += m[0].length; continue; }
    buf += text[i]; i++;
  }
  flush();
  return tokens;
}

function parseMarkdownBlocks(md: string): MdBlock[] {
  const blocks: MdBlock[] = [];
  const lines = md.replace(/\r\n/g, "\n").split("\n");
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    // Be lenient with model-generated indentation. CommonMark only allows up to
    // three leading spaces, but treating deeper-indented fences as prose leaves
    // literal backticks and turns the text between them into inline code.
    const fence = line.match(/^\s*(`{3,}|~{3,})\s*(.*?)\s*$/);
    if (fence) {
      const fenceChar = fence[1][0];
      const fenceLen = fence[1].length;
      const lang = fence[2].split(/\s/)[0] || "";
      const closeRe = new RegExp(`^\\s*${fenceChar === '`' ? '`' : '~'}{${fenceLen},}\\s*$`);
      const buf: string[] = [];
      i++;
      while (i < lines.length && !closeRe.test(lines[i])) { buf.push(lines[i]); i++; }
      if (i < lines.length) i++;
      blocks.push({ type: "code", segments: [{ text: buf.join("\n") }], lang, raw: buf.join("\n") });
      continue;
    }
    if (/^\s*([-*_])\1{2,}\s*$/.test(line)) { blocks.push({ type: "hr", segments: [] }); i++; continue; }
    const hm = line.match(/^(#{1,6})\s+(.*)$/);
    if (hm) { blocks.push({ type: "heading", level: hm[1].length, segments: tokenizeInline(hm[2]) }); i++; continue; }
    if (/^>\s?/.test(line)) {
      const buf: string[] = [];
      while (i < lines.length && /^>\s?/.test(lines[i])) { buf.push(lines[i].replace(/^>\s?/, "")); i++; }
      blocks.push({ type: "blockquote", segments: tokenizeInline(buf.join("\n")) });
      continue;
    }
    const listStart = line.match(/^(\s*)([-*+]|(\d+)[.)])\s+(.*)$/);
    if (listStart) {
      const indentStack: number[] = [];
      while (i < lines.length) {
        const item = lines[i].match(/^(\s*)([-*+]|(\d+)[.)])\s+(.*)$/);
        if (!item) break;
        const indent = item[1].replace(/\t/g, "    ").length;
        while (indentStack.length && indentStack[indentStack.length - 1] > indent) indentStack.pop();
        if (!indentStack.length || indentStack[indentStack.length - 1] < indent) indentStack.push(indent);
        const ordered = item[3] !== undefined;
        blocks.push({
          type: "list-item",
          segments: tokenizeInline(item[4]),
          ordered,
          prefix: ordered ? `${item[3]}.` : "•",
          listDepth: indentStack.length - 1,
        });
        i++;
      }
      continue;
    }
    if (isTableStart(lines, i)) {
      const headerCells = splitTableRow(line).map(c => ({ segments: tokenizeInline(c) }));
      const aligns = splitTableRow(lines[i + 1]).map(parseTableAlign);
      while (aligns.length < headerCells.length) aligns.push("left");
      const rows: MdTableCell[][] = [headerCells];
      i += 2;
      while (i < lines.length) {
        const l = lines[i];
        if (/^\s*$/.test(l) || isTableSeparator(l) || /^\s*(`{3,}|~{3,})/.test(l)
          || /^(#{1,6})\s+/.test(l) || /^>\s?/.test(l) || /^\s*[-*+]\s+/.test(l)
          || /^\s*\d+[.)]\s+/.test(l) || /^\s*([-*_])\1{2,}\s*$/.test(l) || !l.includes("|")) break;
        const cells = splitTableRow(l).map(c => ({ segments: tokenizeInline(c) }));
        while (cells.length < headerCells.length) cells.push({ segments: [{ text: "" }] });
        rows.push(cells.slice(0, headerCells.length));
        i++;
      }
      const plain = rows.map(r => r.map(c => segmentsPlainText(c.segments)).join("\t")).join("\n");
      blocks.push({
        type: "table", segments: [{ text: plain }], rows, aligns: aligns.slice(0, headerCells.length), raw: plain,
      });
      continue;
    }
    if (/^\s*$/.test(line)) { i++; continue; }
    const buf: string[] = [line]; i++;
    while (i < lines.length) {
      const l = lines[i];
      if (/^\s*$/.test(l) || /^\s*(`{3,}|~{3,})/.test(l) || /^(#{1,6})\s+/.test(l) || /^>\s?/.test(l)
        || /^\s*[-*+]\s+/.test(l) || /^\s*\d+[.)]\s+/.test(l) || /^\s*([-*_])\1{2,}\s*$/.test(l)
        || isTableStart(lines, i)) break;
      buf.push(l); i++;
    }
    blocks.push({ type: "paragraph", segments: tokenizeInline(buf.join("\n")) });
  }
  return blocks;
}

function segmentsPlainText(segments: TextSegment[]): string {
  return segments.map(s => s.text).join("");
}

const TABLE_FS = 13;
const TABLE_PAD_X = 10;
const TABLE_PAD_Y = 5;
const TABLE_LH = 1.45;

function layoutMdTable(
  mb: MdBlock,
  result: Block[],
  itemId: number,
  gi: number,
  x: number,
  y: number,
  proseW: number,
  p: Palette,
  opts?: { color?: string; fontSize?: number },
): number {
  const rows = mb.rows;
  if (!rows?.length) return y;
  const fs = opts?.fontSize ?? TABLE_FS;
  const color = opts?.color ?? p.text;
  const aligns = mb.aligns || [];
  const colCount = rows[0].length;
  const colWidths = new Array(colCount).fill(0);
  for (let r = 0; r < rows.length; r++) {
    const fw = r === 0 ? "600" : "400";
    for (let c = 0; c < colCount; c++) {
      const plain = segmentsPlainText(rows[r][c]?.segments || []);
      colWidths[c] = Math.max(colWidths[c], measure(plain, fs, p.sans, fw) + TABLE_PAD_X * 2);
    }
  }
  let totalW = colWidths.reduce((a, b) => a + b, 0);
  if (totalW > proseW && totalW > 0) {
    const scale = proseW / totalW;
    for (let c = 0; c < colCount; c++) colWidths[c] = Math.max(24, colWidths[c] * scale);
    totalW = colWidths.reduce((a, b) => a + b, 0);
  }
  const cellLines: string[][][] = [];
  const cellSeps: string[][][] = [];
  const rowHeights: number[] = [];
  for (let r = 0; r < rows.length; r++) {
    const fw = r === 0 ? "600" : "400";
    const linesPerCell: string[][] = [];
    const sepsPerCell: string[][] = [];
    let maxLines = 1;
    for (let c = 0; c < colCount; c++) {
      const plain = segmentsPlainText(rows[r][c]?.segments || []);
      const maxW = Math.max(1, colWidths[c] - TABLE_PAD_X * 2);
      const { lines, seps } = wrapTextFull(plain, maxW, fs, p.sans, fw);
      linesPerCell.push(lines);
      sepsPerCell.push(seps);
      maxLines = Math.max(maxLines, lines.length);
    }
    cellLines.push(linesPerCell);
    cellSeps.push(sepsPerCell);
    rowHeights.push(maxLines * fs * TABLE_LH + TABLE_PAD_Y * 2);
  }
  const tableH = rowHeights.reduce((a, b) => a + b, 0);
  const tableW = Math.min(proseW, totalW);
  const plain = mb.raw || rows.map(r => r.map(c => segmentsPlainText(c.segments)).join("\t")).join("\n");
  result.push({
    kind: "md-table", id: itemId, groupIdx: gi,
    x, y, w: tableW, h: tableH,
    text: plain, color, fontSize: fs, font: p.sans, selectable: true,
    data: { rows, aligns, colWidths, cellLines, cellSeps, rowHeights, border: p.border },
  });
  return y + tableH + 10;
}

// ─── Tool icon SVG paths (stroke-based, viewBox 0 0 24 24) ──────────────────

function drawToolIcon(ctx: CanvasRenderingContext2D, kind: string, x: number, y: number, size: number, color: string) {
  ctx.save();
  ctx.strokeStyle = color;
  ctx.lineWidth = 2;
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  ctx.fillStyle = "none";
  const s = size / 24;
  ctx.translate(x, y);
  ctx.scale(s, s);
  ctx.beginPath();
  switch (kind) {
    case "read": // file
      ctx.moveTo(15, 2); ctx.lineTo(6, 2); ctx.quadraticCurveTo(4, 2, 4, 4);
      ctx.lineTo(4, 20); ctx.quadraticCurveTo(4, 22, 6, 22);
      ctx.lineTo(18, 22); ctx.quadraticCurveTo(20, 22, 20, 20);
      ctx.lineTo(20, 7); ctx.closePath(); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(14, 2); ctx.lineTo(14, 6);
      ctx.quadraticCurveTo(14, 8, 16, 8); ctx.lineTo(20, 8); ctx.stroke();
      break;
    case "edit": // pencil
      ctx.moveTo(21.17, 6.83);
      ctx.quadraticCurveTo(22.6, 5.4, 21.17, 2.83);
      ctx.quadraticCurveTo(19.7, 1.4, 17.17, 2.83);
      ctx.lineTo(3, 17); ctx.lineTo(3, 21); ctx.lineTo(7, 21); ctx.closePath(); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(15, 5); ctx.lineTo(19, 9); ctx.stroke();
      break;
    case "delete": // trash
      ctx.moveTo(3, 6); ctx.lineTo(21, 6); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(19, 6); ctx.lineTo(19, 20);
      ctx.quadraticCurveTo(19, 22, 17, 22); ctx.lineTo(7, 22);
      ctx.quadraticCurveTo(5, 22, 5, 20); ctx.lineTo(5, 6); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(8, 6); ctx.lineTo(8, 4);
      ctx.quadraticCurveTo(8, 2, 10, 2); ctx.lineTo(14, 2);
      ctx.quadraticCurveTo(16, 2, 16, 4); ctx.lineTo(16, 6); ctx.stroke();
      break;
    case "search": // magnifier
      ctx.arc(11, 11, 8, 0, Math.PI * 2); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(21, 21); ctx.lineTo(16.7, 16.7); ctx.stroke();
      break;
    case "execute": // terminal
      ctx.moveTo(4, 17); ctx.lineTo(10, 11); ctx.lineTo(4, 5); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(12, 19); ctx.lineTo(20, 19); ctx.stroke();
      break;
    case "think": // brain
      ctx.moveTo(12, 5); ctx.bezierCurveTo(12, 2, 9, 2, 9, 5);
      ctx.bezierCurveTo(6, 3, 4, 6, 6, 8);
      ctx.bezierCurveTo(3, 9, 4, 13, 7, 13);
      ctx.bezierCurveTo(5, 15, 7, 18, 10, 18); ctx.lineTo(12, 18); ctx.stroke();
      ctx.beginPath();
      ctx.moveTo(12, 5); ctx.bezierCurveTo(12, 2, 15, 2, 15, 5);
      ctx.bezierCurveTo(18, 3, 20, 6, 18, 8);
      ctx.bezierCurveTo(21, 9, 20, 13, 17, 13);
      ctx.bezierCurveTo(19, 15, 17, 18, 14, 18); ctx.lineTo(12, 18); ctx.stroke();
      break;
    case "fetch": // globe
      ctx.arc(12, 12, 10, 0, Math.PI * 2); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(2, 12); ctx.lineTo(22, 12); ctx.stroke();
      ctx.beginPath();
      ctx.moveTo(12, 2); ctx.bezierCurveTo(14.5, 4, 15.5, 8, 15.5, 12);
      ctx.bezierCurveTo(15.5, 16, 14.5, 20, 12, 22); ctx.stroke();
      ctx.beginPath();
      ctx.moveTo(12, 2); ctx.bezierCurveTo(9.5, 4, 8.5, 8, 8.5, 12);
      ctx.bezierCurveTo(8.5, 16, 9.5, 20, 12, 22); ctx.stroke();
      break;
    case "move": // arrows
      ctx.moveTo(5, 9); ctx.lineTo(2, 12); ctx.lineTo(5, 15); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(9, 5); ctx.lineTo(12, 2); ctx.lineTo(15, 5); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(15, 19); ctx.lineTo(12, 22); ctx.lineTo(9, 19); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(19, 9); ctx.lineTo(22, 12); ctx.lineTo(19, 15); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(2, 12); ctx.lineTo(22, 12); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(12, 2); ctx.lineTo(12, 22); ctx.stroke();
      break;
    default: // wrench
      ctx.moveTo(14.7, 6.3);
      ctx.quadraticCurveTo(14.7, 7, 14.7, 7.7);
      ctx.lineTo(16.3, 9.3);
      ctx.quadraticCurveTo(17, 9.3, 17.7, 9.3);
      ctx.lineTo(21.47, 5.53);
      ctx.stroke();
      ctx.beginPath();
      ctx.arc(8.5, 15.5, 5, 0, Math.PI * 2); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(6.5, 13.5); ctx.lineTo(3.5, 10.5); ctx.stroke();
      break;
  }
  ctx.restore();
}

// ─── Block types ─────────────────────────────────────────────────────────────

interface TextLine { text: string; x: number; y: number; w: number; offset: number; fs: number; lh: number; bold?: boolean; italic?: boolean; code?: boolean; link?: string; charX?: number[]; sepAfter?: string;
  /** markdown 表格单元格坐标（行/列）：复制时按可视行重组成表格结构 */
  tRow?: number; tCol?: number; }
interface Block {
  kind: string; id: number; groupIdx: number;
  x: number; y: number; w: number; h: number;
  text?: string; textLines?: TextLine[];
  segments?: TextSegment[];
  mdBlocks?: MdBlock[];
  color?: string; bg?: string; border?: string;
  borderRadius?: number | number[];
  font?: string; fontSize?: number; fontWeight?: string; lineHeight?: number;
  clickAction?: () => void; hoverBg?: string; hoverColor?: string; hoverKey?: string;
  cursor?: string; selectable?: boolean;
  /** 原生 title 提示（如 token 明细） */
  title?: string;
  data?: Record<string, unknown>;
  _lines?: string[];
  /** 与 _lines 一一对应的行尾连接符（复制用）："\n" / " " / "" */
  _lineSeps?: string[];
  _wrapped?: WrappedLine[];
  _charStyles?: Array<{ bold?: boolean; italic?: boolean; code?: boolean; link?: string }>;
  _charXs?: number[][];
  _lineWidths?: number[];
  /** textLines 构建缓存：内容不变时跨帧复用，避免每帧全量 measure+分配。
   *  行 y 不含 clipped 内滚偏移，内滚时无需重建，命中/选区处再减去 bScroll。 */
  _textLines?: TextLine[];
}

// ─── Selection ───────────────────────────────────────────────────────────────

interface Selection {
  startBlock: number; startOffset: number;
  endBlock: number; endOffset: number;
}

function selectionText(blocks: Block[], sel: Selection): string {
  const forward =
    sel.startBlock < sel.endBlock ||
    (sel.startBlock === sel.endBlock && sel.startOffset <= sel.endOffset);
  const from = forward ? sel.startBlock : sel.endBlock;
  const to = forward ? sel.endBlock : sel.startBlock;
  const fromOff = forward ? sel.startOffset : sel.endOffset;
  const toOff = forward ? sel.endOffset : sel.startOffset;
  const parts: string[] = [];
  for (let i = from; i <= to; i++) {
    const b = blocks[i];
    if (!b?.selectable) continue;
    const sOff = i === from ? fromOff : 0;
    const eOff = i === to ? toOff : Number.POSITIVE_INFINITY;
    // 表格：按可视行重组单元格，输出行内 " | "、行间 "\n" 的表格结构
    if (b.kind === "md-table" && b.textLines?.length) {
      const t = tableSelectionText(b, sOff, eOff);
      if (t) parts.push(t);
      continue;
    }
    // 选区 offset 来自 textLines（含 wrap/trim），不能直接 slice 原始 b.text
    if (b.textLines?.length) {
      const picked: { text: string; sep: string }[] = [];
      for (const ln of b.textLines) {
        const lineEnd = ln.offset + ln.text.length;
        if (eOff <= ln.offset || sOff >= lineEnd) continue;
        const a = Math.max(0, sOff - ln.offset);
        const c = Math.min(ln.text.length, eOff - ln.offset);
        picked.push({ text: ln.text.slice(a, c), sep: ln.sepAfter ?? "\n" });
      }
      if (picked.length) {
        // 软换行只是视觉折行：按行尾连接符拼接，词内折行直接相连，不插入换行
        let joined = "";
        for (let pi = 0; pi < picked.length; pi++) {
          joined += picked[pi].text;
          if (pi < picked.length - 1) joined += picked[pi].sep;
        }
        parts.push(joined);
      }
    } else if (b.text) {
      parts.push(b.text.slice(sOff, Math.min(eOff, b.text.length)));
    }
  }
  return parts.join("\n");
}

/** 表格选区复制：按可视行重组单元格。同一行单元格用 " | " 连接、行间换行；
 *  选区包含表头且跨行时输出 GitHub 风格 markdown 表格（含 | --- | 分隔行）。 */
function tableSelectionText(b: Block, sOff: number, eOff: number): string {
  const cells: { row: number; col: number; pieces: { text: string; sep: string }[] }[] = [];
  for (const ln of b.textLines!) {
    const lineEnd = ln.offset + ln.text.length;
    if (eOff <= ln.offset || sOff >= lineEnd) continue;
    const a = Math.max(0, sOff - ln.offset);
    const c = Math.min(ln.text.length, eOff - ln.offset);
    const row = ln.tRow ?? 0;
    const col = ln.tCol ?? 0;
    // textLines 按 行→列→折行 顺序生成，同一单元格的折行必然相邻
    let cell = cells[cells.length - 1];
    if (!cell || cell.row !== row || cell.col !== col) {
      cell = { row, col, pieces: [] };
      cells.push(cell);
    }
    cell.pieces.push({ text: ln.text.slice(a, c), sep: ln.sepAfter ?? " " });
  }
  const rowMap = new Map<number, string[]>();
  for (const cell of cells) {
    let txt = "";
    for (let pi = 0; pi < cell.pieces.length; pi++) {
      txt += cell.pieces[pi].text;
      // 单元格内折行按行尾连接符拼接（词内折行直接相连，空白折行补空格）
      if (pi < cell.pieces.length - 1) txt += cell.pieces[pi].sep;
    }
    if (!txt) continue;
    const arr = rowMap.get(cell.row) ?? [];
    arr.push(txt);
    rowMap.set(cell.row, arr);
  }
  const rowIdxs = [...rowMap.keys()].sort((x, y) => x - y);
  if (!rowIdxs.length) return "";
  const withHeader = rowIdxs[0] === 0 && rowIdxs.length > 1;
  const lines = rowIdxs.map((r) => {
    const joined = rowMap.get(r)!.join(" | ");
    return withHeader ? `| ${joined} |` : joined;
  });
  if (withHeader) {
    const cols = rowMap.get(0)!.length;
    lines.splice(1, 0, `| ${new Array(cols).fill("---").join(" | ")} |`);
  }
  return lines.join("\n");
}

function blockScrollKey(b: Block): string {
  return `${b.kind}:${b.id}:${b.text?.length ?? 0}:${(b.data?.fullH as number) ?? b.h}`;
}

/** 双击选词的字符分类：空白 / 词字符（字母数字下划线及 CJK）/ 标点。
 *  与浏览器一致，双击选中光标周围同类字符的连续串。 */
function wordClass(ch: string): number {
  if (/\s/.test(ch)) return 0;
  if (/[\w\u3400-\u9fff\uf900-\ufaff]/.test(ch)) return 1;
  return 2;
}

/** 中文分词：Intl.Segmenter（Chromium/WebKit 均支持），不可用时回退 null。 */
let _wordSegmenter: Intl.Segmenter | null | undefined;
function wordSegmenter(): Intl.Segmenter | null {
  if (_wordSegmenter === undefined) {
    try { _wordSegmenter = new Intl.Segmenter("zh", { granularity: "word" }); }
    catch { _wordSegmenter = null; }
  }
  return _wordSegmenter;
}

/** 用 Intl.Segmenter 求 text 中 idx 所在的词区间；idx 不在词段内（空白/标点）或分词器
 *  不可用时返回 null，调用方回退到字符类扩展。 */
function segmentWordRange(text: string, idx: number): { s: number; e: number } | null {
  const seg = wordSegmenter();
  if (!seg) return null;
  for (const part of seg.segment(text)) {
    const st = part.index;
    const en = st + part.segment.length;
    if (idx < st) break;
    if (idx < en) return part.isWordLike ? { s: st, e: en } : null;
  }
  return null;
}

/** 定位 offset 所在的视觉行（offset 落在行尾与下一行行首之间时归前一行）。 */
function lineAtOffset(b: Block, offset: number): TextLine | null {
  const lines = b.textLines;
  if (!lines?.length) return null;
  for (const ln of lines) {
    if (offset >= ln.offset && offset <= ln.offset + ln.text.length) return ln;
  }
  return offset < lines[0].offset ? lines[0] : lines[lines.length - 1];
}

// ─── Main component ──────────────────────────────────────────────────────────

export function CanvasTranscript(props: CanvasTranscriptProps) {
  let canvasEl!: HTMLCanvasElement;
  let backdropCanvasEl!: HTMLCanvasElement;
  let hostEl!: HTMLDivElement;

  // state
  let pal = readPalette();
  let blocks: Block[] = [];
  let totalHeight = 0;
  let scrollY = 0;
  let maxScroll = 0;
  let viewW = 0, viewH = 0;
  let dpr = devicePixelRatio || 1;
  let keepBottom = true;
  /** Expand/collapse near bottom: keep the clicked header fixed in view instead of stick-to-bottom. */
  let scrollLock: { kind: string; id: number; viewOffset: number } | null = null;

  // hover state
  let hoverBlockIdx = -1;
  let cursorStyle = "default";
  /** code-copy-btn feedback: hoverKey → hide-after timestamp */
  const copiedCodeUntil = new Map<string, number>();
  const shownText = new Map<number, string>();
  const targetText = new Map<number, string>();
  const revealReadyAt = new Map<number, number>();
  const revealRemainders = new Map<number, number>();
  let revealRaf = 0;
  let lastRevealAt = performance.now();
  let revealsInitialized = false;

  const visibleText = (item: { id: number; text: string }) => {
    if (!targetText.has(item.id)) return item.text;
    const shown = shownText.get(item.id);
    return shown !== undefined && item.text.startsWith(shown) ? shown : item.text;
  };

  const revealFrame = (now: number) => {
    revealRaf = 0;
    let changed = false;
    let pending = false;
    for (const [id, target] of targetText) {
      const current = shownText.get(id) ?? target;
      if (now < (revealReadyAt.get(id) ?? 0)) {
        pending = current.length < target.length;
        continue;
      }
      const next = advanceStreamText(
        current,
        target,
        now - lastRevealAt,
        revealRemainders.get(id) ?? 0,
      );
      revealRemainders.set(id, next.remainder);
      if (next.text !== current) {
        shownText.set(id, next.text);
        changed = true;
      }
      if (next.text.length < target.length) pending = true;
    }
    lastRevealAt = now;
    if (changed) scheduleRebuild(false, true);
    if (pending) revealRaf = requestAnimationFrame(revealFrame);
  };

  const syncRevealTargets = (showExisting: boolean) => {
    targetText.clear();
    const now = performance.now();
    const latest = latestStreamTextItem(props.groups, props.running);
    if (showExisting) {
      if (revealRaf) cancelAnimationFrame(revealRaf);
      revealRaf = 0;
      shownText.clear();
      revealReadyAt.clear();
      revealRemainders.clear();
    }
    if (latest) {
      targetText.set(latest.id, latest.text);
      if (!shownText.has(latest.id)) {
        shownText.set(latest.id, showExisting ? latest.text : "");
        revealReadyAt.set(latest.id, showExisting ? 0 : now + STREAM_PREBUFFER_MS);
        revealRemainders.set(latest.id, 0);
      }
    }
    for (const id of [...shownText.keys()]) {
      if (targetText.has(id)) continue;
      shownText.delete(id);
      revealReadyAt.delete(id);
      revealRemainders.delete(id);
    }
    const needsReveal = [...targetText].some(([id, target]) =>
      target.startsWith(shownText.get(id) ?? target) &&
      target.length > (shownText.get(id)?.length ?? target.length),
    );
    if (needsReveal && !revealRaf) {
      lastRevealAt = now;
      revealRaf = requestAnimationFrame(revealFrame);
    }
  };

  // per-block scroll for clipped tool-content（按内容身份记，避免 rebuild 丢位置）
  const blockScrolls = new Map<string, number>();
  let rebuildRaf = 0;
  let rebuildTimer: number | undefined;
  let rebuildAfterPaint = false;
  let lastRebuildAt = 0;
  // 流式事件可能高于屏幕刷新率；Markdown 解析/换行无需跟着每个 token 同步执行。
  const STREAM_LAYOUT_INTERVAL_MS = 80;
  interface PrefixLayoutCache {
    /** 宽度+配色签名：任一变化则整份缓存作废 */
    meta: string;
    /** 每个已闭合分组的签名（同 closedGroupSig），用于求最长可复用前缀 */
    sigs: string[];
    blocks: Block[];
    height: number;
    groupYs: number[];
    until: number;
  }
  const prefixLayoutCaches = new Map<string, PrefixLayoutCache>();
  const PREFIX_CACHE_LIMIT = 10;
  // groupItems 会保留已闭合分组的对象身份；缓存其内容签名，避免每个流式 token
  // 都重新遍历整段历史文本。展开状态变化时会整体换新此 WeakMap。
  let closedGroupSigCache = new WeakMap<Group, string>();
  let layoutGeneration = 0;
  let renderedThreadId = props.threadId;
  let waitingForInitialSnapshot = props.loading && props.groups.length === 0;

  // selection state
  let selection: Selection | null = null;
  let selecting = false;
  let selStart: { block: number; offset: number } | null = null;
  /** 本次按压是否拖出了非空选区：用于 click 时区分点选与拖选 */
  let selMoved = false;
  /** 连击计数（双击选词 / 三击选行）：按时间间隔与位置抖动判定 */
  let lastClick = { t: 0, count: 0, x: 0, y: 0 };

  // busy spinner 只重绘自身所在的工具头，不再以 20fps 光栅化整个视口。
  let busyTimer: number | undefined;
  const busyBlockIndices: number[] = [];

  // custom scrollbar drag (drawn on canvas; not a DOM control)
  let scrollDragging = false;
  let scrollDragGrab = 0;
  let blockScrollDrag: { key: string; grab: number } | null = null;


  // spinner
  let spinPhase = 0;

  // editing
  const [editing, setEditing] = createSignal<UserItem | null>(null);
  const [editStyle, setEditStyle] = createSignal<Record<string, string>>({});
  const [draft, setDraft] = createSignal("");
  const editAttachments = createImageAttachments();
  let editHostEl: HTMLDivElement | undefined;
  let editResizeObserver: ResizeObserver | undefined;

  // group Y positions for scrollToGroup
  let groupYs: number[] = [];

  // images cache
  const imgCache = new Map<string, HTMLImageElement>();

  // ─── Layout ────────────────────────────────────────────────────────────────

  let editLayoutY = 0, editLayoutSide = 0, editLayoutW = 0, editLayoutH = 150;

  function editRowCount(text: string): number {
    return Math.min(10, Math.max(2, (text.match(/\n/g)?.length ?? 0) + 1));
  }

  function estimateEditHeight(text: string, imageCount: number): number {
    // Match .canvas-prompt-editor chrome: pad 12*2, gap 10, actions ~32, optional image strip.
    const rows = editRowCount(text);
    const textH = Math.min(240, rows * 14 * 1.6);
    const imgH = imageCount > 0 ? 52 : 0;
    const gaps = 10 + (imageCount > 0 ? 10 : 0);
    return Math.ceil(24 + imgH + textH + 32 + gaps);
  }

  function applyEditStyle() {
    if (!editing()) return;
    setEditStyle({
      left: `${editLayoutSide}px`,
      top: `${Math.max(8, editLayoutY - scrollY)}px`,
      width: `${editLayoutW}px`,
    });
  }

  function syncEditSlotHeight() {
    const el = editHostEl;
    if (!el || !editing()) return;
    const h = Math.ceil(el.getBoundingClientRect().height);
    if (h > 0 && Math.abs(h - editLayoutH) > 1) {
      editLayoutH = h;
      scheduleRebuild();
    }
  }

  function bindEditHost(el: HTMLDivElement) {
    editHostEl = el;
    editResizeObserver?.disconnect();
    editResizeObserver = new ResizeObserver(() => syncEditSlotHeight());
    editResizeObserver.observe(el);
    queueMicrotask(() => syncEditSlotHeight());
  }

  function clearEditing() {
    editResizeObserver?.disconnect();
    editResizeObserver = undefined;
    editHostEl = undefined;
    editLayoutH = 150;
    setEditing(null);
  }

  function userImagesSig(images: PromptImage[] | undefined): string {
    if (!images?.length) return "0";
    return images.map((img) => {
      if (!img.mimeType.startsWith("image/")) return `file:${img.name}`;
      const el = imgCache.get(promptImageSrc(img));
      const loaded = !!(el as unknown as { _loaded?: boolean } | undefined)?._loaded;
      if (!el || !loaded || !el.naturalWidth || !el.naturalHeight) return "pending";
      // Include natural size so layout cache invalidates when aspect-correct size is known.
      return `${el.naturalWidth}x${el.naturalHeight}`;
    }).join(",");
  }

  function textSig(text: string): string {
    // Keep cache checks bounded: scanning every character here blocks session switches
    // before the chunked layout has a chance to yield to the browser.
    let hash = 2166136261;
    const sampleCount = Math.min(64, text.length);
    for (let i = 0; i < sampleCount; i++) {
      const index = sampleCount === text.length
        ? i
        : Math.floor(i * (text.length - 1) / Math.max(1, sampleCount - 1));
      hash ^= text.charCodeAt(index);
      hash = Math.imul(hash, 16777619);
    }
    return `${text.length}:${hash >>> 0}`;
  }

  function bodyExpandedFor(items: Item[]): boolean {
    return items.some((it) => {
      if (it.type === "tool") return !!state.expanded[`tool-${it.id}`];
      if (it.type === "thought") return !!state.expanded[`thought-${it.id}`];
      return !!state.expanded[String(it.id)];
    });
  }

  function closedGroupSig(g: Group): string {
    // foldKey / open must match layout — a narrower key or ignoring bodyExpanded
    // lets prefix cache reuse an expanded fold after the user collapsed it.
    const foldKey = g.turn
      ? `turn-${g.turn.id ?? g.user?.id ?? g.body[0]?.id ?? 0}`
      : "";
    const foldOpen = foldKey
      ? !!(state.expanded[foldKey] ?? bodyExpandedFor(g.body))
      : false;
    const parts = [
      g.user ? `u:${g.user.id}:${textSig(g.user.text)}:${userImagesSig(g.user.images)}` : "-",
      g.turn
        ? `t:${g.turn.id}:${g.turn.durationMs}:${g.turn.totalTokens ?? ""}:${g.turn.actualModel ?? ""}:${foldOpen}`
        : "-",
    ];
    for (const item of g.body) {
      if (item.type === "tool") {
        parts.push(
          `tool:${item.id}:${item.status}:${item.title}:${item.content.length}:${item.locations.length}:${item.rawInput !== undefined}:${JSON.stringify(item.rawOutput)}:${!!state.expanded[`tool-${item.id}`]}`,
        );
      } else if ("text" in item) {
        parts.push(
          `${item.type}:${item.id}:${textSig((item as { text: string }).text)}:${!!state.expanded[`thought-${item.id}`]}`,
        );
      } else {
        parts.push(`${item.type}:${item.id}`);
      }
    }
    return parts.join("|");
  }

  function cachedClosedGroupSig(g: Group): string {
    const cached = closedGroupSigCache.get(g);
    if (cached !== undefined) return cached;
    const sig = closedGroupSig(g);
    closedGroupSigCache.set(g, sig);
    return sig;
  }

  async function computeLayout(generation: number): Promise<boolean> {
    const p = pal;
    const W = viewW;
    const groups = props.groups;
    const running = props.running;
    const threadId = props.threadId;
    // CSS vw unit = window.innerWidth, not element width
    const vw = window.innerWidth;
    // .chat-foot: padding: 10px clamp(14px, 3vw, 24px) 16px
    // max-width: clamp(720px, 78vw, 980px); margin: 0 auto; box-sizing: border-box
    // +10px 左右内边距，避免绘图主体贴边
    const pad = Math.max(14, Math.min(24, vw * 0.03)) + 10;
    const boxW = Math.min(W, Math.min(980, Math.max(720, vw * 0.78)));
    const side = Math.max(0, (W - boxW) / 2) + pad;
    const contentW = boxW - pad * 2;
    const proseW = contentW;
    const proseOff = 0;
    const result: Block[] = [];
    const nextGroupYs: number[] = [];
    let y = 24;
    let chunkStartedAt = performance.now();
    // DOM adjacent vertical margins collapse; canvas must emulate or user bubbles
    // stack 20+16 gaps and look full of blank space between short prompts.
    let pendingBottom = 0;
    const gapBefore = (top: number) => {
      y += Math.max(pendingBottom, top);
      pendingBottom = 0;
    };
    const setBottom = (bottom: number) => { pendingBottom = bottom; };
    const flushBottom = () => { y += pendingBottom; pendingBottom = 0; };

    if (!groups.length) {
      result.push({ kind: "hint", id: 0, groupIdx: 0, x: side, y, w: contentW, h: 60,
        text: props.loading ? "正在加载会话…" : props.emptyHint,
        color: p.faint, fontSize: 13, font: p.sans, selectable: !props.loading });
      y += 60;
      if (generation !== layoutGeneration) return false;
      groupYs = nextGroupYs;
      blocks = result;
      totalHeight = y + 16;
      return true;
    }

    // 已闭合轮次布局缓存：流式输出时只重算尾部，大幅降低每帧布局成本。
    // 编辑中的用户消息所在组及其之后不能复用缓存，否则会叠画旧气泡且占位高度不准。
    let closedUntil = 0;
    const closedSigs: string[] = [];
    const editingId = editing()?.id ?? null;
    for (let i = 0; i < groups.length; i++) {
      if (!groups[i].turn) break;
      if (editingId != null && groups[i].user?.id === editingId) break;
      closedSigs.push(cachedClosedGroupSig(groups[i]));
      closedUntil = i + 1;
    }
    // 逐组比对签名，取最长连续匹配前缀复用：展开/收起中间某个工具不再整份作废，
    // 只从该分组起重排（此前签名是全量拼接、一处变化全部重排，展开后长时间卡顿）
    const metaSig = `${Math.round(W)}|${p.bg}|${p.text}`;
    const cacheKey = threadId ?? "";
    const prefixCache = prefixLayoutCaches.get(cacheKey);
    let reuseUntil = 0;
    if (prefixCache && prefixCache.meta === metaSig) {
      const n = Math.min(closedUntil, prefixCache.sigs.length);
      while (reuseUntil < n && prefixCache.sigs[reuseUntil] === closedSigs[reuseUntil]) {
        reuseUntil++;
      }
    }
    let gi = 0;
    if (reuseUntil > 0 && prefixCache) {
      for (const b of prefixCache.blocks) {
        if (b.groupIdx >= reuseUntil) break;
        result.push(b);
      }
      nextGroupYs.push(...prefixCache.groupYs.slice(0, reuseUntil));
      y = prefixCache.groupYs[reuseUntil] ?? prefixCache.height;
      gi = reuseUntil;
    }

    for (; gi < groups.length; gi++) {
      const g = groups[gi];
      nextGroupYs.push(y);
      const active = running && !g.turn;

      // user message: .msg-user margin 20px 0 16px; bubble max-width 85%
      if (g.user) {
        const item = g.user;
        gapBefore(20); // top margin (collapses with previous bottom)
        if (item.id !== editing()?.id) {
          const maxBubble = contentW * 0.85;
          // DOM .bubble-images: flex-wrap, img max 240×180, gap 6, margin-bottom 6
          const { layouts: imageLayouts, usedW: imgUsedW, stackH: imgH } =
            layoutBubbleImages(item.images, maxBubble - 32, loadImage);
          // Size to content like DOM (no artificial min-width that leaves empty bubble space).
          // Re-wrap at the final inner width so height matches painted lines.
          const lh = 14 * 1.6;
          let textLines = item.text ? wrapText(item.text, maxBubble - 34, 14, p.sans) : [];
          let textW = textLines.reduce((m, l) => Math.max(m, measure(l, 14, p.sans)), 0);
          let bubbleW = Math.min(maxBubble, Math.max(textW + 34, imgUsedW + 32, imgH ? 72 : 34));
          if (item.text) {
            textLines = wrapText(item.text, Math.max(1, bubbleW - 34), 14, p.sans);
            textW = textLines.reduce((m, l) => Math.max(m, measure(l, 14, p.sans)), 0);
            bubbleW = Math.min(maxBubble, Math.max(textW + 34, imgUsedW + 32, imgH ? 72 : 34));
          }
          const textH = textLines.length * lh;
          const bubbleH = Math.max(30, textH + imgH + 20); // padding 10*2
          const bx = side + contentW - bubbleW;

          result.push({ kind: "user-bubble", id: item.id, groupIdx: gi,
            x: bx, y, w: bubbleW, h: bubbleH,
            text: item.text, color: p.text, bg: p.accentDim,
            border: `color-mix(in srgb, ${p.accent} 26%, transparent)`,
            // border-radius: 14px; border-bottom-right-radius: 6px
            borderRadius: [14, 14, 6, 14], fontSize: 14, lineHeight: 1.6, font: p.sans,
            selectable: true, hoverKey: `user-${item.id}`,
            _lines: textLines,
            data: { images: item.images, editItem: item, imageLayouts } });

          // .user-edit-btn: padding 5px, margin 0 2px 4px 0, align-self flex-end
          // 世界线预览是静态快照，即使当前主线仍在运行，也应允许从历史消息编辑并分叉。
          // 使用父组件传入的有效 running 状态，避免直接读取主线状态把预览中的编辑入口隐藏。
          if (!props.running) {
            result.push({ kind: "edit-btn", id: item.id, groupIdx: gi,
              x: bx - 28, y: y + bubbleH - 26 - 4, w: 24, h: 24,
              hoverBg: p.hover, borderRadius: 6, cursor: "pointer",
              hoverKey: `user-${item.id}`,
              clickAction: () => {
                setDraft(item.text);
                editAttachments.set(item.images ?? []);
                editLayoutH = estimateEditHeight(item.text, item.images?.length ?? 0);
                setEditing(item);
              } });
          }
          y += bubbleH;
          setBottom(16); // bottom margin
        } else {
          editLayoutY = y;
          editLayoutSide = side;
          editLayoutW = contentW;
          y += editLayoutH;
          setBottom(16);
        }
      }

      // .turn-actual-model: margin -8px 0 8px auto; padding 2px 7px; border-radius 999
      if (g.turn?.actualModel) {
        flushBottom();
        const tag = `实际模型：${g.turn.actualModel}`;
        const tw = measure(tag, 11, p.mono) + 14; // padding 2*7
        result.push({ kind: "actual-model", id: g.turn.id, groupIdx: gi,
          x: side + contentW - tw, y: y - 8, w: tw, h: 18,
          text: tag, color: p.faint, bg: p.panel, border: p.borderLight,
          borderRadius: 999, fontSize: 11, font: p.mono, selectable: true,
          data: { padX: 7, padY: 2 } });
        y += 18; // -8 top absorbed + 8 bottom + content ~18
      }

      // Match DOM (TurnGroup): only split conclusion after the turn is finalized.
      // Mid-stream progress assistant/system lines must stay in original order.
      let process = g.body;
      let conclusion: typeof g.body = [];
      if (g.turn) {
        const lastConc = g.body.findLastIndex(it => it.type === "assistant" || it.type === "system");
        let firstConc = lastConc;
        if (firstConc >= 0) {
          while (firstConc > 0 && (g.body[firstConc - 1].type === "assistant" || g.body[firstConc - 1].type === "system"))
            firstConc--;
        }
        if (firstConc >= 0) {
          process = [...g.body.slice(0, firstConc), ...g.body.slice(lastConc + 1)];
          conclusion = g.body.slice(firstConc, lastConc + 1);
        }
      }

      // Body content follows: commit user bottom margin (don't defer past fold/assistant).
      if ((g.turn && process.length) || process.length > 0 || conclusion.length > 0) flushBottom();

      if (g.turn && process.length) {
        const foldKey = `turn-${g.turn.id ?? g.user?.id ?? process[0]?.id ?? 0}`;
        const open = state.expanded[foldKey] ?? bodyExpandedFor(process);
        const label = ["已处理", fmtDuration(g.turn.durationMs),
          g.turn.totalTokens ? `· ${fmtTokens(g.turn.totalTokens)} tokens` : ""].filter(Boolean).join(" ");
        const tokenTip = turnTokenTitle(g.turn);

        // .turn-fold: padding 4px 8px; margin 12px 0 2px -8px; font 13; gap 6
        const foldW = measure(label, 13, p.sans) + 8 + 6 + 12 + 8; // padL + gap + chev + padR
        y += 12;
        result.push({ kind: "fold", id: g.turn.id, groupIdx: gi,
          x: side + proseOff - 8, y, w: Math.min(400, foldW), h: 26,
          text: label, color: p.dim, fontSize: 13, font: p.sans,
          hoverBg: p.hover, hoverColor: p.text, borderRadius: 7, cursor: "pointer",
          title: tokenTip,
          data: { open, foldKey },
          // Read live open — cached blocks must not toggle with a stale layout-time flag.
          clickAction: () => {
            const cur = state.expanded[foldKey] ?? bodyExpandedFor(process);
            toggleExpanded(foldKey, !cur);
          } });
        y += 26 + 2;

        if (open) {
          // .turn-process: margin 4px 0 6px; padding 2px 0 2px 12px; border-left 2px
          y += 4;
          const processStartY = y + 2;
          const processPadLeft = 12;
          const borderX = side + proseOff;
          let processY = processStartY;
          for (const item of process) {
            processY = layoutItem(item, result, gi, side, proseOff + processPadLeft, contentW - processPadLeft, proseW - processPadLeft, processY, false);
          }
          if (processY > processStartY) {
            result.push({ kind: "process-border", id: 0, groupIdx: gi,
              x: borderX, y: processStartY, w: 2, h: processY - processStartY,
              bg: p.border });
          }
          y = processY + 2 + 6;
        }
      } else {
        for (const item of process) {
          const isActive = active && item.id === (process[process.length - 1]?.id);
          y = layoutItem(item, result, gi, side, proseOff, contentW, proseW, y, isActive);
        }
      }

      for (const item of conclusion) {
        y = layoutItem(item, result, gi, side, proseOff, contentW, proseW, y, false);
      }

      // Keep each frame responsive while laying out a previously unseen long thread.
      if (gi + 1 < groups.length && performance.now() - chunkStartedAt >= 8) {
        await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
        if (generation !== layoutGeneration || props.threadId !== threadId) return false;
        chunkStartedAt = performance.now();
      }
    }

    flushBottom();
    if (generation !== layoutGeneration || props.threadId !== threadId) return false;
    groupYs = nextGroupYs;
    blocks = result;
    totalHeight = y + 16;

    if (closedUntil > 0 && reuseUntil < closedUntil && cacheKey) {
      prefixLayoutCaches.delete(cacheKey);
      prefixLayoutCaches.set(cacheKey, {
        meta: metaSig,
        sigs: closedSigs,
        until: closedUntil,
        blocks: result.filter((b) => b.groupIdx < closedUntil),
        groupYs: nextGroupYs.slice(0, closedUntil),
        height: closedUntil < groups.length ? nextGroupYs[closedUntil] : y,
      });
      while (prefixLayoutCaches.size > PREFIX_CACHE_LIMIT) {
        const oldest = prefixLayoutCaches.keys().next().value;
        if (oldest == null) break;
        prefixLayoutCaches.delete(oldest);
      }
    }
    return true;
  }

  function pushMdCodeBlock(
    result: Block[],
    opts: {
      id: number; groupIdx: number;
      x: number; y: number; w: number; h: number;
      text: string; color: string; bg: string; border: string;
      borderRadius: number; fontSize: number; lineHeight: number; font: string;
      padX: number; padY: number; lang?: string; hoverKey: string; copyText?: string;
    },
  ) {
    const p = pal;
    result.push({
      kind: "md-code", id: opts.id, groupIdx: opts.groupIdx,
      x: opts.x, y: opts.y, w: opts.w, h: opts.h,
      text: opts.text, color: opts.color, bg: opts.bg, border: opts.border,
      borderRadius: opts.borderRadius, fontSize: opts.fontSize,
      lineHeight: opts.lineHeight, font: opts.font,
      selectable: true, hoverKey: opts.hoverKey,
      data: { padX: opts.padX, padY: opts.padY, lang: opts.lang },
    });
    // Match DOM .code-copy: top/right 7px, padding 5px, icon 13 → ~24px hit target
    const btn = 24;
    const inset = 7;
    const copyKey = opts.hoverKey;
    const codeText = opts.copyText ?? opts.text;
    result.push({
      kind: "code-copy-btn",
      id: opts.id, groupIdx: opts.groupIdx,
      x: opts.x + opts.w - inset - btn,
      y: opts.y + inset,
      w: btn, h: btn,
      bg: p.panel, border: p.borderLight, hoverBg: p.hover,
      borderRadius: 6, cursor: "pointer",
      hoverKey: opts.hoverKey,
      title: "复制",
      data: { copyKey },
      clickAction: () => {
        void navigator.clipboard.writeText(codeText);
        copiedCodeUntil.set(copyKey, performance.now() + 1200);
        requestPaint();
        window.setTimeout(() => {
          const until = copiedCodeUntil.get(copyKey);
          if (until != null && until <= performance.now()) {
            copiedCodeUntil.delete(copyKey);
            requestPaint();
          }
        }, 1220);
      },
    });
  }

  function layoutItem(item: Item, result: Block[], gi: number, pad: number,
    proseOff: number, contentW: number, proseW: number, y: number, active: boolean): number {
    const p = pal;
    const x = pad + proseOff;

    if (item.type === "thought") {
      // .msg-thought: margin 6px 0
      if (item.text === "思考中…") {
        result.push({ kind: "thinking-status", id: item.id, groupIdx: gi,
          x, y: y + 6, w: proseW, h: 22, text: "● 思考中…", color: p.faint, fontSize: 13, font: p.sans });
        return y + 6 + 22 + 6;
      }
      const key = `thought-${item.id}`;
      const open = isExpanded(key, active);
      // .thought-toggle: padding 3px 6px; gap 5; font 12; border-radius 6
      const toggleW = measure("思考过程", 12, p.sans) + 6 + 5 + 12 + 6;
      result.push({ kind: "thought-toggle", id: item.id, groupIdx: gi,
        x, y: y + 6, w: Math.min(200, toggleW), h: 24,
        text: "思考过程", color: p.faint, fontSize: 12, font: p.sans,
        hoverBg: p.hover, hoverColor: p.dim, borderRadius: 6, cursor: "pointer",
        data: { open, key },
        // 未显式设置时，进行中的思考默认展开；点击必须基于同一个默认值取反，
        // 否则首次点击会把 undefined 写成 true，看起来无法收起。
        clickAction: () => { toggleExpanded(key, !isExpanded(key, active)); } });
      y += 6 + 24;
      if (open) {
        // .thought-body: margin-top 6px; padding 8px 14px; border-left 2px border-light
        const text = visibleText(item);
        const mdBl = parseMarkdownBlocks(text);
        let thY = y + 6 + 8; // margin + padding
        let codeIdx = 0;
        for (const mb of mdBl) {
          if (mb.type === "code") {
            const codeText = mb.raw || segmentsPlainText(mb.segments);
            // Match paintCodeBlock: innerW = (proseW - 14) - padX*2
            const codeLines = wrapCodeText(codeText, proseW - 38, 12, p.mono);
            const codeLh = 12 * 1.55;
            const codeH = codeLines.length * codeLh + 20; // padY 10*2
            pushMdCodeBlock(result, {
              id: item.id, groupIdx: gi,
              x: x + 14, y: thY, w: proseW - 14, h: codeH,
              text: codeText, color: p.dim, bg: p.sidebar, border: p.border,
              borderRadius: 6, fontSize: 12, lineHeight: 1.55, font: p.mono,
              padX: 12, padY: 10, copyText: codeText, hoverKey: `md-code-${item.id}-t${codeIdx++}`,
            });
            thY += codeH + 8;
          } else if (mb.type === "table") {
            thY = layoutMdTable(mb, result, item.id, gi, x + 14, thY, proseW - 28, p, {
              color: p.dim, fontSize: 12,
            });
          } else {
            const plain = segmentsPlainText(mb.segments);
            const lines = wrapStyledText(plain, mb.segments, proseW - 28, 13, p.sans, p.mono);
            const lh = 13 * 1.6;
            const pH = lines.length * lh;
            result.push({ kind: "md-paragraph", id: item.id, groupIdx: gi,
              x: x + 14, y: thY, w: proseW - 14, h: pH,
              text: plain, segments: mb.segments, color: p.dim, fontSize: 13,
              lineHeight: 1.6, font: p.sans, selectable: true });
            thY += pH + 6;
          }
        }
        thY += 8; // padding-bottom
        const tbH = thY - (y + 6);
        result.push({ kind: "tool-body-border", id: item.id, groupIdx: gi,
          x, y: y + 6, w: 2, h: tbH, bg: p.borderLight });
        y = thY;
      }
      return y + 6;
    }

    if (item.type === "tool") {
      return layoutTool(item, result, gi, x, proseW, y, active);
    }

    if (item.type === "assistant") {
      const text = visibleText(item);
      const trimmed = text.trim();
      // Skip empty / placeholder replies so they don't leave blank gaps between user prompts.
      if (!trimmed || trimmed === "None") return y;
      // .msg-assistant: margin 14px 0; line-height 1.7
      y += 14;
      const mdBlocks = parseMarkdownBlocks(text);
      for (let mi = 0; mi < mdBlocks.length; mi++) {
        const mb = mdBlocks[mi];
        if (mb.type === "hr") {
          result.push({ kind: "md-hr", id: item.id, groupIdx: gi,
            x, y, w: proseW, h: 25 });
          y += 25;
        } else if (mb.type === "code") {
          const codeText = mb.raw || segmentsPlainText(mb.segments);
          const codeLines = wrapCodeText(codeText, proseW - 28, 12.5, p.mono);
          const codeLh = 12.5 * 1.55;
          const codeH = codeLines.length * codeLh + 24; // padding 12*2
          pushMdCodeBlock(result, {
            id: item.id, groupIdx: gi,
            x, y, w: proseW, h: codeH,
            text: codeText, color: p.text, bg: p.panel, border: p.border,
            borderRadius: 8, fontSize: 12.5, lineHeight: 1.55, font: p.mono,
            padX: 14, padY: 12, lang: mb.lang, copyText: codeText,
            hoverKey: `md-code-${item.id}-${mi}`,
          });
          y += codeH + 10;
        } else if (mb.type === "heading") {
          const hLevel = mb.level || 1;
          const hFs = hLevel === 1 ? 17.5 : hLevel === 2 ? 16.1 : 14.7;
          const plain = segmentsPlainText(mb.segments);
          const hLines = wrapStyledText(plain, mb.segments, proseW, hFs, p.sans, p.mono, "bold");
          const hLh = hFs * 1.3;
          const hH = hLines.length * hLh;
          y += 16;
          result.push({ kind: "md-heading", id: item.id, groupIdx: gi,
            x, y, w: proseW, h: hH,
            text: plain, segments: mb.segments, color: p.text, fontSize: hFs,
            fontWeight: "bold", lineHeight: 1.3, font: p.sans, selectable: true });
          y += hH + 8;
        } else if (mb.type === "blockquote") {
          const plain = segmentsPlainText(mb.segments);
          const bLines = wrapStyledText(plain, mb.segments, proseW - 15, 14, p.sans, p.mono);
          const bLh = 14 * 1.6;
          const bH = bLines.length * bLh + 4;
          result.push({ kind: "md-blockquote", id: item.id, groupIdx: gi,
            x, y, w: proseW, h: bH,
            text: plain, segments: mb.segments, color: p.dim, fontSize: 14,
            lineHeight: 1.6, font: p.sans, selectable: true,
            data: { borderLeft: true, borderColor: p.borderLight, padX: 12, padY: 2 } });
          y += bH + 10;
        } else if (mb.type === "list-item") {
          const plain = segmentsPlainText(mb.segments);
          const listDepth = mb.listDepth || 0;
          const prefixOffset = listDepth * 22;
          const indent = prefixOffset + 22;
          const liLines = wrapStyledText(plain, mb.segments, proseW - indent, 14, p.sans, p.mono);
          const liLh = 14 * 1.7;
          const liH = liLines.length * liLh;
          result.push({ kind: "md-list-item", id: item.id, groupIdx: gi,
            x, y, w: proseW, h: liH,
            text: plain, segments: mb.segments, color: p.text, fontSize: 14,
            lineHeight: 1.7, font: p.sans, selectable: true,
            data: { prefix: mb.prefix, prefixOffset, indent } });
          y += liH + 3;
        } else if (mb.type === "table") {
          y = layoutMdTable(mb, result, item.id, gi, x, y, proseW, p);
        } else {
          const plain = segmentsPlainText(mb.segments);
          const pLines = wrapStyledText(plain, mb.segments, proseW, 14, p.sans, p.mono);
          const pLh = 14 * 1.7;
          const pH = pLines.length * pLh;
          result.push({ kind: "md-paragraph", id: item.id, groupIdx: gi,
            x, y, w: proseW, h: pH,
            text: plain, segments: mb.segments, color: p.text, fontSize: 14,
            lineHeight: 1.7, font: p.sans, selectable: true });
          y += pH + 10;
        }
      }
      if (mdBlocks.length && mdBlocks[mdBlocks.length - 1].type === "paragraph") y -= 10;
      return y + 14;
    }

    if (item.type === "system") {
      const important = item.level === "error" || item.level === "warn" || item.level === "info";
      const color = item.level === "error" ? p.red : item.level === "warn" ? p.yellow
        : item.level === "info" ? `color-mix(in srgb, ${p.accent} 80%, ${p.text})` : p.faint;
      const mix = item.level === "error" ? p.red : item.level === "warn" ? p.yellow : p.accent;
      const lines = wrapText(item.text, proseW - 26, 12.5, p.sans);
      const h = lines.length * 12.5 * 1.5 + 16;
      result.push({ kind: "system", id: item.id, groupIdx: gi,
        x, y: y + 10, w: proseW, h,
        text: item.text, color,
        bg: important ? `color-mix(in srgb, ${mix} 8%, transparent)` : undefined,
        border: important ? `color-mix(in srgb, ${mix} 30%, transparent)` : undefined,
        borderRadius: 8, fontSize: 12.5, font: p.sans, selectable: true,
        data: { padX: 13, padY: 8 } });
      return y + 10 + h + 10;
    }

    return y;
  }

  function layoutTool(item: ToolItem, result: Block[], gi: number,
    x: number, proseW: number, y: number, active: boolean): number {
    const p = pal;
    const key = `tool-${item.id}`;
    const busy = item.status === "pending" || item.status === "in_progress";
    const defaultOpen = active || busy;
    const open = isExpanded(key, defaultOpen);
    // 摘要展示优化仅对 Devin 生效（对齐 DOM ToolCallCard）
    const isDevin = state.agentKind === "devin";
    const contentBlocks = isDevin
      ? item.content.filter((block) => !isTrivialToolOutput(item, block))
      : item.content;
    const hasBody = contentBlocks.length > 0 || item.locations.length > 0
      || item.rawInput !== undefined || item.rawOutput !== undefined;

    // Canvas 的 fillText 不会按制表位展开；工具标题（以及 Devin 的独立详情）
    // 可能直接带有命令参数中的真实 tab，先展开后再测量和绘制，避免详情错位。
    const label = expandCodeTabs(displayToolTitle(stripAnsi(item.title || item.kind)));
    const detail = isDevin ? expandCodeTabs(toolHeadlineDetail(item)) : "";
    // .tool-row margin 1px 0; .tool-line padding 3px 8px; min-height 26; gap 8
    const toolH = 26;

    const rawOutput = item.rawOutput;
    const durationValue = typeof rawOutput === "object" && rawOutput !== null
      ? (rawOutput as Record<string, unknown>).durationMs
        ?? (rawOutput as Record<string, unknown>).duration_ms
      : undefined;
    const durationMs = typeof durationValue === "number" && Number.isFinite(durationValue) && durationValue >= 0
      ? durationValue
      : undefined;
    result.push({ kind: "tool-header", id: item.id, groupIdx: gi,
      x, y: y + 1, w: proseW, h: toolH,
      text: label, color: item.status === "failed" ? p.red : p.dim,
      fontSize: 12, font: p.mono, hoverBg: busy ? undefined : p.hover, borderRadius: 7,
      cursor: hasBody ? "pointer" : "default",
      data: {
        open, busy, hasBody, kind: item.kind, status: item.status, detail,
        durationMs, startedAt: item.ts,
      },
      clickAction: hasBody ? () => {
        const liveBusy = item.status === "pending" || item.status === "in_progress";
        toggleExpanded(key, !isExpanded(key, liveBusy));
      } : undefined });
    y += 1 + toolH + 1;

    if (open && hasBody) {
      // .tool-body: margin 4px 0 8px 14px; padding 4px 0 4px 14px; border-left 2px; gap 8
      const bodyX = x + 14;
      const bodyW = proseW - 14;
      const bodyStartY = y + 4;
      let by = bodyStartY + 4; // padding-top 4
      const contentX = bodyX + 14; // padding-left 14
      const contentW = bodyW - 14;

      // locations: gap 6; .loc-chip padding 2px 9px
      if (item.locations.length) {
        let lx = contentX;
        let rowH = 0;
        for (const loc of item.locations) {
          if (!loc.path) continue;
          const name = `${loc.path.split(/[\\/]/).pop() ?? loc.path}${loc.line != null ? `:${loc.line}` : ""}`;
          const chipW = measure(name, 11.5, p.mono) + 18; // padding 2*9
          if (lx + chipW > contentX + contentW && lx > contentX) {
            lx = contentX; by += 24; // chip h ~20 + gap
          }
          result.push({ kind: "tool-location", id: item.id, groupIdx: gi,
            x: lx, y: by, w: chipW, h: 20,
            text: name, color: p.blue, fontSize: 11.5, font: p.mono,
            bg: p.panel, hoverBg: p.hover, borderRadius: 20, cursor: "pointer",
            selectable: false,
            data: { padX: 9, padY: 2, underlineOnHover: true },
            clickAction: () => { /* openFile */ } });
          lx += chipW + 6;
          rowH = 20;
        }
        by += rowH + 8; // gap 8 after locations row
      }

      // content: .tool-output padding 10px; line-height 1.55; max-height 320
      for (const content of contentBlocks) {
        if (content.type === "diff") {
          const diff = content as { type: "diff"; path: string; oldText?: string | null; newText: string };
          const preview = (diff.oldText ?? "").slice(0, 200) + "\n→\n" + diff.newText.slice(0, 200);
          const lines = wrapText(preview, contentW - 20, 12, p.mono);
          const fullH = lines.length * 12 * 1.55 + 20;
          const h = Math.min(320, fullH);
          result.push({ kind: "tool-content", id: item.id, groupIdx: gi,
            x: contentX, y: by, w: contentW, h,
            text: preview, color: p.dim, fontSize: 12, lineHeight: 1.55, font: p.mono,
            bg: p.sidebar, border: p.border, borderRadius: 7, selectable: true,
            data: { padX: 10, padY: 10, fullH, clipped: fullH > 320 } });
          by += h + 8;
        } else if (content.type === "content") {
          const inner = (content as { content: { type?: string; text?: string } }).content;
          if (inner?.type === "text" && inner.text?.trim()) {
            const clean = stripAnsi(inner.text).trim();
            const lines = wrapText(clean, contentW - 20, 12, p.mono);
            const fullH = lines.length * 12 * 1.55 + 20;
            const h = Math.min(320, fullH);
            result.push({ kind: "tool-content", id: item.id, groupIdx: gi,
              x: contentX, y: by, w: contentW, h,
              text: clean, color: p.dim, fontSize: 12, lineHeight: 1.55, font: p.mono,
              bg: p.sidebar, border: p.border, borderRadius: 7, selectable: true,
              data: { padX: 10, padY: 10, fullH, clipped: fullH > 320 } });
            by += h + 8;
          }
        }
      }

      if (by === bodyStartY + 4 && (item.rawInput !== undefined || item.rawOutput !== undefined)) {
        const preview = textPreview(item.rawInput) || textPreview(item.rawOutput);
        if (preview) {
          const lines = wrapText(preview, contentW - 20, 12, p.mono);
          const fullH = lines.length * 12 * 1.55 + 16;
          const h = Math.min(200, fullH);
          result.push({ kind: "tool-content", id: item.id, groupIdx: gi,
            x: contentX, y: by, w: contentW, h,
            text: preview, color: p.dim, fontSize: 12, lineHeight: 1.55, font: p.mono,
            bg: p.sidebar, border: p.border, borderRadius: 7, selectable: true,
            data: { padX: 8, padY: 8, fullH, clipped: fullH > 200 } });
          by += h + 8;
        }
      }

      // remove trailing gap
      if (by > bodyStartY + 4) by -= 8;
      by += 4; // padding-bottom 4

      if (by > bodyStartY) {
        result.push({ kind: "tool-body-border", id: item.id, groupIdx: gi,
          x: bodyX, y: bodyStartY, w: 2, h: by - bodyStartY, bg: p.border });
      }
      y = by + 8; // margin-bottom 8
    }
    return y;
  }

  function textPreview(value: unknown): string {
    if (value == null) return "";
    if (typeof value === "string") return stripAnsi(value).trim().slice(0, 300);
    try { return stripAnsi(JSON.stringify(value, null, 2)).trim().slice(0, 300); } catch { return ""; }
  }

  // ─── Paint ─────────────────────────────────────────────────────────────────

  const snap = (v: number) => Math.round(v * dpr) / dpr;

  function fillTextCrisp(ctx: CanvasRenderingContext2D, text: string, x: number, y: number) {
    ctx.fillText(text, snap(x), snap(y));
  }

  function paintBackdrop(timestamp = Date.now()) {
    const canvas = backdropCanvasEl;
    if (!canvas || viewW <= 0 || viewH <= 0) return;
    const pixelW = Math.max(1, Math.round(viewW * dpr));
    const pixelH = Math.max(1, Math.round(viewH * dpr));
    if (canvas.width !== pixelW || canvas.height !== pixelH) {
      canvas.width = pixelW;
      canvas.height = pixelH;
    }
    const ctx = canvas.getContext("2d", { alpha: false });
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    paintCanvasBackdrop(ctx, viewW, viewH, pal, timestamp, paintBackdrop, () => !props.running);
  }

  function paintAll() {
    const canvas = canvasEl;
    if (!canvas) return;
    // 前景保持透明，只清空并重绘正文；星图背景由下层 canvas 保留，不再随滚动反复拷贝。
    const ctx = canvas.getContext("2d")!;
    const p = pal;

    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.textRendering = "optimizeLegibility";
    ctx.fontKerning = "normal";

    const visTop = scrollY - 50;
    const visBot = scrollY + viewH + 50;
    spinPhase = (performance.now() / 800) % 1;
    busyBlockIndices.length = 0;

    for (let i = 0; i < blocks.length; i++) {
      const b = blocks[i];
      const sy = b.y - scrollY;
      if (sy + b.h < -50 || sy > viewH + 50) continue;

      const bx = b.x, by = sy;

      // hover highlight
      const isHover = (i === hoverBlockIdx) ||
        (b.hoverKey && blocks[hoverBlockIdx]?.hoverKey === b.hoverKey);

      ctx.save();

      // background — hoverBg for interactive blocks; edit/copy btns only visible on hover (or copied)
      if (b.kind === "edit-btn") {
        if (isHover) {
          ctx.fillStyle = b.hoverBg || pal.hover;
          roundRect(ctx, bx, by, b.w, b.h, b.borderRadius || 6);
          ctx.fill();
        }
      } else if (b.kind === "code-copy-btn") {
        const copied = (copiedCodeUntil.get(b.hoverKey || "") || 0) > performance.now();
        if (isHover || copied) {
          const btnHover = i === hoverBlockIdx;
          ctx.fillStyle = btnHover ? (b.hoverBg || pal.hover) : (b.bg || pal.panel);
          roundRect(ctx, bx, by, b.w, b.h, b.borderRadius || 6);
          ctx.fill();
          ctx.strokeStyle = b.border || pal.borderLight;
          ctx.lineWidth = 1;
          roundRect(ctx, bx + 0.5, by + 0.5, b.w - 1, b.h - 1, b.borderRadius || 6);
          ctx.stroke();
        }
      } else {
        const bg = isHover && b.hoverBg ? b.hoverBg : b.bg;
        if (bg) {
          ctx.fillStyle = bg;
          if (b.borderRadius) {
            roundRect(ctx, bx, by, b.w, b.h, b.borderRadius);
            ctx.fill();
          } else {
            ctx.fillRect(bx, by, b.w, b.h);
          }
        }
      }

      // border
      if (b.border && b.kind !== "edit-btn" && b.kind !== "code-copy-btn") {
        ctx.strokeStyle = b.border;
        ctx.lineWidth = 1;
        if (b.borderRadius) {
          roundRect(ctx, bx + 0.5, by + 0.5, b.w - 1, b.h - 1, b.borderRadius);
          ctx.stroke();
        } else {
          ctx.strokeRect(bx + 0.5, by + 0.5, b.w - 1, b.h - 1);
        }
      }

      // kind-specific rendering
      switch (b.kind) {
        case "user-bubble":
          paintUserBubble(ctx, b, bx, by, p, !!isHover);
          break;
        case "edit-btn":
          if (isHover) paintEditIcon(ctx, bx + 5, by + 5, 13, isHover ? p.text : p.faint);
          break;
        case "code-copy-btn": {
          const copied = (copiedCodeUntil.get(b.hoverKey || "") || 0) > performance.now();
          if (isHover || copied) {
            const btnHover = i === hoverBlockIdx;
            const color = copied ? p.accent : (btnHover ? p.text : p.faint);
            if (copied) paintCheckIcon(ctx, bx + 5.5, by + 5.5, 13, color);
            else paintCopyIcon(ctx, bx + 5.5, by + 5.5, 13, color);
          }
          break;
        }
        case "fold":
        case "thought-toggle":
          paintFoldToggle(ctx, b, bx, by, p, !!isHover);
          break;
        case "tool-header":
          if (b.data?.busy) busyBlockIndices.push(i);
          paintToolHeader(ctx, b, bx, by, p);
          break;
        case "md-paragraph":
        case "md-heading":
        case "md-list-item":
        case "md-blockquote":
          paintStyledText(ctx, b, bx, by);
          break;
        case "md-code":
          paintCodeBlock(ctx, b, bx, by, p);
          break;
        case "md-table":
          paintMdTable(ctx, b, bx, by, p);
          break;
        case "md-hr":
          ctx.fillStyle = b.bg || p.border;
          ctx.fillRect(bx, by + 12, b.w, 1);
          break;
        case "thought-body":
        case "system":
        case "hint":
        case "tool-content":
        case "actual-model":
        case "tool-location":
          paintTextBlock(ctx, b, bx, by, !!isHover, i);
          break;
        case "thinking-status":
          ctx.font = `${b.fontSize}px ${b.font}`;
          ctx.fillStyle = b.color!;
          ctx.textBaseline = "middle";
          fillTextCrisp(ctx, b.text!, bx, by + b.h / 2);
          break;
        case "process-border":
        case "tool-body-border":
          ctx.fillStyle = b.bg || p.border;
          ctx.fillRect(bx, by, b.w, b.h);
          break;
      }

      ctx.restore();
    }

    // selection overlay
    if (selection) paintSelection(ctx);

    // scrollbar
    if (maxScroll > 0) paintScrollbar(ctx);

    scheduleBusyPaint();
  }

  function scheduleBusyPaint() {
    if (busyBlockIndices.length === 0 || busyTimer !== undefined) return;
    busyTimer = window.setTimeout(paintBusyIndicators, 50);
  }

  /** 动画帧仅擦除并重画可见的 busy 工具头，正文、图片和选区均不重复光栅化。 */
  function paintBusyIndicators() {
    busyTimer = undefined;
    const canvas = canvasEl;
    if (!canvas || busyBlockIndices.length === 0) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.textRendering = "optimizeLegibility";
    ctx.fontKerning = "normal";
    spinPhase = (performance.now() / 800) % 1;
    let anyBusy = false;

    for (const index of busyBlockIndices) {
      const b = blocks[index];
      if (!b || b.kind !== "tool-header" || !b.data?.busy) continue;
      const by = b.y - scrollY;
      if (by + b.h < -1 || by > viewH + 1) continue;
      anyBusy = true;
      ctx.save();
      ctx.clearRect(b.x, by, b.w, b.h);
      ctx.restore();
      ctx.save();
      paintToolHeader(ctx, b, b.x, by, pal);
      ctx.restore();
    }

    if (anyBusy) scheduleBusyPaint();
  }

  function paintUserBubble(ctx: CanvasRenderingContext2D, b: Block, bx: number, by: number, p: Palette, hover: boolean) {
    const imageLayouts = (b.data?.imageLayouts as BubbleImageLayout[] | undefined) ?? [];
    for (const layout of imageLayouts) {
      const cached = loadImage(layout.img);
      if (!cached) continue;
      // Always draw with aspect-correct size; stale layout slots trigger rebuild.
      const size = bubbleImageSize(cached);
      if (size.w !== layout.w || size.h !== layout.h) scheduleRebuild();
      const ix = bx + layout.dx;
      const iy = by + layout.dy;
      ctx.save();
      roundRect(ctx, ix, iy, size.w, size.h, 10);
      ctx.clip();
      ctx.drawImage(cached, ix, iy, size.w, size.h);
      ctx.restore();
    }

    // text — left-aligned under images (DOM .user-bubble)
    if (b.text) {
      const fs = b.fontSize || 14;
      const ff = b.font || p.sans;
      const lh = fs * (b.lineHeight || 1.6);
      const imgOffset = imageLayouts.length
        ? Math.max(...imageLayouts.map((l) => l.dy + l.h)) - 10 + BUBBLE_IMG_MARGIN_BOTTOM
        : 0;
      ctx.font = `${fs}px ${ff}`;
      ctx.fillStyle = b.color || p.text;
      ctx.textBaseline = "top";
      let lines = b._lines;
      let seps = b._lineSeps;
      if (!lines || !seps) {
        const full = wrapTextFull(b.text || "", b.w - 34, fs, ff);
        lines = full.lines;
        seps = full.seps;
        b._lines = lines;
        b._lineSeps = seps;
      }
      const halfLead = (lh - fs) / 2;
      if (!b._textLines) {
        b._textLines = [];
        let offset = 0;
        for (let i = 0; i < lines.length; i++) {
          const ty = b.y + 10 + imgOffset + i * lh; // 绝对坐标（= 屏幕 ty + scrollY）
          b._textLines.push({ text: lines[i], x: b.x + 16, y: ty, w: measure(lines[i], fs, ff), offset, fs, lh, sepAfter: seps[i] });
          offset += lines[i].length;
        }
      }
      b.textLines = b._textLines;
      for (let i = 0; i < lines.length; i++) {
        fillTextCrisp(ctx, lines[i], bx + 16, by + 10 + imgOffset + i * lh + halfLead);
      }
    }
  }

  function paintFoldToggle(ctx: CanvasRenderingContext2D, b: Block, bx: number, by: number, p: Palette, hover: boolean) {
    const open = b.data?.open as boolean;
    const isThought = b.kind === "thought-toggle";
    // fold: padding 4px 8px, gap 6; thought: padding 3px 6px, gap 5
    // DOM 的已处理行是「文字 → chevron」，思考行仍是「chevron → 文字」。
    const padL = isThought ? 6 : 8;
    const gap = isThought ? 5 : 6;
    const iconSize = 12;
    const color = hover && b.hoverColor ? b.hoverColor : (b.color || p.dim);

    ctx.font = `${b.fontSize}px ${b.font}`;
    ctx.fillStyle = color;
    ctx.textBaseline = "middle";
    const textX = isThought ? bx + padL + iconSize + gap : bx + padL;
    fillTextCrisp(ctx, b.text!, textX, by + b.h / 2);

    const iconX = isThought
      ? bx + padL
      : textX + measure(b.text!, b.fontSize!, b.font!) + gap;
    ctx.save();
    ctx.strokeStyle = isThought ? (hover ? p.dim : p.faint) : p.faint;
    ctx.lineWidth = 2;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    const s = iconSize / 24;
    ctx.translate(iconX, by + (b.h - iconSize) / 2);
    ctx.scale(s, s);
    ctx.beginPath();
    // IconChevron paths: open "m6 9 6 6 6-6" / closed "m9 6 6 6-6 6"
    if (open) { ctx.moveTo(6, 9); ctx.lineTo(12, 15); ctx.lineTo(18, 9); }
    else { ctx.moveTo(9, 6); ctx.lineTo(15, 12); ctx.lineTo(9, 18); }
    ctx.stroke();
    ctx.restore();
  }

  function paintToolHeader(ctx: CanvasRenderingContext2D, b: Block, bx: number, by: number, p: Palette) {
    const { open, busy, hasBody, kind, status, detail, durationMs, startedAt } = b.data as {
      open: boolean; busy: boolean; hasBody: boolean; kind: string; status: string;
      detail?: string; durationMs?: number; startedAt?: number;
    };
    // padding 3px 8px; gap 8 — match DOM .tool-line: icon → title/detail → duration → status → chevron
    const padX = 8;
    const gap = 8;
    const midY = snap(by + b.h / 2);
    const elapsed = durationMs ?? (busy && startedAt != null ? Math.max(0, Date.now() - startedAt) : undefined);
    const durationText = elapsed !== undefined && (durationMs !== undefined || elapsed >= 1000)
      ? elapsed < 1000
        ? `${Math.round(elapsed)}ms`
        : elapsed < 60_000
          ? `${(elapsed / 1000).toFixed(elapsed < 10_000 ? 2 : 1)}s`
          : `${Math.floor(Math.round(elapsed / 1000) / 60)}m ${Math.round(elapsed / 1000) % 60}s`
      : "";
    const durationFontSize = 11.5;
    const durationW = durationText ? measure(durationText, durationFontSize, p.mono) : 0;
    // 运行中预留稳定宽度，避免秒数增长时标题和尾部图标左右跳动。
    const durationReserve = busy ? measure("99m 59s", durationFontSize, p.mono) : durationW;

    // icon (14px) at left
    drawToolIcon(ctx, kind, bx + padX, by + (b.h - 14) / 2, 14, p.faint);

    // label after icon + gap 8; reserve trailing duration/icons so long titles ellipsize
    const textX = bx + padX + 14 + gap;
    let trailReserve = padX;
    if (busy || status === "failed") trailReserve += gap + 12;
    if (hasBody) trailReserve += gap + 12;
    if (durationMs !== undefined || busy) trailReserve += gap + durationReserve;
    ctx.font = `${b.fontSize}px ${b.font}`;
    ctx.fillStyle = b.color || p.dim;
    ctx.textBaseline = "middle";
    const maxTextW = Math.max(20, b.w - (textX - bx) - trailReserve);
    // 有 detail 时标题只占部分宽度，给详情文本留空间（对齐 DOM .tool-headline-detail）
    const labelMaxW = detail ? Math.max(60, Math.floor(maxTextW * 0.45)) : maxTextW;
    const label = ellipsize(b.text!, labelMaxW, b.fontSize!, b.font!);
    fillTextCrisp(ctx, label, textX, midY);

    const labelW = measure(label, b.fontSize!, b.font!);
    let detailEndX = textX + labelW;
    // detail：标题后 gap 8 + 1px 分隔线 + padding 8（对齐 DOM .tool-title + .tool-headline-detail）
    if (detail) {
      const sepX = textX + labelW + gap;
      const detailX = sepX + 1 + 8;
      const detailMaxW = bx + b.w - trailReserve - detailX;
      if (detailMaxW > 24) {
        const text = ellipsize(detail, detailMaxW, b.fontSize!, b.font!);
        ctx.save();
        ctx.strokeStyle = p.border;
        ctx.lineWidth = 1;
        ctx.beginPath();
        const sx = snap(sepX);
        ctx.moveTo(sx, midY - 6);
        ctx.lineTo(sx, midY + 6);
        ctx.stroke();
        ctx.restore();
        ctx.fillStyle = p.faint;
        fillTextCrisp(ctx, text, detailX, midY);
        detailEndX = detailX + measure(text, b.fontSize!, b.font!);
      }
    }

    // trailing duration / status / chevron sit after the visible headline (not flush-right)
    let nextX = detailEndX + gap;
    if (durationText) {
      ctx.font = `${durationFontSize}px ${p.mono}`;
      ctx.fillStyle = p.faint;
      fillTextCrisp(ctx, durationText, nextX, midY);
    }
    if (durationMs !== undefined || busy) nextX += durationReserve + gap;

    if (busy) {
      const cx = nextX + 6;
      const angle = spinPhase * Math.PI * 2;
      ctx.save();
      ctx.strokeStyle = p.blue;
      ctx.lineWidth = 2;
      ctx.globalAlpha = 0.26;
      ctx.beginPath(); ctx.arc(cx, midY, 5, 0, Math.PI * 2); ctx.stroke();
      ctx.globalAlpha = 1;
      ctx.beginPath(); ctx.arc(cx, midY, 5, angle - Math.PI / 2, angle + Math.PI * 0.15); ctx.stroke();
      ctx.restore();
      nextX += 12 + gap;
    } else if (status === "failed") {
      const cx = nextX + 3.5;
      ctx.fillStyle = p.red;
      ctx.beginPath(); ctx.arc(cx, midY, 3.5, 0, Math.PI * 2); ctx.fill();
      nextX += 7 + gap;
    }

    if (hasBody) {
      ctx.save();
      ctx.strokeStyle = p.faint;
      ctx.lineWidth = 1;
      ctx.lineCap = "round";
      ctx.lineJoin = "round";
      ctx.beginPath();
      const chevX = nextX + 6;
      if (open) { ctx.moveTo(chevX - 3, midY - 1.5); ctx.lineTo(chevX, midY + 1.5); ctx.lineTo(chevX + 3, midY - 1.5); }
      else { ctx.moveTo(chevX - 1.5, midY - 3); ctx.lineTo(chevX + 1.5, midY); ctx.lineTo(chevX - 1.5, midY + 3); }
      ctx.stroke();
      ctx.restore();
    }
  }

  function paintTextBlock(ctx: CanvasRenderingContext2D, b: Block, bx: number, by: number, hover: boolean, blockIdx?: number) {
    const fs = b.fontSize || 14;
    const ff = b.font || pal.sans;
    const fw = b.fontWeight || "400";
    const lh = fs * (b.lineHeight || 1.5);
    const halfLead = (lh - fs) / 2;
    const padX = (b.data?.padX as number) ?? (b.bg ? 10 : 0);
    const padY = (b.data?.padY as number) ?? (b.bg ? 8 : 0);
    const clipped = b.data?.clipped as boolean | undefined;
    const bScroll = clipped ? (blockScrolls.get(blockScrollKey(b)) || 0) : 0;

    // left border for thought body
    if (b.data?.borderLeft) {
      ctx.fillStyle = (b.data.borderColor as string) || pal.borderLight;
      ctx.fillRect(bx, by, 2, b.h);
    }

    if (clipped) {
      ctx.save();
      // 圆角 clip 在 Skia 里是抗锯齿蒙版，每个展开的工具块每帧都要付一次；
      // 内容只需硬边界（padding 大于圆角，文本不会渗进圆角区），改用廉价矩形 clip
      ctx.beginPath();
      ctx.rect(bx, by, b.w, b.h);
      ctx.clip();
    }

    if (b.text) {
      ctx.font = `${fw} ${fs}px ${ff}`;
      const color = b.color || pal.text;
      ctx.fillStyle = color;
      ctx.textBaseline = "top";
      const innerX = b.data?.borderLeft ? bx + (padX || 14) : bx + padX;
      const innerW = b.data?.borderLeft ? b.w - (padX || 14) : b.w - padX * 2;
      let lines = b._lines;
      let seps = b._lineSeps;
      if (!lines || !seps) {
        const full = wrapTextFull(b.text || "", Math.max(1, innerW), fs, ff, fw);
        lines = full.lines;
        seps = full.seps;
        b._lines = lines;
        b._lineSeps = seps;
      }
      // textLines 跨帧缓存：行 y 不含内滚偏移（内滚只改 bScroll，不再触发重建）；
      // 大输出几千行时避免每帧全量 measure + 数组分配（滚动卡顿主因之一）
      if (!b._textLines) {
        b._textLines = [];
        let offset = 0;
        for (let i = 0; i < lines.length; i++) {
          const ty = b.y + padY + i * lh; // 绝对坐标（= 屏幕 ty + scrollY + bScroll）
          b._textLines.push({ text: lines[i], x: innerX, y: ty, w: measure(lines[i], fs, ff, fw), offset, fs, lh, sepAfter: seps[i] });
          offset += lines[i].length;
        }
      }
      b.textLines = b._textLines;
      for (let i = 0; i < lines.length; i++) {
        const ty = by + padY + i * lh - bScroll;
        // 块窗口 + 视口双重裁剪，视口外只算坐标不绘制
        if (ty + lh > by - 10 && ty < by + b.h + 10 && ty + lh > -10 && ty < viewH + 10) {
          fillTextCrisp(ctx, lines[i], innerX, ty + halfLead);
          if (hover && b.data?.underlineOnHover) {
            const tw = measure(lines[i], fs, ff, fw);
            ctx.fillRect(innerX, snap(ty + halfLead) + fs, tw, 1);
          }
        }
      }
    }

    if (clipped) {
      ctx.restore();
      const g = blockScrollbarGeom(b);
      if (g) {
        ctx.fillStyle = pal.scroll;
        ctx.globalAlpha = 0.65;
        roundRect(ctx, g.trackX, g.thumbY, g.trackW, g.thumbH, g.trackW / 2);
        ctx.fill();
        ctx.globalAlpha = 1;
      }
    }
  }

  function paintStyledText(ctx: CanvasRenderingContext2D, b: Block, bx: number, by: number) {
    const fs = b.fontSize || 14;
    const ff = b.font || pal.sans;
    const baseFw = b.fontWeight || "400";
    const lh = fs * (b.lineHeight || 1.7);
    const halfLead = (lh - fs) / 2;
    const segments = b.segments;
    const indent = (b.data?.indent as number) || 0;
    const prefix = b.data?.prefix as string | undefined;
    const prefixOffset = (b.data?.prefixOffset as number) || 0;
    const isBq = b.kind === "md-blockquote";

    if (isBq && b.data?.borderLeft) {
      ctx.fillStyle = (b.data.borderColor as string) || pal.borderLight;
      ctx.fillRect(bx, by, 3, b.h);
    }

    const startX = bx + (isBq ? 15 : indent);
    const maxW = b.w - (isBq ? 15 : indent);
    ctx.textBaseline = "top";
    b.textLines = [];

    if (prefix) {
      ctx.font = `${fs}px ${ff}`;
      ctx.fillStyle = b.color || pal.text;
      fillTextCrisp(ctx, prefix, bx + prefixOffset, by + halfLead);
    }

    if (!segments || segments.length === 0) {
      if (b.text) {
        ctx.font = `${baseFw} ${fs}px ${ff}`;
        ctx.fillStyle = b.color || pal.text;
        let lines = b._lines;
        let seps = b._lineSeps;
        if (!lines || !seps) {
          const full = wrapTextFull(b.text, maxW, fs, ff, baseFw);
          lines = full.lines;
          seps = full.seps;
          b._lines = lines;
          b._lineSeps = seps;
        }
        let offset = 0;
        for (let i = 0; i < lines.length; i++) {
          const ty = by + i * lh;
          fillTextCrisp(ctx, lines[i], startX, ty + halfLead);
          b.textLines.push({ text: lines[i], x: startX, y: ty + scrollY, w: measure(lines[i], fs, ff, baseFw), offset, fs, lh, sepAfter: seps[i] });
          offset += lines[i].length;
        }
      }
      return;
    }

    const plainText = b.text || segmentsPlainText(segments);
    let charStyles = b._charStyles;
    if (!charStyles) {
      charStyles = segmentCharStyles(segments);
      b._charStyles = charStyles;
    }
    const wrapped = b._wrapped || (b._wrapped = wrapStyledTextIndexed(
      plainText, segments, maxW, fs, ff, pal.mono, baseFw, charStyles,
    ));
    b._lines = wrapped.map((l) => l.text);

    const styleAt = (absOff: number | undefined) =>
      absOff == null ? {} : (charStyles![absOff] || {});

    // Pre-compute charX and lineWidths once per rebuild (cached in block)
    let cachedCharXs = b._charXs;
    let cachedLineWidths = b._lineWidths;
    if (!cachedCharXs) {
      cachedCharXs = [];
      cachedLineWidths = [];
      for (let li = 0; li < wrapped.length; li++) {
        const { text: line, offsets } = wrapped[li];
        const charX: number[] = new Array(line.length + 1);
        let cx = 0, ri = 0;
        while (ri < line.length) {
          const cs = styleAt(offsets[ri]);
          let runEnd = ri + 1;
          while (runEnd < line.length) {
            const ns = styleAt(offsets[runEnd]);
            if (ns.bold !== cs.bold || ns.italic !== cs.italic || ns.code !== cs.code || ns.link !== cs.link) break;
            runEnd++;
          }
          const run = line.slice(ri, runEnd);
          const segFw = cs.bold ? "bold" : baseFw;
          const segFs = cs.code ? 12.5 : fs;
          const segFf = cs.code ? pal.mono : ff;
          const segStyle = cs.italic ? "italic" : "normal";
          const rw = measure(run, segFs, segFf, segFw, segStyle);
          const codePad = cs.code ? CODE_PAD : 0;
          const codeGap = cs.code ? CODE_GAP : 0;
          for (let c = 0; c < run.length; c++) {
            charX[ri + c] = cx + codeGap + codePad + measure(run.slice(0, c), segFs, segFf, segFw, segStyle);
          }
          cx += rw + codePad * 2 + codeGap * 2;
          ri = runEnd;
        }
        charX[line.length] = cx;
        cachedCharXs.push(charX);
        cachedLineWidths!.push(cx);
      }
      b._charXs = cachedCharXs;
      b._lineWidths = cachedLineWidths;
    }

    let globalOffset = 0;
    for (let li = 0; li < wrapped.length; li++) {
      const { text: line, offsets } = wrapped[li];
      const ty = by + li * lh;
      const tySnap = snap(ty + halfLead);
      const charX = cachedCharXs[li];
      const lineW = cachedLineWidths![li];
      const lineEntry: TextLine = { text: line, x: startX, y: ty + scrollY, w: lineW, offset: globalOffset, fs, lh, charX,
        sepAfter: wrapped[li].hardBreak ? "\n" : wrapped[li].spaceBreak ? " " : "" };
      b.textLines.push(lineEntry);

      // Render styled runs — look up styles via original offsets (not packed line index)
      let cx = 0, ri = 0;
      while (ri < line.length) {
        const cs = styleAt(offsets[ri]);
        let runEnd = ri + 1;
        while (runEnd < line.length) {
          const ns = styleAt(offsets[runEnd]);
          if (ns.bold !== cs.bold || ns.italic !== cs.italic || ns.code !== cs.code || ns.link !== cs.link) break;
          runEnd++;
        }
        const run = line.slice(ri, runEnd);
        const segFw = cs.bold ? "bold" : baseFw;
        const segFs = cs.code ? 12.5 : fs;
        const segFf = cs.code ? pal.mono : ff;
        const segStyle = cs.italic ? "italic" : "normal";
        const rw = measure(run, segFs, segFf, segFw, segStyle);
        const codePad = cs.code ? CODE_PAD : 0;
        const codeGap = cs.code ? CODE_GAP : 0;
        const chipX = startX + cx + codeGap;
        const runX = chipX + codePad;

        if (cs.code) {
          ctx.fillStyle = pal.panel;
          roundRect(ctx, chipX - 1, tySnap - 2, rw + codePad * 2 + 2, segFs + 5, 5);
          ctx.fill();
          ctx.strokeStyle = pal.border;
          ctx.lineWidth = 1;
          roundRect(ctx, chipX - 0.5, tySnap - 1.5, rw + codePad * 2 + 1, segFs + 4, 5);
          ctx.stroke();
        }

        ctx.fillStyle = cs.link ? pal.blue : (b.color || pal.text);
        ctx.font = `${segStyle} ${segFw} ${segFs}px ${segFf}`;
        fillTextCrisp(ctx, run, runX, tySnap);
        if (cs.link) {
          ctx.fillRect(runX, tySnap + segFs + 1, rw, 1);
        }
        cx += rw + codePad * 2 + codeGap * 2;
        ri = runEnd;
      }
      globalOffset += line.length;
    }
  }

  function paintCodeBlock(ctx: CanvasRenderingContext2D, b: Block, bx: number, by: number, p: Palette) {
    const padX = (b.data?.padX as number) || 14;
    const padY = (b.data?.padY as number) || 12;
    const fs = b.fontSize || 12.5;
    const ff = b.font || p.mono;
    const lh = fs * (b.lineHeight || 1.55);
    const halfLead = (lh - fs) / 2;
    const innerW = Math.max(1, b.w - padX * 2);

    ctx.save();
    // 同 paintTextBlock：矩形 clip 足够（padding 大于圆角），避免每帧抗锯齿蒙版
    ctx.beginPath();
    ctx.rect(bx, by, b.w, b.h);
    ctx.clip();

    ctx.font = `${fs}px ${ff}`;
    ctx.fillStyle = b.color || p.text;
    ctx.textBaseline = "top";
    // Soft-wrap long lines (layout already sizes height via wrapText); do not rely on hard \n only.
    let lines = b._lines;
    let seps = b._lineSeps;
    if (!lines || !seps) {
      const full = wrapCodeTextFull(b.text || "", innerW, fs, ff);
      lines = full.lines;
      seps = full.seps;
      b._lines = lines;
      b._lineSeps = seps;
    }
    if (!b._textLines) {
      b._textLines = [];
      let offset = 0;
      for (let i = 0; i < lines.length; i++) {
        b._textLines.push({ text: lines[i], x: bx + padX, y: b.y + padY + i * lh, w: measure(lines[i], fs, ff), offset, fs, lh, sepAfter: seps[i] });
        offset += lines[i].length;
      }
    }
    b.textLines = b._textLines;
    // 只绘制视口内的行：高代码块部分可见时，原先会对全部行 fillText（每帧文本整形开销极大）
    for (let i = 0; i < lines.length; i++) {
      const ty = by + padY + i * lh;
      if (ty + lh >= -10 && ty <= viewH + 10) {
        fillTextCrisp(ctx, lines[i], bx + padX, ty + halfLead);
      }
    }
    ctx.restore();
  }

  function paintMdTable(ctx: CanvasRenderingContext2D, b: Block, bx: number, by: number, p: Palette) {
    const fs = b.fontSize || TABLE_FS;
    const ff = b.font || p.sans;
    const colWidths = (b.data?.colWidths as number[]) || [];
    const cellLines = (b.data?.cellLines as string[][][]) || [];
    const cellSeps = (b.data?.cellSeps as string[][][]) || [];
    const rowHeights = (b.data?.rowHeights as number[]) || [];
    const aligns = (b.data?.aligns as Array<"left" | "center" | "right">) || [];
    const border = (b.data?.border as string) || p.border;
    // textLines（含逐字 charX 测量）一次构建跨帧复用：
    // 原先每帧对每个单元格 O(n²) slice+measure，滚动经过表格时必然掉帧
    if (!b._textLines) {
      const textLines: TextLine[] = [];
      let charOff = 0;
      let absRowY = b.y;
      for (let r = 0; r < cellLines.length; r++) {
        const rh = rowHeights[r] || fs * TABLE_LH + TABLE_PAD_Y * 2;
        let cellX = b.x;
        for (let c = 0; c < colWidths.length; c++) {
          const cw = colWidths[c];
          const lines = cellLines[r]?.[c] || [""];
          const fw = r === 0 ? "600" : "400";
          const align = aligns[c] || "left";
          const lineH = fs * TABLE_LH;
          const contentH = lines.length * lineH;
          const absTextTop = absRowY + Math.max(TABLE_PAD_Y, (rh - contentH) / 2);
          for (let li = 0; li < lines.length; li++) {
            const line = lines[li];
            const tw = measure(line, fs, ff, fw);
            let tx = cellX + TABLE_PAD_X;
            if (align === "center") tx = cellX + (cw - tw) / 2;
            else if (align === "right") tx = cellX + cw - TABLE_PAD_X - tw;
            // Per cell visual line: selection hit/paint follows column x, not row-joined text
            const charX: number[] = new Array(line.length + 1);
            for (let ci = 0; ci <= line.length; ci++) {
              charX[ci] = measure(line.slice(0, ci), fs, ff, fw);
            }
            textLines.push({
              text: line,
              x: tx,
              y: absTextTop + li * lineH,
              w: tw,
              offset: charOff,
              fs,
              lh: lineH,
              charX,
              sepAfter: cellSeps[r]?.[c]?.[li] ?? "\n",
              tRow: r,
              tCol: c,
            });
            charOff += line.length + 1;
          }
          cellX += cw;
        }
        absRowY += rh;
      }
      b._textLines = textLines;
    }
    b.textLines = b._textLines;

    // 表头背景（DOM 版表头底色）
    const headerH = rowHeights[0] || fs * TABLE_LH + TABLE_PAD_Y * 2;
    if (cellLines.length && by + headerH >= -10 && by <= viewH + 10) {
      ctx.fillStyle = p.panel;
      ctx.globalAlpha = 0.55;
      ctx.fillRect(bx, by, b.w, headerH);
      ctx.globalAlpha = 1;
    }
    // 折叠边框：对齐 DOM border-collapse: collapse，整表网格一次描边，
    // 避免此前每个单元格各描一条边导致相邻边重叠成双线
    const gridW = colWidths.reduce((sum, cw) => sum + cw, 0);
    const gridH = rowHeights.reduce((sum, rh) => sum + rh, 0);
    ctx.strokeStyle = border;
    ctx.lineWidth = 1;
    ctx.beginPath();
    let gridY = by;
    for (let r = 0; r <= cellLines.length; r++) {
      const yy = Math.round(gridY) + 0.5;
      ctx.moveTo(bx, yy);
      ctx.lineTo(bx + gridW, yy);
      gridY += rowHeights[r] ?? 0;
    }
    let gridX = bx;
    for (let c = 0; c <= colWidths.length; c++) {
      const xx = Math.round(gridX) + 0.5;
      ctx.moveTo(xx, by);
      ctx.lineTo(xx, by + gridH);
      gridX += colWidths[c] ?? 0;
    }
    ctx.stroke();

    // 文本绘制按行做视口裁剪，跳过不可见行
    let rowY = by;
    for (let r = 0; r < cellLines.length; r++) {
      const rh = rowHeights[r] || fs * TABLE_LH + TABLE_PAD_Y * 2;
      const rowVisible = rowY + rh >= -10 && rowY <= viewH + 10;
      let cellX = bx;
      for (let c = 0; c < colWidths.length; c++) {
        const cw = colWidths[c];
        if (rowVisible && cellX + cw >= -10 && cellX <= viewW + 10) {
          const lines = cellLines[r]?.[c] || [""];
          const fw = r === 0 ? "600" : "400";
          const align = aligns[c] || "left";
          ctx.fillStyle = b.color || p.text;
          ctx.font = `${fw} ${fs}px ${ff}`;
          ctx.textBaseline = "top";
          const lineH = fs * TABLE_LH;
          const contentH = lines.length * lineH;
          const textTop = rowY + Math.max(TABLE_PAD_Y, (rh - contentH) / 2);
          // 文字在行高内垂直居中（与代码块 halfLead、DOM line-height 一致），
          // 此前从行槽顶直接画，视觉上每行文字偏上、行间距下坠
          const halfLead = (lineH - fs) / 2;
          for (let li = 0; li < lines.length; li++) {
            const line = lines[li];
            const tw = measure(line, fs, ff, fw); // 对齐需要宽度；measure 有缓存
            let tx = cellX + TABLE_PAD_X;
            if (align === "center") tx = cellX + (cw - tw) / 2;
            else if (align === "right") tx = cellX + cw - TABLE_PAD_X - tw;
            fillTextCrisp(ctx, line, tx, textTop + li * lineH + halfLead);
          }
        }
        cellX += cw;
      }
      rowY += rh;
    }
  }

  function paintEditIcon(ctx: CanvasRenderingContext2D, x: number, y: number, size: number, color: string) {
    ctx.save();
    ctx.strokeStyle = color;
    ctx.lineWidth = 1.5;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    const s = size / 24;
    ctx.translate(x, y);
    ctx.scale(s, s);
    ctx.beginPath();
    ctx.moveTo(3, 21); ctx.lineTo(7, 20); ctx.lineTo(20, 7);
    ctx.quadraticCurveTo(22, 5, 19, 3);
    ctx.quadraticCurveTo(17, 1, 15, 3);
    ctx.lineTo(3, 17); ctx.closePath();
    ctx.stroke();
    ctx.restore();
  }

  function paintCopyIcon(ctx: CanvasRenderingContext2D, x: number, y: number, size: number, color: string) {
    // IconCopy: rect 9,9 13x13 rx2 + path M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1
    ctx.save();
    ctx.strokeStyle = color;
    ctx.lineWidth = 2;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    const s = size / 24;
    ctx.translate(x, y);
    ctx.scale(s, s);
    roundRect(ctx, 9, 9, 13, 13, 2);
    ctx.stroke();
    ctx.beginPath();
    ctx.moveTo(5, 15);
    ctx.lineTo(4, 15);
    ctx.quadraticCurveTo(2, 15, 2, 13);
    ctx.lineTo(2, 4);
    ctx.quadraticCurveTo(2, 2, 4, 2);
    ctx.lineTo(13, 2);
    ctx.quadraticCurveTo(15, 2, 15, 4);
    ctx.lineTo(15, 5);
    ctx.stroke();
    ctx.restore();
  }

  function paintCheckIcon(ctx: CanvasRenderingContext2D, x: number, y: number, size: number, color: string) {
    // IconCheck: M20 6 9 17l-5-5
    ctx.save();
    ctx.strokeStyle = color;
    ctx.lineWidth = 2;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    const s = size / 24;
    ctx.translate(x, y);
    ctx.scale(s, s);
    ctx.beginPath();
    ctx.moveTo(20, 6);
    ctx.lineTo(9, 17);
    ctx.lineTo(4, 12);
    ctx.stroke();
    ctx.restore();
  }

  function paintSelection(ctx: CanvasRenderingContext2D) {
    if (!selection) return;
    ctx.save();
    // 对齐 DOM ::selection（app.css: color-mix(in srgb, var(--accent) 30%, transparent)）
    ctx.fillStyle = pal.accent;
    ctx.globalAlpha = 0.3;
    // 同 block 内从下往上也要按 offset 判向，否则 fromOff>toOff 导致高亮宽度为负
    const forward =
      selection.startBlock < selection.endBlock ||
      (selection.startBlock === selection.endBlock && selection.startOffset <= selection.endOffset);
    const from = forward ? selection.startBlock : selection.endBlock;
    const to = forward ? selection.endBlock : selection.startBlock;
    const fromOff = forward ? selection.startOffset : selection.endOffset;
    const toOff = forward ? selection.endOffset : selection.startOffset;
    for (let i = from; i <= to; i++) {
      const b = blocks[i];
      if (!b?.textLines || !b.selectable) continue;
      const sOff = i === from ? fromOff : 0;
      const eOff = i === to ? toOff : Number.POSITIVE_INFINITY;
      const clipped = !!b.data?.clipped;
      const bScr = clipped ? (blockScrolls.get(blockScrollKey(b)) || 0) : 0;
      if (clipped) {
        ctx.save();
        roundRect(ctx, b.x, b.y - scrollY, b.w, b.h, b.borderRadius || 0);
        ctx.clip();
      }
      for (const ln of b.textLines) {
        const ly = ln.y - bScr;
        const lineEnd = ln.offset + ln.text.length;
        if (eOff <= ln.offset || sOff >= lineEnd) continue;
        // 工具详情内部滚动后，裁切区外的行不再画高亮，避免漂到卡片上方
        if (clipped && (ly + ln.lh <= b.y || ly >= b.y + b.h)) continue;
        const a = Math.max(0, sOff - ln.offset);
        const bEnd = Math.min(ln.text.length, eOff - ln.offset);
        let x0: number, x1: number;
        if (ln.charX) {
          x0 = ln.x + (ln.charX[a] ?? 0);
          x1 = ln.x + (ln.charX[bEnd] ?? ln.w);
        } else {
          x0 = ln.x + measure(ln.text.slice(0, a), ln.fs, b.font || pal.sans, b.fontWeight);
          x1 = ln.x + measure(ln.text.slice(0, bEnd), ln.fs, b.font || pal.sans, b.fontWeight);
        }
        // 选区越过行尾换行符时，DOM 会在行尾多画一小段（换行符宽度），补一个空格宽
        if (eOff > lineEnd) x1 += measure(" ", ln.fs, b.font || pal.sans, b.fontWeight);
        // DOM 选区高亮覆盖整行 line-height，而不是字号高度
        ctx.fillRect(x0, ly - scrollY, x1 - x0, ln.lh);
      }
      if (clipped) ctx.restore();
    }
    ctx.restore();
  }

  function blockScrollbarGeom(b: Block) {
    if (!b.data?.clipped || b.kind !== "tool-content") return null;
    const fullH = (b.data.fullH as number) || b.h;
    const maxBlockScroll = Math.max(0, fullH - b.h);
    if (maxBlockScroll <= 0) return null;
    const trackW = 5;
    const trackX = b.x + b.w - trackW - 3;
    const thumbH = Math.max(20, b.h * (b.h / fullH));
    const travel = Math.max(0, b.h - thumbH);
    const key = blockScrollKey(b);
    const blockScroll = Math.max(0, Math.min(maxBlockScroll, blockScrolls.get(key) || 0));
    const thumbY = b.y - scrollY + (blockScroll / maxBlockScroll) * travel;
    const hitPad = 6;
    return { key, trackW, trackX, hitX: trackX - hitPad, hitW: trackW + hitPad * 2,
      thumbH, thumbY, travel, maxBlockScroll, blockTop: b.y - scrollY };
  }

  function hitBlockScrollbar(clientX: number, clientY: number) {
    const rect = canvasEl.getBoundingClientRect();
    const x = clientX - rect.left;
    const y = clientY - rect.top;
    for (let i = blocks.length - 1; i >= 0; i--) {
      const g = blockScrollbarGeom(blocks[i]);
      if (!g) continue;
      if (x < g.hitX || x > g.hitX + g.hitW || y < g.blockTop || y > g.blockTop + blocks[i].h) continue;
      return { ...g, part: y >= g.thumbY && y <= g.thumbY + g.thumbH ? "thumb" as const : "track" as const };
    }
    return null;
  }

  function scrollBlockFromPointerY(clientY: number) {
    if (!blockScrollDrag) return;
    const block = blocks.find((b) => blockScrollKey(b) === blockScrollDrag!.key);
    if (!block) return;
    const g = blockScrollbarGeom(block);
    if (!g || g.travel <= 0) return;
    const rect = canvasEl.getBoundingClientRect();
    const y = clientY - rect.top;
    const thumbY = Math.max(0, Math.min(g.travel, y - g.blockTop - blockScrollDrag.grab));
    blockScrolls.set(g.key, (thumbY / g.travel) * g.maxBlockScroll);
    requestPaint();
  }

  function scrollbarGeom() {
    if (maxScroll <= 0 || viewH <= 0 || totalHeight <= 0) return null;
    const trackW = 5;
    const trackX = viewW - trackW - 3;
    // Wider hit target than the visual thumb.
    const hitPad = 6;
    const hitX = trackX - hitPad;
    const hitW = trackW + hitPad * 2;
    const ratio = viewH / totalHeight;
    const thumbH = Math.max(20, viewH * ratio);
    const travel = Math.max(0, viewH - thumbH);
    const thumbY = travel > 0 ? (scrollY / maxScroll) * travel : 0;
    return { trackW, trackX, hitX, hitW, thumbH, thumbY, travel };
  }

  function paintScrollbar(ctx: CanvasRenderingContext2D) {
    const g = scrollbarGeom();
    if (!g) return;
    ctx.fillStyle = pal.scroll;
    roundRect(ctx, g.trackX, g.thumbY, g.trackW, g.thumbH, 3);
    ctx.fill();
  }

  function hitScrollbar(clientX: number, clientY: number): "thumb" | "track" | null {
    const g = scrollbarGeom();
    if (!g) return null;
    const rect = canvasEl.getBoundingClientRect();
    const x = clientX - rect.left;
    const y = clientY - rect.top;
    if (x < g.hitX || x > g.hitX + g.hitW || y < 0 || y > viewH) return null;
    if (y >= g.thumbY && y <= g.thumbY + g.thumbH) return "thumb";
    return "track";
  }

  function applyScrollY(next: number, user: boolean) {
    scrollY = Math.max(0, Math.min(maxScroll, next));
    keepBottom = maxScroll - scrollY <= 2;
    applyEditStyle();
    props.onScroll?.(scrollY, maxScroll, user);
    requestPaint();
  }

  function scrollFromPointerY(clientY: number) {
    const g = scrollbarGeom();
    if (!g || g.travel <= 0) return;
    const rect = canvasEl.getBoundingClientRect();
    const y = clientY - rect.top;
    const thumbY = Math.max(0, Math.min(g.travel, y - scrollDragGrab));
    applyScrollY((thumbY / g.travel) * maxScroll, true);
  }

  // ─── Interaction ───────────────────────────────────────────────────────────

  function hitTest(clientX: number, clientY: number): number {
    const rect = canvasEl.getBoundingClientRect();
    const x = clientX - rect.left;
    const y = clientY - rect.top + scrollY;
    for (let i = blocks.length - 1; i >= 0; i--) {
      const b = blocks[i];
      if (b.kind === "process-border" || b.kind === "tool-body-border" || b.kind === "md-hr") continue;
      if (x >= b.x && x <= b.x + b.w && y >= b.y && y <= b.y + b.h) return i;
    }
    return -1;
  }

  /**
   * 命中最近的文本位置（对齐 DOM 选区行为）：不要求指针精确落在行内，
   * 垂直方向取最近行、水平方向越界时收敛到行首/行尾，
   * 因此空白区域、行距、块间距都能发起/延续选区，拖动经过空隙也不会卡住。
   */
  function hitTextPosition(clientX: number, clientY: number): { block: number; offset: number } | null {
    const rect = canvasEl.getBoundingClientRect();
    const mx = clientX - rect.left;
    const my = clientY - rect.top + scrollY;
    let best: { block: number; offset: number; dy: number; dx: number } | null = null;
    for (let i = 0; i < blocks.length; i++) {
      const b = blocks[i];
      if (!b.textLines || !b.selectable) continue;
      // textLines 的 y 不含内滚偏移，命中判定时换算
      const bScr = b.data?.clipped ? (blockScrolls.get(blockScrollKey(b)) || 0) : 0;
      for (const ln of b.textLines) {
        const ly = ln.y - bScr;
        if (b.data?.clipped && (ly + ln.lh <= b.y || ly >= b.y + b.h)) continue;
        const dy = my < ly ? ly - my : my >= ly + ln.lh ? my - (ly + ln.lh) : 0;
        // 表格同行多列共享同一 y：按水平距离选最近行，避免永远命中左侧单元格
        const lineRight = ln.x + Math.max(ln.w, 1);
        const dx = mx < ln.x ? ln.x - mx : mx > lineRight ? mx - lineRight : 0;
        if (best && (dy > best.dy || (dy === best.dy && dx >= best.dx))) continue;
        let off = ln.offset + ln.text.length;
        if (ln.charX) {
          for (let c = 0; c < ln.text.length; c++) {
            const left = ln.x + ln.charX[c];
            const right = ln.x + ln.charX[c + 1];
            if (mx < (left + right) / 2) { off = ln.offset + c; break; }
          }
        } else {
          for (let c = 0; c < ln.text.length; c++) {
            const left = ln.x + measure(ln.text.slice(0, c), ln.fs, b.font || pal.sans, b.fontWeight);
            const right = ln.x + measure(ln.text.slice(0, c + 1), ln.fs, b.font || pal.sans, b.fontWeight);
            if (mx < (left + right) / 2) { off = ln.offset + c; break; }
          }
        }
        best = { block: i, offset: off, dy, dx };
      }
    }
    return best ? { block: best.block, offset: best.offset } : null;
  }

  function onMouseMove(e: MouseEvent) {
    if (blockScrollDrag) {
      scrollBlockFromPointerY(e.clientY);
      return;
    }
    if (scrollDragging) {
      scrollFromPointerY(e.clientY);
      return;
    }
    if (selecting) {
      // 拖出视口上下边缘时跟随滚动（对齐 DOM 拖拽选区的自动滚动）
      const rect = canvasEl.getBoundingClientRect();
      if (e.clientY < rect.top) applyScrollY(scrollY - Math.min(48, rect.top - e.clientY), true);
      else if (e.clientY > rect.bottom) applyScrollY(scrollY + Math.min(48, e.clientY - rect.bottom), true);
      const pos = hitTextPosition(e.clientX, e.clientY);
      if (pos && selStart) {
        selection = { startBlock: selStart.block, startOffset: selStart.offset,
          endBlock: pos.block, endOffset: pos.offset };
        if (selection.startBlock !== selection.endBlock || selection.startOffset !== selection.endOffset) {
          selMoved = true;
        }
        requestPaint();
      }
      return;
    }
    if (hitBlockScrollbar(e.clientX, e.clientY) || hitScrollbar(e.clientX, e.clientY)) {
      if (hoverBlockIdx !== -1) {
        hoverBlockIdx = -1;
        requestPaint();
      }
      canvasEl.style.cursor = "default";
      if (canvasEl.title) canvasEl.title = "";
      return;
    }
    const idx = hitTest(e.clientX, e.clientY);
    if (idx !== hoverBlockIdx) {
      hoverBlockIdx = idx;
      const b = idx >= 0 ? blocks[idx] : null;
      // 空白区域也能发起选区（对齐 DOM），无专属光标时一律显示文本光标；
      // 只有纯点击块（不可选的折叠头/按钮等）保持默认光标
      canvasEl.style.cursor = b?.cursor || (b?.selectable || !b?.clickAction ? "text" : "default");
      canvasEl.title = b?.title ?? "";
      requestPaint();
    }
  }

  function endScrollDrag() {
    if (!scrollDragging && !blockScrollDrag) return;
    scrollDragging = false;
    blockScrollDrag = null;
    document.removeEventListener("mousemove", onScrollDragMove);
    document.removeEventListener("mouseup", onScrollDragUp);
  }

  function onScrollDragMove(e: MouseEvent) {
    if (blockScrollDrag) scrollBlockFromPointerY(e.clientY);
    else if (scrollDragging) scrollFromPointerY(e.clientY);
  }

  function onScrollDragUp(_e: MouseEvent) {
    endScrollDrag();
  }

  /** 双击：选中光标所在词，对齐 DOM 双击选词。中文等 CJK 用 Intl.Segmenter 分词，
   *  只选当前词语而非整串同类字符；分词器不可用时回退同类字符连续串。 */
  function selectWordAt(blockIdx: number, offset: number): boolean {
    const b = blocks[blockIdx];
    if (!b) return false;
    const ln = lineAtOffset(b, offset);
    if (!ln || !ln.text.length) return false;
    let i = Math.min(Math.max(0, offset - ln.offset), ln.text.length - 1);
    // 点在两个字符边界上且右侧是空白时，选左侧的词
    if (i > 0 && wordClass(ln.text[i]) === 0 && wordClass(ln.text[i - 1]) !== 0) i--;
    const cls = wordClass(ln.text[i]);
    let s = i;
    let e = i + 1;
    while (s > 0 && wordClass(ln.text[s - 1]) === cls) s--;
    while (e < ln.text.length && wordClass(ln.text[e]) === cls) e++;
    if (cls === 1) {
      // 只对光标周围的最小词字符候选串分词（不对整行分词），精确到词（如“渲染”）；
      // 分词结果不是词段（如纯下划线串）时保留整个候选串
      const range = segmentWordRange(ln.text.slice(s, e), i - s);
      if (range) { s += range.s; e = s - range.s + range.e; }
    }
    selection = { startBlock: blockIdx, startOffset: ln.offset + s, endBlock: blockIdx, endOffset: ln.offset + e };
    return true;
  }

  /** 三击：选中逻辑行（段落）。软折行（sepAfter 为 ""/" "）向前后合并成一段，
   *  与 DOM 三击选段落一致；代码/逐行内容每行都是硬换行，即选单行。 */
  function selectLineAt(blockIdx: number, offset: number): boolean {
    const b = blocks[blockIdx];
    if (!b?.textLines?.length) return false;
    const ln = lineAtOffset(b, offset);
    if (!ln) return false;
    const lines = b.textLines;
    const idx = lines.indexOf(ln);
    let s = idx;
    while (s > 0 && (lines[s - 1].sepAfter ?? "\n") !== "\n") s--;
    let e = idx;
    while (e < lines.length - 1 && (lines[e].sepAfter ?? "\n") !== "\n") e++;
    selection = {
      startBlock: blockIdx, startOffset: lines[s].offset,
      endBlock: blockIdx, endOffset: lines[e].offset + lines[e].text.length,
    };
    return true;
  }

  function onMouseDown(e: MouseEvent) {
    if (e.button !== 0) return;

    const blockSb = hitBlockScrollbar(e.clientX, e.clientY);
    if (blockSb) {
      e.preventDefault();
      const rect = canvasEl.getBoundingClientRect();
      const y = e.clientY - rect.top;
      const grab = blockSb.part === "track"
        ? blockSb.thumbH / 2
        : Math.max(0, Math.min(blockSb.thumbH, y - blockSb.thumbY));
      blockScrollDrag = { key: blockSb.key, grab };
      scrollDragging = false;
      selecting = false;
      selection = null;
      if (blockSb.part === "track") scrollBlockFromPointerY(e.clientY);
      canvasEl.style.cursor = "default";
      document.addEventListener("mousemove", onScrollDragMove);
      document.addEventListener("mouseup", onScrollDragUp);
      requestPaint();
      return;
    }

    const sb = hitScrollbar(e.clientX, e.clientY);
    if (sb) {
      e.preventDefault();
      const g = scrollbarGeom();
      if (!g) return;
      const rect = canvasEl.getBoundingClientRect();
      const y = e.clientY - rect.top;
      if (sb === "track") {
        scrollDragGrab = g.thumbH / 2;
        scrollFromPointerY(e.clientY);
      } else {
        scrollDragGrab = Math.max(0, Math.min(g.thumbH, y - g.thumbY));
      }
      scrollDragging = true;
      selecting = false;
      selection = null;
      canvasEl.style.cursor = "default";
      document.addEventListener("mousemove", onScrollDragMove);
      document.addEventListener("mouseup", onScrollDragUp);
      requestPaint();
      return;
    }

    // 必须在 mousedown 阶段退出吸底。ChatView 在 window pointerup 上排队钉底；若等到
    // click 才处理，钉底微任务可能先改变 scrollY，使同一坐标命中另一个块而吞掉首次点击。
    const pressedIdx = hitTest(e.clientX, e.clientY);
    const pressed = pressedIdx >= 0 ? blocks[pressedIdx] : null;
    if (pressed && (pressed.kind === "fold" || pressed.kind === "thought-toggle" || pressed.kind === "tool-header")) {
      keepBottom = false;
      props.onBrowseDetail?.();
    }

    // 与 DOM 一致：任意位置（含空白、行距、块间距）按下都允许发起文本选区；
    // 不拖动的点击仍由 click 事件触发 clickAction（靠 selMoved 区分点选与拖选）
    clearCanvasChatSelection();
    // 连击判定：500ms 内且位置基本不动
    const nowT = performance.now();
    const nearLast = (e.clientX - lastClick.x) ** 2 + (e.clientY - lastClick.y) ** 2 < 36;
    const clickCount = nowT - lastClick.t < 500 && nearLast ? lastClick.count + 1 : 1;
    lastClick = { t: nowT, count: clickCount, x: e.clientX, y: e.clientY };
    const pos = hitTextPosition(e.clientX, e.clientY);
    if (pos && clickCount >= 2) {
      // 双击选词 / 三击选行（对齐 DOM），选中后不进入拖选
      const ok = clickCount === 2 ? selectWordAt(pos.block, pos.offset) : selectLineAt(pos.block, pos.offset);
      if (ok) {
        selecting = false;
        selMoved = true;
        if (selection) setCanvasChatSelection(selectionText(blocks, selection));
        requestPaint();
        return;
      }
    }
    if (pos) {
      selecting = true;
      selMoved = false;
      selStart = pos;
      selection = { startBlock: pos.block, startOffset: pos.offset, endBlock: pos.block, endOffset: pos.offset };
      canvasEl.focus();
      // 拖出 canvas 后仍能延续选区/自动滚动，松开时收尾（对齐 DOM 拖拽选区）
      document.addEventListener("mousemove", onMouseMove);
      document.addEventListener("mouseup", onSelectDocUp);
      requestPaint();
      return;
    }
    selection = null;
    requestPaint();
  }

  function onSelectDocUp(e: MouseEvent) {
    document.removeEventListener("mousemove", onMouseMove);
    document.removeEventListener("mouseup", onSelectDocUp);
    onMouseUp(e);
  }

  function onMouseUp(_e: MouseEvent) {
    if (scrollDragging || blockScrollDrag) {
      endScrollDrag();
      return;
    }
    if (selecting) {
      selecting = false;
      if (selection && selection.startBlock === selection.endBlock && selection.startOffset === selection.endOffset) {
        selection = null;
      }
      if (selection) setCanvasChatSelection(selectionText(blocks, selection));
      else clearCanvasChatSelection();
      requestPaint();
      return;
    }
  }

  function onClick(e: MouseEvent) {
    if (hitBlockScrollbar(e.clientX, e.clientY) || hitScrollbar(e.clientX, e.clientY)) return;
    // 拖选结束的 click 不触发折叠/按钮动作（DOM 中拖选也不会触发点击）
    if (selMoved) {
      selMoved = false;
      return;
    }
    const idx = hitTest(e.clientX, e.clientY);
    const b = idx >= 0 ? blocks[idx] : null;
    if (b?.clickAction && !selecting) {
      // Fold / thought / tool expands insert content below the header. If we were
      // stick-to-bottom, rebuild would pin scrollY to the new maxScroll and shove
      // the header upward — lock the header's viewport offset instead.
      if (b.kind === "fold" || b.kind === "thought-toggle" || b.kind === "tool-header") {
        keepBottom = false;
        scrollLock = { kind: b.kind, id: b.id, viewOffset: b.y - scrollY };
      }
      b.clickAction();
    }
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    const dy = e.deltaY;

    // When selecting text, always scroll the main canvas (don't trap in block)
    if (!selecting) {
      const idx = hitTest(e.clientX, e.clientY);
      if (idx >= 0) {
        const b = blocks[idx];
        if (b.data?.clipped && b.kind === "tool-content") {
          const fullH = (b.data.fullH as number) || b.h;
          const maxBlockScroll = fullH - b.h;
          const key = blockScrollKey(b);
          const curScroll = blockScrolls.get(key) || 0;
          const newScroll = Math.max(0, Math.min(maxBlockScroll, curScroll + dy));
          if (newScroll !== curScroll) {
            blockScrolls.set(key, newScroll);
            requestPaint();
            return;
          }
        }
      }
    }

    scrollY = Math.max(0, Math.min(maxScroll, scrollY + dy));
    keepBottom = maxScroll - scrollY <= 2;
    applyEditStyle();
    props.onScroll?.(scrollY, maxScroll, true);
    requestPaint();
  }

  function onCopy(e: ClipboardEvent) {
    if (!selection) return;
    const text = selectionText(blocks, selection);
    if (!text) return;
    e.preventDefault();
    e.clipboardData?.setData("text/plain", text);
  }

  // ─── Render scheduling ────────────────────────────────────────────────────
  let paintQueued = false;
  /**
   * 合并高频输入事件（wheel/mousemove/拖拽）的重绘：每帧最多一次全量绘制。
   * 此前每个 wheel 事件都同步 paintAll，一次滚动手势内多次光栅化整帧画布，
   * 展开工具后单帧绘制变贵，叠加放大成滚动卡顿（qwen canvas 走 rAF 合并所以不卡）。
   */
  function requestPaint() {
    if (paintQueued) return;
    paintQueued = true;
    requestAnimationFrame(() => {
      paintQueued = false;
      paintAll();
    });
  }

  // ─── Image loading ─────────────────────────────────────────────────────────

  function loadImage(img: PromptImage): HTMLImageElement | null {
    const src = promptImageSrc(img);
    let el = imgCache.get(src);
    if (el) return (el as unknown as { _loaded?: boolean })._loaded ? el : null;
    el = new Image();
    (el as unknown as { _loaded?: boolean })._loaded = false;
    el.onload = () => { (el as unknown as { _loaded?: boolean })._loaded = true; scheduleRebuild(); };
    el.src = src;
    imgCache.set(src, el);
    return null;
  }

  // ─── Rebuild / effects ─────────────────────────────────────────────────────

  async function rebuild() {
    const generation = ++layoutGeneration;
    pal = readPalette();
    const oldScroll = scrollY;
    const lock = scrollLock;
    scrollLock = null;
    const wasBottom = !lock && (keepBottom || maxScroll - scrollY <= 2);
    if (!await computeLayout(generation)) return;
    // blocks are replaced during layout; an index from the previous block array may now
    // identify an unrelated block (often the first tool), producing a phantom hover card.
    hoverBlockIdx = -1;
    if (canvasEl) {
      canvasEl.style.cursor = "default";
      canvasEl.title = "";
    }
    const liveKeys = new Set(
      blocks.filter((b) => b.data?.clipped).map((b) => blockScrollKey(b)),
    );
    for (const key of [...blockScrolls.keys()]) {
      if (!liveKeys.has(key)) blockScrolls.delete(key);
    }
    maxScroll = Math.max(0, totalHeight - viewH);
    if (lock) {
      const match = blocks.find((x) => x.kind === lock.kind && x.id === lock.id);
      const y = match?.y ?? oldScroll + lock.viewOffset;
      scrollY = Math.max(0, Math.min(maxScroll, y - lock.viewOffset));
    } else if (wasBottom) {
      scrollY = maxScroll;
    } else {
      scrollY = Math.max(0, Math.min(maxScroll, oldScroll));
    }
    applyEditStyle();
    props.onScroll?.(scrollY, maxScroll, false);
    paintAll();
  }

  const queueRebuildFrame = () => {
    if (rebuildRaf) return;
    rebuildRaf = requestAnimationFrame(() => {
      const deferForPaint = rebuildAfterPaint;
      rebuildAfterPaint = false;
      if (deferForPaint) {
        // Let the loading state reach the screen before starting an uncached layout.
        rebuildRaf = requestAnimationFrame(() => {
          rebuildRaf = 0;
          lastRebuildAt = performance.now();
          void rebuild();
        });
        return;
      }
      rebuildRaf = 0;
      lastRebuildAt = performance.now();
      void rebuild();
    });
  };

  function scheduleRebuild(afterPaint = false, immediate = false) {
    rebuildAfterPaint ||= afterPaint;
    // 停止、切会话、展开/折叠等状态要立即落屏，不受流式节流影响。
    // immediate 时也必须清掉已排队的流式 timer：否则展开/收起触发的 rebuild
    // 会被吞并到那次流式 rebuild 里，而流式 rebuild 可能发生在 click 设置
    // scrollLock 之前，导致开合看似失效（先滚到底、要再点一次）。
    if ((afterPaint || immediate || !props.running) && rebuildTimer !== undefined) {
      window.clearTimeout(rebuildTimer);
      rebuildTimer = undefined;
    }
    if (rebuildRaf || rebuildTimer !== undefined) return;
    const elapsed = performance.now() - lastRebuildAt;
    const delay = props.running && !rebuildAfterPaint && !immediate
      ? Math.max(0, STREAM_LAYOUT_INTERVAL_MS - elapsed)
      : 0;
    if (delay > 1) {
      rebuildTimer = window.setTimeout(() => {
        rebuildTimer = undefined;
        queueRebuildFrame();
      }, delay);
      return;
    }
    queueRebuildFrame();
  }

  function resizeCanvas() {
    const w = hostEl.clientWidth;
    const h = hostEl.clientHeight;
    if (w === viewW && h === viewH && dpr === devicePixelRatio) return;
    dpr = devicePixelRatio || 1;
    viewW = w; viewH = h;
    const pixelW = Math.max(1, Math.round(w * dpr));
    const pixelH = Math.max(1, Math.round(h * dpr));
    backdropCanvasEl.width = pixelW;
    backdropCanvasEl.height = pixelH;
    canvasEl.width = pixelW;
    canvasEl.height = pixelH;
    maxScroll = Math.max(0, totalHeight - viewH);
    if (keepBottom) scrollY = maxScroll;
    paintBackdrop();
    paintAll();
  }

  // ─── Lifecycle ─────────────────────────────────────────────────────────────

  onMount(() => {
    resizeCanvas();
    void rebuild();

    let resizeTimer: number | undefined;
    let canvasVisible = false;
    // 窗口尺寸：区分“用户拖动窗口”与“应用内部布局变化”（发送消息、切会话、
    // 侧边栏/阶段栏开合都会改变宿主尺寸）。内部变化时旧位图会被 CSS 拉伸，
    // 文字位图跟着缩放，表现为“字体大小闪变”，必须立即重分配并重排；
    // 只有窗口尺寸本身连续变化（拖动）才保留 200ms 去抖，避免反复分配位图。
    let lastWindowW = window.innerWidth;
    let lastWindowH = window.innerHeight;
    const ro = new ResizeObserver(() => {
      const w = hostEl.clientWidth;
      const h = hostEl.clientHeight;
      const winW = window.innerWidth;
      const winH = window.innerHeight;
      const windowResizing = winW !== lastWindowW || winH !== lastWindowH;
      lastWindowW = winW;
      lastWindowH = winH;
      if (resizeTimer !== undefined) window.clearTimeout(resizeTimer);
      if (!windowResizing && (w !== viewW || h !== viewH)) {
        resizeTimer = undefined;
        resizeCanvas();
        void rebuild();
        return;
      }
      // 拖动窗口：保留旧位图供 CSS 拉伸；停止 resize 200ms 后再分配 canvas 并重排。
      resizeTimer = window.setTimeout(() => {
        resizeTimer = undefined;
        resizeCanvas();
        void rebuild();
      }, 200);
    });
    const visibilityObserver = new IntersectionObserver(([entry]) => {
      canvasVisible = !!entry?.isIntersecting;
      if (canvasVisible && !props.running) paintBackdrop();
    });
    ro.observe(hostEl);
    visibilityObserver.observe(hostEl);

    // Rebuild fallback-font measurements after bundled web fonts become available.
    void document.fonts.ready.then(() => {
      _mCache.clear();
      scheduleRebuild();
    });

    const mo = new MutationObserver(() => {
      // 背景 Canvas 不参与正文 rebuild，主题切换时必须立即重绘；否则会一直保留
      // 旧主题，直到低频星图定时器的下一帧。
      pal = readPalette();
      paintBackdrop();
      void rebuild();
    });
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    const starMapTimer = window.setInterval(() => {
      if (!document.hidden && canvasVisible && !props.running) paintBackdrop();
    }, STAR_MAP_UPDATE_MS);

    function onMouseLeave() {
      if (scrollDragging || selecting) return;
      if (hoverBlockIdx !== -1) {
        hoverBlockIdx = -1;
        requestPaint();
      }
      canvasEl.style.cursor = "default";
      if (canvasEl.title) canvasEl.title = "";
    }

    canvasEl.addEventListener("mousemove", onMouseMove);
    canvasEl.addEventListener("mouseleave", onMouseLeave);
    canvasEl.addEventListener("mousedown", onMouseDown);
    canvasEl.addEventListener("mouseup", onMouseUp);
    canvasEl.addEventListener("click", onClick);
    canvasEl.addEventListener("wheel", onWheel, { passive: false });
    canvasEl.addEventListener("copy", onCopy);

    props.ref?.({
      scrollToBottom() { keepBottom = true; scrollY = maxScroll; applyEditStyle(); paintAll(); props.onScroll?.(scrollY, maxScroll, false); },
      scrollToGroup(idx) { if (groupYs[idx] != null) { scrollY = Math.max(0, Math.min(maxScroll, groupYs[idx] - 20)); keepBottom = false; applyEditStyle(); paintAll(); props.onScroll?.(scrollY, maxScroll, false); } },
      scrollBy(delta) { scrollY = Math.max(0, Math.min(maxScroll, scrollY + delta)); keepBottom = maxScroll - scrollY <= 2; applyEditStyle(); paintAll(); props.onScroll?.(scrollY, maxScroll, true); },
      isAtBottom() { return maxScroll - scrollY <= 2; },
      scrollTop() { return scrollY; },
      maxScrollTop() { return maxScroll; },
      activeGroup() { let a = -1; for (let i = 0; i < groupYs.length; i++) { if (groupYs[i] <= scrollY + 32) a = i; } return a; },
      hasFocusedInput() { return !!editing(); },
    });

    onCleanup(() => {
      ro.disconnect();
      visibilityObserver.disconnect();
      mo.disconnect();
      window.clearInterval(starMapTimer);
      if (resizeTimer !== undefined) window.clearTimeout(resizeTimer);
      editResizeObserver?.disconnect();
      editResizeObserver = undefined;
      editHostEl = undefined;
      if (revealRaf) cancelAnimationFrame(revealRaf);
      if (rebuildRaf) cancelAnimationFrame(rebuildRaf);
      if (rebuildTimer !== undefined) window.clearTimeout(rebuildTimer);
      if (busyTimer !== undefined) window.clearTimeout(busyTimer);
      canvasEl.removeEventListener("mousemove", onMouseMove);
      canvasEl.removeEventListener("mouseleave", onMouseLeave);
      canvasEl.removeEventListener("mousedown", onMouseDown);
      canvasEl.removeEventListener("mouseup", onMouseUp);
      canvasEl.removeEventListener("click", onClick);
      canvasEl.removeEventListener("wheel", onWheel);
      canvasEl.removeEventListener("copy", onCopy);
      endScrollDrag();
      clearCanvasChatSelection();
    });
  });

  createEffect(() => {
    const threadId = props.threadId;
    const groups = props.groups;
    // 已闭合分组不会再流式变化；只深度订阅仍未闭合的尾部，避免每个 token
    // 都遍历整段历史。新增/替换分组仍由 props.groups 的引用变化触发。
    for (const g of groups) {
      if (g.turn) continue;
      if (g.user) {
        void g.user.text;
        void g.user.images?.length;
      }
      for (const item of g.body) {
        if ("text" in item) void item.text;
        if (item.type === "tool") {
          void item.status;
          void item.title;
          void item.content.length;
          void item.locations.length;
        }
      }
    }
    void props.permissions;
    void props.running;
    void props.loading;
    void props.preview;
    void props.emptyHint;
    void editing()?.id;
    const switchedThread = threadId !== renderedThreadId;
    if (switchedThread) waitingForInitialSnapshot = groups.length === 0;
    // 缓存未命中时会先以空 items 进入 loading，再在同一 threadId 下提交快照；
    // 这次首个非空快照也属于已有内容，不能按新 delta 从空串重放。
    const loadedInitialSnapshot = waitingForInitialSnapshot && groups.length > 0;
    if (loadedInitialSnapshot) waitingForInitialSnapshot = false;
    if (switchedThread) {
      renderedThreadId = threadId;
      layoutGeneration++;
      shownText.clear();
      targetText.clear();
      revealReadyAt.clear();
      revealRemainders.clear();
      clearEditing();
      selection = null;
      groupYs = [];
      scrollY = 0;
      maxScroll = 0;
      blocks = [{
        kind: "hint", id: 0, groupIdx: 0,
        x: Math.max(24, viewW * 0.1), y: 32, w: Math.max(0, viewW * 0.8), h: 40,
        text: "正在渲染会话…", color: pal.faint, fontSize: 13, font: pal.sans,
        selectable: false,
      }];
      totalHeight = viewH;
      if (canvasEl) paintAll();
    }
    syncRevealTargets(switchedThread || loadedInitialSnapshot || !revealsInitialized);
    revealsInitialized = true;
    scheduleRebuild(switchedThread);
  });

  let expandedEffectThreadId = props.threadId;
  let expandedEffectReady = false;
  createEffect(() => {
    // 展开态参与闭合分组签名；仅在它变化时废弃签名缓存，而不是流式时反复哈希历史。
    // 用户主动开合必须立即重排（immediate）：不能被套在流式 80ms 节流里，
    // 否则 click 设置的 scrollLock 来不及生效、开合看似失效。
    expandedRevision();
    const threadId = props.threadId;
    closedGroupSigCache = new WeakMap<Group, string>();
    // openThread 重置 expanded 与 thread 切换属于同一批更新，主 effect 已负责 rebuild。
    if (!expandedEffectReady || threadId !== expandedEffectThreadId) {
      expandedEffectReady = true;
      expandedEffectThreadId = threadId;
      return;
    }
    scheduleRebuild(false, true);
  });

  const saveEdit = () => {
    const item = editing();
    const text = draft().trim();
    const images = editAttachments.images();
    if (!item || (!text && !images.length)) return;
    clearEditing();
    void editUserMessage(item.id, text, images);
  };

  return (
    <div class="canvas-transcript-host" ref={hostEl}>
      <canvas ref={backdropCanvasEl} class="transcript-canvas-backdrop" aria-hidden="true" />
      <canvas ref={canvasEl} class="transcript-canvas-only" tabindex="0" aria-label="会话记录" />
      {editing() && (
        <div class="canvas-prompt-editor" style={editStyle()} ref={bindEditHost}>
          <ImageAttachmentStrip images={editAttachments.images()} onRemove={editAttachments.remove} />
          <textarea
            value={draft()}
            rows={editRowCount(draft())}
            onPaste={editAttachments.onPaste}
            onInput={(e) => setDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); saveEdit(); }
              if (e.key === "Escape") clearEditing();
            }}
            ref={(el) => queueMicrotask(() => {
              el.focus();
              // Defer caret jump so huge prompts don't block first paint of the editor.
              requestAnimationFrame(() => {
                try { el.setSelectionRange(el.value.length, el.value.length); } catch { /* ignore */ }
              });
            })}
          />
          <div>
            <span>发送后将从此处重新开始会话</span>
            <button class="btn secondary small" onClick={() => clearEditing()}>取消</button>
            <button class="btn primary small" onClick={saveEdit}>发送</button>
          </div>
        </div>
      )}
    </div>
  );
}

// ─── Canvas helpers ──────────────────────────────────────────────────────────

function roundRect(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number | number[]) {
  const radii = Array.isArray(r) ? r : [r, r, r, r];
  const limit = Math.min(w / 2, h / 2);
  const [tl, tr, br, bl] = radii.map(v => Math.max(0, Math.min(v, limit)));
  ctx.beginPath();
  ctx.moveTo(x + tl, y);
  ctx.lineTo(x + w - tr, y);
  ctx.quadraticCurveTo(x + w, y, x + w, y + tr);
  ctx.lineTo(x + w, y + h - br);
  ctx.quadraticCurveTo(x + w, y + h, x + w - br, y + h);
  ctx.lineTo(x + bl, y + h);
  ctx.quadraticCurveTo(x, y + h, x, y + h - bl);
  ctx.lineTo(x, y + tl);
  ctx.quadraticCurveTo(x, y, x + tl, y);
  ctx.closePath();
}
