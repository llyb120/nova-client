// layout.ts — 排版引擎（移植自参考项目 layout.js，按会话需求重写）：
// - CSS 风格断行：拉丁按词、CJK/假名/全角逐字、break-all、超长硬断、pre/pre-wrap
// - 文本测量 LRU 缓存（流式与长会话的性能命脉）
// - inline 混合样式片段（粗体/斜体/行内 code/链接/chip）同行混排、基线对齐
// - block 兄弟纵向 margin 折叠（max 规则）+ 无 padding/border 时的穿透（markdown 段距）
// - flex row/column、gap、wrap、justify/align/grow/shrink、min/maxHeight、fit-content、ellipsis
// - 增量布局：干净子树按上次排版结果平移，不重新测量

import { Node, edgesOf, type Style, type TextLine } from "./node.js";

// ---------- 字体与测量 ----------

export function fontOf(s: Style): string {
  return `${s.fontStyle} ${s.fontWeight} ${s.fontSize}px ${s.fontFamily}`;
}

let _ctx: CanvasRenderingContext2D | null = null;
function measureCtx(): CanvasRenderingContext2D | null {
  if (!_ctx) {
    const c = typeof document !== "undefined" ? document.createElement("canvas") : null;
    _ctx = c ? c.getContext("2d") : null;
  }
  return _ctx;
}

const MEASURE_LIMIT = 12000;
const measureCache = new Map<string, number>();

/** 字体字符串 → ctx.font 简式（"italic weight 12.5px family"） */
function fontCheckString(font: string): string {
  const m = /^\S+\s+\S+\s+([\d.]+)px\s+(.+)$/.exec(font);
  return m ? `${m[1]}px ${m[2]}` : font;
}

/** 该字体的首选字族是否已可用（未就绪时测量是后备度量，不能缓存） */
function fontReady(font: string): boolean {
  try {
    if (typeof document === "undefined" || !document.fonts?.check) return true;
    return document.fonts.check(fontCheckString(font));
  } catch {
    return true;
  }
}

export function measureText(text: string, font: string, letterSpacing = 0): number {
  if (!text) return 0;
  const key = `${font}${letterSpacing}|${text}`;
  const hit = measureCache.get(key);
  if (hit !== undefined) return hit;
  const ctx = measureCtx();
  let w: number;
  if (!ctx) {
    w = text.length * 8;
  } else {
    ctx.font = font;
    const c2 = ctx as CanvasRenderingContext2D & { letterSpacing?: string };
    c2.letterSpacing = letterSpacing ? `${letterSpacing}px` : "0px";
    w = ctx.measureText(text).width;
    c2.letterSpacing = "0px";
  }
  // 字体未就绪的测量按后备字体计（偏窄）：不缓存，等字体到位后重测；
  // 就绪的测量才进入缓存（loadingdone 兜底会再清一次）
  if (ctx && !fontReady(font)) return w;
  if (measureCache.size >= MEASURE_LIMIT) {
    const oldest = measureCache.keys().next().value;
    if (oldest !== undefined) measureCache.delete(oldest);
  }
  measureCache.set(key, w);
  return w;
}

interface FontMetrics {
  ascent: number;
  descent: number;
}

const metricsCache = new Map<string, FontMetrics>();

export function fontMetrics(font: string, fontSize: number): FontMetrics {
  let m = metricsCache.get(font);
  if (m) return m;
  const ctx = measureCtx();
  if (ctx) {
    ctx.font = font;
    const met = ctx.measureText("Mg中 gj");
    m = {
      ascent: met.fontBoundingBoxAscent || fontSize * 0.8,
      descent: met.fontBoundingBoxDescent || fontSize * 0.25,
    };
  } else {
    m = { ascent: fontSize * 0.8, descent: fontSize * 0.25 };
  }
  metricsCache.set(font, m);
  return m;
}

/** 主题/字体加载变化时清空测量缓存。 */
export function resetMeasureCaches(): void {
  measureCache.clear();
  metricsCache.clear();
}

// ---------- 工具 ----------

function toPx(v: number | string | undefined, ref: number): number | null {
  if (v == null || v === "auto") return null;
  if (typeof v === "number") return v;
  const s = String(v).trim();
  if (s.endsWith("px")) return parseFloat(s);
  if (s.endsWith("%")) return (parseFloat(s) / 100) * ref;
  const n = parseFloat(s);
  return Number.isNaN(n) ? null : n;
}

function isAuto(v: number | string | undefined): boolean {
  return v == null || v === "auto";
}

// CJK / 假名 / 全角 / 谚文 / emoji：CSS 允许在这些字符间断行
const CJK_RE =
  /[\u1100-\u11FF\u2E80-\uA4CF\uAC00-\uD7AF\uF900-\uFAFF\uFE30-\uFE4F\uFF00-\uFFEF\u{1F000}-\u{1FAFF}\u{20000}-\u{2FA1F}]/u;
