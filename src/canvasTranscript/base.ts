/**
 * canvas transcript 基础层：字体/测量/断行、主题取色、矢量图标、通用绘制原语。
 * 颜色全部从 CSS 变量解析（含 color-mix），保证与 DOM 版主题（深/浅色）完全一致。
 */

export const SANS =
  '"Inter Variable","Noto Sans SC Variable","Segoe UI","Microsoft YaHei UI","Microsoft YaHei","PingFang SC",system-ui,sans-serif';
export const MONO = '"JetBrains Mono Variable","Cascadia Mono",Consolas,monospace';

import { paintStarMap } from "./starMap";

export interface FontOpts {
  bold?: boolean;
  italic?: boolean;
  mono?: boolean;
}

export function font(size: number, opts: FontOpts = {}): string {
  return `${opts.italic ? "italic " : ""}${opts.bold ? "600 " : ""}${size}px ${opts.mono ? MONO : SANS}`;
}

/** CanvasRenderingContext2D does not lay out tab characters like a <pre> does. */
export function expandTabs(text: string, tabSize = 4): string {
  const normalized = text.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  let column = 0;
  let result = "";
  for (const ch of normalized) {
    if (ch === "\n") {
      result += ch;
      column = 0;
    } else if (ch === "\t") {
      const spaces = tabSize - (column % tabSize);
      result += " ".repeat(spaces);
      column += spaces;
    } else {
      result += ch;
      column++;
    }
  }
  return result;
}

/* ===== 文本测量（带缓存） ===== */

let measureCtx: CanvasRenderingContext2D | null = null;
const measureCache = new Map<string, number>();

function ctx2d(): CanvasRenderingContext2D {
  if (!measureCtx) measureCtx = document.createElement("canvas").getContext("2d")!;
  return measureCtx;
}

export function measure(text: string, f: string): number {
  if (!text) return 0;
  const key = `${f}\u0001${text}`;
  const hit = measureCache.get(key);
  if (hit !== undefined) return hit;
  const c = ctx2d();
  c.font = f;
  const w = c.measureText(text).width;
  if (measureCache.size > 30000) measureCache.clear();
  measureCache.set(key, w);
  return w;
}

export interface FontMetrics {
  ascent: number;
  descent: number;
}

const metricsCache = new Map<string, FontMetrics>();

export function metrics(f: string): FontMetrics {
  const hit = metricsCache.get(f);
  if (hit) return hit;
  const c = ctx2d();
  c.font = f;
  const m = c.measureText("国Ag");
  const ascent = (m as TextMetrics & { fontBoundingBoxAscent?: number }).fontBoundingBoxAscent || m.actualBoundingBoxAscent;
  const descent = (m as TextMetrics & { fontBoundingBoxDescent?: number }).fontBoundingBoxDescent || m.actualBoundingBoxDescent;
  const value = { ascent, descent };
  metricsCache.set(f, value);
  return value;
}

/** 单行基线位置：让文字在行高内垂直居中 */
export function baselineOf(lineH: number, f: string): number {
  const m = metrics(f);
  return Math.round((lineH + m.ascent - m.descent) / 2);
}

/** 超宽截断为省略号 */
export function ellipsis(text: string, f: string, maxW: number): string {
  if (measure(text, f) <= maxW) return text;
  let low = 0;
  let high = text.length;
  while (low < high) {
    const mid = (low + high + 1) >> 1;
    if (measure(text.slice(0, mid) + "…", f) <= maxW) low = mid;
    else high = mid - 1;
  }
  return text.slice(0, low) + "…";
}

/* ===== 主题取色 ===== */

export interface ThemeColors {
  bg: string;
  bgSidebar: string;
  bgPanel: string;
  bgHover: string;
  bgActive: string;
  bgFloat: string;
  bgInput: string;
  border: string;
  borderLight: string;
  borderStrong: string;
  text: string;
  textDim: string;
  textMuted: string;
  textFaint: string;
  accent: string;
  accentDim: string;
  onAccent: string;
  blue: string;
  red: string;
  yellow: string;
  green: string;
  violet: string;
  scroll: string;
  scrollHover: string;
  wash1: string;
  wash2: string;
  gridDot: string;
  glowAccent: string;
  glowCyan: string;
  glowCorner: string;
  accent7: string;
  accent8: string;
  accent12: string;
  accent13: string;
  accent20: string;
  accent26: string;
  accent30: string;
  accent34: string;
  accent40: string;
  accent58: string;
  accent60Text: string;
  accent80Text: string;
  red8: string;
  red13: string;
  red16: string;
  red30: string;
  red32: string;
  red58Text: string;
  yellow6: string;
  yellow7: string;
  yellow28: string;
  yellow36: string;
  text8: string;
  selection: string;
}

