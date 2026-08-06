/**
 * canvas transcript 布局层：把 Group / PermissionRequest 布局成可绘制、可命中、
 * 可选择文本的块树。所有几何尺寸与 app.css 中 DOM 版一致。
 */
import { convertFileSrc } from "@tauri-apps/api/core";
import { diffLines } from "diff";
import { marked } from "marked";
import { relPath } from "../components/EditedFilesCard";
import { FILE_REFERENCE_RE, IMAGE_FILE_RE, WHOLE_FILE_REFERENCE_RE } from "../components/Markdown";
import type { Group } from "../components/TurnGroup";
import { fmtDuration, fmtTokens } from "../components/TurnGroup";
import { isExpanded, state } from "../store";
import type { Item, PermissionRequest, ToolContent, ToolItem } from "../types";
import { agentLabel, displayToolTitle, stripAnsi } from "../utils";
import {
  type Action,
  type ChipSpec,
  type LLine,
  type Seg,
  type ThemeColors,
  baselineOf,
  chipWidth,
  drawIcon,
  drawSpinner,
  ellipsis,
  expandTabs,
  font,
  measure,
  metrics,
  roundRectPath,
  toolIconName,
  wrapSegs,
} from "./base";

/* ===== 块模型 ===== */

export interface Region {
  x: number;
  y: number;
  w: number;
  h: number;
  action: Action;
  cursor: string;
  title?: string;
  groupId?: string;
}

export interface TRun {
  text: string;
  x: number;
  w: number;
  f: string;
  color: string;
  flags: number;
  cs: number;
  ce: number;
  action?: Action;
  chip?: ChipSpec;
}

export interface TLine {
  x: number;
  y: number;
  h: number;
  base: number;
  runs: TRun[];
  /**  triple-click 选块用的块号（装配时赋值） */
  blockId: number;
  bg?: string;
  bgX?: number;
  bgW?: number;
  /** 行首符号（diff 的 +/-） */
  sign?: string;
  signX?: number;
  signColor?: string;
}

export interface ScrollBox {
  x: number;
  y: number;
  w: number;
  h: number;
  contentH: number;
  key: string;
  scrollTop: number;
  /** 内容坐标（含内边距，y 从 0 起） */
  lines: TLine[];
  /** 随内容滚动的命中区（内容坐标） */
  regions: Region[];
  /** 不随内容滚动的命中区（盒子坐标，如复制按钮） */
  fixedRegions: Region[];
  paint: (ctx: CanvasRenderingContext2D, view: View) => void;
}

export interface Block {
  y: number;
  h: number;
  regions: Region[];
  lines: TLine[];
  boxes: ScrollBox[];
  paint: (ctx: CanvasRenderingContext2D, view: View) => void;
}

export interface View {
  theme: ThemeColors;
  top: number;
  bottom: number;
  hover: Region | null;
  hoverAction: Action | null;
  hoverGroup: string | null;
  selA: number;
  selB: number;
  now: number;
  copied: Set<string>;
}

export interface DocSection {
  top: number;
  blocks: Block[];
  hasSpinner: boolean;
}

export interface Doc {
  sections: DocSection[];
  /** 每个会话分组顶部的文档坐标（含顶部内边距），下标 = group index */
  groupTops: number[];
  height: number;
  hasSpinner: boolean;
  /** 文档坐标下的主流程文本行（按字符序），用于命中 */
  lines: { line: TLine; top: number }[];
  /** 复制用：全部可选文本行（含滚动盒内），按字符序 */
  copyOrder: { line: TLine; top: number; box?: ScrollBox }[];
  boxes: { box: ScrollBox; top: number }[];
  regions: { region: Region; top: number }[];
  charTotal: number;
  /** 增量装配：前 stableSections 个 section 的 lines/boxes/regions/charN 可复用 */
  stableSections: number;
  stableCharN: number;
  stableBlockId: number;
  /** 每个 section 边界处的累积 charN 和 blockId（用于增量装配 O(1) 查找） */
  sectionCharN: number[];
  sectionBlockId: number[];
}

export interface GroupLayout {
  blocks: Block[];
  height: number;
  hasSpinner: boolean;
}

export interface PermState {
  answers: (reqKey: string) => string[][];
  custom: (reqKey: string) => string[];
}

export interface LayoutEnv {
  theme: ThemeColors;
  x0: number;
  width: number;
  reveal: (item: { id: number; text: string }) => string;
  image: (src: string) => { w: number; h: number; img: HTMLImageElement } | null;
  scrollPos: Map<string, number>;
  threadId: string;
  running: boolean;
  editingItemId: number | null;
  perm: PermState;
  /** 提示块里显示的工作目录（worktree 会话为源仓库） */
  cwd: string;
}

const F_UNDER = 1;
const F_STRIKE = 2;

/* ===== 文本行放置 / 绘制 ===== */

interface Flow {
  blocks: Block[];
  y: number;
  x0: number;
  width: number;
  theme: ThemeColors;
  spinners: number;
}

function toTLine(line: LLine, x: number, y: number): TLine {
  return {
    x,
    y,
    h: line.h,
    base: line.asc,
    blockId: -1,
    runs: line.runs.map((r) => ({
      text: r.seg.t.slice(r.s, r.e),
      x: x + r.x,
      w: r.w,
      f: r.seg.f,
      color: r.seg.color,
      flags: r.seg.flags ?? 0,
      cs: 0,
      ce: 0,
      action: r.seg.action,
      chip: r.seg.chip,
    })),
  };
}

function placeLines(flow: Flow, lines: LLine[], x: number, y: number): { tlines: TLine[]; height: number } {
  const tlines: TLine[] = [];
  let cy = y;
  for (const line of lines) {
    tlines.push(toTLine(line, x, cy));
    cy += line.h;
  }
  return { tlines, height: cy - y };
}

function paintChip(ctx: CanvasRenderingContext2D, view: View, run: TRun, line: TLine): void {
  const m = metrics(run.f);
  const hovered = !!run.action && view.hoverAction === run.action;
  if (run.chip?.kind === "file") {
    const top = line.y + line.base - m.ascent - 1;
    const h = m.ascent + m.descent + 2;
    roundRectPath(ctx, run.x, top, run.w, h, 5);
    ctx.fillStyle = hovered ? view.theme.bgHover : view.theme.bgPanel;
    ctx.fill();
    drawIcon(ctx, "file", run.x + 5, top + h / 2 - 6.5, 13, view.theme.blue);
    if (hovered) {
      ctx.strokeStyle = view.theme.blue;
      ctx.lineWidth = 1;
      const ty = line.y + line.base + 1.5;
      ctx.beginPath();
      ctx.moveTo(run.x + 22, ty);
      ctx.lineTo(run.x + run.w - 5, ty);
      ctx.stroke();
    }
  } else {
    const top = line.y + line.base - m.ascent - 2;
    const h = m.ascent + m.descent + 4;
    roundRectPath(ctx, run.x, top, run.w, h, 5);
    ctx.fillStyle = view.theme.bgPanel;
    ctx.fill();
    ctx.strokeStyle = view.theme.border;
    ctx.lineWidth = 1;
    ctx.stroke();
  }
}

export function paintTextLines(ctx: CanvasRenderingContext2D, view: View, lines: TLine[]): void {
  const hasSel = view.selA >= 0 && view.selB > view.selA;
  // 第一遍：选中背景
  if (hasSel) {
    ctx.fillStyle = view.theme.selection;
    for (const line of lines) {
      if (line.y + line.h < view.top || line.y > view.bottom) continue;
      for (const run of line.runs) {
        if (run.ce <= view.selA || run.cs >= view.selB) continue;
        const os = Math.max(view.selA, run.cs) - run.cs;
        const oe = Math.min(view.selB, run.ce) - run.cs;
        const x0 = run.x + measure(run.text.slice(0, os), run.f);
        const w = measure(run.text.slice(os, oe), run.f);
        ctx.fillRect(x0, line.y, w, line.h);
      }
    }
  }
  // 第二遍：chip 背景与文字
  for (const line of lines) {
    if (line.y + line.h < view.top || line.y > view.bottom) continue;
    if (line.bg) {
      ctx.fillStyle = line.bg;
      ctx.fillRect(line.bgX ?? line.x, line.y, line.bgW ?? line.runs.reduce((s, r) => s + r.w, 0), line.h);
    }
    if (line.sign) {
      ctx.font = line.runs[0]?.f ?? "12px monospace";
      ctx.fillStyle = line.signColor ?? "";
      ctx.fillText(line.sign, line.signX ?? line.x - 16, line.y + line.base);
    }
    for (const run of line.runs) {
      if (run.chip) paintChip(ctx, view, run, line);
      const textX = run.chip?.kind === "file" ? run.x + 22 : run.chip ? run.x + 7 : run.x;
      ctx.font = run.f;
      ctx.fillStyle = run.color;
      ctx.fillText(run.text, textX, line.y + line.base);
      if (run.flags & F_UNDER || (run.action && view.hoverAction === run.action && run.action.kind !== "copy")) {
        ctx.strokeStyle = run.color;
        ctx.lineWidth = 1;
        const uy = line.y + line.base + 1.5;
        ctx.beginPath();
        ctx.moveTo(run.x, uy);
        ctx.lineTo(run.x + run.w, uy);
        ctx.stroke();
      }
      if (run.flags & F_STRIKE) {
        ctx.strokeStyle = run.color;
        ctx.lineWidth = 1;
        const sy = line.y + line.base - 4;
        ctx.beginPath();
        ctx.moveTo(run.x, sy);
        ctx.lineTo(run.x + run.w, sy);
        ctx.stroke();
      }
    }
  }
}

function paintCopyButton(
  ctx: CanvasRenderingContext2D,
  view: View,
  r: Region,
  visible: boolean,
): void {
  const copied = view.copied.has(String(r.action.id));
  if (!visible && !copied) return;
  roundRectPath(ctx, r.x, r.y, r.w, r.h, 6);
  ctx.fillStyle = view.theme.bgPanel;
  ctx.fill();
  ctx.strokeStyle = view.theme.borderLight;
  ctx.lineWidth = 1;
  ctx.stroke();
  drawIcon(ctx, copied ? "check" : "copy", r.x + 5, r.y + 5, 13, copied ? view.theme.accent : view.theme.textFaint);
}

/* ===== Markdown ===== */

type MkToken = { type: string; [k: string]: unknown };

/** 缓存 marked.lexer 结果，避免流式期间对未变前缀重复解析 */
const lexerCache = new Map<string, MkToken[]>();
let lexerCacheSize = 0;
const LEXER_CACHE_LIMIT = 4 * 1024 * 1024; // 4MB

function cachedLexer(src: string): MkToken[] {
  const hit = lexerCache.get(src);
  if (hit) return hit;
  let tokens: MkToken[];
  try {
    tokens = marked.lexer(src) as unknown as MkToken[];
  } catch {
    tokens = [{ type: "paragraph", text: src, tokens: [{ type: "text", text: src }] }];
  }
  const size = src.length * 4;
  if (size < LEXER_CACHE_LIMIT / 2) {
    lexerCache.set(src, tokens);
    lexerCacheSize += size;
    while (lexerCacheSize > LEXER_CACHE_LIMIT) {
      const oldest = lexerCache.keys().next().value;
      if (oldest === undefined) break;
      lexerCacheSize -= oldest.length * 4;
      lexerCache.delete(oldest);
    }
  }
  return tokens;
}

interface InlineSt {
  bold?: boolean;
  italic?: boolean;
  strike?: boolean;
  color?: string;
  /** 链接 / 文件引用的蓝色（不随引用块变暗） */
  blue?: string;
  size: number;
  markFiles: boolean;
}

function fileRefSeg(pathWithLine: string, color: string): Seg {
  const lineMatch = pathWithLine.match(/:(\d+)$/);
  const path = lineMatch ? pathWithLine.slice(0, -lineMatch[0].length) : pathWithLine;
  return {
    t: pathWithLine,
    f: font(12.5, { mono: true }),
    color,
    chip: { kind: "file", path, line: lineMatch ? Number(lineMatch[1]) : undefined },
    action: {
      kind: "file",
      path,
      line: lineMatch ? Number(lineMatch[1]) : undefined,
      title: IMAGE_FILE_RE.test(path) ? `打开图片 ${path}` : `打开文件 ${pathWithLine}`,
    },
  };
}

