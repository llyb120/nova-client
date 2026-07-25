// @ts-nocheck
// painter.js - draws the node tree onto a canvas 2d context.

import { wrapText, measureText } from './layout';

function pad(p) {
  if (typeof p === 'number') return { t: p, r: p, b: p, l: p };
  if (Array.isArray(p)) {
    if (p.length === 1) return { t: p[0], r: p[0], b: p[0], l: p[0] };
    if (p.length === 2) return { t: p[0], r: p[1], b: p[0], l: p[1] };
    if (p.length === 4) return { t: p[0], r: p[1], b: p[2], l: p[3] };
  }
  return { t: 0, r: 0, b: 0, l: 0 };
}

// Resolve border width per edge. Accepts a number or an array like [t,r,b,l].
function edges(b) {
  if (typeof b === 'number') return { t: b, r: b, b: b, l: b };
  if (Array.isArray(b)) {
    if (b.length === 1) return { t: b[0], r: b[0], b: b[0], l: b[0] };
    if (b.length === 2) return { t: b[0], r: b[1], b: b[0], l: b[1] };
    if (b.length === 4) return { t: b[0], r: b[1], b: b[2], l: b[3] };
  }
  return { t: 0, r: 0, b: 0, l: 0 };
}

// Stroke each border edge independently (supports per-side widths).
function strokeBorder(ctx, x, y, w, h, e, radius) {
  const r = Math.max(0, Math.min(radius || 0, w / 2, h / 2));
  ctx.beginPath();
  if (e.t > 0) {
    ctx.moveTo(x + r, y + e.t / 2);
    ctx.lineTo(x + w - r, y + e.t / 2);
  }
  if (e.r > 0) {
    ctx.moveTo(x + w - e.r / 2, y + r);
    ctx.lineTo(x + w - e.r / 2, y + h - r);
  }
  if (e.b > 0) {
    ctx.moveTo(x + r, y + h - e.b / 2);
    ctx.lineTo(x + w - r, y + h - e.b / 2);
  }
  if (e.l > 0) {
    ctx.moveTo(x + e.l / 2, y + r);
    ctx.lineTo(x + e.l / 2, y + h - r);
  }
  ctx.stroke();
}

function roundRect(ctx, x, y, w, h, r) {
  r = Math.min(r, w / 2, h / 2);
  r = Math.max(0, r);
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.lineTo(x + w - r, y);
  ctx.quadraticCurveTo(x + w, y, x + w, y + r);
  ctx.lineTo(x + w, y + h - r);
  ctx.quadraticCurveTo(x + w, y + h, x + w - r, y + h);
  ctx.lineTo(x + r, y + h);
  ctx.quadraticCurveTo(x, y + h, x, y + h - r);
  ctx.lineTo(x, y + r);
  ctx.quadraticCurveTo(x, y, x + r, y);
  ctx.closePath();
}

// Paint a node. clipX/clipY/clipW/clipH define the visible clip rect (already in screen coords).
export function paint(ctx, node, clipX, clipY, clipW, clipH, renderer, paintOpts = {}) {
  const s = node.style;
  if (s.display === 'none' || s.opacity <= 0) return;

  const x = node._x, y = node._y, w = node._width, h = node._height;

  // Layer cache blit: skip subtree paint when a valid offscreen layer exists.
  if (!paintOpts.forceRepaint && node.meta?.cacheLayer && node._layerValid && node._layer) {
    try {
      ctx.drawImage(node._layer, x, y, w, h);
    } catch (e) { /* layer not ready */ }
    return;
  }
  // cull
  if (x + w < clipX || x > clipX + clipW || y + h < clipY || y > clipY + clipH) {
    // still might need to paint children that overflow? skip for perf
    return;
  }

  ctx.save();
  if (s.opacity < 1) ctx.globalAlpha *= s.opacity;

  // background and border: skip for inline text nodes (handled per-fragment in paintInlineText)
  const isInline = s.display === 'inline';
  if (!isInline) {
    if (s.background && s.background !== 'transparent') {
      ctx.fillStyle = s.background;
      roundRect(ctx, x, y, w, h, s.borderRadius || 0);
      ctx.fill();
    }
    // border (supports number or [t,r,b,l] array)
    const be = edges(s.border);
    if ((be.t > 0 || be.r > 0 || be.b > 0 || be.l > 0) && s.borderColor) {
      ctx.strokeStyle = s.borderColor;
      ctx.lineWidth = Math.max(be.t, be.r, be.b, be.l);
      strokeBorder(ctx, x, y, w, h, be, s.borderRadius || 0);
    }
  }

  // clipping for overflow
  const overflow = s.overflow;
  const clip = overflow === 'hidden' || overflow === 'scroll' || overflow === 'auto';
  if (clip) {
    ctx.beginPath();
    roundRect(ctx, x, y, w, h, s.borderRadius || 0);
    ctx.clip();
  }

  // content offset by scroll
  const sx = node._scrollX || 0;
  const sy = node._scrollY || 0;
  ctx.translate(-sx, -sy);

  // paint content based on tag
  const p = pad(s.padding);
  const innerX = x + p.l;
  const innerY = y + p.t;

  if (node.tag === 'img' && node._img && node._imgLoaded) {
    const img = node._img;
    let dw = w, dh = h;
    // fit: use node width/height as draw size
    try {
      ctx.drawImage(img, x, y, dw, dh);
    } catch (e) { /* not ready */ }
  } else if (node.tag === 'input' || node.tag === 'button') {
    paintControl(ctx, node, innerX, innerY, w - p.l - p.r, h - p.t - p.b, renderer);
  } else {
    // text content
    if (node.textContent) {
      if (isInline && node._textLines && node._textLines.length > 0) {
        paintInlineText(ctx, node);
      } else {
        paintText(ctx, node, innerX, innerY, w - p.l - p.r, renderer);
      }
    }
    // children
    for (const child of node.children) {
      paint(ctx, child, clipX + sx, clipY + sy, clipW, clipH, renderer, paintOpts);
    }
  }

  ctx.restore();

  // scrollbar (drawn outside clip)
  if (clip && (s.overflow === 'scroll' || (s.overflow === 'auto' && node._maxScrollY > 0))) {
    paintScrollbar(ctx, node);
  }
}

