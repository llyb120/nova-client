// painter.ts — 把节点树绘制到 canvas 2d（移植自参考项目 painter.js，按会话需求扩展）：
// hoverStyle/activeStyle 合并、per-corner 圆角、per-side 边框、inline 片段背景、
// Path2D 图标、spinner 动画、图片圆角裁剪、webkit 风格滚动条、sticky 偏移、文字选区。

import { fontMetrics, measureText } from "./layout.js";
import { edgesOf, radiiOf, Node, type Style, type TextLine } from "./node.js";
import { mix, fade, theme } from "./theme.js";

export interface IconShapePath {
  d: string;
}
export interface IconShapeCircle {
  cx: number;
  cy: number;
  r: number;
}
export interface IconShapeRect {
  x: number;
  y: number;
  w: number;
  h: number;
  rx?: number;
}
export type IconShape =
  | ({ kind: "path" } & IconShapePath)
  | ({ kind: "circle" } & IconShapeCircle)
  | ({ kind: "rect" } & IconShapeRect);

/** 节点的有效样式（hover/active 补丁合并后的视图；:hover 语义含后代悬停） */
export function effStyle(node: Node): Style {
  let s = node.style;
  if (node._active && node.activeStyle) s = Object.assign({}, s, node.activeStyle);
  else if ((node._hover || node._hoverWithin) && node.hoverStyle) {
    s = Object.assign({}, s, node.hoverStyle);
  }
  return s;
}

export function roundRectPath(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  radii: [number, number, number, number],
): void {
  const [tl, tr, br, bl] = radii.map((r) => Math.max(0, Math.min(r, w / 2, h / 2))) as [
    number, number, number, number,
  ];
  ctx.beginPath();
  ctx.moveTo(x + tl, y);
  ctx.lineTo(x + w - tr, y);
  if (tr) ctx.arcTo(x + w, y, x + w, y + tr, tr);
  ctx.lineTo(x + w, y + h - br);
  if (br) ctx.arcTo(x + w, y + h, x + w - br, y + h, br);
  ctx.lineTo(x + bl, y + h);
  if (bl) ctx.arcTo(x, y + h, x, y + h - bl, bl);
  ctx.lineTo(x, y + tl);
  if (tl) ctx.arcTo(x, y, x + tl, y, tl);
  ctx.closePath();
}

/** 每边独立宽度/颜色的边框（stroke 位置居中于边线，近似 CSS border） */
function strokeBorder(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  widths: { t: number; r: number; b: number; l: number },
  colors: string[],
  radii: [number, number, number, number],
): void {
  // 简化：圆角边框仅支持统一宽度/颜色；否则按直角逐边画
  const uniform = widths.t === widths.r && widths.t === widths.b && widths.t === widths.l && colors.every((c) => c === colors[0]);
  if (uniform && widths.t > 0) {
    ctx.strokeStyle = colors[0];
    ctx.lineWidth = widths.t;
    const inset = widths.t / 2;
    roundRectPath(ctx, x + inset, y + inset, w - widths.t, h - widths.t, [
      Math.max(0, radii[0] - inset),
      Math.max(0, radii[1] - inset),
      Math.max(0, radii[2] - inset),
      Math.max(0, radii[3] - inset),
    ]);
    ctx.stroke();
    return;
  }
  const sides: Array<[number, number, number, number, number]> = [
    [widths.t, x, y, x + w, y],
    [widths.r, x + w, y, x + w, y + h],
    [widths.b, x, y + h, x + w, y + h],
    [widths.l, x, y, x, y + h],
  ];
  sides.forEach(([wd, x0, y0, x1, y1], i) => {
    if (wd <= 0) return;
    ctx.strokeStyle = colors[i];
    ctx.lineWidth = wd;
    ctx.beginPath();
    ctx.moveTo(x0, y0);
    ctx.lineTo(x1, y1);
    ctx.stroke();
  });
}

function borderColors(s: Style): string[] {
  const c = s.borderColor;
  if (typeof c === "string") return [c, c, c, c];
  if (Array.isArray(c)) {
    if (c.length === 1) return [c[0], c[0], c[0], c[0]];
    if (c.length === 2) return [c[0], c[1], c[0], c[1]];
    if (c.length === 4) return [c[0], c[1], c[2], c[3]];
  }
  return ["#000", "#000", "#000", "#000"];
}