function pushTextWithFileRefs(out: Seg[], text: string, st: InlineSt, color: string): void {
  const f = font(st.size, { bold: st.bold, italic: st.italic, mono: false });
  const flags = st.strike ? F_STRIKE : 0;
  if (!st.markFiles) {
    if (text) out.push({ t: text, f, color, flags });
    return;
  }
  FILE_REFERENCE_RE.lastIndex = 0;
  let end = 0;
  for (const match of text.matchAll(FILE_REFERENCE_RE)) {
    const idx = match.index ?? 0;
    if (idx > end) out.push({ t: text.slice(end, idx), f, color, flags });
    out.push(fileRefSeg(match[0], st.blue ?? color));
    end = idx + match[0].length;
  }
  if (end < text.length) out.push({ t: text.slice(end), f, color, flags });
}

function inlineSegs(tokens: MkToken[], st: InlineSt, out: Seg[]): void {
  const color = st.color ?? "";
  for (const tok of tokens) {
    switch (tok.type) {
      case "text": {
        const text = String(tok.text ?? "").replace(/\s+/g, " ");
        if (Array.isArray(tok.tokens) && tok.tokens.length > 0) {
          inlineSegs(tok.tokens as MkToken[], st, out);
        } else {
          pushTextWithFileRefs(out, text, st, color);
        }
        break;
      }
      case "strong":
        inlineSegs((tok.tokens ?? []) as MkToken[], { ...st, bold: true }, out);
        break;
      case "em":
        inlineSegs((tok.tokens ?? []) as MkToken[], { ...st, italic: true }, out);
        break;
      case "del":
        inlineSegs((tok.tokens ?? []) as MkToken[], { ...st, strike: true }, out);
        break;
      case "codespan": {
        const text = String(tok.text ?? "");
        const trimmed = text.trim();
        if (st.markFiles && trimmed && WHOLE_FILE_REFERENCE_RE.test(trimmed)) {
          out.push(fileRefSeg(trimmed, st.blue ?? color));
        } else {
          out.push({ t: text, f: font(12.5, { mono: true }), color, chip: { kind: "code" } });
        }
        break;
      }
      case "link": {
        const href = String(tok.href ?? "");
        if (st.markFiles && !/^(?:https?:|mailto:|#)/i.test(href)) {
          const path = decodeURIComponent(href.replace(/^file:\/+/i, ""));
          if (WHOLE_FILE_REFERENCE_RE.test(path)) {
            out.push(fileRefSeg(path, st.blue ?? color));
            break;
          }
        }
        const inner: Seg[] = [];
        inlineSegs((tok.tokens ?? []) as MkToken[], { ...st, color: st.blue ?? color }, inner);
        if (/^https?:\/\//i.test(href)) {
          for (const seg of inner) seg.action = { kind: "url", href };
        }
        out.push(...inner);
        break;
      }
      case "br":
        out.push({ t: "\n", f: font(st.size), color });
        break;
      case "escape":
        pushTextWithFileRefs(out, String(tok.text ?? ""), st, color);
        break;
      case "html": {
        const text = String(tok.raw ?? "").replace(/<[^>]*>/g, "").replace(/\s+/g, " ");
        if (text.trim()) pushTextWithFileRefs(out, text, st, color);
        break;
      }
      case "image":
        out.push({ t: `[${String(tok.text ?? "")}]`, f: font(st.size, { italic: true }), color });
        break;
      default:
        if (Array.isArray(tok.tokens)) inlineSegs(tok.tokens as MkToken[], st, out);
        else if (typeof tok.text === "string") pushTextWithFileRefs(out, tok.text.replace(/\s+/g, " "), st, color);
    }
  }
}

interface MdOpts {
  size: number;
  lineH: number;
  color?: string;
  markFiles: boolean;
  /** 紧凑列表内段落不留底边距 */
  tight?: boolean;
}

function mdParagraph(flow: Flow, segs: Seg[], opts: MdOpts, margin: { first: boolean; last: boolean }): number {
  const theme = flow.theme;
  const color = opts.color ?? theme.text;
  for (const seg of segs) { if (!seg.color) seg.color = color; }
  const lineH = opts.lineH;
  const lines = wrapSegs(segs, flow.width, lineH, font(opts.size));
  const mb = opts.tight || margin.last ? 0 : 10;
  const mt = 0;
  const { tlines, height } = placeLines(flow, lines, flow.x0, flow.y + mt);
  flow.blocks.push({
    y: flow.y,
    h: height + mb,
    regions: [],
    lines: tlines,
    boxes: [],
    paint: (ctx, view) => paintTextLines(ctx, view, tlines),
  });
  flow.y += height + mb;
  return height + mb;
}

function mdHeading(flow: Flow, tok: MkToken, opts: MdOpts, first: boolean): void {
  const theme = flow.theme;
  const depth = Math.min(4, Math.max(1, Number(tok.depth ?? 3)));
  const size = depth === 1 ? 17.5 : depth === 2 ? 16.1 : 14.7;
  const segs: Seg[] = [];
  inlineSegs((tok.tokens ?? []) as MkToken[], { size, markFiles: opts.markFiles, color: theme.text, blue: theme.blue }, segs);
  const lineH = size * 1.4;
  const lines = wrapSegs(segs, flow.width, lineH, font(size, { bold: true }));
  flow.y += 16;
  const { tlines, height } = placeLines(flow, lines, flow.x0, flow.y);
  flow.blocks.push({
    y: flow.y,
    h: height + 8,
    regions: [],
    lines: tlines,
    boxes: [],
    paint: (ctx, view) => paintTextLines(ctx, view, tlines),
  });
  flow.y += height + 8;
}

function mdCode(flow: Flow, tok: MkToken, opts: MdOpts, id: number): void {
  const theme = flow.theme;
  const rawText = String(tok.text ?? "").replace(/\n$/, "");
  // Canvas fillText does not expand tabs; render with tab stops while keeping the
  // original text for the copy action.
  const text = expandTabs(rawText);
  const f = font(12.5, { mono: true });
  const lineH = 12.5 * 1.35;
  const padX = 14;
  const padY = 12;
  const lines = wrapSegs([{ t: text, f, color: theme.text }], flow.width - padX * 2, lineH, f, {
    mode: "all",
    preWrap: true,
  });
  const { tlines, height } = placeLines(flow, lines, flow.x0 + padX, flow.y + padY);
  const blockH = height + padY * 2;
  const groupId = `cb-${id}`;
  const copyRegion: Region = {
    x: flow.x0 + flow.width - 7 - 23,
    y: flow.y + 7,
    w: 23,
    h: 23,
    action: { kind: "copy", id: groupId, text: rawText },
    cursor: "pointer",
    title: "复制",
    groupId,
  };
  const y0 = flow.y;
  flow.blocks.push({
    y: y0,
    h: blockH + 10,
    regions: [copyRegion],
    lines: tlines,
    boxes: [],
    paint: (ctx, view) => {
      roundRectPath(ctx, flow.x0, y0 + 0.5, flow.width, blockH, 10);
      ctx.fillStyle = view.theme.bgPanel;
      ctx.fill();
      ctx.strokeStyle = view.theme.border;
      ctx.lineWidth = 1;
      ctx.stroke();
      paintTextLines(ctx, view, tlines);
      paintCopyButton(ctx, view, copyRegion, view.hoverGroup === groupId);
    },
  });
  flow.y += blockH + 10;
}

function mdHr(flow: Flow): void {
  const y = flow.y + 7;
  flow.blocks.push({
    y: flow.y,
    h: 15,
    regions: [],
    lines: [],
    boxes: [],
    paint: (ctx, view) => {
      ctx.strokeStyle = view.theme.border;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(flow.x0, y + 0.5);
      ctx.lineTo(flow.x0 + flow.width, y + 0.5);
      ctx.stroke();
    },
  });
  flow.y += 15;
}

function mdList(flow: Flow, tok: MkToken, opts: MdOpts): void {
  const theme = flow.theme;
  const items = (tok.items ?? []) as MkToken[];
  const ordered = !!tok.ordered;
  const start = Number(tok.start ?? 1) || 1;
  const tight = !!tok.tight;
  const x0 = flow.x0;
  flow.y += 3;
  items.forEach((item, i) => {
    const marker = ordered ? `${start + i}.` : "•";
    const markerF = font(opts.size);
    const itemX = x0 + 22;
    const itemW = flow.width - 22;
    // 任务列表复选框
    const isTask = !!item.task;
    const checked = !!item.checked;
    const contentX = isTask ? itemX + 20 : itemX;
    const contentW = itemW - (isTask ? 20 : 0);

    const startY = flow.y;
    const subFlow: Flow = { blocks: [], y: flow.y, x0: contentX, width: contentW, theme, spinners: 0 };
    const tokens = (item.tokens ?? []) as MkToken[];
    mdFlowTokens(subFlow, tokens, { ...opts, tight: tight || !item.loose }, true);
    const itemH = Math.max(subFlow.y - flow.y, opts.lineH);

    const markerLineY = flow.y;
    const y0 = flow.y;
    const blocks = subFlow.blocks;
    const markerColor = opts.color ?? theme.text;
    flow.blocks.push({
      y: y0,
      h: itemH + 3,
      regions: blocks.flatMap((b) => b.regions),
      lines: blocks.flatMap((b) => b.lines),
      boxes: blocks.flatMap((b) => b.boxes),
      paint: (ctx, view) => {
        ctx.font = markerF;
        ctx.fillStyle = markerColor;
        if (ordered) {
          ctx.fillText(marker, x0 + 16 - measure(marker, markerF), markerLineY + baselineOf(opts.lineH, markerF));
        } else {
          ctx.fillText(marker, x0 + 6, markerLineY + baselineOf(opts.lineH, markerF));
        }
        if (isTask) {
          const cy = markerLineY + opts.lineH / 2;
          roundRectPath(ctx, itemX, cy - 7, 14, 14, 3);
          if (checked) {
            ctx.fillStyle = view.theme.accent;
            ctx.fill();
            drawIcon(ctx, "check", itemX + 1, cy - 6, 12, view.theme.onAccent);
          } else {
            ctx.strokeStyle = view.theme.borderStrong;
            ctx.lineWidth = 1;
            ctx.stroke();
          }
        }
        for (const b of blocks) b.paint(ctx, view);
      },
    });
    flow.y = startY + itemH + 3; // CSS .markdown li { margin: 3px 0 } collapsed between siblings
  });
  flow.y += 10; // CSS .markdown ul/ol { margin-bottom: 10px }
}

function mdBlockquote(flow: Flow, tok: MkToken, opts: MdOpts): void {
  const theme = flow.theme;
  const y0 = flow.y + 2;
  const subFlow: Flow = {
    blocks: [],
    y: y0,
    x0: flow.x0 + 15, // border(3) + padding-left(12)
    width: flow.width - 15,
    theme,
    spinners: 0,
  };
  mdFlowTokens(subFlow, (tok.tokens ?? []) as MkToken[], { ...opts, color: theme.textDim }, false);
  const innerH = Math.max(subFlow.y - y0, opts.lineH);
  const blocks = subFlow.blocks;
  const yStart = flow.y;
  flow.blocks.push({
    y: yStart,
    h: innerH + 4 + 10,
    regions: blocks.flatMap((b) => b.regions),
    lines: blocks.flatMap((b) => b.lines),
    boxes: blocks.flatMap((b) => b.boxes),
    paint: (ctx, view) => {
      ctx.fillStyle = view.theme.borderLight;
      ctx.fillRect(flow.x0, yStart, 3, innerH + 4); // border spans full element height (pad+content+pad)
      for (const b of blocks) b.paint(ctx, view);
    },
  });
  flow.y = yStart + innerH + 4 + 10;
}

function mdTable(flow: Flow, tok: MkToken, opts: MdOpts): void {
  const theme = flow.theme;
  const header = (tok.header ?? []) as MkToken[];
  const rows = (tok.rows ?? []) as MkToken[][];
  const cols = header.length;
  if (cols === 0) return;
  const size = 13;
  const lineH = size * 1.4;
  const cellPadX = 10;
  const cellPadY = 5;

  const colW: number[] = new Array(cols).fill(80);
  const headerSegs = header.map((cell) => {
    const segs: Seg[] = [];
    inlineSegs((cell.tokens ?? []) as MkToken[], { size, markFiles: false, bold: true, color: theme.text, blue: theme.blue }, segs);
    return segs;
  });
  const rowSegs = rows.map((row) =>
    row.map((cell) => {
      const segs: Seg[] = [];
      inlineSegs((cell.tokens ?? []) as MkToken[], { size, markFiles: false, color: theme.text, blue: theme.blue }, segs);
      return segs;
    }),
  );
  for (let c = 0; c < cols; c++) {
    let natural = measure("…", font(size, { bold: true })) + cellPadX * 2;
    for (const segs of [headerSegs[c], ...rowSegs.map((r) => r[c])]) {
      if (!segs) continue;
      const w = segs.reduce((sum, s) => sum + (s.chip ? chipWidth(s) : measure(s.t, s.f)), 0) + cellPadX * 2;
      natural = Math.max(natural, w);
    }
    colW[c] = Math.min(natural, 360);
  }
  let total = colW.reduce((a, b) => a + b, 0);
  if (total > flow.width) {
    const scale = flow.width / total;
    for (let c = 0; c < cols; c++) colW[c] = Math.max(72, Math.floor(colW[c] * scale));
    total = colW.reduce((a, b) => a + b, 0);
  }
  const wrapCell = (segs: Seg[] | undefined, c: number, bold: boolean): LLine[] =>
    segs && segs.length > 0
      ? wrapSegs(segs, colW[c] - cellPadX * 2, lineH, font(size, { bold }))
      : wrapSegs([], colW[c] - cellPadX * 2, lineH, font(size, { bold }));

  const allRows: { lines: LLine[]; bold: boolean }[][] = [];
  allRows.push(headerSegs.map((segs, c) => ({ lines: wrapCell(segs, c, true), bold: true })));
  for (const row of rowSegs) {
    allRows.push(row.map((segs, c) => ({ lines: wrapCell(segs, c, false), bold: false })));
  }

  const y0 = flow.y;
  const rowH: number[] = allRows.map((row) =>
    Math.max(lineH, ...row.map((cell) => cell.lines.length * lineH)) + cellPadY * 2,
  );
  const tableH = rowH.reduce((a, b) => a + b, 0);
  const colX: number[] = [];
  {
    let cx = flow.x0;
    for (let c = 0; c < cols; c++) {
      colX.push(cx);
      cx += colW[c];
    }
  }
  const tlines: TLine[] = [];
  allRows.forEach((row, r) => {
    const rowY = y0 + rowH.slice(0, r).reduce((a, b) => a + b, 0) + cellPadY;
    row.forEach((cell, c) => {
      let cy = rowY;
      for (const line of cell.lines) {
        tlines.push(toTLine(line, colX[c] + cellPadX, cy));
        cy += lineH;
      }
    });
  });
  flow.blocks.push({
    y: y0,
    h: tableH + 10,
    regions: [],
    lines: tlines,
    boxes: [],
    paint: (ctx, view) => {
      paintTextLines(ctx, view, tlines);
      ctx.strokeStyle = view.theme.border;
      ctx.lineWidth = 1;
      for (let r = 0; r <= allRows.length; r++) {
        const y = y0 + rowH.slice(0, r).reduce((a, b) => a + b, 0) + 0.5;
        ctx.beginPath();
        ctx.moveTo(flow.x0, y);
        ctx.lineTo(flow.x0 + total, y);
        ctx.stroke();
      }
      for (let c = 0; c <= cols; c++) {
        const x = (c === cols ? flow.x0 + total : colX[c]) + 0.5;
        ctx.beginPath();
        ctx.moveTo(x, y0);
        ctx.lineTo(x, y0 + tableH);
        ctx.stroke();
      }
    },
  });
  flow.y += tableH + 10;
}

let mdBlockSeq = 0;

function mdFlowTokens(flow: Flow, tokens: MkToken[], opts: MdOpts, inList: boolean): void {
  const meaningful = tokens.filter((t) => t.type !== "space");
  meaningful.forEach((tok, i) => {
    const first = i === 0;
    const last = i === meaningful.length - 1;
    switch (tok.type) {
      case "paragraph": {
        const segs: Seg[] = [];
        inlineSegs((tok.tokens ?? []) as MkToken[], { size: opts.size, markFiles: opts.markFiles, color: opts.color, blue: flow.theme.blue }, segs);
        mdParagraph(flow, segs, opts, { first, last });
        break;
      }
      case "text": {
        // loose list item 的裸 text
        const segs: Seg[] = [];
        inlineSegs((tok.tokens ?? []) as MkToken[], { size: opts.size, markFiles: opts.markFiles, color: opts.color, blue: flow.theme.blue }, segs);
        mdParagraph(flow, segs, { ...opts, tight: true }, { first, last });
        break;
      }
      case "heading":
        mdHeading(flow, tok, opts, first);
        break;
      case "code":
        mdCode(flow, tok, opts, ++mdBlockSeq);
        break;
      case "hr":
        mdHr(flow);
        break;
      case "list":
        mdList(flow, tok, opts);
        break;
      case "blockquote":
        mdBlockquote(flow, tok, opts);
        break;
      case "table":
        mdTable(flow, tok, opts);
        break;
      case "html": {
        const text = String(tok.raw ?? "").replace(/<[^>]*>/g, "").replace(/\s+/g, " ").trim();
        if (text) {
          const segs: Seg[] = [];
          pushTextWithFileRefs(segs, text, { size: opts.size, markFiles: opts.markFiles, blue: flow.theme.blue }, opts.color ?? flow.theme.text);
          mdParagraph(flow, segs, opts, { first, last });
        }
        break;
      }
      default:
        break;
    }
  });
  void inList;
}

function markdownBlocks(flow: Flow, src: string, opts: MdOpts): void {
  if (!src) return;
  const tokens = cachedLexer(src);
  mdFlowTokens(flow, tokens, opts, false);
}

/* ===== 工具辅助 ===== */

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
  return Object.entries(raw)
    .filter(([, value]) => value != null)
    .slice(0, 3)
    .map(([key, value]) => {
      const text = compactValue(value);
      return text ? `${key}: ${text}` : "";
    })
    .filter(Boolean)
    .join("\n");
}