// 禁则（kinsoku）：闭标点不可开行，开标点不可结尾
const CLOSING_PUNCT = new Set([..."、，。；：！？）】》」』”’…—·～％﹪﹫"]);
const OPENING_PUNCT = new Set([..."（【《「『“‘﹝［｛"]);

type TokenKind = "word" | "space" | "cjk" | "newline";
interface Token {
  s: string;
  kind: TokenKind;
}

/** 断行 token 化：空白 run / 拉丁词 / 单个 CJK 字符（禁则合并相邻字）/ 换行符 */
function tokenize(text: string, whiteSpace: Style["whiteSpace"], wordBreak: Style["wordBreak"]): Token[] {
  const out: Token[] = [];
  const keepNewline = whiteSpace === "pre" || whiteSpace === "pre-wrap";
  let word = "";
  let space = "";
  const flushWord = () => {
    if (word) {
      out.push({ s: word, kind: "word" });
      word = "";
    }
  };
  const flushSpace = () => {
    if (space) {
      out.push({ s: space, kind: "space" });
      space = "";
    }
  };
  const pushCjk = (ch: string) => {
    // 禁则：开标点并入后一字；闭标点并入前一字（不断开）
    const last = out[out.length - 1];
    if (last && last.kind === "cjk") {
      const lastChar = [...last.s].pop()!;
      if (OPENING_PUNCT.has(lastChar)) {
        last.s += ch;
        return;
      }
    }
    if (CLOSING_PUNCT.has(ch) && last && last.kind === "cjk") {
      last.s += ch;
      return;
    }
    out.push({ s: ch, kind: "cjk" });
  };
  for (const ch of text) {
    if (ch === "\n") {
      flushWord();
      flushSpace();
      if (keepNewline) out.push({ s: "\n", kind: "newline" });
      else space += " ";
      continue;
    }
    if (ch === " " || ch === "\t" || ch === "\r") {
      flushWord();
      if (keepNewline) out.push({ s: ch === "\t" ? "    " : " ", kind: "space" });
      else space += ch === "\t" ? "    " : " ";
      continue;
    }
    flushSpace();
    if (wordBreak === "break-all") {
      flushWord();
      out.push({ s: ch, kind: "cjk" });
    } else if (CJK_RE.test(ch)) {
      flushWord();
      pushCjk(ch);
    } else {
      word += ch;
    }
  }
  flushWord();
  flushSpace();
  return out;
}

// ---------- 文本行排版 ----------

/** 一段待排内容：文本片段（带样式）或 inline-block 原子 */
interface WrapPart {
  owner: Node;
  text: string;
  offset: number; // 起始字符在 owner.textContent 中的下标
  w: number;
  font: string;
  lineH: number; // fs * lineHeight 倍数
  ascent: number;
  descent: number;
  atomic?: Node; // inline-block 子节点
  continuesFromPrev?: boolean; // 长词硬断产生的续片（bg padding 不在断裂处重复）
  continuesToNext?: boolean;
}

interface WrapLine {
  parts: WrapPart[];
  w: number;
}

/** 长词按字符硬断（overflow-wrap: anywhere / break-word 语义） */
function splitLongPart(part: WrapPart, maxW: number, ls: number): WrapPart[] {
  const chars = [...part.text];
  const out: WrapPart[] = [];
  let buf = "";
  let bufW = 0;
  let offset = part.offset;
  for (const ch of chars) {
    const cw = measureText(buf + ch, part.font, ls) - bufW;
    if (buf && bufW + cw > maxW) {
      out.push({ ...part, text: buf, w: bufW, offset, continuesFromPrev: out.length > 0, continuesToNext: true });
      offset += buf.length;
      buf = ch;
      bufW = measureText(ch, part.font, ls);
    } else {
      buf += ch;
      bufW += cw;
    }
  }
  if (buf) out.push({ ...part, text: buf, w: bufW, offset, continuesFromPrev: out.length > 0 });
  return out;
}

/** 贪心跳行：空白处 / CJK 边界可断；拉丁词不可断（超宽时 hardSplit 硬断）。
 * mode=pre：仅 \n 断行，超宽溢出；mode=pre-wrap：折行但保留行首空白（缩进是内容）。 */