const VAR_KEYS: Array<[keyof ThemeColors, string]> = [
  ["bg", "--bg"],
  ["bgSidebar", "--bg-sidebar"],
  ["bgPanel", "--bg-panel"],
  ["bgHover", "--bg-hover"],
  ["bgActive", "--bg-active"],
  ["bgFloat", "--bg-float"],
  ["bgInput", "--bg-input"],
  ["border", "--border"],
  ["borderLight", "--border-light"],
  ["borderStrong", "--border-strong"],
  ["text", "--canvas-text"],
  ["textDim", "--canvas-text-dim"],
  ["textMuted", "--canvas-text-muted"],
  ["textFaint", "--canvas-text-faint"],
  ["accent", "--accent"],
  ["accentDim", "--accent-dim"],
  ["onAccent", "--on-accent"],
  ["blue", "--blue"],
  ["red", "--red"],
  ["yellow", "--yellow"],
  ["green", "--green"],
  ["violet", "--violet"],
  ["scroll", "--scroll"],
  ["scrollHover", "--scroll-hover"],
  ["wash1", "--wash-1"],
  ["wash2", "--wash-2"],
  ["gridDot", "--grid-dot"],
  ["glowAccent", "--canvas-glow-accent"],
  ["glowCyan", "--canvas-glow-cyan"],
  ["glowCorner", "--canvas-glow-corner"],
];

const MIX_KEYS: Array<[keyof ThemeColors, string]> = [
  ["accent7", "color-mix(in srgb, var(--accent) 7%, transparent)"],
  ["accent8", "color-mix(in srgb, var(--accent) 8%, transparent)"],
  ["accent12", "color-mix(in srgb, var(--accent) 12%, var(--bg-panel))"],
  ["accent13", "color-mix(in srgb, var(--accent) 13%, transparent)"],
  ["accent20", "color-mix(in srgb, var(--accent) 20%, transparent)"],
  ["accent26", "color-mix(in srgb, var(--accent) 26%, transparent)"],
  ["accent30", "color-mix(in srgb, var(--accent) 30%, transparent)"],
  ["accent34", "color-mix(in srgb, var(--accent) 34%, transparent)"],
  ["accent40", "color-mix(in srgb, var(--accent) 40%, transparent)"],
  ["accent58", "color-mix(in srgb, var(--accent) 58%, transparent)"],
  ["accent60Text", "color-mix(in srgb, var(--accent) 60%, var(--text))"],
  ["accent80Text", "color-mix(in srgb, var(--accent) 80%, var(--text))"],
  ["red8", "color-mix(in srgb, var(--red) 8%, transparent)"],
  ["red13", "color-mix(in srgb, var(--red) 13%, transparent)"],
  ["red16", "color-mix(in srgb, var(--red) 16%, transparent)"],
  ["red30", "color-mix(in srgb, var(--red) 30%, transparent)"],
  ["red32", "color-mix(in srgb, var(--red) 32%, transparent)"],
  ["red58Text", "color-mix(in srgb, var(--red) 58%, var(--text))"],
  ["yellow6", "color-mix(in srgb, var(--yellow) 6%, transparent)"],
  ["yellow7", "color-mix(in srgb, var(--yellow) 7%, transparent)"],
  ["yellow28", "color-mix(in srgb, var(--yellow) 28%, transparent)"],
  ["yellow36", "color-mix(in srgb, var(--yellow) 36%, transparent)"],
  ["text8", "color-mix(in srgb, var(--text) 8%, transparent)"],
  ["selection", "color-mix(in srgb, var(--accent) 32%, transparent)"],
];

let probeEl: HTMLSpanElement | null = null;

function resolveExpr(expr: string): string {
  if (!probeEl) {
    probeEl = document.createElement("span");
    probeEl.style.display = "none";
    document.body.appendChild(probeEl);
  }
  probeEl.style.color = expr;
  const value = getComputedStyle(probeEl).color;
  probeEl.style.color = "";
  return value;
}

let themeCache: { sig: string; colors: ThemeColors } | null = null;