export interface PaintEnv {
  /** 当前时间戳（spinner 等动画） */
  now: number;
}

export function paint(
  ctx: CanvasRenderingContext2D,
  node: Node,
  clipX: number,
  clipY: number,
  clipW: number,
  clipH: number,
  env: PaintEnv,
): void {
  const s0 = node.style;
  if (s0.display === "none") return;
  const s = effStyle(node);
  let opacity = s.opacity;
  if (node.revealOnHover) {
    // .X:hover .Y 语义：Y 只在最近一个 hoverContainer 祖先被 hover 时显示
    let revealed = false;
    let p: Node | null = node.parent;
    while (p) {
      if (p.hoverContainer) {
        revealed = p._hoverWithin || p._hover;
        break;
      }
      p = p.parent;
    }
    if (!revealed) opacity = 0;
  }
  if (opacity <= 0) return;

  const x = node._x;
  const y = node._y + (node._stickyDy || 0);
  const w = node._width;
  const h = node._height;
  if (x + w < clipX || x > clipX + clipW || y + h < clipY || y > clipY + clipH) return;

  ctx.save();
  if (opacity < 1) ctx.globalAlpha *= opacity;

  const radii = radiiOf(s.borderRadius);
  const isInline = s.display === "inline";

  // 背景
  if (!isInline && s.background && s.background !== "transparent") {
    ctx.fillStyle = s.background;
    roundRectPath(ctx, x, y, w, h, radii);
    ctx.fill();
  }

  // 边框
  const be = edgesOf(s.border);
  if (!isInline && (be.t > 0 || be.r > 0 || be.b > 0 || be.l > 0)) {
    strokeBorder(ctx, x, y, w, h, be, borderColors(s), radii);
  }

  // 裁剪
  const overflow = s.overflow;
  const clip = overflow === "hidden" || overflow === "scroll" || overflow === "auto";
  if (clip) {
    ctx.beginPath();
    roundRectPath(ctx, x, y, w, h, radii);
    ctx.clip();
  }

  // 滚动偏移
  const sx = node._scrollX || 0;
  const sy = node._scrollY || 0;
  ctx.translate(-sx, -sy);

  const p = edgesOf(s.padding);

  if (node.tag === "img") {
    if (node._img && node._imgLoaded) {
      ctx.save();
      roundRectPath(ctx, x, y, w, h, radii);
      ctx.clip();
      try {
        ctx.drawImage(node._img, x, y, w, h);
      } catch {
        /* 图片未就绪 */
      }
      ctx.restore();
    }
  } else if (node.tag === "icon") {
    paintIcon(ctx, node, s, x, y);
  } else if (node.tag === "spinner") {
    paintSpinner(ctx, node, s, x, y, env.now);
  } else {
    if (node._textLines && node._textLines.length > 0) {
      paintTextLines(ctx, node);
    }
    for (const child of node.children) {
      paint(ctx, child, clipX + sx, clipY + sy, clipW, clipH, env);
    }
  }

  ctx.restore();

  // 滚动条（画在 clip 外）
  if (clip && (node._maxScrollY > 0 || node._maxScrollX > 0)) {
    paintScrollbars(ctx, node, x, y, w, h);
  }
}

// ---------- 文本 ----------

