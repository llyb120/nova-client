// painter.js - draws the node tree onto a canvas 2d context.

import { wrapText, measureText } from './layout.js';

/** 把 CSS 像素坐标落到设备像素网格，避免半 leading / 分数 dpr 让 fillText 发糊。 */
function snapPx(v, dpr) {
  const s = dpr > 0 ? dpr : 1;
  return Math.round(v * s) / s;
}

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

// Resolve border width per edge. Accepts a number or an array like [t,r,b,l].
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

// Stroke each border edge independently (supports per-side widths).
function strokeBorder(ctx, x, y, w, h, e, radius) {
  const uniform = e.t > 0 && e.t === e.r && e.t === e.b && e.t === e.l;
  if (uniform) {
    const inset = e.t / 2;
    ctx.lineWidth = e.t;
    const insetRadius = Array.isArray(radius)
      ? radius.map(value => Math.max(0, (Number(value) || 0) - inset))
      : Math.max(0, (radius || 0) - inset);
    roundRect(ctx, x + inset, y + inset, Math.max(0, w - e.t), Math.max(0, h - e.t), insetRadius);
    ctx.stroke();
    return;
  }
  const r = Math.max(0, Math.min(radius || 0, w / 2, h / 2));
  ctx.beginPath();
  if (e.t > 0) { ctx.moveTo(x + r, y + e.t / 2); ctx.lineTo(x + w - r, y + e.t / 2); }
  if (e.r > 0) { ctx.moveTo(x + w - e.r / 2, y + r); ctx.lineTo(x + w - e.r / 2, y + h - r); }
  if (e.b > 0) { ctx.moveTo(x + r, y + h - e.b / 2); ctx.lineTo(x + w - r, y + h - e.b / 2); }
  if (e.l > 0) { ctx.moveTo(x + e.l / 2, y + r); ctx.lineTo(x + e.l / 2, y + h - r); }
  ctx.stroke();
}

