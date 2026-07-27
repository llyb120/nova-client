// layout.js - block / inline / flex layout engine.
// Computes _x/_y/_width/_height and content size for each node.

import { Node } from './Node.js';

function toPx(v, ref) {
  if (v == null || v === 'auto') return null;
  if (typeof v === 'number') return v;
  const s = String(v).trim();
  if (s.endsWith('px')) return parseFloat(s);
  if (s.endsWith('%')) return (parseFloat(s) / 100) * ref;
  const n = parseFloat(s);
  return isNaN(n) ? null : n;
}

function isAuto(v) { return v == null || v === 'auto'; }

function pad(p) {
  if (typeof p === 'number') return { t: p, r: p, b: p, l: p };
  if (Array.isArray(p)) {
    if (p.length === 1) return { t: p[0], r: p[0], b: p[0], l: p[0] };
    if (p.length === 2) return { t: p[0], r: p[1], b: p[0], l: p[1] };
    if (p.length === 3) return { t: p[0], r: p[1], b: p[2], l: p[1] };
    if (p.length === 4) return { t: p[0], r: p[1], b: p[2], l: p[3] };
  }
  return { t: 0, r: 0, b: 0, l: 0 };
}

function edges(b) {
  if (typeof b === 'number') return { t: b, r: b, b: b, l: b };
  if (Array.isArray(b)) {
    if (b.length === 1) return { t: b[0], r: b[0], b: b[0], l: b[0] };
    if (b.length === 2) return { t: b[0], r: b[1], b: b[0], l: b[1] };
    if (b.length === 3) return { t: b[0], r: b[1], b: b[2], l: b[1] };
    if (b.length === 4) return { t: b[0], r: b[1], b: b[2], l: b[3] };
  }
  return { t: 0, r: 0, b: 0, l: 0 };
}

// Shared measure context.
let _measureCtx = null;
function measureCtx() {
  if (!_measureCtx) {
    const c = typeof document !== 'undefined' ? document.createElement('canvas') : null;
    _measureCtx = c ? c.getContext('2d') : null;
  }
  return _measureCtx;
}

const _measureCache = new Map();
const MEASURE_CACHE_MAX = 4096;

export function measureText(text, fontSize, fontFamily, fontWeight, fontStyle) {
  const str = String(text);
  const fs = fontSize || 14;
  const ff = fontFamily || 'sans-serif';
  const fw = fontWeight || 'normal';
  const fst = fontStyle || 'normal';
  const key = fst + '|' + fw + '|' + fs + '|' + ff + '|' + str;
  const cached = _measureCache.get(key);
  if (cached != null) return cached;
  const ctx = measureCtx();
  let width;
  if (!ctx) {
    width = str.length * fs * 0.6;
  } else {
    ctx.font = `${fst} ${fw} ${fs}px ${ff}`;
    width = ctx.measureText(str).width;
  }
  if (_measureCache.size >= MEASURE_CACHE_MAX) _measureCache.clear();
  _measureCache.set(key, width);
  return width;
}

/** 仅平移已布局子树的几何（含 inline 片段），避免闭合轮次在上方高度变化时整树重测。 */
function shiftLayoutTree(node, dx, dy) {
  if (!dx && !dy) return;
  node._x += dx;
  node._y += dy;
  if (node._textLines) {
    for (const frag of node._textLines) {
      frag.x += dx;
      frag.y += dy;
    }
  }
  for (const child of node.children) shiftLayoutTree(child, dx, dy);
}

/** 布局算法版本：变更时强制已缓存的 _layoutStable 子树重新排版（避免 HMR/旧几何残留）。 */
export const LAYOUT_REV = 5;

