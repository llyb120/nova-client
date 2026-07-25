// @ts-nocheck
// layout.js - block / inline / flex layout engine.
// Computes _x/_y/_width/_height and content size for each node.

import { Node } from './Node';

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
    if (p.length === 4) return { t: p[0], r: p[1], b: p[2], l: p[3] };
  }
  return { t: 0, r: 0, b: 0, l: 0 };
}

function edges(b) {
  if (typeof b === 'number') return { t: b, r: b, b: b, l: b };
  if (Array.isArray(b)) {
    if (b.length === 1) return { t: b[0], r: b[0], b: b[0], l: b[0] };
    if (b.length === 2) return { t: b[0], r: b[1], b: b[0], l: b[1] };
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

export function measureText(text, fontSize, fontFamily, fontWeight, fontStyle) {
  const ctx = measureCtx();
  if (!ctx) {
    // fallback estimate
    return String(text).length * (fontSize || 14) * 0.6;
  }
  ctx.font = `${fontStyle || 'normal'} ${fontWeight || 'normal'} ${fontSize || 14}px ${fontFamily || 'sans-serif'}`;
  return ctx.measureText(String(text)).width;
}

// Layout a node within a content box (x, y, width, height available).
// Returns the node's outer height consumed (including margin).
export function layout(node, x, y, availW, availH) {
  const s = node.style;

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

  // resolve width
  let width = toPx(s.width, availW);
  if (width == null) width = availW - m.l - m.r;
  width = Math.max(0, width);

  // resolve absolute / relative offset
  let ox = x + m.l, oy = y + m.t;
  if (s.position === 'absolute' || s.position === 'relative') {
    if (!isAuto(s.left)) ox = x + (toPx(s.left, availW) || 0);
    if (!isAuto(s.top)) oy = y + (toPx(s.top, availH) || 0);
  }

  const innerX = ox + p.l;
  const innerY = oy + p.t;
  const innerW = Math.max(0, width - p.l - p.r);

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
    let ih = isAuto(s.height) ? lh + 8 : (toPx(s.height, availH) || lh);
    contentW = width;
    contentH = ih;
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
      const lines = wrapText(node.textContent, innerW, fs, ff, s.whiteSpace);
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
  let height = isAuto(s.height) ? contentH + p.t + p.b : (toPx(s.height, availH) || 0);
  height = Math.max(0, height);

  node._x = ox;
  node._y = oy;
  node._width = width;
  node._height = height;
  node._contentWidth = contentW + p.l + p.r;
  node._contentHeight = contentH + p.t + p.b;

  const scrollable = s.overflow === 'scroll' || s.overflow === 'auto';
  if (scrollable) {
    node._maxScrollX = Math.max(0, node._contentWidth - width);
    node._maxScrollY = Math.max(0, node._contentHeight - height);
  } else {
    node._maxScrollX = 0;
    node._maxScrollY = 0;
  }

  return height + m.t + m.b;
}

// Layout block-level children. Consecutive inline/inline-block children are
// placed in shared inline line boxes; block/flex children each occupy their
// own vertical slot.
function layoutBlockChildren(node, innerX, innerY, innerW, availH) {
  let cy = innerY;
  let contentW = 0;
  let contentH = 0;
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
      contentH = (cy - innerY);
      contentW = Math.max(contentW, r.maxW);
    } else {
      // block / flex child
      const ch = layout(child, innerX, cy, innerW, availH);
      cy += ch;
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
      // inline text: word-level line breaking
      const fs = cs.fontSize || 14;
      const ff = cs.fontFamily || 'sans-serif';
      const fw = cs.fontWeight || 'normal';
      const fst = cs.fontStyle || 'normal';
      const text = child.textContent || '';
      const childLH = fs * (cs.lineHeight || 1.4);

      // Split into tokens (words and whitespace), preserving whitespace
      const tokens = text.split(/(\s+)/).filter(t => t.length > 0);

      let lineText = '';
      let lineStartX = x;
      const fragments = [];
      let offset = 0;
      let childMinX = Infinity;
      let childMaxX = -Infinity;
      let childMinY = Infinity;

      for (const token of tokens) {
        const tokenW = measureText(token, fs, ff, fw, fst);
        const tokenFullW = tokenW + cp.l + cp.r;

        // Wrap if token doesn't fit and we have content on current line
        if (x + tokenFullW > innerX + innerW + 0.001 && lineText.length > 0 && /\S/.test(token)) {
          // Flush current line fragment
          const lineW = measureText(lineText, fs, ff, fw, fst);
          const fragX = lineStartX + cp.l;
          const fragY = y + cm.t + cp.t;
          fragments.push({ text: lineText, x: fragX, y: fragY, w: lineW, fs, lh: childLH, offset });
          offset += lineText.length;
          childMinX = Math.min(childMinX, fragX);
          childMaxX = Math.max(childMaxX, fragX + lineW);
          childMinY = Math.min(childMinY, fragY);
          // Start new line
          x = innerX;
          y += lineH;
          lineIdx++;
          lineText = '';
          lineStartX = x;
        }

        if (lineText === '') {
          lineStartX = x;
          lineText = token;
        } else {
          lineText += token;
        }
        x += tokenFullW;
        maxW = Math.max(maxW, x - innerX);
      }

      // Flush remaining text
      if (lineText) {
        const lineW = measureText(lineText, fs, ff, fw, fst);
        const fragX = lineStartX + cp.l;
        const fragY = y + cm.t + cp.t;
        fragments.push({ text: lineText, x: fragX, y: fragY, w: lineW, fs, lh: childLH, offset });
        offset += lineText.length;
        childMinX = Math.min(childMinX, fragX);
        childMaxX = Math.max(childMaxX, fragX + lineW);
        childMinY = Math.min(childMinY, fragY);
      }

      // Store fragments for painting and hit testing
      child._textLines = fragments.length > 0 ? fragments
        : [{ text: '', x: x + cp.l, y: y + cm.t + cp.t, w: 0, fs, lh: childLH, offset: 0 }];

      // Set bounding box for hit testing
      child._x = (childMinX !== Infinity ? childMinX : x) - cm.l - cp.l;
      child._y = (childMinY !== Infinity ? childMinY : y) - cm.t - cp.t;
      child._width = childMaxX !== -Infinity ? (childMaxX - childMinX) + cp.l + cp.r : 0;
      child._height = fragments.length * lineH + cp.t + cp.b + cm.t + cm.b;
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
    // layout with a generous main size to measure intrinsic main size
    if (isRow) {
      layout(child, innerX, innerY, mainAuto ? Infinity : innerW, crossAvail);
    } else {
      layout(child, innerX, innerY, innerW, mainAuto ? Infinity : crossAvail);
    }
    let mainSize, crossSize;
    if (mainAuto) {
      // use intrinsic content size on the main axis (avoids Infinity)
      mainSize = isRow ? (child._contentWidth || 0) : (child._contentHeight || 0);
      // fix the node's main size so later code sees a finite value
      if (isRow) child._width = mainSize; else child._height = mainSize;
    } else {
      mainSize = isRow ? child._width : child._height;
    }
    crossSize = isRow ? child._height : child._width;
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
        // re-layout with final main size and possibly stretched cross
        const targetH = ai === 'stretch' && isAuto(child.style.height) ? crossMax : child._height;
        layout(child, cpos, cy, it.mainSize, crossMax);
        if (ai === 'stretch' && isAuto(child.style.height)) {
          child._height = crossMax;
        }
        child._width = it.mainSize;
      } else {
        let cx;
        if (ai === 'flex-start' || ai === 'stretch') cx = innerX;
        else if (ai === 'center') cx = innerX + (innerW - it.crossSize) / 2;
        else if (ai === 'flex-end') cx = innerX + innerW - it.crossSize;
        else cx = innerX;
        layout(child, cx, cpos, it.crossSize, it.mainSize);
        if (ai === 'stretch' && isAuto(child.style.width)) {
          child._width = innerW;
        }
        child._height = it.mainSize;
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

// Simple text wrapping using canvas measureText via a shared context.
export function wrapText(text, maxW, fontSize, fontFamily, whiteSpace) {
  if (whiteSpace === 'nowrap') return [String(text)];
  if (whiteSpace === 'pre') return String(text).split('\n');
  const ctx = measureCtx();
  const lines = [];
  if (!ctx) {
    // fallback: char-based
    let cur = '';
    for (const ch of String(text)) {
      cur += ch;
      if (cur.length * fontSize * 0.6 > maxW) {
        lines.push(cur);
        cur = '';
      }
    }
    if (cur) lines.push(cur);
    return lines.length ? lines : [''];
  }
  ctx.font = `${fontSize}px ${fontFamily}`;
  const paragraphs = String(text).split('\n');
  for (const para of paragraphs) {
    const words = para.split(/(\s+)/);
    let cur = '';
    for (const w of words) {
      const test = cur + w;
      if (ctx.measureText(test).width > maxW && cur) {
        lines.push(cur);
        cur = w.trimStart();
      } else {
        cur = test;
      }
    }
    lines.push(cur || '');
  }
  return lines;
}