function paintTextLines(ctx: CanvasRenderingContext2D, node: Node): void {
  ctx.textBaseline = "alphabetic";
  const lines = node._textLines!;
  const owner = lines[0]?.owner ?? node;
  const os = effStyle(owner);
  // 第一遍：inline 片段背景（行内 code 等）。
  // 遵从 box-decoration-break: slice——同一元素的相邻片段（含跨行断点）合并为
  // 一条连续背景：padding/圆角/侧边只出现在元素首尾，内部不断开。
  // 必须先把整段背景画完再画文字，否则后续片段的背景会盖住前面片段的文本。
  if (os.display === "inline" && os.background && os.background !== "transparent") {
    const pad = edgesOf(os.padding);
    const be = edgesOf(os.border);
    const radii = radiiOf(os.borderRadius);
    const bColors = borderColors(os);
    // 按行分组（同一视觉行内片段 x 连续）
    const runs: Array<{ a: number; b: number }> = [];
    for (let i = 0; i < lines.length; i++) {
      if (runs.length && lines[i].y === lines[runs[runs.length - 1].b].y) {
        runs[runs.length - 1].b = i;
      } else {
        runs.push({ a: i, b: i });
      }
    }
    for (const run of runs) {
      const first = lines[run.a];
      const last = lines[run.b];
      const atStart = run.a === 0;
      const atEnd = run.b === lines.length - 1;
      const padL = atStart ? pad.l : 0;
      const padR = atEnd ? pad.r : 0;
      const bx = first.x - padL;
      const by = first.y + first.baseline - first.ascent - pad.t;
      const bw = last.x + last.w - first.x + padL + padR;
      const bh = first.ascent + first.descent + pad.t + pad.b;
      // 断裂处（非元素首/尾的一侧）圆角归零
      const rr: [number, number, number, number] = [
        atStart ? radii[0] : 0,
        atEnd ? radii[1] : 0,
        atEnd ? radii[2] : 0,
        atStart ? radii[3] : 0,
      ];
      ctx.fillStyle = os.background;
      roundRectPath(ctx, bx, by, bw, bh, rr);
      ctx.fill();
      if (be.t > 0 || be.r > 0 || be.b > 0 || be.l > 0) {
        if (atStart && atEnd) {
          strokeBorder(ctx, bx, by, bw, bh, be, bColors, rr);
        } else {
          // 断裂侧不画边线：用 evenodd clip 挖掉断裂侧的窄条后再描边
          ctx.save();
          ctx.beginPath();
          ctx.rect(bx - 2, by - 2, bw + 4, bh + 4);
          if (!atStart) ctx.rect(bx - 2, by - 2, 2 + be.l, bh + 4);
          if (!atEnd) ctx.rect(bx + bw - be.r, by - 2, 2 + be.r, bh + 4);
          ctx.clip("evenodd");
          strokeBorder(ctx, bx, by, bw, bh, be, bColors, rr);
          ctx.restore();
        }
      }
    }
  }
  // 第二遍：文字与装饰线
  for (const ln of lines) {
    if (!ln.text) continue;
    ctx.font = ln.font;
    const c2 = ctx as CanvasRenderingContext2D & { letterSpacing?: string };
    c2.letterSpacing = os.letterSpacing ? `${os.letterSpacing}px` : "0px";
    ctx.fillStyle = os.color;
    ctx.fillText(ln.text, ln.x, ln.y + ln.baseline);
    c2.letterSpacing = "0px";
    if (os.textDecoration === "underline") {
      const uy = ln.y + ln.baseline + Math.max(1, ln.fs * 0.09);
      ctx.fillRect(ln.x, uy, ln.w, Math.max(1, ln.fs / 13));
    } else if (os.textDecoration === "line-through") {
      const uy = ln.y + ln.baseline - ln.fs * 0.28;
      ctx.fillRect(ln.x, uy, ln.w, Math.max(1, ln.fs / 13));
    }
  }
}

// ---------- 图标 / spinner ----------

const path2dCache = new Map<string, Path2D>();

function getPath(d: string): Path2D {
  let p = path2dCache.get(d);
  if (!p) {
    p = new Path2D(d);
    if (path2dCache.size > 200) path2dCache.clear();
    path2dCache.set(d, p);
  }
  return p;
}

function paintIcon(ctx: CanvasRenderingContext2D, node: Node, s: Style, x: number, y: number): void {
  const shapes = (node.data?.iconShapes as IconShape[] | undefined) ?? [];
  if (!shapes.length) return;
  const size = node._width;
  const scale = size / 24;
  ctx.save();
  ctx.translate(x, y);
  ctx.scale(scale, scale);
  ctx.strokeStyle = s.color;
  ctx.lineWidth = 2;
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  for (const shape of shapes) {
    if (shape.kind === "path") {
      ctx.stroke(getPath(shape.d));
    } else if (shape.kind === "circle") {
      ctx.beginPath();
      ctx.arc(shape.cx, shape.cy, shape.r, 0, Math.PI * 2);
      ctx.stroke();
    } else {
      ctx.beginPath();
      if (shape.rx) {
        roundRectPath(ctx, shape.x, shape.y, shape.w, shape.h, [shape.rx, shape.rx, shape.rx, shape.rx]);
      } else {
        ctx.rect(shape.x, shape.y, shape.w, shape.h);
      }
      ctx.stroke();
    }
  }
  ctx.restore();
}