function wrapParts(parts: WrapPart[], maxW: number, mode: "normal" | "pre" | "pre-wrap" = "normal"): WrapLine[] {
  const preMode = mode === "pre";
  const preserveSpaces = mode === "pre-wrap";
  const lines: WrapLine[] = [];
  let cur: WrapPart[] = [];
  let curW = 0;

  const isSpacePart = (p: WrapPart) => !p.atomic && p.text !== "\n" && p.text.trim() === "";

  const pushLine = () => {
    let w = curW;
    while (cur.length && isSpacePart(cur[cur.length - 1])) {
      w -= cur[cur.length - 1].w;
      cur.pop();
    }
    lines.push({ parts: cur, w: Math.max(0, w) });
    cur = [];
    curW = 0;
  };

  for (const part of parts) {
    if (part.text === "\n") {
      pushLine();
      continue;
    }
    if (preMode) {
      // pre：不换行，空白照常保留
      cur.push(part);
      curW += part.w;
      continue;
    }
    if (isSpacePart(part) && cur.length === 0 && !preserveSpaces) continue; // 行首空白丢弃（pre-wrap 保留）
    if (part.w > maxW && !isSpacePart(part)) {
      // 单 token 超行宽：先换行再硬断
      if (cur.length > 0) pushLine();
      for (const sp of splitLongPart(part, maxW, part.owner.style.letterSpacing)) {
        cur.push(sp);
        curW += sp.w;
        if (sp.continuesToNext) pushLine();
      }
      continue;
    }
    if (curW + part.w > maxW && cur.length > 0) {
      // 行尾空白不占宽度后再判一次
      let trailing = 0;
      for (let i = cur.length - 1; i >= 0 && isSpacePart(cur[i]); i--) trailing += cur[i].w;
      if (curW - trailing + part.w > maxW) {
        pushLine();
        if (isSpacePart(part)) continue; // 换行处空白丢弃
      }
    }
    cur.push(part);
    curW += part.w;
  }
  if (cur.length > 0) pushLine();
  if (lines.length === 0) lines.push({ parts: [], w: 0 });
  return lines;
}

interface Strut {
  font: string;
  lineH: number;
  ascent: number;
  descent: number;
}

function strutOf(style: Style): Strut {
  const font = fontOf(style);
  const met = fontMetrics(font, style.fontSize);
  return { font, lineH: style.fontSize * style.lineHeight, ascent: met.ascent, descent: met.descent };
}

/** 逐行放置：算行高/基线，把 TextLine（绝对坐标）写到各 part.owner._textLines */
function placeLines(
  wrapLines: WrapLine[],
  startX: number,
  startY: number,
  maxW: number,
  strut: Strut,
  textAlign: Style["textAlign"],
): { height: number; maxLineW: number } {
  let y = startY;
  let maxLineW = 0;
  const strutHalf = Math.max(0, (strut.lineH - strut.ascent - strut.descent) / 2);
  for (const wl of wrapLines) {
    let lineAscent = strutHalf + strut.ascent;
    let lineDescent = strutHalf + strut.descent;
    for (const p of wl.parts) {
      if (p.atomic) continue;
      const hl = Math.max(0, (p.lineH - p.ascent - p.descent) / 2);
      lineAscent = Math.max(lineAscent, hl + p.ascent);
      lineDescent = Math.max(lineDescent, hl + p.descent);
    }
    // inline-block 原子（chip/图片）：行盒至少装下原子高（底对齐近似）
    for (const p of wl.parts) {
      if (!p.atomic) continue;
      const need = p.atomic._height - lineAscent;
      if (need > lineDescent) lineDescent = need;
    }
    const lineH = lineAscent + lineDescent;
    let x = startX;
    if (textAlign === "center") x = startX + (maxW - wl.w) / 2;
    else if (textAlign === "right") x = startX + (maxW - wl.w);
    for (const p of wl.parts) {
      if (p.atomic) {
        const ay = y + lineH - p.atomic._height;
        shiftTree(p.atomic, x - p.atomic._x, ay - p.atomic._y);
        x += p.w;
        continue;
      }
      const hl = Math.max(0, (p.lineH - p.ascent - p.descent) / 2);
      p.owner._textLines ??= [];
      p.owner._textLines.push({
        text: p.text,
        x,
        y: y + lineAscent - hl - p.ascent,
        w: p.w,
        lh: p.lineH,
        baseline: hl + p.ascent,
        fs: p.owner.style.fontSize,
        ascent: p.ascent,
        descent: p.descent,
        offset: p.offset,
        owner: p.owner,
        font: p.font,
        continuesFromPrev: p.continuesFromPrev,
        continuesToNext: p.continuesToNext,
      });
      x += p.w;
    }
    maxLineW = Math.max(maxLineW, wl.w);
    y += lineH;
  }
  return { height: y - startY, maxLineW };
}

/** 单样式文本 → WrapPart 流 */
function partsOfText(node: Node): WrapPart[] {
  const s = node.style;
  const font = fontOf(s);
  const met = fontMetrics(font, s.fontSize);
  const lineH = s.fontSize * s.lineHeight;
  const tokens = tokenize(node.textContent, s.whiteSpace, s.wordBreak);
  const parts: WrapPart[] = [];
  let offset = 0;
  for (const t of tokens) {
    if (t.kind === "newline") {
      parts.push({ owner: node, text: "\n", offset: offset++, w: 0, font, lineH, ascent: met.ascent, descent: met.descent });
      continue;
    }
    parts.push({
      owner: node,
      text: t.s,
      offset,
      w: measureText(t.s, font, s.letterSpacing),
      font,
      lineH,
      ascent: met.ascent,
      descent: met.descent,
    });
    offset += t.s.length;
  }
  return parts;
}

