import { convertFileSrc } from "@tauri-apps/api/core";
import { createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { editUserMessage, isExpanded, state, toggleExpanded } from "../store";
import type { Item, PermissionRequest, PromptImage, ToolItem, UserItem } from "../types";
import { displayToolTitle, stripAnsi } from "../utils";
import { createImageAttachments, ImageAttachmentStrip } from "./ImageAttachmentStrip";
import type { Group } from "./TurnGroup";
import { fmtDuration, fmtTokens } from "./TurnGroup";

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
  groups: Group[];
  permissions: PermissionRequest[];
  running: boolean;
  emptyHint: string;
  preview: boolean;
  onReturnToCurrent: () => void;
  onScroll?: (top: number, max: number, user: boolean) => void;
  ref?: (handle: CanvasTranscriptHandle) => void;
}

// ─── Theme / palette ─────────────────────────────────────────────────────────

interface Palette {
  bg: string; panel: string; sidebar: string; hover: string;
  border: string; borderLight: string;
  text: string; dim: string; muted: string; faint: string;
  accent: string; accentDim: string;
  red: string; yellow: string; green: string; blue: string;
  scroll: string; mono: string; sans: string;
}

function readPalette(): Palette {
  const s = getComputedStyle(document.documentElement);
  const v = (n: string, fb: string) => s.getPropertyValue(n).trim() || fb;
  return {
    bg: v("--bg", "#0e1014"), panel: v("--bg-panel", "#171b23"),
    sidebar: v("--bg-sidebar", "#0a0c10"), hover: v("--bg-hover", "#1e232d"),
    border: v("--border", "#252a33"), borderLight: v("--border-light", "#313844"),
    text: v("--text", "#e4e7ec"), dim: v("--text-dim", "#a9b0bd"),
    muted: v("--text-muted", "#7d8593"), faint: v("--text-faint", "#5d6470"),
    accent: v("--accent", "#6e93f8"), accentDim: v("--accent-dim", "rgba(110,147,248,.14)"),
    red: v("--red", "#e07d76"), yellow: v("--yellow", "#d4b26e"),
    green: v("--green", "#8ec489"), blue: v("--blue", "#7aa2f2"),
    scroll: v("--scroll", "#2e333d"), mono: v("--mono", "monospace"),
    sans: v("--sans", "sans-serif"),
  };
}

// ─── Text measurement (cached) ──────────────────────────────────────────────

let _mCtx: CanvasRenderingContext2D | null = null;
function mCtx() {
  if (!_mCtx) _mCtx = document.createElement("canvas").getContext("2d")!;
  return _mCtx;
}
const _mCache = new Map<string, number>();
function measure(text: string, fs: number, ff: string, fw = "400"): number {
  const key = `${fw}|${fs}|${ff}|${text}`;
  let w = _mCache.get(key);
  if (w != null) return w;
  const ctx = mCtx();
  ctx.font = `${fw} ${fs}px ${ff}`;
  w = ctx.measureText(text).width;
  if (_mCache.size > 8192) _mCache.clear();
  _mCache.set(key, w);
  return w;
}