function paintSpinner(
  ctx: CanvasRenderingContext2D,
  node: Node,
  s: Style,
  x: number,
  y: number,
  now: number,
): void {
  const t = theme();
  const size = node._width;
  const lw = node.data?.lineWidth as number | undefined ?? 2;
  const cx = x + size / 2;
  const cy = y + size / 2;
  const r = size / 2 - lw / 2;
  ctx.lineWidth = lw;
  ctx.lineCap = "butt";
  ctx.strokeStyle = mix(t.blue, 0.26);
  ctx.beginPath();
  ctx.arc(cx, cy, r, 0, Math.PI * 2);
  ctx.stroke();
  // CSS border-top-color 起转：彩色四分之一弧绕圈
  const angle = ((now % 800) / 800) * Math.PI * 2 - Math.PI / 2;
  ctx.strokeStyle = t.blue;
  ctx.beginPath();
  ctx.arc(cx, cy, r, angle, angle + Math.PI / 2);
  ctx.stroke();
}

// ---------- 滚动条 ----------

function paintScrollbars(
  ctx: CanvasRenderingContext2D,
  node: Node,
  x: number,
  y: number,
  w: number,
  h: number,
): void {
  const t = theme();
  const hover = node._hoverWithin || node._hover;
  const thumbColor = hover ? t.scrollHover : t.scroll;
  ctx.save();
  if (node._maxScrollY > 0) {
    const trackX = x + w - 10;
    const visibleRatio = h / node._contentHeight;
    const thumbH = Math.max(24, h * visibleRatio);
    const range = h - thumbH - (node._maxScrollX > 0 ? 10 : 0);
    const thumbY = y + (node._scrollY / node._maxScrollY) * range;
    ctx.fillStyle = thumbColor;
    roundRectPath(ctx, trackX + 3, thumbY, 4, thumbH, [2, 2, 2, 2]);
    ctx.fill();
  }
  if (node._maxScrollX > 0) {
    const trackY = y + h - 10;
    const visibleRatio = w / node._contentWidth;
    const thumbW = Math.max(24, w * visibleRatio);
    const range = w - thumbW - (node._maxScrollY > 0 ? 10 : 0);
    const thumbX = x + (node._scrollX / node._maxScrollX) * range;
    ctx.fillStyle = thumbColor;
    roundRectPath(ctx, thumbX, trackY + 3, thumbW, 4, [2, 2, 2, 2]);
    ctx.fill();
  }
  ctx.restore();
}

/** 命中滚动条区域：返回 "v" | "h" | null 及 thumb 几何 */
export function scrollbarHit(
  node: Node,
  px: number,
  py: number,
): { axis: "v" | "h"; thumbStart: number; thumbLen: number } | null {
  const x = node._x;
  const y = node._y + (node._stickyDy || 0);
  const w = node._width;
  const h = node._height;
  if (node._maxScrollY > 0 && px >= x + w - 10 && px <= x + w && py >= y && py <= y + h) {
    const thumbH = Math.max(24, (h / node._contentHeight) * h);
    const range = h - thumbH - (node._maxScrollX > 0 ? 10 : 0);
    const thumbY = y + (node._scrollY / node._maxScrollY) * range;
    return { axis: "v", thumbStart: thumbY, thumbLen: thumbH };
  }
  if (node._maxScrollX > 0 && py >= y + h - 10 && py <= y + h && px >= x && px <= x + w) {
    const thumbW = Math.max(24, (w / node._contentWidth) * w);
    const range = w - thumbW - (node._maxScrollY > 0 ? 10 : 0);
    const thumbX = x + (node._scrollX / node._maxScrollX) * range;
    return { axis: "h", thumbStart: thumbX, thumbLen: thumbW };
  }
  return null;
}

// ---------- 文字选区 ----------

/** 累计所有可滚动祖先的滚动偏移（_textLines 是布局坐标，减去它得到视口坐标） */
export function scrollOffsetOf(node: Node): { sx: number; sy: number } {
  let sx = 0;
  let sy = 0;
  let p = node.parent;
  while (p) {
    sx += p._scrollX || 0;
    sy += p._scrollY || 0;
    p = p.parent;
  }
  return { sx, sy };
}