function isUsefulText(text: string | undefined): text is string {
  if (!text) return false;
  const clean = stripAnsi(text).trim();
  return clean !== "" && clean !== "null" && clean !== "undefined";
}

export interface FileEdit {
  path: string;
  oldText: string | null;
  newText: string;
  add: number;
  del: number;
}

/** 与 EditedFilesCard.collectEdits 一致 */
export function collectEdits(body: Item[]): FileEdit[] {
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

interface DiffRow {
  sign: string;
  text: string;
  cls: "add" | "del" | "ctx" | "skip";
}

/** 与 DiffView 的行折叠规则一致 */
function diffRows(oldText: string, newText: string): DiffRow[] {
  const out: DiffRow[] = [];
  for (const part of diffLines(oldText, newText)) {
    const cls: DiffRow["cls"] = part.added ? "add" : part.removed ? "del" : "ctx";
    const sign = part.added ? "+" : part.removed ? "-" : " ";
    const lines = part.value.replace(/\n$/, "").split("\n");
    if (!part.added && !part.removed && lines.length > 8) {
      for (const l of lines.slice(0, 3)) out.push({ sign, text: l, cls });
      out.push({ sign: " ", text: `… 省略 ${lines.length - 6} 行 …`, cls: "skip" });
      for (const l of lines.slice(-3)) out.push({ sign, text: l, cls });
    } else {
      for (const l of lines) out.push({ sign, text: l, cls });
    }
  }
  return out;
}

/* ===== 滚动盒（tool-output / tool-raw / diff-body / perm-preview） ===== */

function makeScrollBox(opts: {
  x: number;
  y: number;
  w: number;
  maxH: number;
  padX: number;
  padY: number;
  text: string;
  f: string;
  color: string;
  lineH: number;
  key: string;
  env: LayoutEnv;
  radius?: number;
  copyId?: string;
}): ScrollBox {
  const { x, y, w, maxH, padX, padY, text, f, color, lineH, key, env } = opts;
  const lines = wrapSegs([{ t: text, f, color }], w - padX * 2, lineH, f, { mode: "all", preWrap: true });
  const placed: TLine[] = [];
  let cy = padY;
  for (const line of lines) {
    placed.push(toTLine(line, x + padX, cy));
    cy += lineH;
  }
  const contentH = cy + padY;
  const h = Math.min(contentH, maxH);
  const scrollTop = Math.min(env.scrollPos.get(key) ?? 0, Math.max(0, contentH - h));
  const fixedRegions: Region[] = [];
  if (opts.copyId) {
    fixedRegions.push({
      x: x + w - 7 - 23,
      y: y + 7,
      w: 23,
      h: 23,
      action: { kind: "copy", id: opts.copyId, text },
      cursor: "pointer",
      title: "复制",
      groupId: opts.copyId,
    });
  }
  const radius = opts.radius ?? 7;
  const box: ScrollBox = {
    x,
    y,
    w,
    h,
    contentH,
    key,
    scrollTop,
    lines: placed,
    regions: [],
    fixedRegions,
    paint: (ctx, view) => {
      roundRectPath(ctx, x + 0.5, y + 0.5, w - 1, box.h - 1, radius);
      ctx.fillStyle = view.theme.bgSidebar;
      ctx.fill();
      ctx.strokeStyle = view.theme.border;
      ctx.lineWidth = 1;
      ctx.stroke();
      ctx.save();
      roundRectPath(ctx, x, y, w, box.h, radius);
      ctx.clip();
      ctx.translate(0, y - box.scrollTop);
      const local: View = { ...view, top: view.top - y + box.scrollTop, bottom: view.bottom - y + box.scrollTop };
      paintTextLines(ctx, local, box.lines);
      ctx.restore();
      if (box.contentH > box.h + 1) {
        const trackH = box.h - 6;
        const thumbH = Math.max(24, (box.h / box.contentH) * trackH);
        const maxScroll = box.contentH - box.h;
        const thumbY = y + 3 + (box.scrollTop / maxScroll) * (trackH - thumbH);
        roundRectPath(ctx, x + w - 7, thumbY, 4, thumbH, 2);
        ctx.fillStyle = view.theme.scroll;
        ctx.fill();
      }
      for (const r of box.fixedRegions) paintCopyButton(ctx, view, r, view.hoverGroup === r.groupId);
    },
  };
  return box;
}

/* ===== 各类块构建器 ===== */

function userBlock(flow: Flow, item: Extract<Item, { type: "user" }>, env: LayoutEnv): void {
  const theme = flow.theme;
  const y0 = flow.y + 20;
  if (env.editingItemId === item.id) {
    // 编辑浮层替代气泡：占住近似高度
    const rows = Math.min(10, Math.max(2, item.text.split("\n").length));
    const h = 12 + rows * 22.4 + 10 + 27 + 12;
    flow.blocks.push({ y: y0, h: h + 16, regions: [], lines: [], boxes: [], paint: () => {} });
    flow.y = y0 + h + 16;
    return;
  }
  const lineH = 14 * 1.6;
  const f = font(14);
  const maxBubbleW = flow.width * 0.85;
  const textMaxW = maxBubbleW - 32;

  // 附件行
  interface ImgBox {
    w: number;
    h: number;
    img: HTMLImageElement | null;
    src?: string;
    file?: { name: string; path?: string };
  }
  const imgs: ImgBox[] = [];
  for (const img of item.images ?? []) {
    if (img.mimeType.startsWith("image/")) {
      const path = img.data || !img.uri ? undefined : decodeURI(img.uri.replace(/^file:\/+/, ""));
      const src = img.data ? `data:${img.mimeType};base64,${img.data}` : path ? convertFileSrc(path) : "";
      const loaded = env.image(src);
      if (loaded) {
        const scale = Math.min(1, 240 / loaded.w, 180 / loaded.h);
        imgs.push({ w: loaded.w * scale, h: loaded.h * scale, img: loaded.img, src });
      } else {
        imgs.push({ w: 180, h: 120, img: null, src });
      }
    } else {
      const path = img.data || !img.uri ? undefined : decodeURI(img.uri.replace(/^file:\/+/, ""));
      imgs.push({ w: 0, h: 27, img: null, file: { name: img.name, path } });
    }
  }
  // flex-wrap 布局附件
  const imgRegions: Region[] = [];
  interface ImgPlaced {
    box: ImgBox;
    x: number;
    y: number;
  }
  const imgPlaced: ImgPlaced[] = [];
  let imgsH = 0;
  if (imgs.length > 0) {
    let cx = 0;
    let rowH = 0;
    for (const box of imgs) {
      const w = box.file ? Math.min(240, 10 + 15 + 6 + measure(box.file.name, font(12.5)) + 10) : box.w;
      if (cx > 0 && cx + w > textMaxW) {
        cx = 0;
        imgsH += rowH + 6;
        rowH = 0;
      }
      imgPlaced.push({ box, x: cx, y: imgsH });
      cx += w + 6;
      rowH = Math.max(rowH, box.h);
    }
    imgsH += rowH;
  }

  const text = item.text;
  // 先按最大宽度断行，取最宽行宽作为气泡内容宽（等价 CSS fit-content + max-width:85%）
  const probeLines = wrapSegs([{ t: text, f, color: theme.text }], textMaxW, lineH, f, { preWrap: true });
  const probeW = Math.max(...probeLines.map((l) => l.w), 0);
  const imgMaxW = imgPlaced.length > 0 ? Math.max(...imgPlaced.map((p) => p.box.file ? Math.min(240, 41 + measure(p.box.file.name, font(12.5))) : p.box.w)) : 0;
  const contentW = Math.max(probeW, imgMaxW);
  const bubbleW = Math.min(maxBubbleW, contentW + 32);
  // 用实际气泡宽重新断行
  const textLines = wrapSegs([{ t: text, f, color: theme.text }], bubbleW - 32, lineH, f, { preWrap: true });
  const textH = text ? textLines.length * lineH : 0;
  const bubbleH = 10 + (imgs.length > 0 ? imgsH + 6 : 0) + (text ? textH : 0) + 10;
  const bubbleX = flow.x0 + flow.width - bubbleW;
  const { tlines } = placeLines(flow, textLines, bubbleX + 16, y0 + 10 + (imgs.length > 0 ? imgsH + 6 : 0));

  const groupId = `user-${item.id}`;
  const regions: Region[] = [];
  for (const p of imgPlaced) {
    if (p.box.file) {
      regions.push({
        x: bubbleX + 16 + p.x,
        y: y0 + 10 + p.y,
        w: Math.min(240, 41 + measure(p.box.file.name, font(12.5))),
        h: 27,
        action: { kind: "file-menu", path: p.box.file.path ?? "" },
        cursor: "default",
        title: p.box.file.name,
      });
    }
  }
  const editBtn: Region = {
    x: bubbleX - 23 - 2,
    y: y0 + bubbleH - 23 - 4,
    w: 23,
    h: 23,
    action: { kind: "edit-user", id: item.id, ex: env.x0, ey: y0, ew: env.width },
    cursor: "pointer",
    title: "编辑此消息，并从这里重新开始",
    groupId,
  };
  if (!env.running) regions.push(editBtn);

  const blockH = Math.max(bubbleH, env.running ? 0 : 27) ;
  flow.blocks.push({
    y: y0,
    h: blockH + 16,
    regions,
    lines: tlines,
    boxes: [],
    paint: (ctx, view) => {
      // 附件
      for (const p of imgPlaced) {
        const px = bubbleX + 16 + p.x;
        const py = y0 + 10 + p.y;
        if (p.box.img) {
          ctx.save();
          roundRectPath(ctx, px, py, p.box.w, p.box.h, 10);
          ctx.clip();
          ctx.drawImage(p.box.img, px, py, p.box.w, p.box.h);
          ctx.restore();
        } else if (p.box.file) {
          const w = Math.min(240, 41 + measure(p.box.file.name, font(12.5)));
          roundRectPath(ctx, px, py, w, 27, 8);
          ctx.fillStyle = view.theme.text8;
          ctx.fill();
          drawIcon(ctx, "file", px + 10, py + 6, 15, view.theme.textDim);
          ctx.font = font(12.5);
          ctx.fillStyle = view.theme.text;
          ctx.fillText(ellipsis(p.box.file.name, font(12.5), w - 41), px + 31, py + baselineOf(27, font(12.5)));
        } else {
          roundRectPath(ctx, px, py, p.box.w, p.box.h, 10);
          ctx.fillStyle = view.theme.text8;
          ctx.fill();
        }
      }
      roundRectPath(ctx, bubbleX, y0, bubbleW, bubbleH, [14, 14, 6, 14]);
      ctx.fillStyle = view.theme.accentDim;
      ctx.fill();
      ctx.strokeStyle = view.theme.accent26;
      ctx.lineWidth = 1;
      ctx.stroke();
      if (text) paintTextLines(ctx, view, tlines);
      if (!env.running && view.hoverGroup === groupId) {
        const hovered = view.hover === editBtn;
        roundRectPath(ctx, editBtn.x, editBtn.y, editBtn.w, editBtn.h, 6);
        if (hovered) {
          ctx.fillStyle = view.theme.bgHover;
          ctx.fill();
        }
        drawIcon(ctx, "pencil", editBtn.x + 5, editBtn.y + 5, 13, hovered ? view.theme.text : view.theme.textFaint);
      }
    },
  });
  flow.y = y0 + blockH + 16;
}

function assistantBlock(flow: Flow, item: Extract<Item, { type: "assistant" }>, env: LayoutEnv): void {
  const text = env.reveal(item);
  if (text.trim() === "None") return;
  const y0 = flow.y + 14;
  const sub: Flow = { blocks: [], y: y0, x0: flow.x0, width: flow.width, theme: flow.theme, spinners: 0 };
  markdownBlocks(sub, text, { size: 14, lineH: 14 * 1.7, markFiles: true });
  const h = Math.max(sub.y - y0, 1);
  flow.blocks.push({
    y: y0,
    h: h + 14,
    regions: sub.blocks.flatMap((b) => b.regions),
    lines: sub.blocks.flatMap((b) => b.lines),
    boxes: sub.blocks.flatMap((b) => b.boxes),
    paint: (ctx, view) => {
      for (const b of sub.blocks) b.paint(ctx, view);
    },
  });
  flow.y = y0 + h + 14;
}

function normalizeThoughtMarkdown(text: string): string {
  return state.agentKind === "opencode" ? text.replace(/(\S)\*{4}(?=\S)/g, "$1**\n\n**") : text;
}

function thoughtBlock(flow: Flow, item: Extract<Item, { type: "thought" }>, env: LayoutEnv, active: boolean): void {
  const theme = flow.theme;
  const text = env.reveal(item);
  if (text === "思考中…") {
    const f = font(13);
    const lineH = 13 * 1.5;
    const { tlines } = placeLines(flow, wrapSegs([{ t: text, f, color: theme.textFaint }], flow.width, lineH, f), flow.x0, flow.y + 6);
    flow.blocks.push({ y: flow.y + 6, h: lineH + 6, regions: [], lines: tlines, boxes: [], paint: (ctx, view) => paintTextLines(ctx, view, tlines) });
    flow.y += lineH + 12;
    return;
  }
  const key = `thought-${item.id}`;
  const open = isExpanded(key, active);
  const y0 = flow.y + 6;
  const toggleF = font(12);
  const toggleLH = 12 * 1.2;
  const rowH = 3 + toggleLH + 3;
  const toggle: Region = {
    x: flow.x0 - 6,
    y: y0,
    w: 6 + 12 + 5 + measure("思考过程", toggleF) + 6,
    h: rowH,
    action: { kind: "toggle", key, value: !open },
    cursor: "pointer",
    groupId: `th-${item.id}`,
  };
  const blocks: Block[] = [
    {
      y: y0,
      h: rowH,
      regions: [toggle],
      lines: [],
      boxes: [],
      paint: (ctx, view) => {
        const hovered = view.hoverGroup === `th-${item.id}`;
        if (hovered) {
          roundRectPath(ctx, toggle.x, toggle.y, toggle.w, toggle.h, 6);
          ctx.fillStyle = view.theme.bgHover;
          ctx.fill();
        }
        const iconY = y0 + 3 + (toggleLH - 12) / 2;
        drawIcon(ctx, open ? "chevronDown" : "chevronRight", flow.x0, iconY, 12, hovered ? view.theme.textDim : view.theme.textFaint);
        ctx.font = toggleF;
        ctx.fillStyle = hovered ? view.theme.textDim : view.theme.textFaint;
        ctx.fillText("思考过程", flow.x0 + 17, y0 + 3 + baselineOf(toggleLH, toggleF));
      },
    },
  ];
  let totalH = rowH;
  if (open) {
    const bodyY = y0 + rowH + 6;
    const sub: Flow = { blocks: [], y: bodyY + 8, x0: flow.x0 + 16, width: flow.width - 16, theme, spinners: 0 };
    markdownBlocks(sub, normalizeThoughtMarkdown(text), { size: 13, lineH: 13 * 1.5, color: theme.textDim, markFiles: false });
    const innerH = Math.max(sub.y - (bodyY + 8), 0);
    blocks.push({
      y: bodyY,
      h: innerH + 16,
      regions: sub.blocks.flatMap((b) => b.regions),
      lines: sub.blocks.flatMap((b) => b.lines),
      boxes: sub.blocks.flatMap((b) => b.boxes),
      paint: (ctx, view) => {
        ctx.fillStyle = view.theme.borderLight;
        ctx.fillRect(flow.x0, bodyY, 2, innerH + 16);
        for (const b of sub.blocks) b.paint(ctx, view);
      },
    });
    totalH = rowH + 6 + innerH + 16;
  }
  flow.blocks.push(...blocks);
  flow.y = y0 + totalH + 6;
}

function toolBlock(flow: Flow, item: ToolItem, env: LayoutEnv, active: boolean): void {
  const theme = flow.theme;
  const y0 = flow.y + 1;
  const rowH = 32; // CSS: padding 3px + min-height 26px + padding 3px
  const key = `tool-${item.id}`;
  const rawKey = `tool-raw-${item.id}`;
  const scrollKey = (part: string) => `${env.threadId}-${item.id}-${part}`;
  const defaultOpen = active || item.status === "pending" || item.status === "in_progress";
  const open = isExpanded(key, defaultOpen);
  const showRaw = isExpanded(rawKey);

  const hasBody =
    item.content.length > 0 ||
    item.locations.length > 0 ||
    item.rawInput !== undefined ||
    item.rawOutput !== undefined;
  const visibleContent = item.content.some((block) => {
    if (block.type !== "content") return true;
    const inner = (block as { content?: { type?: string; text?: string } }).content;
    return inner?.type !== "text" || isUsefulText(inner.text);
  });
  const summary = rawPreview(item.rawInput) || rawPreview(item.rawOutput);
  let add = 0;
  let del = 0;
  for (const b of item.content) {
    if (b.type !== "diff") continue;
    const d = b as Extract<ToolContent, { type: "diff" }>;
    for (const part of diffLines(d.oldText ?? "", d.newText ?? "")) {
      if (part.added) add += part.count ?? 0;
      else if (part.removed) del += part.count ?? 0;
    }
  }
  const label = (() => {
    const t = (item.title || "").trim();
    if (t) return displayToolTitle(stripAnsi(t));
    return KIND_LABEL[item.kind] ?? item.kind;
  })();

  const titleF = font(12, { mono: true });
  const titleColor = item.status === "failed" ? theme.red : theme.textDim;
  const busy = item.status === "in_progress" || item.status === "pending";
  if (busy) flow.spinners++;

  // 行内从右到左：chevron / spinner|dot / stats
  let rightX = flow.x0 + flow.width - 8;
  const regions: Region[] = [];
  const lineRegion: Region = {
    x: flow.x0,
    y: y0,
    w: flow.width,
    h: rowH,
    action: hasBody ? { kind: "toggle", key, value: !open } : { kind: "noop" },
    cursor: "pointer",
    title: label,
    groupId: `tl-${item.id}`,
  };
  regions.push(lineRegion);

  let chevronX = 0;
  if (hasBody) {
    rightX -= 12;
    chevronX = rightX;
    rightX -= 8;
  }
  let spinnerCx = 0;
  let dotCx = 0;
  if (busy) {
    rightX -= 12;
    spinnerCx = rightX + 6;
    rightX -= 8;
  } else if (item.status === "failed") {
    rightX -= 7;
    dotCx = rightX + 3.5;
    rightX -= 8;
  }
  const addText = `+${add}`;
  const delText = `-${del}`;
  const statsF = font(11.5, { mono: true });
  let statsX = 0;
  if (add + del > 0) {
    const statsW = measure(addText, statsF) + 6 + measure(delText, statsF);
    rightX -= statsW;
    statsX = rightX;
    rightX -= 8;
  }
  const titleX = flow.x0 + 8 + 14 + 8;
  const titleMaxW = Math.max(40, rightX - 8 - titleX);

  const rowBlock: Block = {
    y: y0,
    h: rowH,
    regions,
    lines: [],
    boxes: [],
    paint: (ctx, view) => {
      if (view.hoverGroup === `tl-${item.id}`) {
        roundRectPath(ctx, flow.x0, y0, flow.width, rowH, 7);
        ctx.fillStyle = view.theme.bgHover;
        ctx.fill();
      }
      drawIcon(ctx, toolIconName(item.kind), flow.x0 + 8, y0 + rowH / 2 - 7, 14, view.theme.textFaint);
      ctx.font = titleF;
      ctx.fillStyle = titleColor;
      ctx.fillText(ellipsis(label, titleF, titleMaxW), titleX, y0 + baselineOf(rowH, titleF));
      if (statsX) {
        ctx.font = statsF;
        ctx.fillStyle = view.theme.accent;
        ctx.fillText(addText, statsX, y0 + baselineOf(rowH, statsF));
        ctx.fillStyle = view.theme.red;
        ctx.fillText(delText, statsX + measure(addText, statsF) + 6, y0 + baselineOf(rowH, statsF));
      }
      if (busy) drawSpinner(ctx, spinnerCx, y0 + rowH / 2, 12, view.now, view.theme.blue, view.theme.accent26);
      if (dotCx) {
        ctx.fillStyle = view.theme.red;
        ctx.beginPath();
        ctx.arc(dotCx, y0 + rowH / 2, 3.5, 0, Math.PI * 2);
        ctx.fill();
      }
      if (hasBody) drawIcon(ctx, open ? "chevronDown" : "chevronRight", chevronX, y0 + rowH / 2 - 6, 12, view.theme.textFaint);
    },
  };
  flow.blocks.push(rowBlock);
  let totalH = rowH;

  if (open && hasBody) {
    const bodyX = flow.x0 + 30;
    const bodyW = flow.width - 30;
    let by = y0 + rowH + 4 + 4;
    const bodyBlocks: Block[] = [];

    // locations chips
    if (item.locations.length > 0) {
      const chipF = font(11.5, { mono: true });
      const chipH = 19;
      let cx = bodyX;
      const chips: { x: number; y: number; w: number; text: string; region: Region }[] = [];
      const chipRegions: Region[] = [];
      for (const loc of item.locations) {
        const baseName = (loc.path ?? "").split(/[\\/]/).pop() || loc.path || "";
        const text = `${baseName}${loc.line != null ? `:${loc.line}` : ""}`;
        const w = 18 + measure(text, chipF);
        if (cx > bodyX && cx + w > bodyX + bodyW) {
          cx = bodyX;
          by += chipH + 6;
        }
        const region: Region = {
          x: cx,
          y: by,
          w,
          h: chipH,
          action: { kind: "file", path: loc.path ?? "", line: loc.line ?? undefined },
          cursor: "pointer",
          title: `在编辑器中打开 ${loc.path ?? ""}`,
        };
        chips.push({ x: cx, y: by, w, text, region });
        chipRegions.push(region);
        cx += w + 6;
      }
      by += chipH;
      bodyBlocks.push({
        y: chips[0]?.y ?? by,
        h: by - (chips[0]?.y ?? by),
        regions: chipRegions,
        lines: [],
        boxes: [],
        paint: (ctx, view) => {
          for (const chip of chips) {
            const hovered = view.hover === chip.region;
            roundRectPath(ctx, chip.x, chip.y, chip.w, chipH, 9.5);
            ctx.fillStyle = hovered ? view.theme.bgHover : view.theme.bgPanel;
            ctx.fill();
            ctx.font = chipF;
            ctx.fillStyle = view.theme.blue;
            ctx.fillText(chip.text, chip.x + 9, chip.y + baselineOf(chipH, chipF));
            if (hovered) {
              ctx.strokeStyle = view.theme.blue;
              ctx.lineWidth = 1;
              ctx.beginPath();
              ctx.moveTo(chip.x + 9, chip.y + baselineOf(chipH, chipF) + 1.5);
              ctx.lineTo(chip.x + 9 + measure(chip.text, chipF), chip.y + baselineOf(chipH, chipF) + 1.5);
              ctx.stroke();
            }
          }
        },
      });
      by += 8;
    }

    // content blocks
    for (const [index, block] of item.content.entries()) {
      if (block.type === "diff") {
        const d = block as Extract<ToolContent, { type: "diff" }>;
        const rows = diffRows(d.oldText ?? "", d.newText ?? "");
        const lineH = 18;
        const headH = 25;
        const headRegion: Region = {
          x: bodyX,
          y: by,
          w: bodyW,
          h: headH,
          action: { kind: "file", path: d.path },
          cursor: "pointer",
          title: `在编辑器中打开 ${d.path}`,
        };
        const box = makeScrollBox({
          x: bodyX,
          y: by + headH,
          w: bodyW,
          maxH: 360,
          padX: 0,
          padY: 0,
          text: "",
          f: font(12, { mono: true }),
          color: theme.textDim,
          lineH,
          key: scrollKey(`diff-${index}`),
          env,
          radius: 0,
        });
        // diff 行自己布局（带符号列与行背景）
        const dlines: TLine[] = [];
        let ry = 0;
        for (const row of rows) {
          const wrapped = wrapSegs(
            [{ t: row.text, f: font(12, { mono: true }), color: "" }],
            bodyW - 26,
            lineH,
            font(12, { mono: true }),
            { mode: "all", preWrap: true },
          );
          const bg = row.cls === "add" ? theme.accent13 : row.cls === "del" ? theme.red13 : undefined;
          const color =
            row.cls === "add"
              ? theme.accent60Text
              : row.cls === "del"
                ? theme.red58Text
                : row.cls === "skip"
                  ? theme.textFaint
                  : theme.textDim;
          let first = true;
          for (const line of wrapped) {
            const tl = toTLine(line, bodyX + 26, ry);
            tl.bg = bg;
            tl.bgX = bodyX;
            tl.bgW = bodyW;
            if (first) {
              tl.sign = row.sign;
              tl.signX = bodyX + 10;
              tl.signColor = row.cls === "skip" ? theme.textFaint : theme.textFaint;
              first = false;
            }
            for (const run of tl.runs) run.color = color;
            dlines.push(tl);
            ry += lineH;
          }
        }
        box.lines = dlines;
        box.contentH = Math.max(ry, lineH);
        box.h = Math.min(box.contentH, 360);
        box.scrollTop = Math.min(env.scrollPos.get(box.key) ?? 0, Math.max(0, box.contentH - box.h));
        const boxH = box.h;
        const blockY = by;
        const headF = font(11.5, { mono: true });
        bodyBlocks.push({
          y: blockY,
          h: headH + boxH,
          regions: [headRegion],
          lines: [],
          boxes: [box],
          paint: (ctx, view) => {
            const hovered = view.hover === headRegion;
            // 头部
            roundRectPath(ctx, bodyX, blockY, bodyW, headH, [7, 7, 0, 0]);
            ctx.fillStyle = hovered ? view.theme.bgHover : view.theme.bgPanel;
            ctx.fill();
            ctx.font = headF;
            ctx.fillStyle = view.theme.blue;
            ctx.fillText(relPath(d.path), bodyX + 10, blockY + baselineOf(headH, headF));
            if (hovered) {
              ctx.strokeStyle = view.theme.blue;
              ctx.lineWidth = 1;
              ctx.beginPath();
              ctx.moveTo(bodyX + 10, blockY + baselineOf(headH, headF) + 1.5);
              ctx.lineTo(bodyX + 10 + measure(relPath(d.path), headF), blockY + baselineOf(headH, headF) + 1.5);
              ctx.stroke();
            }
            // 内容区（滚动盒绘制背景与文本）
            box.paint(ctx, view);
            // 外框与头部分隔线
            roundRectPath(ctx, bodyX + 0.5, blockY + 0.5, bodyW - 1, headH + boxH - 1, 7);
            ctx.strokeStyle = view.theme.border;
            ctx.lineWidth = 1;
            ctx.stroke();
            ctx.beginPath();
            ctx.moveTo(bodyX, blockY + headH + 0.5);
            ctx.lineTo(bodyX + bodyW, blockY + headH + 0.5);
            ctx.stroke();
          },
        });
        by += headH + boxH + 8;
        continue;
      }
      if (block.type === "content") {
        const inner = (block as { content: { type: string; text?: string } }).content;
        const text = inner?.type === "text" && isUsefulText(inner.text) ? stripAnsi(inner.text) : "";
        if (!text) continue;
        const copyId = `tout-${scrollKey(`output-${index}`)}`;
        const box = makeScrollBox({
          x: bodyX,
          y: by,
          w: bodyW,
          maxH: 320,
          padX: 10,
          padY: 10,
          text,
          f: font(12, { mono: true }),
          color: theme.textDim,
          lineH: 12 * 1.55,
          key: scrollKey(`output-${index}`),
          env,
          copyId,
        });
        bodyBlocks.push({ y: by, h: box.h, regions: [], lines: [], boxes: [box], paint: (ctx, view) => box.paint(ctx, view) });
        by += box.h + 8;
      }
    }

    if (!visibleContent && summary) {
      const f = font(12, { mono: true });
      const lineH = 12 * 1.55;
      const lines = wrapSegs([{ t: summary, f, color: theme.textDim }], bodyW - 20, lineH, f, { preWrap: true });
      const w = Math.min(bodyW, Math.max(...lines.map((l) => l.w), 0) + 20);
      const { tlines, height } = placeLines(flow, lines, bodyX + 10, by + 7);
      const y = by;
      bodyBlocks.push({
        y,
        h: height + 14,
        regions: [],
        lines: tlines,
        boxes: [],
        paint: (ctx, view) => {
          roundRectPath(ctx, bodyX, y, w, height + 14, 7);
          ctx.fillStyle = view.theme.bgPanel;
          ctx.fill();
          ctx.strokeStyle = view.theme.border;
          ctx.lineWidth = 1;
          ctx.stroke();
          paintTextLines(ctx, view, tlines);
        },
      });
      by += height + 14 + 8;
    }

    if (item.rawInput !== undefined || item.rawOutput !== undefined) {
      const toggleF = font(12);
      const toggleText = showRaw ? "隐藏原始数据" : "原始数据";
      const rawToggle: Region = {
        x: bodyX,
        y: by,
        w: measure(toggleText, toggleF),
        h: 18,
        action: { kind: "toggle", key: rawKey },
        cursor: "pointer",
      };
      const ty = by;
      bodyBlocks.push({
        y: ty,
        h: 18,
        regions: [rawToggle],
        lines: [],
        boxes: [],
        paint: (ctx, view) => {
          ctx.font = toggleF;
          ctx.fillStyle = view.theme.blue;
          ctx.fillText(toggleText, bodyX, ty + baselineOf(18, toggleF));
          if (view.hover === rawToggle) {
            ctx.strokeStyle = view.theme.blue;
            ctx.lineWidth = 1;
            ctx.beginPath();
            ctx.moveTo(bodyX, ty + baselineOf(18, toggleF) + 1.5);
            ctx.lineTo(bodyX + rawToggle.w, ty + baselineOf(18, toggleF) + 1.5);
            ctx.stroke();
          }
        },
      });
      by += 18 + 8;
      if (showRaw) {
        const rawParts: { label: string; value: unknown; part: string }[] = [];
        if (item.rawInput !== undefined) rawParts.push({ label: "输入", value: item.rawInput, part: "raw-input" });
        if (item.rawOutput !== undefined) rawParts.push({ label: "输出", value: item.rawOutput, part: "raw-output" });
        for (const part of rawParts) {
          const text = `${part.label}: ${stringifyJson(part.value)}`;
          const copyId = `traw-${scrollKey(part.part)}`;
          const box = makeScrollBox({
            x: bodyX,
            y: by,
            w: bodyW,
            maxH: 200,
            padX: 8,
            padY: 8,
            text,
            f: font(11.5, { mono: true }),
            color: theme.textFaint,
            lineH: 11.5 * 1.5,
            key: scrollKey(part.part),
            env,
            copyId,
          });
          bodyBlocks.push({ y: by, h: box.h, regions: [], lines: [], boxes: [box], paint: (ctx, view) => box.paint(ctx, view) });
          by += box.h + 8;
        }
      }
    }

    const bodyH = by - (y0 + rowH + 4 + 4);
    const bodyTop = y0 + rowH + 4;
    // CSS .tool-body: margin 4px 0 8px 14px; padding 4px 0 4px 14px; border-left 2px; gap 8px
    const toolBorderH = 4 + bodyH + 4; // pad-top + content + pad-bottom
    flow.blocks.push({
      y: bodyTop,
      h: toolBorderH + 8, // + margin-bottom 8px
      regions: [],
      lines: [],
      boxes: [],
      paint: (ctx, view) => {
        ctx.fillStyle = view.theme.border;
        ctx.fillRect(flow.x0 + 14, bodyTop, 2, toolBorderH);
      },
    });
    flow.blocks.push(...bodyBlocks);
    totalH = rowH + 4 + toolBorderH + 8; // margin-top + element(pad+content+pad) + margin-bottom
  }
  flow.y = y0 + totalH + 1;
}

function systemBlock(flow: Flow, item: Extract<Item, { type: "system" }>): void {
  const theme = flow.theme;
  const y0 = flow.y + 10;
  const level = item.level;
  const f = font(12.5);
  const lineH = 12.5 * 1.5;
  const color =
    level === "error" ? theme.red : level === "warn" ? theme.yellow : level === "info" ? theme.accent80Text : theme.textFaint;
  const bg = level === "error" ? theme.red8 : level === "warn" ? theme.yellow7 : level === "info" ? theme.accent7 : undefined;
  const border = level === "error" ? theme.red32 : level === "warn" ? theme.yellow28 : level === "info" ? theme.accent26 : undefined;
  const centered = level !== "error" && level !== "warn";
  const lines = wrapSegs([{ t: item.text.replace(/\s+/g, " "), f, color }], flow.width - 26, lineH, f);
  const { tlines, height } = placeLines(flow, lines, flow.x0 + 13, y0 + 8);
  if (centered) {
    for (const line of tlines) {
      const w = line.runs.reduce((sum, r) => sum + r.w, 0);
      const shift = (flow.width - 26 - w) / 2;
      if (shift > 0) for (const r of line.runs) r.x += shift;
    }
  }
  const blockH = height + 16;
  flow.blocks.push({
    y: y0,
    h: blockH + 10,
    regions: [],
    lines: tlines,
    boxes: [],
    paint: (ctx, view) => {
      if (bg) {
        roundRectPath(ctx, flow.x0 + 0.5, y0 + 0.5, flow.width - 1, blockH - 1, 8);
        ctx.fillStyle = bg;
        ctx.fill();
        if (border) {
          ctx.strokeStyle = border;
          ctx.lineWidth = 1;
          ctx.stroke();
        }
      }
      paintTextLines(ctx, view, tlines);
    },
  });
  flow.y = y0 + blockH + 10;
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

function isBusyItem(item: Item): boolean {
  return (
    (item.type === "tool" && (item.status === "pending" || item.status === "in_progress")) ||
    (item.type === "thought" && item.text === "思考中…")
  );
}

function filesCard(flow: Flow, body: Item[], foldKey: string, env: LayoutEnv): void {
  const theme = flow.theme;
  const edits = collectEdits(body);
  if (edits.length === 0) return;
  const undoneKey = `undone-${foldKey}`;
  const undone = isExpanded(undoneKey);
  const y0 = flow.y + 12;
  const headH = 40; // CSS .files-head padding 9px + content(~22px button) + 9px
  const rowH = 29;  // CSS .files-row padding 7px + content(~14px) + 7px + 1px border
  let add = 0;
  let del = 0;
  for (const e of edits) {
    add += e.add;
    del += e.del;
  }
  const regions: Region[] = [];
  const undoText = undone ? "已撤销" : "撤销";
  const undoW = 12 + 12 + 5 + measure(undoText, font(12)) + 12;
  const undoH = 22;
  const undoRegion: Region = {
    x: flow.x0 + flow.width - 12 - undoW,
    y: y0 + (headH - undoH) / 2,
    w: undoW,
    h: undoH,
    action: { kind: "revert", undoneKey, edits },
    cursor: undone ? "default" : "pointer",
    title: undone ? "本轮改动已撤销" : "把这些文件恢复到本轮编辑前的内容（被后续修改过的文件会跳过）",
  };
  regions.push(undoRegion);
  const rowRegions: Region[] = edits.map((e, i) => ({
    x: flow.x0,
    y: y0 + headH + i * rowH,
    w: flow.width,
    h: rowH,
    action: { kind: "file", path: e.path },
    cursor: "pointer",
    title: `在编辑器中打开 ${e.path}`,
  }));
  regions.push(...rowRegions);
  const totalH = headH + edits.length * rowH;
  const statsF = font(11.5, { mono: true });
  flow.blocks.push({
    y: y0,
    h: totalH + 4,
    regions,
    lines: [],
    boxes: [],
    paint: (ctx, view) => {
      roundRectPath(ctx, flow.x0 + 0.5, y0 + 0.5, flow.width - 1, totalH - 1, 10);
      ctx.fillStyle = view.theme.bgSidebar;
      ctx.fill();
      ctx.strokeStyle = view.theme.border;
      ctx.lineWidth = 1;
      ctx.stroke();
      // head
      ctx.font = font(13, { bold: true });
      ctx.fillStyle = view.theme.text;
      ctx.fillText(`已编辑 ${edits.length} 个文件`, flow.x0 + 12, y0 + baselineOf(headH, font(13, { bold: true })));
      let sx = flow.x0 + 12 + measure(`已编辑 ${edits.length} 个文件`, font(13, { bold: true })) + 10;
      ctx.font = statsF;
      ctx.fillStyle = view.theme.accent;
      ctx.fillText(`+${add}`, sx, y0 + baselineOf(headH, statsF));
      sx += measure(`+${add}`, statsF) + 6;
      ctx.fillStyle = view.theme.red;
      ctx.fillText(`-${del}`, sx, y0 + baselineOf(headH, statsF));
      // undo
      const disabled = undone;
      ctx.globalAlpha = disabled ? 0.5 : 1;
      roundRectPath(ctx, undoRegion.x, undoRegion.y, undoRegion.w, undoRegion.h, 7);
      if (view.hover === undoRegion && !disabled) {
        ctx.fillStyle = view.theme.bgHover;
        ctx.fill();
      }
      ctx.strokeStyle = view.theme.borderLight;
      ctx.lineWidth = 1;
      ctx.stroke();
      drawIcon(ctx, "undo", undoRegion.x + 12, undoRegion.y + (undoH - 12) / 2, 12, view.theme.textDim);
      ctx.font = font(12);
      ctx.fillStyle = view.theme.textDim;
      ctx.fillText(undoText, undoRegion.x + 29, undoRegion.y + baselineOf(undoH, font(12)));
      ctx.globalAlpha = 1;
      ctx.strokeStyle = view.theme.border;
      ctx.beginPath();
      ctx.moveTo(flow.x0, y0 + headH + 0.5);
      ctx.lineTo(flow.x0 + flow.width, y0 + headH + 0.5);
      ctx.stroke();
      // rows
      edits.forEach((e, i) => {
        const ry = y0 + headH + i * rowH;
        const region = rowRegions[i];
        if (view.hover === region) {
          ctx.fillStyle = view.theme.bgHover;
          ctx.fillRect(flow.x0 + 1, ry, flow.width - 2, rowH);
        }
        if (i < edits.length - 1) {
          ctx.strokeStyle = view.theme.border;
          ctx.beginPath();
          ctx.moveTo(flow.x0, ry + rowH + 0.5);
          ctx.lineTo(flow.x0 + flow.width, ry + rowH + 0.5);
          ctx.stroke();
        }
        const pathF = font(12, { mono: true });
        const statsText = `+${e.add} -${e.del}`;
        const statsW = measure(statsText, statsF);
        ctx.font = pathF;
        ctx.fillStyle = view.theme.blue;
        ctx.fillText(ellipsis(relPath(e.path), pathF, flow.width - 24 - statsW - 10), flow.x0 + 12, ry + baselineOf(rowH, pathF));
        if (view.hover === region) {
          const pw = Math.min(measure(relPath(e.path), pathF), flow.width - 24 - statsW - 10);
          ctx.strokeStyle = view.theme.blue;
          ctx.beginPath();
          ctx.moveTo(flow.x0 + 12, ry + baselineOf(rowH, pathF) + 1.5);
          ctx.lineTo(flow.x0 + 12 + pw, ry + baselineOf(rowH, pathF) + 1.5);
          ctx.stroke();
        }
        ctx.font = statsF;
        ctx.fillStyle = view.theme.accent;
        const stX = flow.x0 + flow.width - 12 - statsW;
        ctx.fillText(`+${e.add}`, stX, ry + baselineOf(rowH, statsF));
        ctx.fillStyle = view.theme.red;
        ctx.fillText(`-${e.del}`, stX + measure(`+${e.add} `, statsF), ry + baselineOf(rowH, statsF));
      });
    },
  });
  flow.y = y0 + totalH + 4;
}

/* ===== 权限卡片 ===== */

function permBlock(flow: Flow, req: PermissionRequest, env: LayoutEnv): void {
  const theme = flow.theme;
  const y0 = flow.y + 10;
  const padX = 14;
  const innerW = flow.width - padX * 2;
  let cy = y0 + 12;
  const regions: Region[] = [];
  const blocks: Block[] = [];
  const agent = agentLabel(req.agentKind ?? "devin");
  const title = `${agent} ${req.questions ? "需要确认" : "请求权限"}：${req.toolCall?.title ?? "执行工具"}`;

  // head
  const titleF = font(13, { bold: true });
  const headLines = wrapSegs([{ t: title, f: titleF, color: theme.text }], innerW - 9 - 14, 13 * 1.5, titleF);
  const headX = flow.x0 + padX + 14 + 9;
  const { tlines: headT, height: headH } = placeLines(flow, headLines, headX, cy);
  cy += headH + 9; // gap: 9px between flex children
  const iconY = y0 + 12;
  const iconX = flow.x0 + padX;

  const preview = (() => {
    const raw = req.toolCall?.rawInput;
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
  })();

  let boxes: ScrollBox[] = [];
  if (!req.questions && preview) {
    const box = makeScrollBox({
      x: flow.x0 + padX,
      y: cy,
      w: innerW,
      maxH: 140,
      padX: 10,
      padY: 8,
      text: preview,
      f: font(12, { mono: true }),
      color: theme.textDim,
      lineH: 12 * 1.5,
      key: `perm-${req.requestKey}`,
      env,
    });
    boxes = [box];
    cy += box.h + 9; // gap: 9px
  }

  // 问题 / 选项
  // 未交互过时默认每个问题为空答案（与 DOM 版 signal 初始值一致，避免 canAnswer 误判）
  const qCount = req.questions?.length ?? 0;
  const storedAnswers = env.perm.answers(req.requestKey);
  const answers = Array.from({ length: qCount }, (_, i) => storedAnswers[i] ?? []);
  const storedCustom = env.perm.custom(req.requestKey);
  const custom = Array.from({ length: qCount }, (_, i) => storedCustom[i] ?? "");
  if (req.questions) {
    req.questions.forEach((question, qi) => {
      const headerF = font(11, { bold: true });
      const headerLines = wrapSegs(
        [{ t: (question.header ?? "").toUpperCase(), f: headerF, color: theme.textDim }],
        innerW,
        11 * 1.5,
        headerF,
      );
      const hp = placeLines(flow, headerLines, flow.x0 + padX, cy);
      cy += hp.height + 7;
      const qF = font(13);
      const qLines = wrapSegs([{ t: question.question ?? "", f: qF, color: theme.text }], innerW, 13 * 1.5, qF);
      const qp = placeLines(flow, qLines, flow.x0 + padX, cy);
      cy += qp.height + 7;
      for (const option of question.options) {
        const selected = (answers[qi] ?? []).includes(option.label);
        const labelLines = wrapSegs([{ t: option.label, f: font(14), color: theme.text }], innerW - 20, 14 * 1.5, font(14));
        const descLines = option.description
          ? wrapSegs([{ t: option.description, f: font(11.2), color: theme.textDim }], innerW - 20, 11.2 * 1.5, font(11.2))
          : [];
        const optH = 8 + labelLines.length * 21 + descLines.length * 16.8 + 2 + 8;
        const region: Region = {
          x: flow.x0 + padX,
          y: cy,
          w: innerW,
          h: optH,
          action: { kind: "perm-select", reqKey: req.requestKey, index: qi, label: option.label, multiple: !!question.multiple },
          cursor: "pointer",
        };
        regions.push(region);
        const oy = cy;
        const lp = placeLines(flow, labelLines, flow.x0 + padX + 10, oy + 8);
        const dp = placeLines(flow, descLines, flow.x0 + padX + 10, oy + 8 + labelLines.length * 21 + 2);
        blocks.push({
          y: oy,
          h: optH,
          regions: [region],
          lines: [...lp.tlines, ...dp.tlines],
          boxes: [],
          paint: (ctx, view) => {
            roundRectPath(ctx, flow.x0 + padX + 0.5, oy + 0.5, innerW - 1, optH - 1, 7);
            ctx.fillStyle = selected ? view.theme.accentDim : view.theme.bgPanel;
            ctx.fill();
            ctx.strokeStyle = selected ? view.theme.accent34 : view.theme.borderLight;
            ctx.lineWidth = 1;
            ctx.stroke();
            paintTextLines(ctx, view, [...lp.tlines, ...dp.tlines]);
          },
        });
        cy += optH + 6;
      }
      if (question.custom) {
        const value = custom[qi] ?? "";
        const region: Region = {
          x: flow.x0 + padX,
          y: cy,
          w: innerW,
          h: 33,
          action: { kind: "perm-custom", reqKey: req.requestKey, index: qi, multiple: !!question.multiple },
          cursor: "text",
        };
        regions.push(region);
        const iy = cy;
        blocks.push({
          y: iy,
          h: 33,
          regions: [region],
          lines: [],
          boxes: [],
          paint: (ctx, view) => {
            roundRectPath(ctx, flow.x0 + padX + 0.5, iy + 0.5, innerW - 1, 32, 7);
            ctx.fillStyle = view.theme.bgPanel;
            ctx.fill();
            ctx.strokeStyle = view.theme.borderLight;
            ctx.lineWidth = 1;
            ctx.stroke();
            ctx.font = font(12.5);
            ctx.fillStyle = value ? view.theme.text : view.theme.textFaint;
            ctx.fillText(
              ellipsis(value || "输入其他答案", font(12.5), innerW - 20),
              flow.x0 + padX + 10,
              iy + baselineOf(33, font(12.5)),
            );
          },
        });
        cy += 33 + 7;
      }
      cy += 9; // gap between question blocks
    });
  }

  // 操作按钮
  interface Btn {
    text: string;
    style: "allow" | "reject" | "plain";
    region: Region;
    disabled?: boolean;
  }
  const btns: Btn[] = [];
  let bx = flow.x0 + padX;
  const btnH = 30;
  const pushBtn = (text: string, style: Btn["style"], action: Action, disabled = false) => {
    const w = measure(text, font(12.5)) + 28;
    const region: Region = {
      x: bx,
      y: cy,
      w,
      h: btnH,
      action,
      cursor: disabled ? "default" : "pointer",
    };
    btns.push({ text, style, region, disabled });
    regions.push(region);
    bx += w + 8;
  };
  if (req.questions) {
    const questionAnswers = answers.map((selected, index) => {
      const c = (custom[index] ?? "").trim();
      return c ? [...selected, c] : selected;
    });
    const canAnswer = questionAnswers.every((answer) => answer.length > 0);
    pushBtn("提交回答", "allow", { kind: "perm-submit", reqKey: req.requestKey, answers: questionAnswers }, !canAnswer);
    pushBtn("拒绝回答", "reject", { kind: "perm-reject", reqKey: req.requestKey });
  } else {
    for (const opt of req.options) {
      const style: Btn["style"] = opt.kind.startsWith("allow") ? "allow" : opt.kind.startsWith("reject") ? "reject" : "plain";
      pushBtn(opt.name, style, { kind: "perm-option", reqKey: req.requestKey, optionId: opt.optionId });
    }
  }
  const btnY = cy;
  blocks.push({
    y: btnY,
    h: btnH,
    regions: [],
    lines: [],
    boxes: [],
    paint: (ctx, view) => {
      for (const btn of btns) {
        const r = btn.region;
        const hovered = view.hover === r && !btn.disabled;
        ctx.globalAlpha = btn.disabled ? 0.45 : 1;
        roundRectPath(ctx, r.x + 0.5, r.y + 0.5, r.w - 1, r.h - 1, 7);
        if (btn.style === "allow") {
          ctx.fillStyle = hovered ? view.theme.accent20 : view.theme.accentDim;
          ctx.fill();
          ctx.strokeStyle = view.theme.accent34;
        } else if (btn.style === "reject") {
          ctx.fillStyle = hovered ? view.theme.red16 : view.theme.red8;
          ctx.fill();
          ctx.strokeStyle = view.theme.red30;
        } else {
          ctx.fillStyle = view.theme.bgPanel;
          ctx.fill();
          ctx.strokeStyle = view.theme.borderLight;
        }
        ctx.lineWidth = 1;
        ctx.stroke();
        ctx.font = font(12.5);
        ctx.fillStyle = btn.style === "allow" ? view.theme.accent : btn.style === "reject" ? view.theme.red : view.theme.textDim;
        ctx.fillText(btn.text, r.x + 14, r.y + baselineOf(btnH, font(12.5)));
        ctx.globalAlpha = 1;
      }
    },
  });
  cy += btnH;

  const totalH = cy + 12 - y0;
  const cardLines = [...headT];
  flow.blocks.push({
    y: y0,
    h: totalH,
    regions: [],
    lines: cardLines,
    boxes,
    paint: (ctx, view) => {
      roundRectPath(ctx, flow.x0 + 0.5, y0 + 0.5, flow.width - 1, totalH - 1, 10);
      ctx.fillStyle = view.theme.yellow6;
      ctx.fill();
      ctx.strokeStyle = view.theme.yellow36;
      ctx.lineWidth = 1;
      ctx.stroke();
      drawIcon(ctx, toolIconName(req.toolCall?.kind ?? "other"), iconX, iconY, 14, view.theme.yellow);
      paintTextLines(ctx, view, cardLines);
    },
  });
  flow.blocks.push(...blocks);
  for (const box of boxes) {
    flow.blocks.push({ y: box.y, h: box.h, regions: [], lines: [], boxes: [box], paint: (ctx, view) => box.paint(ctx, view) });
  }
  flow.y = y0 + totalH + 10;
}

/* ===== 分组装配 ===== */

export function layoutGroup(group: Group, env: LayoutEnv, active: boolean): GroupLayout {
  const flow: Flow = { blocks: [], y: 0, x0: env.x0, width: env.width, theme: env.theme, spinners: 0 };

  if (group.user) userBlock(flow, group.user, env);
  if (group.turn?.actualModel) {
    const theme = env.theme;
    const text = `实际模型：${group.turn.actualModel}`;
    const f = font(11, { mono: true });
    const w = measure(text, f) + 14;
    const pillH = 11 * 1.4 + 4;
    const x = env.x0 + env.width - w;
    const y = flow.y - 8;
    flow.blocks.push({
      y,
      h: pillH + 8,
      regions: [],
      lines: [],
      boxes: [],
      paint: (ctx, view) => {
        roundRectPath(ctx, x + 0.5, y + 0.5, w - 1, pillH - 1, pillH / 2);
        ctx.fillStyle = view.theme.bgPanel;
        ctx.fill();
        ctx.strokeStyle = view.theme.borderLight;
        ctx.lineWidth = 1;
        ctx.stroke();
        ctx.font = f;
        ctx.fillStyle = view.theme.textFaint;
        ctx.fillText(text, x + 7, y + baselineOf(pillH, f));
      },
    });
    flow.y += pillH;
    void theme;
  }

  // 与 TurnGroup.split 一致
  const body = group.body;
  let process: Item[];
  let conclusion: Item[];
  if (!group.turn) {
    process = body;
    conclusion = [];
  } else {
    let last = -1;
    for (let i = body.length - 1; i >= 0; i--) {
      if (body[i].type === "assistant" || body[i].type === "system") {
        last = i;
        break;
      }
    }
    if (last < 0) {
      process = body;
      conclusion = [];
    } else {
      let first = last;
      while (first > 0 && (body[first - 1].type === "assistant" || body[first - 1].type === "system")) first--;
      process = [...body.slice(0, first), ...body.slice(last + 1)];
      conclusion = body.slice(first, last + 1);
    }
  }

  const foldKey = `turn-${group.turn?.id ?? group.user?.id ?? group.body[0]?.id ?? 0}`;
  const bodyExpanded = body.some((it) => state.expanded[String(it.id)]);
  const open = state.expanded[foldKey] ?? bodyExpanded;
  const foldable = !!group.turn && !active;

  const activeBodyId = (() => {
    if (!active) return -1;
    for (let i = body.length - 1; i >= 0; i--) {
      if (isBusyItem(body[i])) return body[i].id;
    }
    return body.length ? body[body.length - 1].id : -1;
  })();

  if (process.length > 0) {
    if (foldable) {
      const t = group.turn!;
      const dur = fmtDuration(t.durationMs);
      const tok = t.totalTokens ? `${fmtTokens(t.totalTokens)} tokens` : "";
      const label = ["已处理", dur, tok ? `· ${tok}` : ""].filter(Boolean).join(" ");
      const cacheRead = t.cacheReadTokens ?? 0;
      const cacheWrite = t.cacheWriteTokens ?? 0;
      const read = Math.max(0, (t.inputTokens ?? 0) - cacheRead - cacheWrite);
      const parts = [`读取 ${fmtTokens(read)}`, `写入 ${fmtTokens(t.outputTokens ?? 0)}`];
      if (t.cacheReadTokens != null) parts.push(`缓存读取 ${fmtTokens(cacheRead)}`);
      if (t.cacheWriteTokens != null) parts.push(`缓存写入 ${cacheWrite}`);
      const tokenTitle = t.totalTokens ? `${parts.join(" / ")} tokens` : undefined;

      const f = font(13);
      const y0 = flow.y + 12;
      const rowH = 26;
      const textW = measure(label, f);
      const foldX = env.x0 - 8;
      const region: Region = {
        x: foldX,
        y: y0,
        w: 8 + textW + 6 + 12 + 8,
        h: rowH,
        action: { kind: "toggle", key: foldKey, value: !open },
        cursor: "pointer",
        title: tokenTitle,
        groupId: `fold-${foldKey}`,
      };
      flow.blocks.push({
        y: y0,
        h: rowH + 2,
        regions: [region],
        lines: [],
        boxes: [],
        paint: (ctx, view) => {
          const hovered = view.hoverGroup === `fold-${foldKey}`;
          if (hovered) {
            roundRectPath(ctx, region.x, region.y, region.w, region.h, 7);
            ctx.fillStyle = view.theme.bgHover;
            ctx.fill();
          }
          ctx.font = f;
          ctx.fillStyle = hovered ? view.theme.text : view.theme.textDim;
          ctx.fillText(label, foldX + 8, y0 + baselineOf(rowH, f));
          drawIcon(ctx, open ? "chevronDown" : "chevronRight", foldX + 8 + textW + 6, y0 + rowH / 2 - 6, 12, hovered ? view.theme.text : view.theme.textDim);
        },
      });
      flow.y = y0 + rowH + 2;
      if (open) {
        const procY = flow.y + 4;
        // 子流程直接在绝对 y=procY 上布局，使块内 paint 闭包捕获的坐标与最终位置一致。
        // CSS .turn-process: margin 4px 0 6px; padding 2px 0 2px 12px; border-left 2px
        const sub = layoutItems(process, env, false, activeBodyId, true, procY + 2);
        const procBorderH = 2 + sub.height + 2; // pad-top + content + pad-bottom
        flow.blocks.push({
          y: procY,
          h: procBorderH + 6, // + margin-bottom 6px
          regions: [],
          lines: [],
          boxes: [],
          paint: (ctx, view) => {
            ctx.fillStyle = view.theme.border;
            ctx.fillRect(env.x0, procY, 2, procBorderH);
          },
        });
        flow.blocks.push(...sub.blocks);
        flow.y = procY + 2 + sub.height + 2 + 6; // pad-top + content + pad-bottom + margin-bottom
        if (sub.hasSpinner) flow.spinners++;
      }
    } else {
      const startY = flow.y;
      const sub = layoutItems(process, env, active, activeBodyId, false, startY);
      flow.blocks.push(...sub.blocks);
      flow.y = startY + sub.height;
      if (sub.hasSpinner) flow.spinners++;
    }
  }

  // live tail
  const showLiveTail = (() => {
    if (!active) return false;
    if (body.length === 0 || !body.some(isBusyItem)) return false;
    const last = body[body.length - 1];
    return last.type === "assistant" || last.type === "system";
  })();
  if (showLiveTail) {
    const y0 = flow.y + 4;
    const f = font(12.5);
    flow.spinners++;
    const ltH = 12.5 * 1.5;
    flow.blocks.push({
      y: y0,
      h: ltH + 10,
      regions: [],
      lines: [],
      boxes: [],
      paint: (ctx, view) => {
        drawSpinner(ctx, env.x0 + 5.5, y0 + ltH / 2, 11, view.now, view.theme.blue, view.theme.accent26);
        ctx.font = f;
        ctx.fillStyle = view.theme.textFaint;
        ctx.fillText("继续处理中…", env.x0 + 17, y0 + baselineOf(ltH, f));
      },
    });
    flow.y = y0 + ltH + 10;
  }

  for (const item of conclusion) {
    const before = flow.y;
    layoutItem(flow, item, env, false);
    void before;
  }

  if (foldable) filesCard(flow, group.body, foldKey, env);

  return { blocks: flow.blocks, height: flow.y, hasSpinner: flow.spinners > 0 };
}

function layoutItems(
  items: Item[],
  env: LayoutEnv,
  active: boolean,
  activeBodyId: number,
  indented: boolean,
  startY: number,
): GroupLayout {
  const x0 = indented ? env.x0 + 14 : env.x0;
  const width = indented ? env.width - 14 : env.width;
  // 从绝对 startY 起布局：块内 paint 闭包直接捕获正确坐标，无需事后偏移。
  const flow: Flow = { blocks: [], y: startY, x0, width, theme: env.theme, spinners: 0 };
  for (const item of items) layoutItem(flow, item, env, active && item.id === activeBodyId);
  return { blocks: flow.blocks, height: flow.y - startY, hasSpinner: flow.spinners > 0 };
}

function layoutItem(flow: Flow, item: Item, env: LayoutEnv, active: boolean): void {
  switch (item.type) {
    case "user":
      userBlock(flow, item, env);
      break;
    case "assistant":
      assistantBlock(flow, item, env);
      break;
    case "thought":
      thoughtBlock(flow, item, env, active);
      break;
    case "tool":
      toolBlock(flow, item, env, active);
      break;
    case "system":
      if (!isCodexModelResumeWarning(item)) systemBlock(flow, item);
      break;
    case "turn":
      break;
  }
}

/* ===== 提示与权限装配 ===== */

export function layoutHint(env: LayoutEnv): GroupLayout {
  const flow: Flow = { blocks: [], y: 0, x0: env.x0, width: env.width, theme: env.theme, spinners: 0 };
  const theme = env.theme;
  const f = font(13);
  const lineH = 13 * 1.8;
  const label = agentLabel(state.agentKind);
  const segs: Seg[] = [
    { t: "在下方输入任务，", f, color: theme.textFaint },
    { t: label, f, color: theme.textFaint },
    { t: " 将在 ", f, color: theme.textFaint },
    { t: env.cwd, f: font(12, { mono: true }), color: theme.textFaint, chip: { kind: "code" } },
    { t: " 中工作。", f, color: theme.textFaint },
  ];
  const totalW = segs.reduce((sum, s) => sum + (s.chip ? chipWidth(s) : measure(s.t, s.f)), 0);
  const lines = wrapSegs(segs, env.width, lineH, f);
  const x = env.x0 + Math.max(0, (env.width - totalW) / 2);
  const y0 = 40;
  const placed: TLine[] = [];
  let cy = y0;
  for (const line of lines) {
    placed.push(toTLine(line, x, cy));
    cy += lineH;
  }
  flow.blocks.push({
    y: y0,
    h: cy - y0 + 40,
    regions: [],
    lines: placed,
    boxes: [],
    paint: (ctx, view) => paintTextLines(ctx, view, placed),
  });
  flow.y = cy + 40;
  return { blocks: flow.blocks, height: flow.y, hasSpinner: false };
}

export function layoutPermission(req: PermissionRequest, env: LayoutEnv): GroupLayout {
  const flow: Flow = { blocks: [], y: 0, x0: env.x0, width: env.width, theme: env.theme, spinners: 0 };
  permBlock(flow, req, env);
  return { blocks: flow.blocks, height: flow.y, hasSpinner: false };
}

/* ===== 文档装配：分配全局字符序号 ===== */

export function assembleDoc(
  groupSections: { top: number; layout: GroupLayout }[],
  permSections: { top: number; layout: GroupLayout }[],
  hintSection: { top: number; layout: GroupLayout } | null,
  height: number,
  prev?: Doc | null,
): Doc {
  const allSections = [...groupSections, ...permSections];
  if (hintSection) allSections.push(hintSection);

  // 增量装配：检测前缀多少个 section 的 layout 对象未变
  let stableSections = 0;
  let charN = 0;
  let blockId = 0;
  if (prev && prev.sectionCharN.length > 0) {
    const maxStable = Math.min(prev.sections.length, allSections.length);
    for (let i = 0; i < maxStable; i++) {
      if (prev.sections[i].blocks !== allSections[i].layout.blocks) break;
      stableSections = i + 1;
    }
    if (stableSections > 0) {
      // O(1) 查找稳定前缀末尾的累积值
      charN = prev.sectionCharN[stableSections - 1];
      blockId = prev.sectionBlockId[stableSections - 1];
    }
  }

  const sections: DocSection[] = [];
  const lines: { line: TLine; top: number }[] = [];
  const copyOrder: { line: TLine; top: number; box?: ScrollBox }[] = [];
  const boxes: { box: ScrollBox; top: number }[] = [];
  const regions: { region: Region; top: number }[] = [];
  const sectionCharN: number[] = [];
  const sectionBlockId: number[] = [];
  let hasSpinner = false;

  // 复用稳定前缀的索引数据（O(1) 切片，不遍历）
  if (stableSections > 0 && prev) {
    for (let i = 0; i < stableSections; i++) sections.push(prev.sections[i]);
    hasSpinner = prev.sections.slice(0, stableSections).some(s => s.hasSpinner);
    // 复用 prev 的 lines/copyOrder/boxes/regions 中属于稳定前缀的部分
    // 用 sectionCharN 边界定位：稳定前缀的最后一个 section 的 top
    const stableTopMax = stableSections < prev.sections.length
      ? prev.sections[stableSections].top
      : Infinity;
    // 二分查找 prev.lines 中 top < stableTopMax 的范围
    const prevLines = prev.lines;
    let hi = prevLines.length;
    if (stableTopMax < Infinity) {
      let lo = 0;
      while (lo < hi) {
        const mid = (lo + hi) >> 1;
        if (prevLines[mid].top < stableTopMax) lo = mid + 1;
        else hi = mid;
      }
    }
    for (let i = 0; i < hi; i++) lines.push(prevLines[i]);
    // copyOrder/boxes/regions 同样用 top 边界切片
    const prevCopy = prev.copyOrder;
    let hiC = prevCopy.length;
    if (stableTopMax < Infinity) {
      let lo = 0;
      while (lo < hiC) {
        const mid = (lo + hiC) >> 1;
        if (prevCopy[mid].top < stableTopMax) lo = mid + 1;
        else hiC = mid;
      }
    }
    for (let i = 0; i < hiC; i++) copyOrder.push(prevCopy[i]);
    const prevBoxes = prev.boxes;
    let hiB = prevBoxes.length;
    if (stableTopMax < Infinity) {
      let lo = 0;
      while (lo < hiB) {
        const mid = (lo + hiB) >> 1;
        if (prevBoxes[mid].top < stableTopMax) lo = mid + 1;
        else hiB = mid;
      }
    }
    for (let i = 0; i < hiB; i++) boxes.push(prevBoxes[i]);
    const prevRegions = prev.regions;
    let hiR = prevRegions.length;
    if (stableTopMax < Infinity) {
      let lo = 0;
      while (lo < hiR) {
        const mid = (lo + hiR) >> 1;
        if (prevRegions[mid].top < stableTopMax) lo = mid + 1;
        else hiR = mid;
      }
    }
    for (let i = 0; i < hiR; i++) regions.push(prevRegions[i]);
    // 填充稳定前缀的 sectionCharN/sectionBlockId
    for (let i = 0; i < stableSections; i++) {
      sectionCharN.push(prev.sectionCharN[i]);
      sectionBlockId.push(prev.sectionBlockId[i]);
    }
  }

  const pushSection = (top: number, layout: GroupLayout) => {
    sections.push({ top, blocks: layout.blocks, hasSpinner: layout.hasSpinner });
    if (layout.hasSpinner) hasSpinner = true;
    for (const block of layout.blocks) {
      const hasLines = block.lines.length > 0;
      if (hasLines) blockId++;
      for (const line of block.lines) {
        line.blockId = blockId;
        for (const run of line.runs) {
          run.cs = charN;
          charN += run.text.length;
          run.ce = charN;
        }
        lines.push({ line, top });
        copyOrder.push({ line, top });
      }
      for (const box of block.boxes) {
        boxes.push({ box, top });
        const hasBoxLines = box.lines.length > 0;
        if (hasBoxLines) blockId++;
        for (const line of box.lines) {
          line.blockId = blockId;
          for (const run of line.runs) {
            run.cs = charN;
            charN += run.text.length;
            run.ce = charN;
          }
          copyOrder.push({ line, top, box });
        }
        for (const region of box.fixedRegions) regions.push({ region, top });
      }
      for (const region of block.regions) regions.push({ region, top });
    }
    sectionCharN.push(charN);
    sectionBlockId.push(blockId);
  };

  for (let i = stableSections; i < allSections.length; i++) {
    pushSection(allSections[i].top, allSections[i].layout);
  }

  return {
    sections,
    groupTops: groupSections.map((s) => s.top),
    height,
    hasSpinner,
    lines,
    copyOrder,
    boxes,
    regions,
    charTotal: charN,
    stableSections,
    stableCharN: charN,
    stableBlockId: blockId,
    sectionCharN,
    sectionBlockId,
  };
}

/** 生成用于分组布局缓存的签名：展开态 + 内容指纹（最后一条 item 的 text 长度） */
export function groupCacheSig(group: Group): string {
  const foldId = group.turn?.id ?? group.user?.id ?? group.body[0]?.id ?? 0;
  const keys = [`turn-${foldId}`, `undone-turn-${foldId}`];
  for (const it of group.body) {
    keys.push(`tool-${it.id}`, `tool-raw-${it.id}`, `thought-${it.id}`, String(it.id));
  }
  let sig = "";
  for (const k of keys) sig += state.expanded[k] ? "1" : "0";
  // 内容指纹：最后一条 assistant/thought 的 text 长度 + user text 长度
  // 流式期间 text 增长 → sig 变 → 缓存失效 → 重布局
  // 非流式期间 text 不变 → sig 不变 → 缓存命中 → 跳过布局
  const last = group.body[group.body.length - 1];
  if (last && (last.type === "assistant" || last.type === "thought")) {
    sig += `:${last.text.length}`;
  }
  if (group.user) sig += `:u${group.user.text.length}`;
  return sig;
}