function wrapText(text: string, maxW: number, fs: number, ff: string, fw = "400"): string[] {
  const lines: string[] = [];
  for (const para of text.split("\n")) {
    const words = para.split(/(\s+)/);
    let cur = "", curW = 0;
    for (const word of words) {
      const ww = measure(word, fs, ff, fw);
      if (curW + ww <= maxW) { cur += word; curW += ww; continue; }
      if (cur) { lines.push(cur.trimEnd()); cur = ""; curW = 0; }
      for (const ch of Array.from(word)) {
        const cw = measure(ch, fs, ff, fw);
        if (cur && curW + cw > maxW) { lines.push(cur); cur = ch; curW = cw; }
        else { cur += ch; curW += cw; }
      }
    }
    lines.push(cur || "");
  }
  while (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  return lines;
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
    m = rest.match(/^\*\*(.+?)\*\*/);
    if (m) { flush(); tokens.push({ text: m[1], bold: true }); i += m[0].length; continue; }
    m = rest.match(/^__(.+?)__/);
    if (m) { flush(); tokens.push({ text: m[1], bold: true }); i += m[0].length; continue; }
    m = rest.match(/^\*([^*]+)\*/);
    if (m) { flush(); tokens.push({ text: m[1], italic: true }); i += m[0].length; continue; }
    m = rest.match(/^_([^_]+)_/);
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
    const fence = line.match(/^\s{0,3}(`{3,}|~{3,})\s*(.*?)\s*$/);
    if (fence) {
      const fenceChar = fence[1][0];
      const fenceLen = fence[1].length;
      const lang = fence[2].split(/\s/)[0] || "";
      const closeRe = new RegExp(`^\\s{0,3}${fenceChar === '`' ? '`' : '~'}{${fenceLen},}\\s*$`);
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
    if (/^\s*[-*+]\s+/.test(line)) {
      while (i < lines.length && /^\s*[-*+]\s+/.test(lines[i])) {
        const itemText = lines[i].replace(/^\s*[-*+]\s+/, "");
        blocks.push({ type: "list-item", segments: tokenizeInline(itemText), prefix: "•" });
        i++;
      }
      continue;
    }
    if (/^\s*\d+\.\s+/.test(line)) {
      let n = 1;
      while (i < lines.length && /^\s*\d+\.\s+/.test(lines[i])) {
        const itemText = lines[i].replace(/^\s*\d+\.\s+/, "");
        blocks.push({ type: "list-item", segments: tokenizeInline(itemText), ordered: true, prefix: `${n}.` });
        n++; i++;
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
        if (/^\s*$/.test(l) || isTableSeparator(l) || /^\s{0,3}(`{3,}|~{3,})/.test(l)
          || /^(#{1,6})\s+/.test(l) || /^>\s?/.test(l) || /^\s*[-*+]\s+/.test(l)
          || /^\s*\d+\.\s+/.test(l) || /^\s*([-*_])\1{2,}\s*$/.test(l) || !l.includes("|")) break;
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
      if (/^\s*$/.test(l) || /^\s{0,3}(`{3,}|~{3,})/.test(l) || /^(#{1,6})\s+/.test(l) || /^>\s?/.test(l)
        || /^\s*[-*+]\s+/.test(l) || /^\s*\d+\.\s+/.test(l) || /^\s*([-*_])\1{2,}\s*$/.test(l)
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
  const rowHeights: number[] = [];
  for (let r = 0; r < rows.length; r++) {
    const fw = r === 0 ? "600" : "400";
    const linesPerCell: string[][] = [];
    let maxLines = 1;
    for (let c = 0; c < colCount; c++) {
      const plain = segmentsPlainText(rows[r][c]?.segments || []);
      const maxW = Math.max(1, colWidths[c] - TABLE_PAD_X * 2);
      const lines = wrapText(plain, maxW, fs, p.sans, fw);
      linesPerCell.push(lines.length ? lines : [""]);
      maxLines = Math.max(maxLines, linesPerCell[c].length);
    }
    cellLines.push(linesPerCell);
    rowHeights.push(maxLines * fs * TABLE_LH + TABLE_PAD_Y * 2);
  }
  const tableH = rowHeights.reduce((a, b) => a + b, 0);
  const tableW = Math.min(proseW, totalW);
  const plain = mb.raw || rows.map(r => r.map(c => segmentsPlainText(c.segments)).join("\t")).join("\n");
  result.push({
    kind: "md-table", id: itemId, groupIdx: gi,
    x, y, w: tableW, h: tableH,
    text: plain, color, fontSize: fs, font: p.sans, selectable: true,
    data: { rows, aligns, colWidths, cellLines, rowHeights, border: p.border },
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

interface TextLine { text: string; x: number; y: number; w: number; offset: number; fs: number; lh: number; bold?: boolean; italic?: boolean; code?: boolean; link?: string; charX?: number[]; }
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
  data?: Record<string, unknown>;
  _lines?: string[];
  _charStyles?: Array<{ bold?: boolean; italic?: boolean; code?: boolean; link?: string }>;
  _charXs?: number[][];
  _lineWidths?: number[];
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
    // 选区 offset 来自 textLines（含 wrap/trim），不能直接 slice 原始 b.text
    if (b.textLines?.length) {
      const lineParts: string[] = [];
      for (const ln of b.textLines) {
        const lineEnd = ln.offset + ln.text.length;
        if (eOff <= ln.offset || sOff >= lineEnd) continue;
        const a = Math.max(0, sOff - ln.offset);
        const c = Math.min(ln.text.length, eOff - ln.offset);
        lineParts.push(ln.text.slice(a, c));
      }
      if (lineParts.length) parts.push(lineParts.join("\n"));
    } else if (b.text) {
      parts.push(b.text.slice(sOff, Math.min(eOff, b.text.length)));
    }
  }
  return parts.join("\n");
}

function blockScrollKey(b: Block): string {
  return `${b.kind}:${b.id}:${b.text?.length ?? 0}:${(b.data?.fullH as number) ?? b.h}`;
}

// ─── Main component ──────────────────────────────────────────────────────────

export function CanvasTranscript(props: CanvasTranscriptProps) {
  let canvasEl!: HTMLCanvasElement;
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

  // hover state
  let hoverBlockIdx = -1;
  let cursorStyle = "default";

  // per-block scroll for clipped tool-content（按内容身份记，避免 rebuild 丢位置）
  const blockScrolls = new Map<string, number>();
  let rebuildRaf = 0;
  let prefixCacheSig = "";
  let prefixCacheBlocks: Block[] = [];
  let prefixCacheHeight = 0;
  let prefixCacheGroupYs: number[] = [];
  let prefixCacheUntil = 0;

  // selection state
  let selection: Selection | null = null;
  let selecting = false;
  let selStart: { block: number; offset: number } | null = null;

  // scroll physics
  let velocityY = 0;
  let rafId = 0;
  let lastWheelTime = 0;

  // spinner
  let spinPhase = 0;
  let hasBusy = false;

  // editing
  const [editing, setEditing] = createSignal<UserItem | null>(null);
  const [editStyle, setEditStyle] = createSignal<Record<string, string>>({});
  const [draft, setDraft] = createSignal("");
  const editAttachments = createImageAttachments();

  // group Y positions for scrollToGroup
  let groupYs: number[] = [];

  // images cache
  const imgCache = new Map<string, HTMLImageElement>();

  // ─── Layout ────────────────────────────────────────────────────────────────

  let editLayoutY = 0, editLayoutSide = 0, editLayoutW = 0;

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

  function closedGroupSig(g: Group): string {
    const foldKey = g.turn ? `turn-${g.turn.id ?? g.user?.id ?? 0}` : "";
    const foldOpen = foldKey ? !!(state.expanded[foldKey] ?? false) : false;
    const parts = [
      g.user ? `u:${g.user.id}:${g.user.text}:${userImagesSig(g.user.images)}` : "-",
      g.turn
        ? `t:${g.turn.id}:${g.turn.durationMs}:${g.turn.totalTokens ?? ""}:${g.turn.actualModel ?? ""}:${foldOpen}`
        : "-",
    ];
    for (const item of g.body) {
      if (item.type === "tool") {
        parts.push(
          `tool:${item.id}:${item.status}:${item.title}:${item.content.length}:${item.locations.length}:${!!state.expanded[`tool-${item.id}`]}`,
        );
      } else if ("text" in item) {
        parts.push(
          `${item.type}:${item.id}:${(item as { text: string }).text}:${!!state.expanded[`thought-${item.id}`]}`,
        );
      } else {
        parts.push(`${item.type}:${item.id}`);
      }
    }
    return parts.join("|");
  }

  function computeLayout() {
    const p = pal;
    const W = viewW;
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
    groupYs = [];
    let y = 24;

    if (!props.groups.length) {
      result.push({ kind: "hint", id: 0, groupIdx: 0, x: side, y, w: contentW, h: 60,
        text: props.emptyHint, color: p.faint, fontSize: 13, font: p.sans, selectable: true });
      y += 60;
      blocks = result; totalHeight = y + 16;
      prefixCacheSig = "";
      prefixCacheUntil = 0;
      prefixCacheBlocks = [];
      prefixCacheGroupYs = [];
      prefixCacheHeight = 0;
      return;
    }

    // 已闭合轮次布局缓存：流式输出时只重算尾部，大幅降低每帧布局成本
    let closedUntil = 0;
    const closedSigs: string[] = [];
    for (let i = 0; i < props.groups.length; i++) {
      if (!props.groups[i].turn) break;
      closedSigs.push(closedGroupSig(props.groups[i]));
      closedUntil = i + 1;
    }
    const prefixSig = `${Math.round(W)}|${p.bg}|${p.text}|${closedSigs.join("||")}`;
    const reusePrefix =
      closedUntil > 0 && prefixSig === prefixCacheSig && prefixCacheUntil === closedUntil;
    let gi = 0;
    if (reusePrefix) {
      for (const b of prefixCacheBlocks) result.push(b);
      groupYs = prefixCacheGroupYs.slice();
      y = prefixCacheHeight;
      gi = closedUntil;
    }

    for (; gi < props.groups.length; gi++) {
      const g = props.groups[gi];
      groupYs.push(y);
      const active = props.running && !g.turn;

      // user message: .msg-user margin 20px 0 16px; bubble max-width 85%
      if (g.user) {
        const item = g.user;
        y += 20; // top margin
        if (item.id !== editing()?.id) {
          const maxBubble = contentW * 0.85;
          const textLines = item.text ? wrapText(item.text, maxBubble - 34, 14, p.sans) : [];
          const lh = 14 * 1.6;
          const textH = textLines.length * lh;
          const textW = textLines.reduce((m, l) => Math.max(m, measure(l, 14, p.sans)), 0);
          // DOM .bubble-images: flex-wrap, img max 240×180, gap 6, margin-bottom 6
          const { layouts: imageLayouts, usedW: imgUsedW, stackH: imgH } =
            layoutBubbleImages(item.images, maxBubble - 32, loadImage);
          const bubbleW = Math.min(maxBubble, Math.max(72, textW + 34, imgUsedW + 32));
          const bubbleH = Math.max(30, textH + imgH + 20); // padding 10*2
          const bx = side + contentW - bubbleW;

          result.push({ kind: "user-bubble", id: item.id, groupIdx: gi,
            x: bx, y, w: bubbleW, h: bubbleH,
            text: item.text, color: p.text, bg: p.accentDim,
            border: `color-mix(in srgb, ${p.accent} 26%, transparent)`,
            // border-radius: 14px; border-bottom-right-radius: 6px
            borderRadius: [14, 14, 6, 14], fontSize: 14, lineHeight: 1.6, font: p.sans,
            selectable: true, hoverKey: `user-${item.id}`,
            data: { images: item.images, editItem: item, imageLayouts } });

          // .user-edit-btn: padding 5px, margin 0 2px 4px 0, align-self flex-end
          if (state.currentId && !state.running[state.currentId]) {
            result.push({ kind: "edit-btn", id: item.id, groupIdx: gi,
              x: bx - 28, y: y + bubbleH - 26 - 4, w: 24, h: 24,
              hoverBg: p.hover, borderRadius: 6, cursor: "pointer",
              hoverKey: `user-${item.id}`,
              clickAction: () => {
                setDraft(item.text);
                editAttachments.set(item.images ?? []);
                setEditing(item);
              } });
          }
          y += bubbleH + 16; // bottom margin
        } else {
          editLayoutY = y;
          editLayoutSide = side;
          editLayoutW = contentW;
          y += 150 + 16;
        }
      }

      // .turn-actual-model: margin -8px 0 8px auto; padding 2px 7px; border-radius 999
      if (g.turn?.actualModel) {
        const tag = `实际模型：${g.turn.actualModel}`;
        const tw = measure(tag, 11, p.mono) + 14; // padding 2*7
        result.push({ kind: "actual-model", id: g.turn.id, groupIdx: gi,
          x: side + contentW - tw, y: y - 8, w: tw, h: 18,
          text: tag, color: p.faint, bg: p.panel, border: p.borderLight,
          borderRadius: 999, fontSize: 11, font: p.mono, selectable: true,
          data: { padX: 7, padY: 2 } });
        y += 18; // -8 top absorbed + 8 bottom + content ~18
      }

      const lastConc = g.body.findLastIndex(it => it.type === "assistant" || it.type === "system");
      let firstConc = lastConc;
      if (firstConc >= 0) {
        while (firstConc > 0 && (g.body[firstConc - 1].type === "assistant" || g.body[firstConc - 1].type === "system"))
          firstConc--;
      }
      const process = firstConc < 0 ? g.body : [...g.body.slice(0, firstConc), ...g.body.slice(lastConc + 1)];
      const conclusion = firstConc < 0 ? [] : g.body.slice(firstConc, lastConc + 1);

      if (g.turn && process.length) {
        const foldKey = `turn-${g.turn.id ?? g.user?.id ?? process[0]?.id ?? 0}`;
        const open = state.expanded[foldKey] ?? process.some((it) => {
          if (it.type === "tool") return !!state.expanded[`tool-${it.id}`];
          if (it.type === "thought") return !!state.expanded[`thought-${it.id}`];
          return !!state.expanded[String(it.id)];
        });
        const label = ["已处理", fmtDuration(g.turn.durationMs),
          g.turn.totalTokens ? `· ${fmtTokens(g.turn.totalTokens)} tokens` : ""].filter(Boolean).join(" ");

        // .turn-fold: padding 4px 8px; margin 12px 0 2px -8px; font 13; gap 6
        const foldW = measure(label, 13, p.sans) + 8 + 6 + 12 + 8; // padL + gap + chev + padR
        y += 12;
        result.push({ kind: "fold", id: g.turn.id, groupIdx: gi,
          x: side + proseOff - 8, y, w: Math.min(400, foldW), h: 26,
          text: label, color: p.dim, fontSize: 13, font: p.sans,
          hoverBg: p.hover, hoverColor: p.text, borderRadius: 7, cursor: "pointer",
          data: { open, foldKey },
          clickAction: () => { toggleExpanded(foldKey, !open); } });
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
    }

    blocks = result;
    totalHeight = y + 16;

    if (closedUntil > 0) {
      if (!reusePrefix) {
        prefixCacheSig = prefixSig;
        prefixCacheUntil = closedUntil;
        prefixCacheBlocks = result.filter((b) => b.groupIdx < closedUntil);
        prefixCacheGroupYs = groupYs.slice(0, closedUntil);
        prefixCacheHeight = closedUntil < props.groups.length ? groupYs[closedUntil] : y;
      }
    } else {
      prefixCacheSig = "";
      prefixCacheUntil = 0;
      prefixCacheBlocks = [];
      prefixCacheGroupYs = [];
      prefixCacheHeight = 0;
    }
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
        clickAction: () => { toggleExpanded(key, !open); } });
      y += 6 + 24;
      if (open) {
        // .thought-body: margin-top 6px; padding 8px 14px; border-left 2px border-light
        const mdBl = parseMarkdownBlocks(item.text);
        let thY = y + 6 + 8; // margin + padding
        for (const mb of mdBl) {
          if (mb.type === "code") {
            const codeText = mb.raw || segmentsPlainText(mb.segments);
            const codeLines = wrapText(codeText, proseW - 42, 12, p.mono);
            const codeLh = 12 * 1.55;
            const codeH = codeLines.length * codeLh + 24;
            result.push({ kind: "md-code", id: item.id, groupIdx: gi,
              x: x + 14, y: thY, w: proseW - 14, h: codeH,
              text: codeText, color: p.dim, bg: p.sidebar, border: p.border,
              borderRadius: 6, fontSize: 12, lineHeight: 1.55, font: p.mono,
              selectable: true, data: { padX: 12, padY: 10 } });
            thY += codeH + 8;
          } else if (mb.type === "table") {
            thY = layoutMdTable(mb, result, item.id, gi, x + 14, thY, proseW - 28, p, {
              color: p.dim, fontSize: 12,
            });
          } else {
            const plain = segmentsPlainText(mb.segments);
            const lines = wrapText(plain, proseW - 28, 13, p.sans);
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
      if (item.text.trim() === "None") return y;
      // .msg-assistant: margin 14px 0; line-height 1.7
      y += 14;
      const mdBlocks = parseMarkdownBlocks(item.text);
      for (let mi = 0; mi < mdBlocks.length; mi++) {
        const mb = mdBlocks[mi];
        if (mb.type === "hr") {
          result.push({ kind: "md-hr", id: item.id, groupIdx: gi,
            x, y, w: proseW, h: 25, bg: p.border });
          y += 25;
        } else if (mb.type === "code") {
          const codeText = mb.raw || segmentsPlainText(mb.segments);
          const codeLines = wrapText(codeText, proseW - 28, 12.5, p.mono);
          const codeLh = 12.5 * 1.55;
          const codeH = codeLines.length * codeLh + 24; // padding 12*2
          result.push({ kind: "md-code", id: item.id, groupIdx: gi,
            x, y, w: proseW, h: codeH,
            text: codeText, color: p.text, bg: p.panel, border: p.border,
            borderRadius: 8, fontSize: 12.5, lineHeight: 1.55, font: p.mono,
            selectable: true, data: { padX: 14, padY: 12, lang: mb.lang } });
          y += codeH + 10;
        } else if (mb.type === "heading") {
          const hLevel = mb.level || 1;
          const hFs = hLevel === 1 ? 17.5 : hLevel === 2 ? 16.1 : 14.7;
          const plain = segmentsPlainText(mb.segments);
          const hLines = wrapText(plain, proseW, hFs, p.sans, "bold");
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
          const bLines = wrapText(plain, proseW - 15, 14, p.sans);
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
          const indent = 22;
          const liLines = wrapText(plain, proseW - indent, 14, p.sans);
          const liLh = 14 * 1.7;
          const liH = liLines.length * liLh;
          result.push({ kind: "md-list-item", id: item.id, groupIdx: gi,
            x, y, w: proseW, h: liH,
            text: plain, segments: mb.segments, color: p.text, fontSize: 14,
            lineHeight: 1.7, font: p.sans, selectable: true,
            data: { prefix: mb.prefix, indent } });
          y += liH + 3;
        } else if (mb.type === "table") {
          y = layoutMdTable(mb, result, item.id, gi, x, y, proseW, p);
        } else {
          const plain = segmentsPlainText(mb.segments);
          const pLines = wrapText(plain, proseW, 14, p.sans);
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
    const hasBody = item.content.length > 0 || item.locations.length > 0
      || item.rawInput !== undefined || item.rawOutput !== undefined;

    const label = displayToolTitle(stripAnsi(item.title || item.kind));
    // .tool-row margin 1px 0; .tool-line padding 3px 8px; min-height 26; gap 8
    const toolH = 26;

    result.push({ kind: "tool-header", id: item.id, groupIdx: gi,
      x, y: y + 1, w: proseW, h: toolH,
      text: label, color: item.status === "failed" ? p.red : p.dim,
      fontSize: 12, font: p.mono, hoverBg: p.hover, borderRadius: 7,
      cursor: hasBody ? "pointer" : "default",
      data: { open, busy, hasBody, kind: item.kind, status: item.status },
      clickAction: hasBody ? () => { toggleExpanded(key, !open); } : undefined });
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
      for (const content of item.content) {
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

  function paintAll() {
    const canvas = canvasEl;
    if (!canvas) return;
    const ctx = canvas.getContext("2d")!;
    const p = pal;

    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, viewW, viewH);

    const visTop = scrollY - 50;
    const visBot = scrollY + viewH + 50;
    spinPhase = (performance.now() / 800) % 1;
    hasBusy = false;

    for (let i = 0; i < blocks.length; i++) {
      const b = blocks[i];
      const sy = b.y - scrollY;
      if (sy + b.h < -50 || sy > viewH + 50) continue;

      const bx = b.x, by = sy;

      // hover highlight
      const isHover = (i === hoverBlockIdx) ||
        (b.hoverKey && blocks[hoverBlockIdx]?.hoverKey === b.hoverKey);

      ctx.save();

      // background — hoverBg for interactive blocks; edit-btn only visible on hover
      if (b.kind === "edit-btn") {
        if (isHover) {
          ctx.fillStyle = b.hoverBg || pal.hover;
          roundRect(ctx, bx, by, b.w, b.h, b.borderRadius || 6);
          ctx.fill();
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
      if (b.border && b.kind !== "edit-btn") {
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
        case "fold":
        case "thought-toggle":
          paintFoldToggle(ctx, b, bx, by, p, !!isHover);
          break;
        case "tool-header":
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
          ctx.fillText(b.text!, bx, snap(by + b.h / 2));
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

    if (hasBusy) requestRender();
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
      const lines = b._lines || (b._lines = wrapText(b.text, b.w - 34, fs, ff));
      const halfLead = (lh - fs) / 2;
      b.textLines = [];
      let offset = 0;
      for (let i = 0; i < lines.length; i++) {
        const tx = bx + 16;
        const ty = by + 10 + imgOffset + i * lh;
        ctx.fillText(lines[i], tx, snap(ty + halfLead));
        b.textLines.push({ text: lines[i], x: tx, y: ty + scrollY, w: measure(lines[i], fs, ff), offset, fs, lh });
        offset += lines[i].length;
      }
    }
  }

  function paintFoldToggle(ctx: CanvasRenderingContext2D, b: Block, bx: number, by: number, p: Palette, hover: boolean) {
    const open = b.data?.open as boolean;
    const isThought = b.kind === "thought-toggle";
    // fold: padding 4px 8px, gap 6; thought: padding 3px 6px, gap 5
    const padL = isThought ? 6 : 8;
    const gap = isThought ? 5 : 6;
    const color = hover && b.hoverColor ? b.hoverColor : (b.color || p.dim);

    ctx.save();
    ctx.strokeStyle = isThought ? (hover ? p.dim : p.faint) : p.faint;
    ctx.lineWidth = 1.5;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.beginPath();
    const cx = bx + padL + 4;
    const cy = by + b.h / 2;
    if (open) { ctx.moveTo(cx - 3, cy - 1.5); ctx.lineTo(cx, cy + 1.5); ctx.lineTo(cx + 3, cy - 1.5); }
    else { ctx.moveTo(cx - 1.5, cy - 3); ctx.lineTo(cx + 1.5, cy); ctx.lineTo(cx - 1.5, cy + 3); }
    ctx.stroke();
    ctx.restore();

    ctx.font = `${b.fontSize}px ${b.font}`;
    ctx.fillStyle = color;
    ctx.textBaseline = "middle";
    ctx.fillText(b.text!, bx + padL + 8 + gap, snap(by + b.h / 2));
  }

  function paintToolHeader(ctx: CanvasRenderingContext2D, b: Block, bx: number, by: number, p: Palette) {
    const { open, busy, hasBody, kind, status } = b.data as { open: boolean; busy: boolean; hasBody: boolean; kind: string; status: string };
    // padding 3px 8px; gap 8 — match DOM .tool-line: icon → title → busy → chevron
    const padX = 8;
    const gap = 8;
    const midY = snap(by + b.h / 2);

    // icon (14px) at left
    drawToolIcon(ctx, kind, bx + padX, by + (b.h - 14) / 2, 14, p.faint);

    // label after icon + gap 8; reserve trailing icons so long titles ellipsize
    const textX = bx + padX + 14 + gap;
    let trailReserve = padX;
    if (busy || status === "failed") trailReserve += gap + 12;
    if (hasBody) trailReserve += gap + 12;
    ctx.font = `${b.fontSize}px ${b.font}`;
    ctx.fillStyle = b.color || p.dim;
    ctx.textBaseline = "middle";
    const maxTextW = Math.max(20, b.w - (textX - bx) - trailReserve);
    let label = b.text!;
    if (measure(label, b.fontSize!, b.font!) > maxTextW) {
      while (label.length > 1 && measure(label + "…", b.fontSize!, b.font!) > maxTextW) label = label.slice(0, -1);
      label += "…";
    }
    ctx.fillText(label, textX, midY);

    // trailing status / chevron sit after the label (not flush-right)
    let nextX = textX + measure(label, b.fontSize!, b.font!) + gap;
    if (busy) {
      hasBusy = true;
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
      roundRect(ctx, bx, by, b.w, b.h, b.borderRadius || 0);
      ctx.clip();
    }

    if (b.text) {
      ctx.font = `${fw} ${fs}px ${ff}`;
      const color = b.color || pal.text;
      ctx.fillStyle = color;
      ctx.textBaseline = "top";
      const innerX = b.data?.borderLeft ? bx + (padX || 14) : bx + padX;
      const innerW = b.data?.borderLeft ? b.w - (padX || 14) : b.w - padX * 2;
      const lines = b._lines || (b._lines = wrapText(b.text, Math.max(1, innerW), fs, ff, fw));
      b.textLines = [];
      let offset = 0;
      for (let i = 0; i < lines.length; i++) {
        const tx = innerX;
        const ty = by + padY + i * lh - bScroll;
        if (ty + lh > by - 10 && ty < by + b.h + 10) {
          ctx.fillText(lines[i], tx, snap(ty + halfLead));
          if (hover && b.data?.underlineOnHover) {
            const tw = measure(lines[i], fs, ff, fw);
            ctx.fillRect(tx, snap(ty + halfLead) + fs, tw, 1);
          }
        }
        b.textLines.push({ text: lines[i], x: tx, y: ty + scrollY, w: measure(lines[i], fs, ff, fw), offset, fs, lh });
        offset += lines[i].length;
      }
    }

    if (clipped) {
      ctx.restore();
      // scrollbar
      const fullH = (b.data?.fullH as number) || b.h;
      const maxBScroll = fullH - b.h;
      const ratio = b.h / fullH;
      const thumbH = Math.max(20, b.h * ratio);
      const scrollRatio = maxBScroll > 0 ? bScroll / maxBScroll : 0;
      const thumbY = by + scrollRatio * (b.h - thumbH);
      const trackX = bx + b.w - 7;
      ctx.fillStyle = pal.scroll;
      ctx.globalAlpha = 0.6;
      roundRect(ctx, trackX, thumbY, 4, thumbH, 2);
      ctx.fill();
      ctx.globalAlpha = 1;
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
      ctx.fillText(prefix, bx, snap(by + halfLead));
    }

    if (!segments || segments.length === 0) {
      if (b.text) {
        ctx.font = `${baseFw} ${fs}px ${ff}`;
        ctx.fillStyle = b.color || pal.text;
        const lines = wrapText(b.text, maxW, fs, ff, baseFw);
        let offset = 0;
        for (let i = 0; i < lines.length; i++) {
          const ty = by + i * lh;
          ctx.fillText(lines[i], startX, snap(ty + halfLead));
          b.textLines.push({ text: lines[i], x: startX, y: ty + scrollY, w: measure(lines[i], fs, ff, baseFw), offset, fs, lh });
          offset += lines[i].length;
        }
      }
      return;
    }

    const plainText = b.text || segmentsPlainText(segments);
    const wrappedLines = b._lines || (b._lines = wrapText(plainText, maxW, fs, ff, baseFw));

    let charStyles = b._charStyles;
    if (!charStyles) {
      charStyles = [];
      for (const seg of segments) {
        for (let si = 0; si < seg.text.length; si++) {
          charStyles.push({ bold: seg.bold, italic: seg.italic, code: seg.code, link: seg.link });
        }
      }
      b._charStyles = charStyles;
    }

    // Pre-compute charX and lineWidths once per rebuild (cached in block)
    let cachedCharXs = b._charXs;
    let cachedLineWidths = b._lineWidths;
    if (!cachedCharXs) {
      cachedCharXs = [];
      cachedLineWidths = [];
      let off = 0;
      for (let li = 0; li < wrappedLines.length; li++) {
        const line = wrappedLines[li];
        const charX: number[] = new Array(line.length + 1);
        let cx = 0, ri = 0;
        while (ri < line.length) {
          const cs = charStyles[off + ri] || {};
          let runEnd = ri + 1;
          while (runEnd < line.length) {
            const ns = charStyles[off + runEnd] || {};
            if (ns.bold !== cs.bold || ns.italic !== cs.italic || ns.code !== cs.code || ns.link !== cs.link) break;
            runEnd++;
          }
          const run = line.slice(ri, runEnd);
          const segFw = cs.bold ? "bold" : baseFw;
          const segFs = cs.code ? 12.5 : fs;
          const segFf = cs.code ? pal.mono : ff;
          const rw = measure(run, segFs, segFf, segFw);
          const codePad = cs.code ? 6 : 0;
          for (let c = 0; c < run.length; c++) {
            charX[ri + c] = cx + codePad + measure(run.slice(0, c), segFs, segFf, segFw);
          }
          cx += rw + codePad * 2;
          ri = runEnd;
        }
        charX[line.length] = cx;
        cachedCharXs.push(charX);
        cachedLineWidths!.push(cx);
        off += line.length;
      }
      b._charXs = cachedCharXs;
      b._lineWidths = cachedLineWidths;
    }

    let globalOffset = 0;
    for (let li = 0; li < wrappedLines.length; li++) {
      const line = wrappedLines[li];
      const ty = by + li * lh;
      const tySnap = snap(ty + halfLead);
      const charX = cachedCharXs[li];
      const lineW = cachedLineWidths![li];
      const lineEntry: TextLine = { text: line, x: startX, y: ty + scrollY, w: lineW, offset: globalOffset, fs, lh, charX };
      b.textLines.push(lineEntry);

      // Render styled runs
      let cx = 0, ri = 0;
      while (ri < line.length) {
        const cs = charStyles[globalOffset + ri] || {};
        let runEnd = ri + 1;
        while (runEnd < line.length) {
          const ns = charStyles[globalOffset + runEnd] || {};
          if (ns.bold !== cs.bold || ns.italic !== cs.italic || ns.code !== cs.code || ns.link !== cs.link) break;
          runEnd++;
        }
        const run = line.slice(ri, runEnd);
        const segFw = cs.bold ? "bold" : baseFw;
        const segFs = cs.code ? 12.5 : fs;
        const segFf = cs.code ? pal.mono : ff;
        const segStyle = cs.italic ? "italic" : "normal";
        const rw = measure(run, segFs, segFf, segFw);
        const codePad = cs.code ? 6 : 0;

        if (cs.code) {
          ctx.fillStyle = pal.panel;
          roundRect(ctx, startX + cx - 1, tySnap - 2, rw + codePad * 2 + 2, segFs + 5, 5);
          ctx.fill();
          ctx.strokeStyle = pal.border;
          ctx.lineWidth = 1;
          roundRect(ctx, startX + cx - 0.5, tySnap - 1.5, rw + codePad * 2 + 1, segFs + 4, 5);
          ctx.stroke();
        }

        ctx.fillStyle = cs.link ? pal.blue : (b.color || pal.text);
        ctx.font = `${segStyle} ${segFw} ${segFs}px ${segFf}`;
        ctx.fillText(run, startX + cx + codePad, tySnap);
        if (cs.link) {
          ctx.fillRect(startX + cx + codePad, tySnap + segFs + 1, rw, 1);
        }
        cx += rw + codePad * 2;
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

    ctx.save();
    roundRect(ctx, bx, by, b.w, b.h, b.borderRadius || 8);
    ctx.clip();

    ctx.font = `${fs}px ${ff}`;
    ctx.fillStyle = b.color || p.text;
    ctx.textBaseline = "top";
    const lines = (b.text || "").split("\n");
    b.textLines = [];
    let offset = 0;
    for (let i = 0; i < lines.length; i++) {
      const tx = bx + padX;
      const ty = by + padY + i * lh;
      if (ty + lh < by - 10 || ty > by + b.h + 10) { offset += lines[i].length + 1; continue; }
      ctx.fillText(lines[i], tx, snap(ty + halfLead));
      b.textLines.push({ text: lines[i], x: tx, y: ty + scrollY, w: measure(lines[i], fs, ff), offset, fs, lh });
      offset += lines[i].length + 1;
    }
    ctx.restore();
  }

  function paintMdTable(ctx: CanvasRenderingContext2D, b: Block, bx: number, by: number, p: Palette) {
    const fs = b.fontSize || TABLE_FS;
    const ff = b.font || p.sans;
    const colWidths = (b.data?.colWidths as number[]) || [];
    const cellLines = (b.data?.cellLines as string[][][]) || [];
    const rowHeights = (b.data?.rowHeights as number[]) || [];
    const aligns = (b.data?.aligns as Array<"left" | "center" | "right">) || [];
    const border = (b.data?.border as string) || p.border;
    const textLines: TextLine[] = [];
    let charOff = 0;
    let rowY = by;
    let absRowY = b.y;

    for (let r = 0; r < cellLines.length; r++) {
      const rh = rowHeights[r] || fs * TABLE_LH + TABLE_PAD_Y * 2;
      let cellX = bx;
      if (r === 0) {
        ctx.fillStyle = p.panel;
        ctx.globalAlpha = 0.55;
        ctx.fillRect(bx, rowY, b.w, rh);
        ctx.globalAlpha = 1;
      }
      for (let c = 0; c < colWidths.length; c++) {
        const cw = colWidths[c];
        const lines = cellLines[r]?.[c] || [""];
        const fw = r === 0 ? "600" : "400";
        const align = aligns[c] || "left";
        ctx.strokeStyle = border;
        ctx.lineWidth = 1;
        ctx.strokeRect(cellX + 0.5, rowY + 0.5, cw - 1, rh - 1);
        ctx.fillStyle = b.color || p.text;
        ctx.font = `${fw} ${fs}px ${ff}`;
        ctx.textBaseline = "top";
        const lineH = fs * TABLE_LH;
        const contentH = lines.length * lineH;
        const textTop = rowY + Math.max(TABLE_PAD_Y, (rh - contentH) / 2);
        const absTextTop = absRowY + Math.max(TABLE_PAD_Y, (rh - contentH) / 2);
        for (let li = 0; li < lines.length; li++) {
          const line = lines[li];
          const tw = measure(line, fs, ff, fw);
          let tx = cellX + TABLE_PAD_X;
          if (align === "center") tx = cellX + (cw - tw) / 2;
          else if (align === "right") tx = cellX + cw - TABLE_PAD_X - tw;
          ctx.fillText(line, tx, snap(textTop + li * lineH));
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
          });
          charOff += line.length + 1;
        }
        cellX += cw;
      }
      rowY += rh;
      absRowY += rh;
    }
    b.textLines = textLines;
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

  function paintSelection(ctx: CanvasRenderingContext2D) {
    if (!selection) return;
    ctx.save();
    ctx.fillStyle = "rgba(74, 144, 217, 0.35)";
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
      if (clipped) {
        ctx.save();
        roundRect(ctx, b.x, b.y - scrollY, b.w, b.h, b.borderRadius || 0);
        ctx.clip();
      }
      for (const ln of b.textLines) {
        const lineEnd = ln.offset + ln.text.length;
        if (eOff <= ln.offset || sOff >= lineEnd) continue;
        // 工具详情内部滚动后，裁切区外的行不再画高亮，避免漂到卡片上方
        if (clipped && (ln.y + ln.lh <= b.y || ln.y >= b.y + b.h)) continue;
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
        const halfLead = (ln.lh - ln.fs) / 2;
        ctx.fillRect(x0, ln.y - scrollY + halfLead, x1 - x0, ln.fs);
      }
      if (clipped) ctx.restore();
    }
    ctx.restore();
  }

  function paintScrollbar(ctx: CanvasRenderingContext2D) {
    const trackW = 4, trackX = viewW - trackW - 3;
    const ratio = viewH / totalHeight;
    const thumbH = Math.max(20, viewH * ratio);
    const thumbY = (scrollY / maxScroll) * (viewH - thumbH);
    ctx.fillStyle = pal.scroll;
    roundRect(ctx, trackX, thumbY, trackW, thumbH, 3);
    ctx.fill();
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

  function hitTextPosition(clientX: number, clientY: number): { block: number; offset: number } | null {
    const rect = canvasEl.getBoundingClientRect();
    const mx = clientX - rect.left;
    const my = clientY - rect.top + scrollY;
    let best: { block: number; offset: number; xDist: number } | null = null;
    for (let i = 0; i < blocks.length; i++) {
      const b = blocks[i];
      if (!b.textLines || !b.selectable) continue;
      if (b.data?.clipped && (my < b.y || my >= b.y + b.h)) continue;
      for (const ln of b.textLines) {
        if (b.data?.clipped && (ln.y + ln.lh <= b.y || ln.y >= b.y + b.h)) continue;
        if (my < ln.y || my >= ln.y + ln.lh) continue;
        // 表格同行多列共享同一 y：按水平距离选最近行，避免永远命中左侧单元格
        const lineRight = ln.x + Math.max(ln.w, 1);
        const xDist = mx < ln.x ? ln.x - mx : mx > lineRight ? mx - lineRight : 0;
        if (best && xDist >= best.xDist) continue;
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
        best = { block: i, offset: off, xDist };
      }
    }
    return best ? { block: best.block, offset: best.offset } : null;
  }

  function onMouseMove(e: MouseEvent) {
    if (selecting) {
      const pos = hitTextPosition(e.clientX, e.clientY);
      if (pos && selStart) {
        selection = { startBlock: selStart.block, startOffset: selStart.offset,
          endBlock: pos.block, endOffset: pos.offset };
        paintAll();
      }
      return;
    }
    const idx = hitTest(e.clientX, e.clientY);
    if (idx !== hoverBlockIdx) {
      hoverBlockIdx = idx;
      const b = idx >= 0 ? blocks[idx] : null;
      canvasEl.style.cursor = b?.cursor || (b?.selectable ? "text" : "default");
      paintAll();
    }
  }

  function onMouseDown(e: MouseEvent) {
    velocityY = 0;
    const idx = hitTest(e.clientX, e.clientY);
    const b = idx >= 0 ? blocks[idx] : null;

    if (b?.selectable) {
      const pos = hitTextPosition(e.clientX, e.clientY);
      if (pos) {
        selecting = true;
        selStart = pos;
        selection = { startBlock: pos.block, startOffset: pos.offset, endBlock: pos.block, endOffset: pos.offset };
        canvasEl.focus();
        paintAll();
        return;
      }
    }
    selection = null;
    paintAll();
  }

  function onMouseUp(_e: MouseEvent) {
    if (selecting) {
      selecting = false;
      if (selection && selection.startBlock === selection.endBlock && selection.startOffset === selection.endOffset) {
        selection = null;
      }
      paintAll();
      return;
    }
  }

  function onClick(e: MouseEvent) {
    const idx = hitTest(e.clientX, e.clientY);
    const b = idx >= 0 ? blocks[idx] : null;
    if (b?.clickAction && !selecting) b.clickAction();
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
            paintAll();
            return;
          }
        }
      }
    }

    scrollY = Math.max(0, Math.min(maxScroll, scrollY + dy));
    keepBottom = maxScroll - scrollY <= 2;
    velocityY = dy * 0.3;
    lastWheelTime = performance.now();
    if (editing()) {
      setEditStyle({
        left: `${editLayoutSide}px`,
        top: `${Math.max(8, editLayoutY - scrollY)}px`,
        width: `${editLayoutW}px`
      });
    }
    props.onScroll?.(scrollY, maxScroll, true);
    paintAll();
  }

  function onCopy(e: ClipboardEvent) {
    if (!selection) return;
    const text = selectionText(blocks, selection);
    if (!text) return;
    e.preventDefault();
    e.clipboardData?.setData("text/plain", text);
  }

  // ─── Scroll physics & render loop ─────────────────────────────────────────

  function requestRender() {
    if (rafId) return;
    rafId = requestAnimationFrame(tick);
  }

  function tick() {
    rafId = 0;
    if (Math.abs(velocityY) > 0.5) {
      scrollY = Math.max(0, Math.min(maxScroll, scrollY + velocityY));
      velocityY *= 0.92;
      keepBottom = maxScroll - scrollY <= 2;
      props.onScroll?.(scrollY, maxScroll, true);
      paintAll();
      requestRender();
    } else if (hasBusy) {
      paintAll();
    }
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

  function rebuild() {
    pal = readPalette();
    const oldScroll = scrollY;
    const wasBottom = keepBottom || maxScroll - scrollY <= 2;
    computeLayout();
    const liveKeys = new Set(
      blocks.filter((b) => b.data?.clipped).map((b) => blockScrollKey(b)),
    );
    for (const key of [...blockScrolls.keys()]) {
      if (!liveKeys.has(key)) blockScrolls.delete(key);
    }
    maxScroll = Math.max(0, totalHeight - viewH);
    if (wasBottom) scrollY = maxScroll;
    else scrollY = Math.max(0, Math.min(maxScroll, oldScroll));
    if (editing()) {
      setEditStyle({
        left: `${editLayoutSide}px`,
        top: `${Math.max(8, editLayoutY - scrollY)}px`,
        width: `${editLayoutW}px`
      });
    }
    props.onScroll?.(scrollY, maxScroll, false);
    paintAll();
  }

  function scheduleRebuild() {
    if (rebuildRaf) return;
    rebuildRaf = requestAnimationFrame(() => {
      rebuildRaf = 0;
      rebuild();
    });
  }

  function resizeCanvas() {
    const w = canvasEl.clientWidth;
    const h = canvasEl.clientHeight;
    if (w === viewW && h === viewH && dpr === devicePixelRatio) return;
    dpr = devicePixelRatio || 1;
    viewW = w; viewH = h;
    canvasEl.width = Math.round(w * dpr);
    canvasEl.height = Math.round(h * dpr);
    maxScroll = Math.max(0, totalHeight - viewH);
    if (keepBottom) scrollY = maxScroll;
  }

  // ─── Lifecycle ─────────────────────────────────────────────────────────────

  onMount(() => {
    resizeCanvas();
    rebuild();

    const ro = new ResizeObserver(() => { resizeCanvas(); rebuild(); });
    ro.observe(canvasEl);

    const mo = new MutationObserver(rebuild);
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });

    canvasEl.addEventListener("mousemove", onMouseMove);
    canvasEl.addEventListener("mousedown", onMouseDown);
    canvasEl.addEventListener("mouseup", onMouseUp);
    canvasEl.addEventListener("click", onClick);
    canvasEl.addEventListener("wheel", onWheel, { passive: false });
    canvasEl.addEventListener("copy", onCopy);

    props.ref?.({
      scrollToBottom() { keepBottom = true; scrollY = maxScroll; paintAll(); props.onScroll?.(scrollY, maxScroll, false); },
      scrollToGroup(idx) { if (groupYs[idx] != null) { scrollY = Math.max(0, Math.min(maxScroll, groupYs[idx] - 20)); keepBottom = false; paintAll(); props.onScroll?.(scrollY, maxScroll, false); } },
      scrollBy(delta) { scrollY = Math.max(0, Math.min(maxScroll, scrollY + delta)); keepBottom = maxScroll - scrollY <= 2; paintAll(); props.onScroll?.(scrollY, maxScroll, true); },
      isAtBottom() { return maxScroll - scrollY <= 2; },
      scrollTop() { return scrollY; },
      maxScrollTop() { return maxScroll; },
      activeGroup() { let a = -1; for (let i = 0; i < groupYs.length; i++) { if (groupYs[i] <= scrollY + 32) a = i; } return a; },
      hasFocusedInput() { return !!editing(); },
    });

    onCleanup(() => {
      ro.disconnect();
      mo.disconnect();
      if (rebuildRaf) cancelAnimationFrame(rebuildRaf);
      if (rafId) cancelAnimationFrame(rafId);
      canvasEl.removeEventListener("mousemove", onMouseMove);
      canvasEl.removeEventListener("mousedown", onMouseDown);
      canvasEl.removeEventListener("mouseup", onMouseUp);
      canvasEl.removeEventListener("click", onClick);
      canvasEl.removeEventListener("wheel", onWheel);
      canvasEl.removeEventListener("copy", onCopy);
    });
  });

  createEffect(() => {
    // track reactive dependencies
    for (const g of props.groups) {
      if (g.user) void `${g.user.text}:${g.user.images?.length ?? 0}`;
      for (const item of g.body) {
        if ("text" in item) void item.text;
        if (item.type === "tool") void `${item.status}:${item.title}:${item.content.length}:${item.locations.length}`;
      }
      if (g.turn) void `${g.turn.durationMs}:${g.turn.totalTokens ?? ""}`;
    }
    props.running; props.preview;
    JSON.stringify(state.expanded);
    void editing()?.id;
    scheduleRebuild();
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
    <div class="canvas-transcript-host" ref={hostEl}>
      <canvas ref={canvasEl} class="transcript-canvas-only" tabindex="0" aria-label="会话记录" />
      {editing() && (
        <div class="canvas-prompt-editor" style={editStyle()}>
          <ImageAttachmentStrip images={editAttachments.images()} onRemove={editAttachments.remove} />
          <textarea
            value={draft()}
            onPaste={editAttachments.onPaste}
            onInput={(e) => setDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); saveEdit(); }
              if (e.key === "Escape") setEditing(null);
            }}
            ref={(el) => queueMicrotask(() => { el.focus(); el.setSelectionRange(el.value.length, el.value.length); })}
          />
          <div>
            <span>发送后将从此处重新开始会话</span>
            <button class="btn secondary small" onClick={() => setEditing(null)}>取消</button>
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