function paintText(ctx, node, x, y, maxW, renderer) {
  const s = node.style;
  const fs = s.fontSize || 14;
  const ff = s.fontFamily || 'sans-serif';
  const fw = s.fontWeight || 'normal';
  const fst = s.fontStyle || 'normal';
  const lh = fs * (s.lineHeight || 1.4);
  ctx.font = `${fst} ${fw} ${fs}px ${ff}`;
  ctx.fillStyle = s.color;
  ctx.textBaseline = 'top';
  const text = String(node.textContent || '');
  const lines = wrapText(text, maxW, fs, ff, s.whiteSpace);
  // record line geometry for selection hit testing (layout coordinates)
  const lines2 = [];
  let offset = 0;
  for (let i = 0; i < lines.length; i++) {
    const lineText = lines[i];
    let tx = x;
    const lineW = measureText(lineText, fs, ff, fw, fst);
    if (s.textAlign === 'center') tx = x + (maxW - lineW) / 2;
    else if (s.textAlign === 'right') tx = x + (maxW - lineW);
    ctx.fillText(lineText, tx, y + i * lh);
    // text decoration
    if (s.textDecoration === 'underline') {
      ctx.fillRect(tx, y + i * lh + fs, lineW, Math.max(1, fs / 12));
    } else if (s.textDecoration === 'line-through') {
      ctx.fillRect(tx, y + i * lh + fs * 0.55, lineW, Math.max(1, fs / 12));
    }
    lines2.push({ text: lineText, x: tx, y: y + i * lh, w: lineW, fs, lh, offset });
    offset += lineText.length;
  }
  node._textLines = lines2;
}

// Paint inline text using pre-computed _textLines from layoutInlineRun.
// Draws background/border per-fragment (per line) and text at fragment positions.
// _textLines are in layout coordinates; ctx has already been translated for scroll.
function paintInlineText(ctx, node) {
  const s = node.style;
  const fs = s.fontSize || 14;
  const ff = s.fontFamily || 'sans-serif';
  const fw = s.fontWeight || 'normal';
  const fst = s.fontStyle || 'normal';
  const p = pad(s.padding);
  const lh = fs * (s.lineHeight || 1.4);
  ctx.font = `${fst} ${fw} ${fs}px ${ff}`;
  ctx.textBaseline = 'top';
  ctx.fillStyle = s.color;

  // Draw background per-fragment (using layout coords; ctx is already translated)
  if (s.background && s.background !== 'transparent') {
    ctx.fillStyle = s.background;
    for (const ln of node._textLines) {
      roundRect(ctx, ln.x - p.l, ln.y - p.t, ln.w + p.l + p.r, lh, s.borderRadius || 0);
      ctx.fill();
    }
    ctx.fillStyle = s.color;
  }

  // Draw text and decorations per-fragment
  for (const ln of node._textLines) {
    if (ln.text) {
      ctx.fillText(ln.text, ln.x, ln.y);
    }
    if (s.textDecoration === 'underline') {
      ctx.fillRect(ln.x, ln.y + fs, ln.w, Math.max(1, fs / 12));
    } else if (s.textDecoration === 'line-through') {
      ctx.fillRect(ln.x, ln.y + fs * 0.55, ln.w, Math.max(1, fs / 12));
    }
  }
}