export function themeSig(): string {
  return document.documentElement.getAttribute("data-theme") ?? "";
}

export function getTheme(): ThemeColors {
  const sig = themeSig();
  if (themeCache && themeCache.sig === sig) return themeCache.colors;
  const cs = getComputedStyle(document.documentElement);
  const colors = {} as ThemeColors;
  for (const [key, varName] of VAR_KEYS) {
    colors[key] = cs.getPropertyValue(varName).trim() || "#000";
  }
  for (const [key, expr] of MIX_KEYS) {
    colors[key] = resolveExpr(expr);
  }
  themeCache = { sig, colors };
  return colors;
}

/** 主题切换时强制失效缓存 */
export function invalidateTheme(): void {
  themeCache = null;
}

/**
 * 背景缓存在独立的离屏 Canvas：主题或尺寸变化时才重绘柔光与点阵；正文每帧只做一次位图拷贝。
 * 比两个可见 Canvas 更稳：前景仍可使用 alpha:false，避免透明合成让暗色小字重新发虚。
 */
const backdropCache = new Map<string, HTMLCanvasElement>();

export function paintCanvasBackdrop(
  ctx: CanvasRenderingContext2D,
  width: number,
  height: number,
  theme: Pick<ThemeColors, "bg" | "glowAccent" | "glowCyan" | "glowCorner" | "gridDot">,
  now = Date.now(),
  region?: { x: number; y: number; w: number; h: number },
): void {
  const dpr = window.devicePixelRatio || 1;
  const pixelW = Math.max(1, Math.round(width * dpr));
  const pixelH = Math.max(1, Math.round(height * dpr));
  // 星图随恒星时旋转：缓存按分钟分桶，每分钟重建一次背景位图，旧桶自然被淘汰
  const skyBucket = Math.floor(now / 60000);
  const key = [pixelW, pixelH, skyBucket, theme.bg, theme.glowAccent, theme.glowCyan, theme.glowCorner, theme.gridDot].join("|");
  let surface = backdropCache.get(key);

  if (!surface) {
    surface = document.createElement("canvas");
    surface.width = pixelW;
    surface.height = pixelH;
    const bg = surface.getContext("2d", { alpha: false })!;
    bg.setTransform(dpr, 0, 0, dpr, 0, 0);
    bg.fillStyle = theme.bg;
    bg.fillRect(0, 0, width, height);

    const radialEllipse = (
      x: number,
      y: number,
      radiusX: number,
      radiusY: number,
      color: string,
      fadeAt: number,
    ) => {
      bg.save();
      bg.translate(x, y);
      bg.scale(radiusX, radiusY);
      const gradient = bg.createRadialGradient(0, 0, 0, 0, 0, 1);
      gradient.addColorStop(0, color);
      gradient.addColorStop(fadeAt, "transparent");
      gradient.addColorStop(1, "transparent");
      bg.fillStyle = gradient;
      bg.fillRect(-2, -2, 4, 4);
      bg.restore();
    };
    // Canvas 是不透明表面，不能直接裁切位于其下方的 body::before；按相同比例在会话区复现。
    radialEllipse(width * 0.28, height * -0.14, 1000, 520, theme.glowAccent, 0.7);
    radialEllipse(width * 0.86, height * -0.1, 780, 460, theme.glowCyan, 0.7);
    radialEllipse(width * 1.04, height * 1.12, 820, 560, theme.glowCorner, 0.72);

    bg.fillStyle = theme.gridDot;
    const dot = 1 / dpr;
    for (let y = 0; y < height; y += 26) {
      for (let x = 0; x < width; x += 26) bg.fillRect(x, y, dot, dot);
    }

    // 星图层：低透明度星座，画在文字之下的背景缓存里
    paintStarMap(bg, width, height, theme, skyBucket * 60000);

    if (backdropCache.size >= 4) backdropCache.delete(backdropCache.keys().next().value!);
    backdropCache.set(key, surface);
  }

  if (region) {
    // spinner 等局部动画只恢复自身覆盖的背景区域，避免每帧重拷整张背景。
    const x0 = Math.max(0, region.x);
    const y0 = Math.max(0, region.y);
    const x1 = Math.min(width, region.x + region.w);
    const y1 = Math.min(height, region.y + region.h);
    if (x1 > x0 && y1 > y0) {
      ctx.drawImage(
        surface,
        x0 * dpr,
        y0 * dpr,
        (x1 - x0) * dpr,
        (y1 - y0) * dpr,
        x0,
        y0,
        x1 - x0,
        y1 - y0,
      );
    }
  } else {
    ctx.drawImage(surface, 0, 0, pixelW, pixelH, 0, 0, width, height);
  }
}