// Layout a node within a content box (x, y, width, height available).
// Returns the node's outer height consumed (including margin).
export function layout(node, x, y, availW, availH) {
  const s = node.style;
  // 已闭合轮次的节点树不可再变化。宽度未变时复用布局；仅 Y 变化则平移子树。
  // 流式尾部更新无需重新测量全部历史文本。
  if (node._layoutStable && node._layoutRev === LAYOUT_REV && node._layoutInputX === x
      && node._layoutInputWidth === availW && node._outerHeight != null) {
    if (node._layoutInputY !== y) {
      shiftLayoutTree(node, 0, y - node._layoutInputY);
      node._layoutInputY = y;
    }
    return node._outerHeight;
  }

  // display:none -> invisible, zero size
  if (s.display === 'none') {
    node._x = x; node._y = y;
    node._width = 0; node._height = 0;
    node._contentWidth = 0; node._contentHeight = 0;
    node._maxScrollX = 0; node._maxScrollY = 0;
    return 0;
  }

  const p = pad(s.padding);
  const m = pad(s.margin);
  const b = edges(s.border);

  // resolve width
  let width = toPx(s.width, availW);
  if (width == null && node.tag === 'button') {
    const fs = s.fontSize || 14;
    const statsWidth = s.trailingStats
      ? measureText(`+${s.trailingStats.add} -${s.trailingStats.del}`, 11.5,
        s.fontFamily, 'normal', 'normal') + 14 : 0;
    const adornment = (s.leadingIcon ? 22 : 0) + (s.leadingChevron != null ? 17 : 0)
      + (s.trailingChevron != null ? 20 : 0) + (s.trailingStatus ? 20 : 0) + statsWidth;
    width = measureText(node.textContent || s.value || '', fs, s.fontFamily, s.fontWeight, s.fontStyle)
      + adornment + b.l + b.r + p.l + p.r + (p.l + p.r > 0 ? 0 : 12);
  }
  if (width == null) width = availW - m.l - m.r;
  width = Math.max(0, width);

  // resolve absolute / relative offset
  let ox = x + m.l, oy = y + m.t;
  if (s.position === 'absolute' || s.position === 'relative') {
    if (!isAuto(s.left)) ox = x + (toPx(s.left, availW) || 0);
    if (!isAuto(s.top)) oy = y + (toPx(s.top, availH) || 0);
  }

  const innerX = ox + b.l + p.l;
  const innerY = oy + b.t + p.t;
  const innerW = Math.max(0, width - b.l - b.r - p.l - p.r);

  // measure content
  let contentH = 0;
  let contentW = 0;

  if (node.tag === 'img') {
    const img = node._img;
    let iw = width, ih = 0;
    if (img && node._imgLoaded) {
      const ratio = img.naturalWidth / img.naturalHeight || 1;
      if (isAuto(s.height)) ih = width / ratio;
      else ih = toPx(s.height, availH) || 0;
    } else if (isAuto(s.height)) {
      ih = 0;
    } else {
      ih = toPx(s.height, availH) || 0;
    }
    contentW = width;
    contentH = ih;
  } else if (node.tag === 'input' || node.tag === 'button') {
    const fs = s.fontSize || 14;
    const lh = fs * (s.lineHeight || 1.2);
    const statsSpace = s.trailingStats
      ? measureText(`+${s.trailingStats.add} -${s.trailingStats.del}`, 11.5,
        s.fontFamily, 'normal', 'normal') + 14 : 0;
    const iconSpace = node.tag === 'button'
      ? (s.leadingIcon ? 22 : 0) + (s.leadingChevron != null ? 17 : 0)
        + (s.trailingChevron != null ? 20 : 0) + (s.trailingStatus ? 20 : 0) + statsSpace
      : 0;
    const controlW = Math.max(1, width - b.l - b.r - p.l - p.r - iconSpace);
    const controlText = String(node.textContent || s.value || '');
    const controlCacheHit = node._controlLines && node._controlWrapText === controlText
      && node._controlWrapWidth === controlW && node._controlWrapFontSize === fs
      && node._controlWrapFont === s.fontFamily && node._controlWrapWeight === s.fontWeight
      && node._controlWrapStyle === s.fontStyle && node._controlWrapWhiteSpace === s.whiteSpace;
    const lines = controlCacheHit ? node._controlLines
      : node.tag === 'button' && s.whiteSpace !== 'nowrap'
        ? wrapText(controlText, controlW, fs, s.fontFamily, s.whiteSpace, s.fontWeight, s.fontStyle)
        : [controlText];
    node._controlWrapText = controlText; node._controlWrapWidth = controlW;
    node._controlWrapFontSize = fs; node._controlWrapFont = s.fontFamily;
    node._controlWrapWeight = s.fontWeight; node._controlWrapStyle = s.fontStyle;
    node._controlWrapWhiteSpace = s.whiteSpace; node._controlLines = lines;
    contentW = Math.max(0, width - b.l - b.r - p.l - p.r);
    contentH = isAuto(s.height)
      ? Math.max(lh, lines.length * lh)
      : Math.max(0, (toPx(s.height, availH) || lh) - b.t - b.b - p.t - p.b);
  } else if (s.display === 'flex') {
    const r = layoutFlex(node, innerX, innerY, innerW, availH);
    contentW = r.contentW;
    contentH = r.contentH;
  } else {
    // block container (default). children may be block or inline.
    const r = layoutBlockChildren(node, innerX, innerY, innerW, availH);
    contentW = r.contentW;
    contentH = r.contentH;
    if (node.textContent) {
      const fs = s.fontSize || 14;
      const ff = s.fontFamily || 'sans-serif';
      const fw = s.fontWeight || 'normal';
      const fst = s.fontStyle || 'normal';
      const lh = fs * (s.lineHeight || 1.4);
      const wrapCacheHit = node._wrappedLines && node._wrapText === node.textContent
        && node._wrapWidth === innerW && node._wrapFontSize === fs && node._wrapFont === ff
        && node._wrapWeight === fw && node._wrapStyle === fst && node._wrapWhiteSpace === s.whiteSpace;
      const lines = wrapCacheHit ? node._wrappedLines
        : wrapText(node.textContent, innerW, fs, ff, s.whiteSpace, fw, fst);
      node._wrapText = node.textContent; node._wrapWidth = innerW; node._wrapFontSize = fs;
      node._wrapFont = ff; node._wrapWeight = fw; node._wrapStyle = fst;
      node._wrapWhiteSpace = s.whiteSpace; node._wrappedLines = lines;
      const textH = lines.length * lh;
      // account for text width in contentW (max line width)
      let textW = 0;
      for (const ln of lines) textW = Math.max(textW, measureText(ln, fs, ff, fw, fst));
      contentW = Math.max(contentW, textW);
      if (node.children.length === 0) contentH = textH;
      else contentH = Math.max(contentH, textH);
    }
  }

  // resolve height
  let height = isAuto(s.height)
    ? contentH + b.t + b.b + p.t + p.b
    : (toPx(s.height, availH) || 0);
  const minHeight = toPx(s.minHeight, availH);
  if (minHeight != null) height = Math.max(height, minHeight);
  const maxHeight = toPx(s.maxHeight, availH);
  if (maxHeight != null) height = Math.min(height, maxHeight);
  height = Math.max(0, height);

  node._x = ox;
  node._y = oy;
  node._width = width;
  node._height = height;
  node._contentWidth = contentW + b.l + b.r + p.l + p.r;
  node._contentHeight = contentH + b.t + b.b + p.t + p.b;

  const scrollable = s.overflow === 'scroll' || s.overflow === 'auto';
  if (scrollable) {
    node._maxScrollX = Math.max(0, node._contentWidth - width);
    node._maxScrollY = Math.max(0, node._contentHeight - height);
  } else {
    node._maxScrollX = 0;
    node._maxScrollY = 0;
  }

  const outerHeight = height + m.t + m.b;
  if (node._layoutStable) {
    node._layoutRev = LAYOUT_REV;
    node._layoutInputX = x;
    node._layoutInputY = y;
    node._layoutInputWidth = availW;
    node._outerHeight = outerHeight;
  }
  return outerHeight;
}