/** nowrap + ellipsis：截断为单行 */
function truncateEllipsis(node: Node, innerX: number, innerY: number, maxW: number): { height: number; maxLineW: number } {
  const s = node.style;
  const font = fontOf(s);
  const strut = strutOf(s);
  const text = node.textContent;
  const ellW = measureText("…", font, s.letterSpacing);
  let out = text;
  if (measureText(text, font, s.letterSpacing) > maxW) {
    let lo = 0;
    let hi = text.length;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (measureText(text.slice(0, mid), font, s.letterSpacing) + ellW <= maxW) lo = mid;
      else hi = mid - 1;
    }
    out = text.slice(0, lo) + "…";
  }
  const w = Math.min(measureText(out, font, s.letterSpacing), maxW);
  node._textLines = [{
    text: out,
    x: innerX,
    y: innerY,
    w,
    lh: strut.lineH,
    baseline: strut.ascent + Math.max(0, (strut.lineH - strut.ascent - strut.descent) / 2),
    fs: s.fontSize,
    ascent: strut.ascent,
    descent: strut.descent,
    offset: 0,
    owner: node,
    font,
  }];
  return { height: strut.lineH, maxLineW: w };
}

// ---------- 增量布局 ----------

/** 子树整体平移（干净子树复用上次排版结果时修正绝对坐标） */
export function shiftTree(node: Node, dx: number, dy: number): void {
  if (dx === 0 && dy === 0) return;
  for (const n of node.walk()) {
    n._x += dx;
    n._y += dy;
    if (n._textLines) {
      for (const ln of n._textLines) {
        ln.x += dx;
        ln.y += dy;
      }
    }
  }
}

// ---------- margin 折叠 ----------

function collapseThroughTop(n: Node): boolean {
  const s = n.style;
  if (s.display === "flex" || s.overflow !== "visible") return false;
  const p = edgesOf(s.padding);
  const b = edgesOf(s.border);
  return p.t + b.t === 0;
}

function collapseThroughBottom(n: Node): boolean {
  const s = n.style;
  if (s.display === "flex" || s.overflow !== "visible") return false;
  const p = edgesOf(s.padding);
  const b = edgesOf(s.border);
  return p.b + b.b === 0;
}

function firstBlockChild(n: Node): Node | null {
  for (const c of n.children) {
    if (c.style.display === "none" || c.style.position === "absolute") continue;
    const d = c.style.display;
    if (d === "inline" || d === "inline-block") return null;
    return c;
  }
  return null;
}

function lastBlockChild(n: Node): Node | null {
  for (let i = n.children.length - 1; i >= 0; i--) {
    const c = n.children[i];
    if (c.style.display === "none" || c.style.position === "absolute") continue;
    const d = c.style.display;
    if (d === "inline" || d === "inline-block") return null;
    return c;
  }
  return null;
}

/** CSS margin 折叠：同号取绝对值大者，异号代数相加 */
function collapseMargin(a: number, b: number): number {
  if (a >= 0 && b >= 0) return Math.max(a, b);
  if (a <= 0 && b <= 0) return Math.min(a, b);
  return a + b;
}

/** 有效 margin-top（含穿透链）：静态样式即可算，无需布局 */
export function effMarginTop(n: Node): number {
  const m = edgesOf(n.style.margin);
  if (!collapseThroughTop(n)) return m.t;
  const first = firstBlockChild(n);
  if (!first) return m.t;
  return collapseMargin(m.t, effMarginTop(first));
}

/** 有效 margin-bottom（含穿透链） */
export function effMarginBottom(n: Node): number {
  const m = edgesOf(n.style.margin);
  if (!collapseThroughBottom(n)) return m.b;
  const last = lastBlockChild(n);
  if (!last) return m.b;
  return collapseMargin(m.b, effMarginBottom(last));
}

// ---------- 主布局 ----------

/**
 * 布局节点（border-box 位于 x+m.l, y+m.t）：返回占高（height + margin）。
 * 干净且同宽的子树直接平移复用，不重新测量。
 * pctRef：% 宽度/maxWidth 的解析基准（默认 availW；flex 子项传容器宽，避免
 * 分配后二次按 % 收缩——CSS 的 % 解析目标是包含块而非分配尺寸）。
 */