/* ===== 矢量图标（与 icons.tsx 的 feather 风格 path 一致，Path2D 直绘） ===== */

const ICON_PATHS: Record<string, string[]> = {
  file: ["M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z", "M14 2v4a2 2 0 0 0 2 2h4"],
  pencil: ["M21.17 6.83a2.83 2.83 0 0 0-4-4L3 17v4h4Z", "m15 5 4 4"],
  trash: ["M3 6h18", "M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6", "M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"],
  move: ["M5 9l-3 3 3 3M9 5l3-3 3 3M15 19l-3 3-3-3M19 9l3 3-3 3M2 12h20M12 2v20"],
  search: ["M19 11a8 8 0 1 1-16 0 8 8 0 0 1 16 0Z", "m21 21-4.3-4.3"],
  terminal: ["m4 17 6-6-6-6", "M12 19h8"],
  brain: [
    "M12 5a3 3 0 1 0-5.997.125 4 4 0 0 0-2.526 5.77 4 4 0 0 0 .556 6.588A4 4 0 1 0 12 18Z",
    "M12 5a3 3 0 1 1 5.997.125 4 4 0 0 1 2.526 5.77 4 4 0 0 1-.556 6.588A4 4 0 1 1 12 18Z",
  ],
  globe: ["M22 12a10 10 0 1 1-20 0 10 10 0 0 1 20 0Z", "M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20", "M2 12h20"],
  wrench: [
    "M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z",
  ],
  check: ["M20 6 9 17l-5-5"],
  copy: ["M11 9h9a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2h-9a2 2 0 0 1-2-2v-9a2 2 0 0 1 2-2Z", "M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"],
  undo: ["M3 7v6h6", "M21 17a9 9 0 0 0-9-9 9 9 0 0 0-6.7 3L3 13"],
  chevronDown: ["m6 9 6 6 6-6"],
  chevronRight: ["m9 6 6 6-6 6"],
};

const pathCache = new Map<string, Path2D>();

function getPath(d: string): Path2D {
  let p = pathCache.get(d);
  if (!p) {
    p = new Path2D(d);
    pathCache.set(d, p);
  }
  return p;
}

/** kind → 图标名，与 icons.tsx 的 toolIcon 一致 */
export function toolIconName(kind: string): string {
  switch (kind) {
    case "read":
      return "file";
    case "edit":
      return "pencil";
    case "delete":
      return "trash";
    case "move":
      return "move";
    case "search":
      return "search";
    case "execute":
      return "terminal";
    case "think":
      return "brain";
    case "fetch":
      return "globe";
    default:
      return "wrench";
  }
}

export function drawIcon(
  ctx: CanvasRenderingContext2D,
  name: string,
  x: number,
  y: number,
  size: number,
  color: string,
): void {
  const paths = ICON_PATHS[name];
  if (!paths) return;
  ctx.save();
  ctx.translate(x, y);
  const s = size / 24;
  ctx.scale(s, s);
  ctx.strokeStyle = color;
  ctx.lineWidth = 2;
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  for (const d of paths) ctx.stroke(getPath(d));
  ctx.restore();
}

/** CSS .spinner 等价：0.8s 一圈的旋转弧 */
export function drawSpinner(
  ctx: CanvasRenderingContext2D,
  cx: number,
  cy: number,
  size: number,
  now: number,
  color: string,
  trackColor: string,
): void {
  const border = size >= 12 ? 2 : 1.5;
  const r = (size - border) / 2;
  const angle = ((now % 800) / 800) * Math.PI * 2 - Math.PI / 2;
  ctx.save();
  ctx.lineWidth = border;
  ctx.strokeStyle = trackColor;
  ctx.beginPath();
  ctx.arc(cx, cy, r, 0, Math.PI * 2);
  ctx.stroke();
  ctx.strokeStyle = color;
  ctx.beginPath();
  ctx.arc(cx, cy, r, angle, angle + Math.PI / 2);
  ctx.stroke();
  ctx.restore();
}