function paintControl(ctx, node, x, y, w, h, renderer) {
  const s = node.style;
  const fs = s.fontSize || 14;
  const ff = s.fontFamily || 'sans-serif';
  ctx.font = `${s.fontWeight || 'normal'} ${fs}px ${ff}`;
  ctx.textBaseline = 'middle';

  let bg = s.background;
  let color = s.color;
  if (node.tag === 'button') {
    if (node._active) bg = shade(bg, -15);
    else if (node._hover) bg = shade(bg, 10);
  }
  if (node.tag === 'input' && node._focused) {
    // focus ring
    ctx.strokeStyle = '#4a90d9';
    ctx.lineWidth = 2;
    roundRect(ctx, x - 1, y - 1, w + 2, h + 2, (s.borderRadius || 0) + 2);
    ctx.stroke();
  }

  if (bg && bg !== 'transparent') {
    ctx.fillStyle = bg;
    roundRect(ctx, x, y, w, h, s.borderRadius || 0);
    ctx.fill();
  }
  const be2 = edges(s.border);
  if (be2.t > 0 || be2.r > 0 || be2.b > 0 || be2.l > 0) {
    ctx.strokeStyle = s.borderColor;
    ctx.lineWidth = Math.max(be2.t, be2.r, be2.b, be2.l);
    strokeBorder(ctx, x, y, w, h, be2, s.borderRadius || 0);
  }

  const text = node.tag === 'input' ? (node.style.value || '') : (node.textContent || node.style.value || '');
  const displayText = String(text);
  ctx.fillStyle = color;

  if (node.tag === 'input') {
    // text with cursor + placeholder
    if (!displayText && s.placeholder) {
      ctx.fillStyle = '#999';
      ctx.fillText(s.placeholder, x + 6, y + h / 2);
    } else {
      ctx.save();
      ctx.beginPath();
      ctx.rect(x + 4, y, w - 8, h);
      ctx.clip();
      ctx.fillText(displayText, x + 6, y + h / 2);
      // caret
      if (node._focused && renderer._caretOn) {
        const caretX = x + 6 + ctx.measureText(displayText).width;
        ctx.fillStyle = color;
        ctx.fillRect(caretX, y + 4, 1, h - 8);
      }
      ctx.restore();
    }
  } else {
    // button: center text
    const tw = ctx.measureText(displayText).width;
    ctx.fillText(displayText, x + (w - tw) / 2, y + h / 2);
  }
}

function paintScrollbar(ctx, node) {
  if (node._maxScrollY <= 0) return;
  const x = node._x, y = node._y, w = node._width, h = node._height;
  const trackW = 6;
  const trackX = x + w - trackW - 2;
  const visibleRatio = h / node._contentHeight;
  const thumbH = Math.max(20, h * visibleRatio);
  const scrollRange = h - thumbH;
  const thumbY = y + (node._scrollY / node._maxScrollY) * scrollRange;
  ctx.fillStyle = 'rgba(0,0,0,0.15)';
  roundRect(ctx, trackX, y, trackW, h, 3);
  ctx.fill();
  ctx.fillStyle = 'rgba(0,0,0,0.45)';
  roundRect(ctx, trackX, thumbY, trackW, thumbH, 3);
  ctx.fill();
}