export function layout(node: Node, x: number, y: number, availW: number, availH: number, pctRef?: number): number {
  const s = node.style;
  const m = edgesOf(s.margin);
  const pct = pctRef ?? availW;

  if (!node._layoutDirty && node._layoutAvailW === availW) {
    shiftTree(node, x + m.l - node._x, y + m.t - node._y);
    return node._height + m.t + m.b;
  }

  if (s.display === "none") {
    node._x = x;
    node._y = y;
    node._width = 0;
    node._height = 0;
    node._contentWidth = 0;
    node._contentHeight = 0;
    node._maxScrollX = 0;
    node._maxScrollY = 0;
    node._layoutDirty = false;
    node._layoutAvailW = availW;
    return 0;
  }

  const p = edgesOf(s.padding);

  // 宽度
  let width = toPx(s.width, pct);
  // shrink-to-fit：显式 fit-content，或 inline-block 的 auto 宽度
  const fitContent =
    s.width === "fit-content" || (width == null && s.display === "inline-block");
  if (width == null && !fitContent) width = Math.max(0, availW - m.l - m.r);
  const maxWStyle = toPx(s.maxWidth, pct);
  if (maxWStyle != null && width != null) width = Math.min(width, maxWStyle);

  const ox = x + m.l;
  const oy = y + m.t;
  const innerX = ox + p.l;
  const innerY = oy + p.t;
  const gutter =
    s.scrollbarGutter === "stable" && (s.overflow === "auto" || s.overflow === "scroll") ? 10 : 0;
  const resolvedW = width ?? Math.max(0, availW - m.l - m.r);
  const innerW = Math.max(0, resolvedW - p.l - p.r - gutter);

  node._textLines = null;

  let contentH = 0;
  let contentW = 0;

  if (node.tag === "img") {
    const img = node._img;
    let iw = 0;
    let ih = 0;
    if (img && node._imgLoaded && img.naturalWidth > 0) {
      const ratio = img.naturalWidth / img.naturalHeight;
      const mw = toPx(s.maxWidth, pct) ?? Infinity;
      const mh = toPx(s.maxHeight, availH) ?? Infinity;
      iw = Math.min(
        isAuto(s.width) ? img.naturalWidth : (toPx(s.width, pct) ?? img.naturalWidth),
        mw,
        pct - m.l - m.r,
      );
      ih = iw / ratio;
      if (ih > mh) {
        ih = mh;
        iw = ih * ratio;
      }
    } else if (!isAuto(s.height)) {
      ih = toPx(s.height, availH) || 0;
    }
    // img 的宽度完全由内容决定（未加载 = 0，不走块级默认占满）
    width = isAuto(s.width) ? iw : (toPx(s.width, pct) ?? iw);
    const imgMaxW = toPx(s.maxWidth, pct);
    if (imgMaxW != null) width = Math.min(width, imgMaxW);
    contentW = iw;
    contentH = ih;
  } else if (node.tag === "icon" || node.tag === "spinner") {
    const size = isAuto(s.width) ? s.fontSize : (toPx(s.width, availW) ?? s.fontSize);
    contentW = size;
    contentH = size;
    if (width == null) width = size;
  } else if (s.display === "flex") {
    const r = layoutFlex(node, innerX, innerY, innerW, availH);
    contentW = r.contentW;
    contentH = r.contentH;
  } else {
    const r = layoutBlock(node, innerX, innerY, innerW, availH);
    contentW = r.contentW;
    contentH = r.contentH;
  }

  // fit-content：先量内容再定宽（min(内容宽, 可用宽)）；inline-block 同语义（shrink-to-fit）
  if (fitContent && width == null) {
    const wanted = contentW + p.l + p.r;
    width = Math.min(wanted, availW - m.l - m.r);
    if (width < wanted) {
      const finalInnerW = Math.max(0, width - p.l - p.r - gutter);
      node._textLines = null;
      for (const c of node.children) c.markLayoutDirty();
      if (s.display === "flex") {
        const r = layoutFlex(node, innerX, innerY, finalInnerW, availH);
        contentW = r.contentW;
        contentH = r.contentH;
      } else {
        const r = layoutBlock(node, innerX, innerY, finalInnerW, availH);
        contentW = r.contentW;
        contentH = r.contentH;
      }
    }
  }

  let height = isAuto(s.height) ? contentH + p.t + p.b : (toPx(s.height, availH) || 0);
  if (s.minHeight != null) height = Math.max(height, s.minHeight);
  if (s.maxHeight != null) height = Math.min(height, s.maxHeight);
  height = Math.max(0, height);

  node._x = ox;
  node._y = oy;
  node._width = width ?? resolvedW;
  node._height = height;
  node._contentWidth = contentW + p.l + p.r + gutter;
  node._contentHeight = contentH + p.t + p.b;

  // absolute 子节点：相对本节点 padding 盒定位（不参与文档流）
  for (const c of node.children) {
    if (c.style.position !== "absolute") continue;
    layout(c, 0, 0, innerW, availH, innerW);
    const cm = edgesOf(c.style.margin);
    const left = toPx(c.style.left, node._width);
    const right = toPx(c.style.right, node._width);
    const top = toPx(c.style.top, node._height);
    const bottom = toPx(c.style.bottom, node._height);
    let cx: number;
    if (left != null) cx = ox + p.l + left;
    else if (right != null) cx = ox + node._width - p.r - right - c._width - cm.r;
    else cx = ox + p.l;
    let cyAbs: number;
    if (top != null) cyAbs = oy + p.t + top;
    else if (bottom != null) cyAbs = oy + node._height - p.b - bottom - c._height - cm.b;
    else cyAbs = oy + p.t;
    shiftTree(c, cx - c._x, cyAbs - c._y);
  }

  const scrollable = s.overflow === "scroll" || s.overflow === "auto";
  if (scrollable) {
    node._maxScrollY = Math.max(0, node._contentHeight - height);
    node._maxScrollX = Math.max(0, node._contentWidth - node._width);
  } else {
    node._maxScrollX = 0;
    node._maxScrollY = 0;
  }

  node._layoutDirty = false;
  node._layoutAvailW = availW;
  return node._height + m.t + m.b;
}