export function roundRectPath(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  r: number | [number, number, number, number],
): void {
  const [tl, tr, br, bl] = typeof r === "number" ? [r, r, r, r] : r;
  ctx.beginPath();
  ctx.moveTo(x + tl, y);
  ctx.lineTo(x + w - tr, y);
  ctx.arcTo(x + w, y, x + w, y + tr, tr);
  ctx.lineTo(x + w, y + h - br);
  ctx.arcTo(x + w, y + h, x + w - br, y + h, br);
  ctx.lineTo(x + bl, y + h);
  ctx.arcTo(x, y + h, x, y + h - bl, bl);
  ctx.lineTo(x, y + tl);
  ctx.arcTo(x, y, x + tl, y, tl);
  ctx.closePath();
}

/* ===== 断行引擎 ===== */

export const F_UNDERLINE = 1;
export const F_STRIKE = 2;

export interface Action {
  kind: string;
  [k: string]: unknown;
}

export type ChipSpec = { kind: "code" } | { kind: "file"; path: string; line?: number };

export interface Seg {
  t: string;
  f: string;
  color: string;
  flags?: number;
  chip?: ChipSpec;
  action?: Action;
}

export interface LRun {
  seg: Seg;
  s: number;
  e: number;
  x: number;
  w: number;
}

export interface LLine {
  runs: LRun[];
  w: number;
  h: number;
  asc: number;
}

const CJK_RE = /[\u2E80-\u9FFF\uAC00-\uD7AF\uF900-\uFAFF\u3040-\u30FF\u31F0-\u31FF\uFF00-\uFFEF]/;

interface Atom {
  seg: Seg;
  s: number;
  e: number;
  w: number;
  space: boolean;
  br: boolean;
}

export function chipWidth(seg: Seg): number {
  if (seg.chip?.kind === "file") {
    // padding 1 5 + icon 13 + gap 4 + text(mono 12.5)
    return 5 + 13 + 4 + measure(seg.t, seg.f) + 5;
  }
  // inline code：padding 1 6 + border 1 x2
  return 7 + measure(seg.t, seg.f) + 7;
}

function buildAtoms(segs: Seg[], maxW: number, mode: "word" | "all", preWrap: boolean): Atom[] {
  const atoms: Atom[] = [];
  const pushChars = (seg: Seg, s: number, e: number) => {
    for (let i = s; i < e; i++) {
      const ch = seg.t.slice(i, i + 1);
      atoms.push({ seg, s: i, e: i + 1, w: measure(ch, seg.f), space: false, br: false });
    }
  };
  for (const seg of segs) {
    if (seg.chip) {
      atoms.push({ seg, s: 0, e: seg.t.length, w: chipWidth(seg), space: false, br: false });
      continue;
    }
    const t = seg.t;
    let i = 0;
    while (i < t.length) {
      const ch = t[i];
      if (ch === "\n") {
        atoms.push({ seg, s: i, e: i + 1, w: 0, space: false, br: true });
        i++;
        continue;
      }
      if (ch === " " || ch === "\t") {
        let j = i + 1;
        while (j < t.length && (t[j] === " " || t[j] === "\t")) j++;
        atoms.push({ seg, s: i, e: j, w: measure(t.slice(i, j), seg.f), space: true, br: false });
        i = j;
        continue;
      }
      if (mode === "all") {
        pushChars(seg, i, i + 1);
        i++;
        continue;
      }
      if (CJK_RE.test(ch)) {
        atoms.push({ seg, s: i, e: i + 1, w: measure(ch, seg.f), space: false, br: false });
        i++;
        continue;
      }
      let j = i + 1;
      while (j < t.length && t[j] !== " " && t[j] !== "\t" && !(preWrap && t[j] === "\n") && !CJK_RE.test(t[j])) j++;
      const w = measure(t.slice(i, j), seg.f);
      if (w > maxW) pushChars(seg, i, j);
      else atoms.push({ seg, s: i, e: j, w, space: false, br: false });
      i = j;
    }
  }
  return atoms;
}

export interface WrapOpts {
  /** word = break-word（优先词边界），all = break-all（任意字符） */
  mode?: "word" | "all";
  /** 保留换行符（pre-wrap） */
  preWrap?: boolean;
}

/**
 * mode:"all" + preWrap 的快速路径：不创建 per-char atom，直接按行+宽度贪心切分。
 * 对于 5000 字符的工具输出，避免创建 5000 个临时对象。
 */