function roundRect(ctx, x, y, w, h, radius) {
  const source = Array.isArray(radius) ? radius : [radius || 0, radius || 0, radius || 0, radius || 0];
  const limit = Math.max(0, Math.min(w / 2, h / 2));
  const [tl, tr, br, bl] = source.map(value => Math.max(0, Math.min(Number(value) || 0, limit)));
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

function isUnder(node, ancestor) {
  let current = node;
  while (current) {
    if (current === ancestor) return true;
    current = current.parent;
  }
  return false;
}

export function findEditHoverRoot(node) {
  let current = node;
  while (current) {
    if (current._editHoverRoot) return current;
    current = current.parent;
  }
  return null;
}

/** Hover 是否会产生可见变化；用于跳过 Markdown 文本上的无效全量重绘。 */
export function hoverPaintTarget(node) {
  if (!node) return null;
  const editRoot = findEditHoverRoot(node);
  let interactive = null;
  let current = node;
  while (current) {
    const s = current.style;
    if (current.tag === 'button' || current.tag === 'input' || current.tag === 'a'
        || s.hoverBackground || s.hoverOpacity != null || s.hoverDecoration || s.hoverColor) {
      interactive = current;
      break;
    }
    current = current.parent;
  }
  const dirty = editRoot || interactive;
  if (!dirty) return null;
  return {
    key: `${editRoot ? editRoot.id : 0}:${interactive ? interactive.id : 0}`,
    dirty,
  };
}

export function nodeScreenBounds(node) {
  if (!node) return null;
  const { sx, sy } = getScrollOffset(node);
  return {
    x: node._x - sx,
    y: node._y - sy,
    w: node._width,
    h: node._height,
  };
}

export function findNodeTitle(node) {
  let current = node;
  while (current) {
    if (current.title) return String(current.title);
    current = current.parent;
  }
  return '';
}

export function treeHasBusyStatus(node) {
  if (node?.style?.trailingStatus === 'busy') return true;
  if (!node?.children) return false;
  for (const child of node.children) {
    if (treeHasBusyStatus(child)) return true;
  }
  return false;
}

/** 仅扫描视口附近的节点，避免 busy 时每帧 DFS 整棵历史树。 */
export function treeHasBusyStatusVisible(node, viewTop, viewBottom) {
  if (!node) return false;
  if (node.style?.display === 'none') return false;
  const y = node._y;
  const h = node._height;
  // 尚未布局或尺寸未知时保守继续检查。
  if (h > 0 && (y + h < viewTop || y > viewBottom)) return false;
  if (node.style?.trailingStatus === 'busy') return true;
  for (const child of node.children) {
    if (treeHasBusyStatusVisible(child, viewTop, viewBottom)) return true;
  }
  return false;
}

/** 收集视口附近 busy 控件，供转圈时脏矩形重绘。 */
export function collectBusyStatusVisible(node, viewTop, viewBottom, out = []) {
  if (!node) return out;
  if (node.style?.display === 'none') return out;
  const y = node._y;
  const h = node._height;
  if (h > 0 && (y + h < viewTop || y > viewBottom)) return out;
  if (node.style?.trailingStatus === 'busy') out.push(node);
  for (const child of node.children) collectBusyStatusVisible(child, viewTop, viewBottom, out);
  return out;
}

// Paint a node. clipX/clipY/clipW/clipH define the visible clip rect (already in screen coords).
export function paint(ctx, node, clipX, clipY, clipW, clipH, renderer) {
  const s = node.style;
  if (s.display === 'none') return;

  // Effective opacity: hoverOpacity can reveal nodes with opacity:0 (e.g. edit button).
  let opacity = s.opacity;
  if (s.hoverOpacity != null) {
    const hoverRoot = findEditHoverRoot(node);
    const hovered = node._hover
      || (renderer?._hoverNode && hoverRoot && isUnder(renderer._hoverNode, hoverRoot));
    opacity = hovered ? s.hoverOpacity : s.opacity;
  }
  if (opacity <= 0) return;

  const x = node._x, y = node._y, w = node._width, h = node._height;
  // cull
  if (x + w < clipX || x > clipX + clipW || y + h < clipY || y > clipY + clipH) {
    // still might need to paint children that overflow? skip for perf
    return;
  }

  ctx.save();
  if (opacity < 1) ctx.globalAlpha *= opacity;

  // background and border: inline 由片段绘制，控件由 paintControl 处理 hover/focus 状态。
  const isInline = s.display === 'inline';
  const isControl = node.tag === 'input' || node.tag === 'button';
  if (!isInline && !isControl) {
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
  const be = edges(s.border);
  const innerX = x + be.l + p.l;
  const innerY = y + be.t + p.t;

  if (node.tag === 'img' && node._img && node._imgLoaded) {
    const img = node._img;
    let dw = w, dh = h;
    // fit: use node width/height as draw size
    try {
      ctx.drawImage(img, x, y, dw, dh);
    } catch (e) { /* not ready */ }
  } else if (node.tag === 'input' || node.tag === 'button') {
    paintControl(ctx, node, x, y, w, h, renderer);
  } else {
    // text content
    if (node.textContent) {
      if (isInline && node._textLines && node._textLines.length > 0) {
        paintInlineText(ctx, node, renderer);
      } else {
        paintText(ctx, node, innerX, innerY, w - be.l - be.r - p.l - p.r, renderer);
      }
    }
    // children
    // overflow 节点把子树裁剪窗口收窄到自身可见内容带，避免长工具输出/diff 按整画布误判可见。
    let childClipX = clipX + sx;
    let childClipY = clipY + sy;
    let childClipW = clipW;
    let childClipH = clipH;
    if (clip) {
      const vx = x + sx;
      const vy = y + sy;
      const x0 = Math.max(childClipX, vx);
      const y0 = Math.max(childClipY, vy);
      const x1 = Math.min(childClipX + childClipW, vx + w);
      const y1 = Math.min(childClipY + childClipH, vy + h);
      childClipX = x0;
      childClipY = y0;
      childClipW = Math.max(0, x1 - x0);
      childClipH = Math.max(0, y1 - y0);
    }
    for (const child of node.children) {
      paint(ctx, child, childClipX, childClipY, childClipW, childClipH, renderer);
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
  const lines = node._wrappedLines || wrapText(text, maxW, fs, ff, s.whiteSpace, fw, fst);
  // CSS line-height 把半 leading 分到字形上下；canvas top baseline 若不补偿，
  // 多余行距会全部堆在底部，看起来像多了一截 padding-bottom。
  const halfLeading = Math.max(0, (lh - fs) / 2);
  // 只提交视口内的 fillText；完整行几何仍保留给选择和复制。
  // 祖先 scroll + 自身 scroll 都要计入，否则 overflow:auto 工具输出滚到底后行被误裁成空白。
  const lines2 = [];
  const ownSy = node._scrollY || 0;
  const totalSy = getScrollOffset(node).sy + ownSy;
  const canvasH = renderer?.height || Infinity;
  const localTop = y + ownSy;
  const localBottom = localTop + (node._height || canvasH);
  const visibleTop = Math.max(totalSy, localTop) - lh;
  const visibleBottom = Math.min(totalSy + canvasH, localBottom) + lh;
  let offset = 0;
  for (let i = 0; i < lines.length; i++) {
    const lineText = lines[i];
    let tx = x;
    const lineY = y + i * lh;
    const textY = lineY + halfLeading;
    const lineW = measureText(lineText, fs, ff, fw, fst);
    if (s.textAlign === 'center') tx = x + (maxW - lineW) / 2;
    else if (s.textAlign === 'right') tx = x + (maxW - lineW);
    const visible = lineY >= visibleTop && lineY <= visibleBottom;
    const dpr = renderer?.dpr || 1;
    const sx = snapPx(tx, dpr);
    const sy = snapPx(textY, dpr);
    if (visible) ctx.fillText(lineText, sx, sy);
    if (visible && s.textDecoration === 'underline') {
      ctx.fillRect(sx, sy + fs, lineW, Math.max(1, fs / 12));
    } else if (visible && s.textDecoration === 'line-through') {
      ctx.fillRect(sx, sy + fs * 0.55, lineW, Math.max(1, fs / 12));
    }
    lines2.push({ text: lineText, x: tx, y: lineY, w: lineW, fs, lh, offset });
    offset += lineText.length;
  }
  node._textLines = lines2;
}

// Paint inline text using pre-computed _textLines from layoutInlineRun.
// Draws background/border per-fragment (per line) and text at fragment positions.
// _textLines are in layout coordinates; ctx has already been translated for scroll.
function paintInlineText(ctx, node, renderer) {
  const s = node.style;
  const fs = s.fontSize || 14;
  const ff = s.fontFamily || 'sans-serif';
  const fw = s.fontWeight || 'normal';
  const fst = s.fontStyle || 'normal';
  const p = pad(s.padding);
  const lh = fs * (s.lineHeight || 1.4);
  const dpr = renderer?.dpr || 1;
  ctx.font = `${fst} ${fw} ${fs}px ${ff}`;
  ctx.textBaseline = 'top';
  ctx.fillStyle = s.color;

  // Draw background and border per fragment (inline code uses both).
  const be = edges(s.border);
  for (const ln of node._textLines) {
    const bx = ln.x - p.l - be.l;
    const by = ln.y - p.t - be.t;
    const bw = ln.w + p.l + p.r + be.l + be.r;
    const bh = lh + be.t + be.b;
    if (s.background && s.background !== 'transparent') {
      ctx.fillStyle = s.background;
      roundRect(ctx, bx, by, bw, bh, s.borderRadius || 0);
      ctx.fill();
    }
    if ((be.t || be.r || be.b || be.l) && s.borderColor) {
      ctx.strokeStyle = s.borderColor;
      strokeBorder(ctx, bx, by, bw, bh, be, s.borderRadius || 0);
    }
  }
  ctx.fillStyle = s.color;

  // Draw text and decorations per-fragment（半 leading，与 paintText / CSS 对齐）
  const halfLeading = Math.max(0, (lh - fs) / 2);
  for (const ln of node._textLines) {
    const textY = ln.y + halfLeading;
    const sx = snapPx(ln.x, dpr);
    const sy = snapPx(textY, dpr);
    if (ln.text) {
      ctx.fillText(ln.text, sx, sy);
    }
    if (s.textDecoration === 'underline') {
      ctx.fillRect(sx, sy + fs, ln.w, Math.max(1, fs / 12));
    } else if (s.textDecoration === 'line-through') {
      ctx.fillRect(sx, sy + fs * 0.55, ln.w, Math.max(1, fs / 12));
    }
  }
}

function paintToolIcon(ctx, kind, x, y, size, color) {
  const s = size / 24;
  ctx.save();
  ctx.translate(x, y);
  ctx.scale(s, s);
  ctx.strokeStyle = color;
  ctx.lineWidth = 2;
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';
  ctx.beginPath();
  if (kind === 'execute') {
    ctx.moveTo(4, 6); ctx.lineTo(10, 12); ctx.lineTo(4, 18);
    ctx.moveTo(12, 19); ctx.lineTo(20, 19);
  } else if (kind === 'read') {
    ctx.moveTo(6, 2); ctx.lineTo(15, 2); ctx.lineTo(20, 7); ctx.lineTo(20, 22);
    ctx.lineTo(6, 22); ctx.closePath(); ctx.moveTo(14, 2); ctx.lineTo(14, 8); ctx.lineTo(20, 8);
  } else if (kind === 'edit') {
    ctx.moveTo(3, 21); ctx.lineTo(7, 20); ctx.lineTo(20, 7);
    ctx.quadraticCurveTo(22, 5, 19, 3); ctx.quadraticCurveTo(17, 1, 15, 3);
    ctx.lineTo(3, 17); ctx.closePath(); ctx.moveTo(14, 4); ctx.lineTo(20, 10);
  } else if (kind === 'undo') {
    ctx.moveTo(9, 8); ctx.lineTo(4, 12); ctx.lineTo(9, 16);
    ctx.moveTo(5, 12); ctx.lineTo(14, 12);
    ctx.bezierCurveTo(18, 12, 20, 15, 20, 19);
  } else if (kind === 'search') {
    ctx.arc(11, 11, 7, 0, Math.PI * 2); ctx.moveTo(16, 16); ctx.lineTo(22, 22);
  } else if (kind === 'fetch') {
    ctx.arc(12, 12, 10, 0, Math.PI * 2); ctx.moveTo(2, 12); ctx.lineTo(22, 12);
    ctx.moveTo(12, 2); ctx.bezierCurveTo(7, 7, 7, 17, 12, 22);
    ctx.moveTo(12, 2); ctx.bezierCurveTo(17, 7, 17, 17, 12, 22);
  } else if (kind === 'think') {
    ctx.arc(9, 10, 5, Math.PI * .55, Math.PI * 1.95);
    ctx.arc(15, 10, 5, Math.PI * 1.05, Math.PI * 2.45);
    ctx.moveTo(7, 14); ctx.lineTo(7, 19); ctx.lineTo(17, 19); ctx.lineTo(17, 14);
  } else {
    ctx.moveTo(14, 6); ctx.arc(12, 9, 5, -1.1, 2.2); ctx.lineTo(4, 17);
    ctx.quadraticCurveTo(2, 19, 5, 22); ctx.quadraticCurveTo(7, 24, 9, 21); ctx.lineTo(16, 14);
  }
  ctx.stroke();
  ctx.restore();
}

function paintChevron(ctx, open, x, y, color) {
  ctx.save();
  ctx.strokeStyle = color;
  ctx.lineWidth = 1;
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';
  ctx.beginPath();
  if (open) { ctx.moveTo(x - 3, y - 1.5); ctx.lineTo(x, y + 1.5); ctx.lineTo(x + 3, y - 1.5); }
  else { ctx.moveTo(x - 1.5, y - 3); ctx.lineTo(x + 1.5, y); ctx.lineTo(x - 1.5, y + 3); }
  ctx.stroke();
  ctx.restore();
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
    if (node._active) bg = s.activeBackground || s.hoverBackground || shade(bg, -15);
    else if (node._hover) {
      bg = s.hoverBackground || shade(bg, 10);
      if (s.hoverColor) color = s.hoverColor;
    }
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
  const padding = pad(s.padding);
  const border = edges(s.border);
  const leadingSpace = node.tag === 'button'
    ? (s.leadingIcon ? 22 : 0) + (s.leadingChevron != null ? 17 : 0)
    : 0;
  const statsText = s.trailingStats ? `+${s.trailingStats.add} -${s.trailingStats.del}` : '';
  const statsSpace = statsText
    ? measureText(statsText, 11.5, ff, 'normal', 'normal') + 14 : 0;
  const trailingSpace = node.tag === 'button'
    ? (s.trailingChevron != null ? 20 : 0) + (s.trailingStatus ? 20 : 0) + statsSpace : 0;
  const textX = x + border.l + padding.l + leadingSpace;
  const textW = Math.max(0,
    w - border.l - border.r - padding.l - padding.r - leadingSpace - trailingSpace);
  ctx.fillStyle = color;
  if (node.tag === 'button' && s.leadingIcon) {
    paintToolIcon(ctx, s.leadingIcon, x + border.l + padding.l,
      y + (h - 14) / 2, 14, s.iconColor || color);
  }
  if (node.tag === 'button' && s.leadingChevron != null) {
    const offset = s.leadingIcon ? 27 : 7;
    paintChevron(ctx, !!s.leadingChevron,
      x + border.l + padding.l + offset, y + h / 2, s.chevronColor || color);
  }
  let trailingX = x + w - border.r - padding.r;
  if (node.tag === 'button' && s.trailingInline) {
    const textWidth = Math.min(ctx.measureText(displayText).width, textW);
    let inlineSpace = 0;
    if (s.trailingStats) {
      const add = `+${s.trailingStats.add}`;
      const del = `-${s.trailingStats.del}`;
      inlineSpace += 8 + measureText(add, 11.5, ff, 'normal', 'normal')
        + 6 + measureText(del, 11.5, ff, 'normal', 'normal');
    }
    if (s.trailingStatus) inlineSpace += 20;
    if (s.trailingChevron != null) inlineSpace += 20;
    trailingX = textX + textWidth + inlineSpace;
  }
  if (node.tag === 'button' && s.trailingChevron != null) {
    paintChevron(ctx, !!s.trailingChevron, trailingX - 6, y + h / 2, s.chevronColor || color);
    trailingX -= 20;
  }
  if (node.tag === 'button' && s.trailingStatus) {
    const cx = trailingX - 6;
    ctx.save();
    ctx.strokeStyle = s.statusColor || color;
    ctx.fillStyle = s.statusColor || color;
    if (s.trailingStatus === 'failed') {
      ctx.beginPath(); ctx.arc(cx, y + h / 2, 3.5, 0, Math.PI * 2); ctx.fill();
    } else {
      // 与 .spinner 一致：12px 环、2px 线宽、持续旋转。
      const angle = ((renderer?._spinPhase || 0) % 1) * Math.PI * 2;
      ctx.globalAlpha = 0.26;
      ctx.lineWidth = 2;
      ctx.beginPath(); ctx.arc(cx, y + h / 2, 5, 0, Math.PI * 2); ctx.stroke();
      ctx.globalAlpha = 1;
      ctx.beginPath(); ctx.arc(cx, y + h / 2, 5, angle - Math.PI / 2, angle + Math.PI * .15); ctx.stroke();
    }
    ctx.restore();
    trailingX -= 20;
  }
  if (node.tag === 'button' && s.trailingStats) {
    ctx.save();
    ctx.font = `normal 11.5px ${ff}`;
    ctx.textBaseline = 'middle';
    const del = `-${s.trailingStats.del}`;
    const add = `+${s.trailingStats.add}`;
    const delW = ctx.measureText(del).width;
    const addW = ctx.measureText(add).width;
    ctx.fillStyle = s.trailingStats.delColor;
    ctx.fillText(del, trailingX - delW, y + h / 2);
    ctx.fillStyle = s.trailingStats.addColor;
    ctx.fillText(add, trailingX - delW - 6 - addW, y + h / 2);
    ctx.restore();
    trailingX -= statsSpace;
  }

  if (node.tag === 'input') {
    // text with cursor + placeholder
    if (!displayText && s.placeholder) {
      ctx.fillStyle = '#999';
      ctx.fillText(s.placeholder, textX + 6, y + h / 2);
    } else {
      ctx.save();
      ctx.beginPath();
      ctx.rect(textX + 4, y, Math.max(0, textW - 8), h);
      ctx.clip();
      ctx.fillText(displayText, textX + 6, y + h / 2);
      // caret
      if (node._focused && renderer._caretOn) {
        const caretX = textX + 6 + ctx.measureText(displayText).width;
        ctx.fillStyle = color;
        ctx.fillRect(caretX, y + 4, 1, h - 8);
      }
      ctx.restore();
    }
  } else {
    let lines = node._controlLines?.length ? node._controlLines : [displayText];
    if (s.whiteSpace === 'nowrap' && s.textOverflow === 'ellipsis'
        && ctx.measureText(lines[0]).width > textW) {
      let value = lines[0];
      while (value.length && ctx.measureText(`${value}…`).width > textW) value = value.slice(0, -1);
      lines = [`${value}…`];
    }
    const lh = fs * (s.lineHeight || 1.2);
    const startY = y + (h - lines.length * lh) / 2 + lh / 2;
    for (let index = 0; index < lines.length; index++) {
      const line = lines[index];
      const tw = ctx.measureText(line).width;
      const tx = s.textAlign === 'left' ? textX
        : s.textAlign === 'right' ? textX + textW - tw : textX + (textW - tw) / 2;
      ctx.fillText(line, tx, startY + index * lh);
    }
  }
}

function paintScrollbar(ctx, node) {
  if (node._maxScrollY <= 0) return;
  const x = node._x, y = node._y, w = node._width, h = node._height;
  const trackW = node.style.scrollbarWidth || 4;
  const trackX = x + w - trackW - 3;
  const visibleRatio = h / node._contentHeight;
  const thumbH = Math.max(20, h * visibleRatio);
  const scrollRange = h - thumbH;
  const thumbY = y + (node._scrollY / node._maxScrollY) * scrollRange;
  ctx.fillStyle = node.style.scrollbarTrack || 'rgba(127,127,127,0.12)';
  roundRect(ctx, trackX, y, trackW, h, 3);
  ctx.fill();
  ctx.fillStyle = node.style.scrollbarThumb || 'rgba(127,127,127,0.58)';
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
export function getScrollOffset(node) {
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
      const halfLeading = Math.max(0, ((ln.lh || fs) - fs) / 2);
      ctx.fillRect(x0, ln.y - sy + halfLeading, Math.max(1, x1 - x0), ln.fs);
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