// Layout block-level children. Consecutive inline/inline-block children are
// placed in shared inline line boxes; block/flex children each occupy their
// own vertical slot.
function collapsedMargin(a, b) {
  if (a >= 0 && b >= 0) return Math.max(a, b);
  if (a <= 0 && b <= 0) return Math.min(a, b);
  return a + b;
}

function layoutBlockChildren(node, innerX, innerY, innerW, availH) {
  let cy = innerY;
  let contentW = 0;
  let contentH = 0;
  let previousBottomMargin = 0;
  let hasBlock = false;
  const kids = node.children;
  let i = 0;
  while (i < kids.length) {
    const child = kids[i];
    const disp = child.style.display;
    if (disp === 'inline' || disp === 'inline-block') {
      // gather a run of inline children
      const run = [];
      while (i < kids.length) {
        const d = kids[i].style.display;
        if (d === 'inline' || d === 'inline-block') { run.push(kids[i]); i++; }
        else break;
      }
      const r = layoutInlineRun(run, innerX, cy, innerW, availH);
      cy += r.lineHeight * r.lineCount;
      previousBottomMargin = 0;
      hasBlock = true;
      contentH = (cy - innerY);
      contentW = Math.max(contentW, r.maxW);
    } else {
      // 普通 block 的相邻垂直 margin 按 CSS 规则折叠；flex 容器内部则由 layoutFlex 处理，不折叠。
      const cm = pad(child.style.margin);
      const overlap = hasBlock
        ? previousBottomMargin + cm.t - collapsedMargin(previousBottomMargin, cm.t)
        : 0;
      const startY = cy - overlap;
      const ch = layout(child, innerX, startY, innerW, availH);
      cy = startY + ch;
      previousBottomMargin = cm.b;
      hasBlock = true;
      if (child._x + child._width - innerX > contentW) {
        contentW = (child._x + child._width) - innerX;
      }
      contentH = cy - innerY;
      i++;
    }
  }
  return { contentW, contentH };
}