function wrapAllFast(seg: Seg, maxW: number, lineH: number, mainFont: string): LLine[] {
  const asc = baselineOf(lineH, mainFont);
  const f = seg.f;
  const color = seg.color;
  const flags = seg.flags ?? 0;
  const text = seg.t;
  const lines: LLine[] = [];

  const pushLine = (s: number, e: number, w: number) => {
    if (s === e) {
      lines.push({ runs: [], w: 0, h: lineH, asc });
    } else {
      lines.push({ runs: [{ seg, s, e, x: 0, w }], w, h: lineH, asc });
    }
  };

  let i = 0;
  while (i < text.length) {
    // 找下一个换行符
    const nl = text.indexOf("\n", i);
    const lineEnd = nl === -1 ? text.length : nl;
    const lineText = text.slice(i, lineEnd);

    if (lineText.length === 0) {
      pushLine(i, i, 0);
      i = lineEnd + 1;
      continue;
    }

    const lineW = measure(lineText, f);
    if (lineW <= maxW) {
      pushLine(i, lineEnd, lineW);
    } else {
      // 超宽行：贪心按字符宽度切分（等宽字体下可用二分加速）
      let cs = i;
      while (cs < lineEnd) {
        // 二分找最大前缀宽度 <= maxW
        let lo = 1;
        let hi = lineEnd - cs;
        while (lo < hi) {
          const mid = (lo + hi + 1) >> 1;
          if (measure(text.slice(cs, cs + mid), f) <= maxW) lo = mid;
          else hi = mid - 1;
        }
        const ce = cs + lo;
        const w = measure(text.slice(cs, ce), f);
        pushLine(cs, ce, w);
        cs = ce;
      }
    }
    i = lineEnd + 1;
    if (nl === -1) break;
  }
  if (lines.length === 0) lines.push({ runs: [], w: 0, h: lineH, asc });
  return lines;
}

export function wrapSegs(segs: Seg[], maxW: number, lineH: number, mainFont: string, opts: WrapOpts = {}): LLine[] {
  // 快速路径：mode:"all" + preWrap 且只有单 seg 无 chip（代码块/工具输出/diff 行）
  // 不创建 per-char atom，直接按行+宽度贪心切分，避免 GC 风暴
  if ((opts.mode ?? "word") === "all" && opts.preWrap && segs.length === 1 && !segs[0].chip) {
    return wrapAllFast(segs[0], maxW, lineH, mainFont);
  }
  const atoms = buildAtoms(segs, maxW, opts.mode ?? "word", !!opts.preWrap);
  const asc = baselineOf(lineH, mainFont);
  const lines: LLine[] = [];
  let runs: LRun[] = [];
  let curW = 0;

  const flush = () => {
    // 去掉行尾空白（等价 CSS 折叠）
    while (runs.length > 0) {
      const last = runs[runs.length - 1];
      if (!last.seg.chip && /^\s+$/.test(last.seg.t.slice(last.s, last.e))) {
        curW -= last.w;
        runs.pop();
      } else break;
    }
    if (runs.length > 0) lines.push({ runs, w: curW, h: lineH, asc });
    runs = [];
    curW = 0;
  };

  for (const atom of atoms) {
    if (atom.br) {
      flush();
      continue;
    }
    if (atom.space) {
      if (runs.length === 0) continue; // 行首空白折叠
      if (curW + atom.w > maxW) {
        flush();
        continue;
      }
    } else if (curW + atom.w > maxW && runs.length > 0) {
      flush();
    }
    const last = runs[runs.length - 1];
    if (last && last.seg === atom.seg && last.e === atom.s && !atom.space && !last.seg.chip) {
      last.e = atom.e;
      last.w += atom.w;
    } else {
      runs.push({ seg: atom.seg, s: atom.s, e: atom.e, x: curW, w: atom.w });
    }
    curW += atom.w;
  }
  flush();
  if (lines.length === 0) lines.push({ runs: [], w: 0, h: lineH, asc });
  return lines;
}

/** 把 LLine 序列平铺成绝对坐标行（y 递增），返回总高度 */
export function stackLines(lines: LLine[], x: number, y: number): { placed: PlacedLine[]; height: number } {
  const placed: PlacedLine[] = [];
  let cy = y;
  for (const line of lines) {
    placed.push({ line, x, y: cy });
    cy += line.h;
  }
  return { placed, height: cy - y };
}

export interface PlacedLine {
  line: LLine;
  x: number;
  y: number;
}