/** 节点自身 textContent 的排版（绝对坐标写入 node._textLines） */
function layoutOwnText(node: Node, innerX: number, innerY: number, innerW: number): { height: number; maxLineW: number } {
  const s = node.style;
  if (!node.textContent) return { height: 0, maxLineW: 0 };
  if (s.whiteSpace === "nowrap" && s.textOverflow === "ellipsis") {
    return truncateEllipsis(node, innerX, innerY, innerW);
  }
  const strut = strutOf(s);
  if (s.whiteSpace === "pre") {
    // pre：仅 \n 断行，不折行（超宽溢出 → 横向滚动）
    const font = fontOf(s);
    const met = fontMetrics(font, s.fontSize);
    const lineH = s.fontSize * s.lineHeight;
    const baseline = met.ascent + Math.max(0, (lineH - met.ascent - met.descent) / 2);
    node._textLines = [];
    let y = innerY;
    let maxLineW = 0;
    let offset = 0;
    const lines = node.textContent.split("\n");
    for (const lineText of lines) {
      const w = measureText(lineText, font, s.letterSpacing);
      node._textLines.push({
        text: lineText,
        x: innerX,
        y,
        w,
        lh: lineH,
        baseline,
        fs: s.fontSize,
        ascent: met.ascent,
        descent: met.descent,
        offset,
        owner: node,
        font,
      });
      offset += lineText.length + 1; // +1 为 \n
      maxLineW = Math.max(maxLineW, w);
      y += lineH;
    }
    return { height: y - innerY, maxLineW };
  }
  const parts = partsOfText(node);
  const wrapLines = wrapParts(parts, innerW, s.whiteSpace === "pre-wrap" ? "pre-wrap" : "normal");
  return placeLines(wrapLines, innerX, innerY, innerW, strut, s.textAlign);
}

/** block 容器：自身文本 + 子节点（inline run / block / flex），纵向 margin 折叠 */
function layoutBlock(node: Node, innerX: number, innerY: number, innerW: number, availH: number) {
  const s = node.style;
  let cy = innerY;
  let contentW = 0;

  if (node.textContent) {
    const tr = layoutOwnText(node, innerX, innerY, innerW);
    cy += tr.height;
    contentW = Math.max(contentW, tr.maxLineW);
  }

  const kids = node.children;
  let prevBottom = 0;
  let first = true;
  let i = 0;
  const throughTop = collapseThroughTop(node);
  const throughBottom = collapseThroughBottom(node);

  while (i < kids.length) {
    const child = kids[i];
    const disp = child.style.display;
    if (disp === "none") {
      layout(child, innerX, cy, innerW, availH, innerW);
      i++;
      continue;
    }
    if (child.style.position === "absolute") {
      i++; // 不参与文档流；layout() 尾部统一相对父盒定位
      continue;
    }
    if (disp === "inline" || disp === "inline-block") {
      const run: Node[] = [];
      while (i < kids.length) {
        const d = kids[i].style.display;
        if (d === "inline" || d === "inline-block") {
          run.push(kids[i]);
          i++;
        } else break;
      }
      if (!first) cy += prevBottom;
      const r = layoutInlineRun(run, node, innerX, cy, innerW, availH);
      cy += r.height;
      contentW = Math.max(contentW, r.maxLineW);
      prevBottom = 0;
      first = false;
    } else {
      const cm = edgesOf(child.style.margin);
      const effT = effMarginTop(child);
      // 折叠语义：穿透链上所有 border-box 重合于 cy + gap；gap 取全链最大 margin。
      // layout() 内部会给 border 加 cm.t → yArg 预减 cm.t。
      const gap = first && throughTop ? 0 : collapseMargin(prevBottom, effT);
      layout(child, innerX, cy + gap - cm.t, innerW, availH, innerW);
      cy = child._y + child._height;
      // 内容宽用 max-content（_contentWidth）而非解析宽：fit-content 父级需要固有尺寸
      contentW = Math.max(contentW, child._x + child._contentWidth + cm.r - innerX);
      prevBottom = effMarginBottom(child);
      first = false;
      i++;
    }
  }
  if (!throughBottom) cy += prevBottom;
  return { contentW, contentH: cy - innerY };
}