// Layout a run of inline children into line boxes (left-to-right, wrapping).
// Inline text children are broken at word boundaries across lines.
// Inline-block children are treated as atomic units.
// Sets _x/_y/_width/_height and _textLines on each inline text child.
// Returns metrics.
function layoutInlineRun(run, innerX, startY, innerW, availH) {
  const lineH = computeLineHeightForRun(run);
  let x = innerX;
  let y = startY;
  let lineIdx = 0;
  let maxW = 0;
  for (let k = 0; k < run.length; k++) {
    const child = run[k];
    const cs = child.style;
    const cp = pad(cs.padding);
    const cm = pad(cs.margin);

    // marked({ breaks: true }) emits <br> for source newlines. A br is a forced
    // line break, not a zero-width inline span on the current line.
    if (child.tag === 'br') {
      child._x = x; child._y = y; child._width = 0; child._height = lineH;
      child._textLines = [];
      x = innerX;
      y += lineH;
      lineIdx++;
      continue;
    }

    if (cs.display === 'inline-block') {
      // inline-block: atomic unit
      layout(child, x + cm.l, y + cm.t, innerW, availH);
      const cw = child._width + cp.l + cp.r + cm.l + cm.r;
      // wrap if doesn't fit and not at line start
      if (x + cw > innerX + innerW + 0.001 && x > innerX) {
        x = innerX;
        y += lineH;
        lineIdx++;
        layout(child, x + cm.l, y + cm.t, innerW, availH);
      }
      x += cw;
      maxW = Math.max(maxW, x - innerX);
    } else {
      // inline text: 先按空白分词，放不下的词（中文长句/路径）再按字符折行，
      // 与 wrapText 对齐；否则整段贴在一行会凸出容器。
      const fs = cs.fontSize || 14;
      const ff = cs.fontFamily || 'sans-serif';
      const fw = cs.fontWeight || 'normal';
      const fst = cs.fontStyle || 'normal';
      const text = child.textContent || '';
      const childLH = fs * (cs.lineHeight || 1.4);
      const tokens = text.split(/(\s+)/).filter(t => t.length > 0);
      const lineLimit = innerX + innerW;

      let lineText = '';
      let lineStartX = x;
      const fragments = [];
      let offset = 0;
      let childMinX = Infinity;
      let childMaxX = -Infinity;
      let childMinY = Infinity;

      const flushLine = () => {
        if (!lineText) return;
        const lineW = measureText(lineText, fs, ff, fw, fst);
        const fragX = lineStartX + cp.l;
        const fragY = y + cm.t + cp.t;
        fragments.push({ text: lineText, x: fragX, y: fragY, w: lineW, fs, lh: childLH, offset });
        offset += lineText.length;
        childMinX = Math.min(childMinX, fragX);
        childMaxX = Math.max(childMaxX, fragX + lineW);
        childMinY = Math.min(childMinY, fragY);
        lineText = '';
      };

      const breakLine = () => {
        flushLine();
        x = innerX;
        y += lineH;
        lineIdx++;
        lineStartX = x;
      };

      for (const token of tokens) {
        const tokenW = measureText(token, fs, ff, fw, fst);

        // 当前行已有内容且本词放不下：先换行（空白不强制换行）
        if (lineText.length > 0 && /\S/.test(token) && x + tokenW > lineLimit + 0.001) {
          breakLine();
        }

        if (lineText === '') lineStartX = x;

        // 整词能放下（或纯空白）：直接追加
        if (x + tokenW <= lineLimit + 0.001 || !/\S/.test(token)) {
          lineText += token;
          x += tokenW;
          maxW = Math.max(maxW, x - innerX);
          continue;
        }

        // 超长无空格 token：按字符拆分（与 wrapText 一致）
        for (const ch of Array.from(token)) {
          const charW = measureText(ch, fs, ff, fw, fst);
          if (lineText && x + charW > lineLimit + 0.001) breakLine();
          if (lineText === '') lineStartX = x;
          lineText += ch;
          x += charW;
          maxW = Math.max(maxW, x - innerX);
        }
      }

      flushLine();

      child._textLines = fragments.length > 0 ? fragments
        : [{ text: '', x: x + cp.l, y: y + cm.t + cp.t, w: 0, fs, lh: childLH, offset: 0 }];

      child._x = (childMinX !== Infinity ? childMinX : x) - cm.l - cp.l;
      child._y = (childMinY !== Infinity ? childMinY : y) - cm.t - cp.t;
      child._width = childMaxX !== -Infinity ? (childMaxX - childMinX) + cp.l + cp.r : 0;
      child._height = fragments.length * lineH + cp.t + cp.b + cm.t + cm.b;

      // 与 paintInlineText 一致：灰底/边框按 padding+border 外扩，布局游标也要推进，
      // 否则后续英文会画进上一段 inline code 的背景里。
      const cb = edges(cs.border);
      x += cp.l + cp.r + cb.l + cb.r;
      maxW = Math.max(maxW, x - innerX);
    }
  }
  return { lineHeight: lineH, lineCount: lineIdx + 1, maxW };
}