function shade(color, percent) {
  if (!color || color === 'transparent') return color;
  // simple hex shade
  const m = color.match(/^#([0-9a-f]{6})$/i);
  if (!m) return color;
  let r = parseInt(m[1].slice(0, 2), 16);
  let g = parseInt(m[1].slice(2, 4), 16);
  let b = parseInt(m[1].slice(4, 6), 16);
  const f = percent / 100;
  r = Math.max(0, Math.min(255, Math.round(r + 255 * f)));
  g = Math.max(0, Math.min(255, Math.round(g + 255 * f)));
  b = Math.max(0, Math.min(255, Math.round(b + 255 * f)));
  return `#${r.toString(16).padStart(2, '0')}${g.toString(16).padStart(2, '0')}${b.toString(16).padStart(2, '0')}`;
}

// ---- text selection support ----

// Compute accumulated scroll offset from all scrollable ancestors of a node.
// _textLines store layout (pre-scroll) coordinates; subtracting this offset
// converts them to screen coordinates for hit testing and selection painting.
function getScrollOffset(node) {
  let sx = 0, sy = 0;
  let p = node.parent;
  while (p) {
    sx += p._scrollX || 0;
    sy += p._scrollY || 0;
    p = p.parent;
  }
  return { sx, sy };
}

// Collect all visible text-bearing nodes in document order (DFS).
export function collectTextNodes(root, out = []) {
  if (!root) return out;
  const s = root.style;
  if (s.display === 'none' || s.opacity <= 0) return out;
  if (root.tag !== 'input' && root.tag !== 'button' && root.tag !== 'img') {
    if (root.textContent && (s.userSelect !== 'none')) {
      out.push(root);
    }
  }
  for (const c of root.children) collectTextNodes(c, out);
  return out;
}

// Given a screen point (x, y), find the closest text position {node, offset}.
// offset is a character index within node.textContent.
export function hitTextPosition(root, x, y) {
  const nodes = collectTextNodes(root);
  let best = null;
  let bestDist = Infinity;
  for (const n of nodes) {
    if (!n._textLines) continue;
    const { sx, sy } = getScrollOffset(n);
    for (const ln of n._textLines) {
      // convert layout coords to screen coords by subtracting scroll offset
      const left = ln.x - sx;
      const right = ln.x + ln.w - sx;
      const top = ln.y - sy;
      const bottom = ln.y + ln.lh - sy;
      // if point within line vertical band, compute char offset
      if (y >= top - 2 && y <= bottom + 2) {
        let cx = Math.max(left, Math.min(right, x));
        // walk characters to find offset
        const fs = ln.fs;
        const ff = n.style.fontFamily || 'sans-serif';
        const fw = n.style.fontWeight || 'normal';
        const fst = n.style.fontStyle || 'normal';
        const text = ln.text;
        // default: end of line (so clicking past the last char selects it)
        let off = ln.offset + text.length;
        for (let i = 0; i < text.length; i++) {
          const charLeft = left + (i === 0 ? 0 : measureText(text.slice(0, i), fs, ff, fw, fst));
          const charRight = left + measureText(text.slice(0, i + 1), fs, ff, fw, fst);
          if (cx < (charLeft + charRight) / 2) {
            off = ln.offset + i;
            break;
          }
        }
        const dist = Math.abs(y - (top + bottom) / 2) + Math.max(0, left - x) + Math.max(0, x - right);
        if (dist < bestDist) { bestDist = dist; best = { node: n, offset: off }; }
      }
    }
  }
  return best;
}

// Draw selection highlight for a normalized selection range.
// sel = { startNode, startOffset, endNode, endOffset } in document order.
export function paintSelection(ctx, root, sel) {
  if (!sel || !sel.startNode || !sel.endNode) return;
  const nodes = collectTextNodes(root);
  const si = nodes.indexOf(sel.startNode);
  const ei = nodes.indexOf(sel.endNode);
  if (si < 0 || ei < 0) return;
  ctx.save();
  ctx.fillStyle = 'rgba(74, 144, 217, 0.35)';
  for (let i = si; i <= ei; i++) {
    const n = nodes[i];
    if (!n._textLines) continue;
    const { sx, sy } = getScrollOffset(n);
    const startOff = i === si ? sel.startOffset : 0;
    const endOff = i === ei ? sel.endOffset : (n.textContent ? n.textContent.length : 0);
    for (const ln of n._textLines) {
      const lineStart = ln.offset;
      const lineEnd = ln.offset + ln.text.length;
      if (endOff <= lineStart || startOff >= lineEnd) continue;
      const a = Math.max(0, startOff - lineStart);
      const b = Math.min(ln.text.length, endOff - lineStart);
      if (b <= a) continue;
      const fs = ln.fs;
      const ff = n.style.fontFamily || 'sans-serif';
      const fw = n.style.fontWeight || 'normal';
      const fst = n.style.fontStyle || 'normal';
      const x0 = (ln.x - sx) + measureText(ln.text.slice(0, a), fs, ff, fw, fst);
      const x1 = (ln.x - sx) + measureText(ln.text.slice(0, b), fs, ff, fw, fst);
      ctx.fillRect(x0, ln.y - sy, Math.max(1, x1 - x0), ln.fs);
    }
  }
  ctx.restore();
}

// Compute the selected plain text for clipboard copy.
export function selectionText(root, sel) {
  if (!sel || !sel.startNode || !sel.endNode) return '';
  const nodes = collectTextNodes(root);
  const si = nodes.indexOf(sel.startNode);
  const ei = nodes.indexOf(sel.endNode);
  if (si < 0 || ei < 0) return '';
  let out = '';
  for (let i = si; i <= ei; i++) {
    const n = nodes[i];
    const startOff = i === si ? sel.startOffset : 0;
    const endOff = i === ei ? sel.endOffset : (n.textContent ? n.textContent.length : 0);
    out += String(n.textContent || '').slice(startOff, endOff);
    if (i < ei) out += '\n';
  }
  return out;
}