/** inline 兄弟的混排（多样式片段 + inline-block 原子） */
function layoutInlineRun(
  run: Node[],
  blockNode: Node,
  innerX: number,
  startY: number,
  innerW: number,
  availH: number,
): { height: number; maxLineW: number } {
  const parts: WrapPart[] = [];
  for (const child of run) {
    const cs = child.style;
    child._textLines = null;
    if (cs.display === "inline-block") {
      layout(child, innerX, startY, innerW, availH, innerW);
      const font = fontOf(blockNode.style);
      const met = fontMetrics(font, blockNode.style.fontSize);
      parts.push({
        owner: child,
        text: "",
        offset: 0,
        w: child._width,
        font,
        lineH: Math.max(child._height, blockNode.style.fontSize * blockNode.style.lineHeight),
        ascent: met.ascent,
        descent: met.descent,
        atomic: child,
      });
      continue;
    }
    const font = fontOf(cs);
    const met = fontMetrics(font, cs.fontSize);
    const lineH = cs.fontSize * cs.lineHeight;
    const tokens = tokenize(child.textContent, cs.whiteSpace, cs.wordBreak);
    let offset = 0;
    for (const t of tokens) {
      if (t.kind === "newline") {
        parts.push({ owner: child, text: "\n", offset: offset++, w: 0, font, lineH, ascent: met.ascent, descent: met.descent });
        continue;
      }
      parts.push({
        owner: child,
        text: t.s,
        offset,
        w: measureText(t.s, font, cs.letterSpacing),
        font,
        lineH,
        ascent: met.ascent,
        descent: met.descent,
      });
      offset += t.s.length;
    }
  }

  const wsMode = blockNode.style.whiteSpace === "pre" ? "pre" : blockNode.style.whiteSpace === "pre-wrap" ? "pre-wrap" : "normal";
  const wrapLines = wrapParts(parts, innerW, wsMode);
  const strut = strutOf(blockNode.style);
  const r = placeLines(wrapLines, innerX, startY, innerW, strut, blockNode.style.textAlign);

  // inline 子节点的包围盒（命中测试/hover 用）
  for (const child of run) {
    if (child.style.display === "inline-block") continue;
    const lines = child._textLines;
    if (!lines || lines.length === 0) {
      child._x = innerX;
      child._y = startY;
      child._width = 0;
      child._height = 0;
      continue;
    }
    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;
    for (const ln of lines) {
      minX = Math.min(minX, ln.x);
      minY = Math.min(minY, ln.y);
      maxX = Math.max(maxX, ln.x + ln.w);
      maxY = Math.max(maxY, ln.y + ln.lh);
    }
    child._x = minX;
    child._y = minY;
    child._width = maxX - minX;
    child._height = maxY - minY;
  }
  return r;
}

// ---------- flex ----------

function parseFlex(flex: number | string): { grow: number; shrink: number } {
  // CSS flex 缩写：单值 `flex: 1` = grow:1 shrink:1（shrink 省略时默认为 1）
  if (typeof flex === "number") return { grow: flex, shrink: flex > 0 ? 1 : 0 };
  const parts = String(flex).split(/\s+/);
  const grow = parseFloat(parts[0]) || 0;
  const shrink = parts.length > 1 ? parseFloat(parts[1]) || 0 : grow > 0 ? 1 : 0;
  return { grow, shrink };
}