function alignInlineVertical(child, lineH) {
  const va = child.style.verticalAlign || 'baseline';
  const ch = child._height;
  if (va === 'top') return 0;
  if (va === 'middle') return Math.max(0, (lineH - ch) / 2);
  if (va === 'bottom') return Math.max(0, lineH - ch);
  return 0; // baseline: keep at top of line box (simplified)
}

function computeLineHeightForRun(run) {
  let max = 0;
  for (const c of run) {
    const fs = c.style.fontSize || 14;
    const lh = fs * (c.style.lineHeight || 1.4);
    if (lh > max) max = lh;
  }
  return max || 14 * 1.4;
}

// Flex layout (row / column). Supports gap, justifyContent, alignItems, wrap.
function layoutFlex(node, innerX, innerY, innerW, availH) {
  const s = node.style;
  const dir = s.flexDirection || 'row';
  const wrap = s.flexWrap === 'wrap';
  const gap = typeof s.gap === 'number' ? s.gap : 0;
  const kids = node.children.filter(c => c.style.display !== 'none');
  const isRow = dir !== 'column';

  // First pass: measure each child's main/cross size.
  // For auto-sized children we lay them out with a very large main axis to
  // obtain their intrinsic content size (no wrapping), then read _contentWidth/Height.
  const items = kids.map(child => {
    const mainAuto = isAuto(isRow ? child.style.width : child.style.height);
    const crossAvail = isRow ? availH : innerW;
    const consumedHeight = isRow
      ? layout(child, innerX, innerY, mainAuto ? Infinity : innerW, crossAvail)
      : layout(child, innerX, innerY, innerW, mainAuto ? Infinity : crossAvail);
    const margin = pad(child.style.margin);
    let mainSize;
    let crossSize;
    if (isRow) {
      mainSize = mainAuto
        ? (child._contentWidth || 0) + margin.l + margin.r
        : child._width + margin.l + margin.r;
      crossSize = consumedHeight;
    } else {
      mainSize = consumedHeight;
      crossSize = child._width + margin.l + margin.r;
    }
    return { child, mainSize, crossSize, mainAuto, flex: parseFlex(child.style.flex) };
  });

  // Determine total main size and free space.
  let totalMain = 0;
  for (const it of items) totalMain += it.mainSize;
  totalMain += gap * Math.max(0, items.length - 1);
  const containerMain = isRow ? innerW : (isAuto(s.height) ? totalMain : (node._height || totalMain));
  let free = containerMain - totalMain;

  // Grow flex items if free > 0.
  let totalGrow = 0;
  for (const it of items) totalGrow += it.flex.grow;
  if (free > 0 && totalGrow > 0) {
    let used = 0;
    for (const it of items) {
      const add = (it.flex.grow / totalGrow) * free;
      it.mainSize += add;
      used += add;
    }
    // shrink if overflow (basic)
  } else if (free < 0) {
    let totalShrink = 0;
    for (const it of items) totalShrink += it.flex.shrink;
    if (totalShrink > 0) {
      for (const it of items) {
        it.mainSize += (it.flex.shrink / totalShrink) * free;
      }
    }
  }

  // Place items.
  let cursor = innerX;
  let crossCursor = innerY;
  let lineMain = 0;
  let lineCrossMax = 0;
  let contentW = 0;
  let contentH = 0;
  const lineItems = [];

  const placeLine = (items, lineStart) => {
    const lineMainSize = items.reduce((a, it) => a + it.mainSize, 0) + gap * Math.max(0, items.length - 1);
    let leading = 0;
    const jc = s.justifyContent || 'flex-start';
    if (jc === 'center') leading = (containerMain - lineMainSize) / 2;
    else if (jc === 'flex-end') leading = containerMain - lineMainSize;
    else if (jc === 'space-between' && items.length > 1) {
      // handled per-item below
      leading = 0;
    } else if (jc === 'space-around' && items.length > 1) {
      leading = (containerMain - lineMainSize) / items.length / 2;
    }
    let pos = lineStart + leading;
    const betweenGap = jc === 'space-between' && items.length > 1
      ? (containerMain - lineMainSize) / (items.length - 1) + gap
      : jc === 'space-around' && items.length > 1
        ? (containerMain - lineMainSize) / items.length + gap
        : gap;
    let crossMax = 0;
    for (const it of items) crossMax = Math.max(crossMax, it.crossSize);
    let cpos = pos;
    for (const it of items) {
      const child = it.child;
      const ai = s.alignItems || 'stretch';
      let cross;
      if (isRow) {
        let cy;
        if (ai === 'flex-start' || ai === 'stretch') cy = innerY + (lineStart === innerX ? 0 : 0);
        else if (ai === 'center') cy = innerY + (crossMax - it.crossSize) / 2;
        else if (ai === 'flex-end') cy = innerY + crossMax - it.crossSize;
        else cy = innerY;
        // cpos/it.mainSize 均按包含 margin 的 flex 外尺寸计算；layout 再按 border-box 放置内容盒。
        layout(child, cpos, cy, it.mainSize, crossMax);
      } else {
        let cx;
        if (ai === 'flex-start' || ai === 'stretch') cx = innerX;
        else if (ai === 'center') cx = innerX + (innerW - it.crossSize) / 2;
        else if (ai === 'flex-end') cx = innerX + innerW - it.crossSize;
        else cx = innerX;
        layout(child, cx, cpos, ai === 'stretch' ? innerW : it.crossSize, it.mainSize);
      }
      cpos += it.mainSize + betweenGap;
    }
    return { lineMainSize, crossMax };
  };

  // Single-line placement (wrap not heavily supported; we do basic wrap for row).
  if (!wrap || isRow && totalMain <= containerMain + 0.001 || !isRow) {
    const r = placeLine(items, isRow ? innerX : innerY);
    contentW = isRow ? r.lineMainSize : innerW;
    contentH = isRow ? r.crossMax : r.lineMainSize;
  } else {
    // wrap rows
    let cur = [];
    let curMain = 0;
    let yy = innerY;
    let maxMain = 0;
    let totalCross = 0;
    for (const it of items) {
      if (cur.length && curMain + it.mainSize + gap > containerMain + 0.001) {
        const r = placeLine(cur, innerX);
        totalCross += r.crossMax + gap;
        cur = [];
        curMain = 0;
      }
      if (cur.length) curMain += gap;
      cur.push(it);
      curMain += it.mainSize;
    }
    if (cur.length) {
      const r = placeLine(cur, innerX);
      totalCross += r.crossMax;
    }
    contentW = containerMain;
    contentH = totalCross;
  }

  return { contentW, contentH };
}

