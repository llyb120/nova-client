// interaction.ts — 命中测试 + 滚动路由（移植自参考项目 interaction.js，扩展 sticky 与
// overscroll-behavior 语义）。

import { Node } from "./node.js";

/** 视口坐标命中：返回最深层节点（子节点后添加的在上面）。
 * inline 文本节点按片段（_textLines）命中而非包围盒——跨行片段的包围盒会互相覆盖。 */
export function hitTest(node: Node, x: number, y: number): Node | null {
  const dy = node._stickyDy || 0;
  const nx = node._x;
  const ny = node._y + dy;
  if (x < nx || x > nx + node._width || y < ny || y > ny + node._height) return null;
  if (node.style.display === "none") return null;

  const sx = node._scrollX || 0;
  const sy = node._scrollY || 0;
  const o = node.style.overflow;
  const clip = o === "hidden" || o === "scroll" || o === "auto";

  for (let i = node.children.length - 1; i >= 0; i--) {
    const child = node.children[i];
    if (clip) {
      // 粗剔除：子节点需在可视区内（计入滚动）
      const cx = child._x - sx;
      const cy = child._y - sy + (child._stickyDy || 0);
      if (
        cx + child._width < nx ||
        cx > nx + node._width ||
        cy + child._height < ny ||
        cy > ny + node._height
      ) {
        continue;
      }
    }
    // inline 文本子节点：按片段矩形命中
    if (child.style.display === "inline" && child._textLines) {
      const lx = x + sx;
      const ly = y + sy;
      let fragHit = false;
      for (const ln of child._textLines) {
        if (lx >= ln.x && lx <= ln.x + ln.w && ly >= ln.y && ly <= ln.y + ln.lh) {
          fragHit = true;
          break;
        }
      }
      if (!fragHit) continue;
      return child;
    }
    const hit = hitTest(child, x + sx, y + sy);
    if (hit) return hit;
  }
  return node;
}

export interface ScrollRoute {
  /** 实际滚动目标（null 且 blocked=true 表示被 overscroll-behavior 吞掉） */
  target: Node | null;
  blocked: boolean;
}

/** 向上找最近能在该方向滚动的祖先；途经 overscroll-behavior: contain/none 即吞掉 */
export function routeScroll(node: Node, dx: number, dy: number): ScrollRoute {
  let p: Node | null = node;
  while (p) {
    const s = p.style;
    if (s.overflow === "scroll" || s.overflow === "auto") {
      const canV = (dy > 0 && p._scrollY < p._maxScrollY) || (dy < 0 && p._scrollY > 0);
      const canH = (dx > 0 && p._scrollX < p._maxScrollX) || (dx < 0 && p._scrollX > 0);
      if ((dy !== 0 && canV) || (dx !== 0 && canH)) return { target: p, blocked: false };
      if (s.overscrollBehavior !== "auto") return { target: null, blocked: true };
    }
    p = p.parent;
  }
  return { target: null, blocked: false };
}

export function scrollBy(node: Node, dx: number, dy: number): boolean {
  const bx = node._scrollX;
  const by = node._scrollY;
  node._scrollX = Math.max(0, Math.min(node._maxScrollX, node._scrollX + dx));
  node._scrollY = Math.max(0, Math.min(node._maxScrollY, node._scrollY + dy));
  return node._scrollX !== bx || node._scrollY !== by;
}