/** DFS 收集可视文本节点（文档顺序） */
export function collectTextNodes(root: Node, out: Node[] = []): Node[] {
  const s = root.style;
  if (s.display === "none" || s.opacity <= 0) return out;
  const skipTags = root.tag === "img" || root.tag === "icon" || root.tag === "spinner";
  if (!skipTags && root.textContent && s.userSelect !== "none") out.push(root);
  for (const c of root.children) collectTextNodes(c, out);
  return out;
}

export interface TextPos {
  node: Node;
  offset: number;
}

/** 视口坐标 → 最近的文本位置（按行盒垂直带 + 字符半宽命中） */
export function hitTextPosition(root: Node, x: number, y: number): TextPos | null {
  const nodes = collectTextNodes(root);
  let best: TextPos | null = null;
  let bestDist = Infinity;
  for (const n of nodes) {
    if (!n._textLines) continue;
    const { sx, sy } = scrollOffsetOf(n);
    for (const ln of n._textLines) {
      const left = ln.x - sx;
      const right = ln.x + ln.w - sx;
      const top = ln.y - sy;
      const bottom = ln.y + ln.lh - sy;
      if (y >= top - 2 && y <= bottom + 2) {
        const cx = Math.max(left, Math.min(right, x));
        const ls = ln.owner.style.letterSpacing || 0;
        // 按码点走、按 UTF-16 记账（slice/复制都基于 UTF-16 下标）
        let off = ln.offset + ln.text.length;
        const chars = [...ln.text];
        let acc = 0;
        let utf16 = 0;
        for (let i = 0; i < chars.length; i++) {
          const wCh = measureText(chars.slice(0, i + 1).join(""), ln.font, ls) - acc;
          if (cx < left + acc + wCh / 2) {
            off = ln.offset + utf16;
            break;
          }
          acc += wCh;
          utf16 += chars[i].length;
        }
        const dist =
          Math.abs(y - (top + bottom) / 2) + Math.max(0, left - x) + Math.max(0, x - right);
        if (dist < bestDist) {
          bestDist = dist;
          best = { node: n, offset: off };
        }
      }
    }
  }
  return best;
}

export interface Selection {
  startNode: Node;
  startOffset: number;
  endNode: Node;
  endOffset: number;
}

/** 选区高亮（画在文本之上，近似 ::selection 的半透明着色） */
export function paintSelection(ctx: CanvasRenderingContext2D, root: Node, sel: Selection): void {
  const nodes = collectTextNodes(root);
  const si = nodes.indexOf(sel.startNode);
  const ei = nodes.indexOf(sel.endNode);
  if (si < 0 || ei < 0) return;
  const t = theme();
  ctx.save();
  ctx.fillStyle = mix(t.accent, 0.3);
  for (let i = si; i <= ei; i++) {
    const n = nodes[i];
    if (!n._textLines) continue;
    const { sx, sy } = scrollOffsetOf(n);
    const startOff = i === si ? sel.startOffset : 0;
    const endOff = i === ei ? sel.endOffset : n.textContent.length;
    for (const ln of n._textLines) {
      const lineStart = ln.offset;
      const lineEnd = ln.offset + ln.text.length;
      if (endOff <= lineStart || startOff >= lineEnd) continue;
      const a = Math.max(0, startOff - lineStart);
      const b = Math.min(ln.text.length, endOff - lineStart);
      if (b <= a) continue;
      const ls = n.style.letterSpacing || 0;
      const x0 = ln.x - sx + measureText(ln.text.slice(0, a), ln.font, ls);
      const x1 = ln.x - sx + measureText(ln.text.slice(0, b), ln.font, ls);
      ctx.fillRect(x0, ln.y - sy, Math.max(1, x1 - x0), ln.lh);
    }
  }
  ctx.restore();
}

/** 选区纯文本（复制用） */
export function selectionText(root: Node, sel: Selection): string {
  const nodes = collectTextNodes(root);
  const si = nodes.indexOf(sel.startNode);
  const ei = nodes.indexOf(sel.endNode);
  if (si < 0 || ei < 0) return "";
  let out = "";
  for (let i = si; i <= ei; i++) {
    const n = nodes[i];
    const startOff = i === si ? sel.startOffset : 0;
    const endOff = i === ei ? sel.endOffset : n.textContent.length;
    out += n.textContent.slice(startOff, endOff);
    if (i < ei) out += "\n";
  }
  return out;
}

/** 供 tooltip 等使用：有效字体度量 */
export { fontMetrics, measureText, fade, mix };
