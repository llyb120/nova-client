// interaction.js - hit testing + event dispatch + scroll inertia.

import { Node } from './Node.js';

// Find topmost node at screen coords (x, y), respecting overflow clip and scroll.
export function hitTest(node, x, y) {
  // check self bounds
  if (x < node._x || x > node._x + node._width || y < node._y || y > node._y + node._height) {
    return null;
  }
  // scroll offset applied to children
  const sx = node._scrollX || 0;
  const sy = node._scrollY || 0;
  const s = node.style;
  const overflow = s.overflow;
  const clip = overflow === 'hidden' || overflow === 'scroll' || overflow === 'auto';

  // iterate children in reverse (topmost last drawn = last in array, but we draw in order so last is on top)
  for (let i = node.children.length - 1; i >= 0; i--) {
    const child = node.children[i];
    if (clip) {
      // child must be within content box accounting scroll
      const cx = child._x - sx;
      const cy = child._y - sy;
      // approximate: only hit if child intersects visible area
      if (cx + child._width < node._x || cx > node._x + node._width ||
          cy + child._height < node._y || cy > node._y + node._height) {
        continue;
      }
    }
    const hit = hitTest(child, x + sx, y + sy);
    if (hit) return hit;
  }
  // self is hit
  return node;
}

// Find nearest scrollable ancestor for wheel/drag scrolling.
export function findScrollable(node, dx, dy) {
  let p = node;
  while (p) {
    const s = p.style.overflow;
    if (s === 'scroll' || s === 'auto') {
      if (dy > 0 && p._maxScrollY > 0) return p;
      if (dy < 0 && p._scrollY > 0) return p;
      if (dx > 0 && p._maxScrollX > 0) return p;
      if (dx < 0 && p._scrollX > 0) return p;
      // if can't scroll this direction, try ancestor
    }
    p = p.parent;
  }
  return null;
}

export function scrollBy(node, dx, dy) {
  if (!node) return false;
  const beforeX = node._scrollX, beforeY = node._scrollY;
  node._scrollX = Math.max(0, Math.min(node._maxScrollX, node._scrollX + dx));
  node._scrollY = Math.max(0, Math.min(node._maxScrollY, node._scrollY + dy));
  return node._scrollX !== beforeX || node._scrollY !== beforeY;
}