function parseFlex(flex) {
  if (typeof flex === 'number') return { grow: flex, shrink: 0, basis: 'auto' };
  if (typeof flex === 'string') {
    const parts = flex.split(/\s+/);
    return {
      grow: parseFloat(parts[0]) || 0,
      shrink: parseFloat(parts[1]) || 0,
      basis: parts[2] || 'auto'
    };
  }
  return { grow: 0, shrink: 0, basis: 'auto' };
}

// Simple text wrapping via shared measureText cache (avoids repeated canvas measure).
export function wrapText(text, maxW, fontSize, fontFamily, whiteSpace, fontWeight = 'normal', fontStyle = 'normal') {
  if (whiteSpace === 'nowrap') return [String(text)];
  if (whiteSpace === 'pre') return String(text).split('\n');
  const fs = fontSize || 14;
  const ff = fontFamily || 'sans-serif';
  const fw = fontWeight || 'normal';
  const fst = fontStyle || 'normal';
  const lines = [];
  const preserveWhitespace = whiteSpace === 'pre-wrap';
  const paragraphs = String(text).split('\n');
  for (const para of paragraphs) {
    const words = para.split(/(\s+)/);
    let cur = '';
    let curWidth = 0;
    for (const word of words) {
      let w = word;
      const wordWidth = measureText(w, fs, ff, fw, fst);
      if (curWidth + wordWidth <= maxW) {
        cur += w;
        curWidth += wordWidth;
        continue;
      }
      if (cur) {
        lines.push(preserveWhitespace ? cur : cur.trimEnd());
        cur = '';
        curWidth = 0;
        if (!preserveWhitespace) w = w.trimStart();
      }
      // 无空格命令/路径按字符拆分。只测每个字符一次，避免超长命令 O(n²)。
      for (const ch of Array.from(w)) {
        const charWidth = measureText(ch, fs, ff, fw, fst);
        if (cur && curWidth + charWidth > maxW) {
          lines.push(cur);
          cur = ch;
          curWidth = charWidth;
        } else {
          cur += ch;
          curWidth += charWidth;
        }
      }
    }
    lines.push(cur || '');
  }
  return lines;
}