function layoutFlex(node: Node, innerX: number, innerY: number, innerW: number, availH: number) {
  const s = node.style;
  const dir = s.flexDirection || "row";
  const gap = typeof s.gap === "number" ? s.gap : 0;
  const kids = node.children.filter((c) => c.style.display !== "none" && c.style.position !== "absolute");

  if (dir === "column") {
    let cy = innerY;
    let contentW = 0;
    for (const child of kids) {
      // flex 容器内不发生折叠，但子节点 margin 穿透链（block 孙级）仍要算
      const effT = effMarginTop(child);
      const effB = effMarginBottom(child);
      const cm = edgesOf(child.style.margin);
      layout(child, innerX, cy + effT - cm.t, innerW, availH, innerW);
      const ai = child.style.alignSelf && child.style.alignSelf !== "auto" ? child.style.alignSelf : s.alignItems;
      if (ai === "center") shiftTree(child, innerX + (innerW - child._width) / 2 - child._x, 0);
      else if (ai === "flex-end") shiftTree(child, innerX + innerW - child._width - cm.r - child._x, 0);
      cy = child._y + child._height + effB + gap;
      contentW = Math.max(contentW, child._width + cm.l + cm.r);
    }
    if (kids.length) cy -= gap;
    return { contentW, contentH: cy - innerY };
  }

  // row：先量固有主尺寸
  const items = kids.map((child) => {
    const explicitW = toPx(child.style.width, innerW);
    layout(child, innerX, innerY, explicitW ?? innerW, availH, innerW);
    let mainSize: number;
    if (explicitW != null) {
      mainSize = explicitW;
    } else if (child.textContent && child.children.length === 0 && child.tag !== "img") {
      const font = fontOf(child.style);
      const p = edgesOf(child.style.padding);
      mainSize = measureText(child.textContent, font, child.style.letterSpacing) + p.l + p.r;
    } else {
      mainSize = child._contentWidth || child._width;
    }
    const mw = toPx(child.style.maxWidth, innerW);
    if (mw != null) mainSize = Math.min(mainSize, mw);
    return { child, mainSize, crossSize: child._height, flex: parseFlex(child.style.flex), margin: edgesOf(child.style.margin) };
  });

  let totalMain = 0;
  for (const it of items) totalMain += it.mainSize + it.margin.l + it.margin.r;
  totalMain += gap * Math.max(0, items.length - 1);
  const free = innerW - totalMain;

  const totalGrow = items.reduce((a, it) => a + it.flex.grow, 0);
  if (free > 0 && totalGrow > 0) {
    for (const it of items) it.mainSize += (it.flex.grow / totalGrow) * free;
  } else if (free < 0) {
    // shrink：按 shrink × mainSize 权重压缩（min-width:0 语义）
    const shrinkables = items.filter((it) => it.flex.shrink > 0 && toPx(it.child.style.width, innerW) == null);
    const totalWeight = shrinkables.reduce((a, it) => a + it.flex.shrink * it.mainSize, 0);
    if (totalWeight > 0) {
      for (const it of shrinkables) {
        it.mainSize = Math.max(0, it.mainSize + (free * it.flex.shrink * it.mainSize) / totalWeight);
      }
    }
  }

  // justify（无 grow 吸收时）
  let leading = 0;
  let between = gap;
  const usedMain = items.reduce((a, it) => a + it.mainSize + it.margin.l + it.margin.r, 0) + gap * Math.max(0, items.length - 1);
  const rest = innerW - usedMain;
  const absorbed = free > 0 && totalGrow > 0;
  if (!absorbed && rest > 0) {
    const jc = s.justifyContent;
    if (jc === "center") leading = rest / 2;
    else if (jc === "flex-end") leading = rest;
    else if (jc === "space-between" && items.length > 1) between = gap + rest / (items.length - 1);
    else if (jc === "space-around" && items.length > 0) {
      between = gap + rest / items.length;
      leading = rest / items.length / 2;
    }
  }

  const wrap = s.flexWrap === "wrap";
  const lines: (typeof items)[] = [];
  if (wrap) {
    let cur: typeof items = [];
    let curMain = 0;
    for (const it of items) {
      const need = it.mainSize + it.margin.l + it.margin.r + (cur.length ? between : 0);
      if (cur.length && curMain + need > innerW) {
        lines.push(cur);
        cur = [];
        curMain = 0;
      }
      if (cur.length) curMain += between;
      cur.push(it);
      curMain += it.mainSize + it.margin.l + it.margin.r;
    }
    if (cur.length) lines.push(cur);
  } else {
    lines.push(items);
  }

  let y = innerY;
  let contentW = 0;
  for (const lineItems of lines) {
    const lineCross = Math.max(0, ...lineItems.map((it) => it.crossSize + it.margin.t + it.margin.b));
    let x = innerX + leading;
    for (const it of lineItems) {
      const ai = it.child.style.alignSelf && it.child.style.alignSelf !== "auto" ? it.child.style.alignSelf : s.alignItems;
      let cyMargin = y + it.margin.t;
      if (ai === "center") cyMargin = y + it.margin.t + (lineCross - it.margin.t - it.margin.b - it.crossSize) / 2;
      else if (ai === "flex-end") cyMargin = y + lineCross - it.margin.b - it.crossSize;
      else if (ai === "stretch" && isAuto(it.child.style.height)) {
        it.child._height = lineCross - it.margin.t - it.margin.b;
      }
      layout(it.child, x + it.margin.l - it.margin.l, cyMargin - it.margin.t + 0, it.mainSize + it.margin.l + it.margin.r, availH, innerW);
      // layout() 以 availW-margins 为宽：上面传入含 margin 的可用宽，使 border 宽 = mainSize
      x += it.mainSize + it.margin.l + it.margin.r + between;
    }
    contentW = Math.max(contentW, x - innerX - (lineItems.length ? between : 0));
    y += lineCross + gap;
  }
  if (lines.length) y -= gap;
  return { contentW, contentH: y - innerY };
}
