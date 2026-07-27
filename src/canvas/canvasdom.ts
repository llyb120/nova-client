// canvasdom.ts — 渲染器主循环与事件层（移植自参考项目 CanvasDOM.js，事件/滚动按
// 会话需求重写）：
// - rAF 脏标记渲染 + DPR 适配 + 视口剔除；spinner 在场时持续驱动帧
// - 外层滚动由渲染器自管（scrollTop + 内容高），右侧 10px 滚动条（经典占位，与
//   WebView2 ::-webkit-scrollbar 行为一致：内容出现时内容区收窄 10px）
// - 原生滚动语义：wheel（含 shift 横向 / deltaMode）、方向键 40px、PageUp/Down
//   0.85 屏、Space/Shift+Space、Home/End、滚动条拖拽；无惯性、无内容拖拽滚动
// - 文字选择：拖选（跨节点）+ Ctrl/Cmd+C/copy 事件 + 边缘自动滚动
// - hover 上报（tooltip DOM 浮层）、onClick/onContextMenu 沿命中链分发

import { hitTest, routeScroll, scrollBy } from "./interaction.js";
import { layout, resetMeasureCaches } from "./layout.js";
import { Node, type CanvasHitEvent } from "./node.js";
import {
  collectTextNodes,
  hitTextPosition,
  paint,
  paintSelection,
  scrollbarHit,
  scrollOffsetOf,
  selectionText,
  type Selection,
  type TextPos,
} from "./painter.js";
import { onFontsLoadingDone, onFontsReady, onThemeChange, theme, watchTheme } from "./theme.js";

export interface RendererHooks {
  /** 外层 scrollTop 或内层滚动变化后回调（吸底状态机 / 覆盖层重定位用） */
  onScrollChange?: () => void;
  /** 每帧布局完成后回调（此刻 contentHeight / 节点几何才有效） */
  onAfterLayout?: () => void;
  /** hover 到带 style.title 的节点（已等待 600ms）；null = 隐藏 tooltip */
  onHoverTitle?: (node: Node | null, x: number, y: number) => void;
  /** 选区变化 */
  onSelectionChange?: (hasSelection: boolean) => void;
}

const ARROW_SCROLL = 40;
const PAGE_RATIO = 0.85;
const TOOLTIP_DELAY = 600;

export class CanvasRenderer {
  readonly canvas: HTMLCanvasElement;
  private ctx: CanvasRenderingContext2D;
  private root: Node | null = null;
  private width = 0;
  private height = 0;
  private dpr = 1;
  private gutterApplied = false;

  private rafId = 0;
  private dirty = true;
  private animating = false;

  scrollTop = 0;

  private hoverNode: Node | null = null;
  private pressedNode: Node | null = null;
  private selection: Selection | null = null;
  private selecting = false;
  private anchor: TextPos | null = null;
  private selectPointer = { x: 0, y: 0 };

  private scrollDrag: {
    node: Node | null; // null = 外层滚动
    axis: "v" | "h";
    grabOffset: number;
  } | null = null;

  private tooltipNode: Node | null = null;
  private tooltipTimer: ReturnType<typeof setTimeout> | undefined;
  private lastPointer = { x: -1, y: -1 };

  private removeThemeListener: (() => void) | undefined;
  private removeFontLoadListener: (() => void) | undefined;
  private disposed = false;

  constructor(canvas: HTMLCanvasElement, private hooks: RendererHooks = {}) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d", { alpha: true })!;
    this.dpr = typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1;
    this.bindEvents();
    watchTheme();
    this.removeThemeListener = onThemeChange(() => {
      resetMeasureCaches();
      this.markAllDirty();
    });
    onFontsReady(() => {
      resetMeasureCaches();
      this.markAllDirty();
    });
    // 后到字体加载完成：后备字体的测量缓存全部作废并重排（行内代码背景错位根因）
    this.removeFontLoadListener = onFontsLoadingDone(() => {
      resetMeasureCaches();
      this.markAllDirty();
    });
  }

  // ---------- 对外 API ----------

  setRoot(node: Node | null): void {
    this.root = node;
    this.dirty = true;
    this.requestRender();
  }

  resize(w: number, h: number): void {
    if (w === this.width && h === this.height) return;
    this.width = w;
    this.height = h;
    this.dpr = window.devicePixelRatio || 1;
    this.canvas.width = Math.max(1, Math.round(w * this.dpr));
    this.canvas.height = Math.max(1, Math.round(h * this.dpr));
    this.canvas.style.width = `${w}px`;
    this.canvas.style.height = `${h}px`;
    this.ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    if (this.root) this.root.markLayoutDirty();
    this.dirty = true;
    this.requestRender();
  }

  contentHeight(): number {
    return this.root?._contentHeight ?? 0;
  }

  maxScrollTop(): number {
    return Math.max(0, this.contentHeight() - this.height);
  }

  isAtBottom(): boolean {
    return this.maxScrollTop() - this.scrollTop <= 1;
  }

  setScrollTop(v: number, emit = true): void {
    const next = Math.max(0, Math.min(this.maxScrollTop(), v));
    if (next === this.scrollTop) return;
    this.scrollTop = next;
    if (this.root) this.root._scrollY = next;
    this.dirty = true;
    this.requestRender();
    if (emit) this.hooks.onScrollChange?.();
  }

  scrollByOuter(dy: number): boolean {
    const before = this.scrollTop;
    this.setScrollTop(this.scrollTop + dy);
    return this.scrollTop !== before;
  }

  /** 滚动到指定节点（节点 content 坐标减去顶部留白，等价 scroll-margin-top） */
  scrollToNode(node: Node, marginTop = 20): void {
    this.setScrollTop(node._y - marginTop);
  }

  /** 节点 → 视口坐标矩形（覆盖层定位用） */
  nodeScreenRect(node: Node): { x: number; y: number; w: number; h: number } {
    let sx = 0;
    let sy = 0;
    let p = node.parent;
    while (p) {
      sx += p._scrollX || 0;
      sy += p._scrollY || 0;
      p = p.parent;
    }
    return {
      x: node._x - sx,
      y: node._y + (node._stickyDy || 0) - sy,
      w: node._width,
      h: node._height,
    };
  }

  getSelectionText(): string {
    return this.root && this.selection ? selectionText(this.root, this.selection) : "";
  }

  /** 走查/测试用的内部状态快照 */
  debugState(): Record<string, unknown> {
    const selMatch =
      this.selection && this.root
        ? (() => {
            const nodes = collectTextNodes(this.root!);
            const si = nodes.indexOf(this.selection!.startNode);
            const ei = nodes.indexOf(this.selection!.endNode);
            const rects: Array<Record<string, number>> = [];
            if (si >= 0 && ei >= 0) {
              for (let i = si; i <= Math.min(ei, si + 3); i++) {
                const n = nodes[i];
                if (!n._textLines) continue;
                const { sx, sy } = scrollOffsetOf(n);
                const ln = n._textLines[0];
                if (ln) {
                  rects.push({
                    tag: 0,
                    x: ln.x - sx,
                    y: ln.y - sy,
                    w: ln.w,
                    lh: ln.lh,
                    offset: ln.offset,
                    lineTextLen: ln.text.length,
                    nodeTextLen: n.textContent.length,
                  });
                }
              }
            }
            return { si, ei, total: nodes.length, rects };
          })()
        : null;
    return {
      scrollTop: this.scrollTop,
      maxScrollTop: this.maxScrollTop(),
      gutter: this.gutterApplied,
      dirty: this.dirty,
      rafPending: this.rafId !== 0,
      renderCount: this.renderCount_,
      imgCache: imageCache.size,
      imgCacheStates: [...imageCache.values()].map((e) => ({ loaded: e.loaded, failed: e.failed, waiters: e.waiters.size })),
      selecting: this.selecting,
      selection: this.selection
        ? {
            start: `${this.selection.startNode.tag}#${this.selection.startNode.id}@${this.selection.startOffset}`,
            end: `${this.selection.endNode.tag}#${this.selection.endNode.id}@${this.selection.endOffset}`,
          }
        : null,
      selMatch,
      hover: this.hoverNode ? `${this.hoverNode.tag}#${this.hoverNode.id}` : null,
    };
  }

  clearSelection(): void {
    if (!this.selection) return;
    this.selection = null;
    this.dirty = true;
    this.requestRender();
    this.hooks.onSelectionChange?.(false);
  }

  markDirty(): void {
    this.dirty = true;
    this.requestRender();
  }

  markAllDirty(): void {
    if (this.root) {
      for (const n of this.root.walk()) n._layoutDirty = true;
    }
    this.dirty = true;
    this.requestRender();
  }

  // ---------- 主循环 ----------

  private requestRender(): void {
    if (this.rafId || this.disposed) return;
    this.rafId = requestAnimationFrame(this.tick);
  }

  private tick = (): void => {
    this.rafId = 0;
    if (this.disposed) return;
    // 选区边缘自动滚动
    if (this.selecting) {
      const M = 28;
      const SPEED = 14;
      if (this.selectPointer.y < M) this.setScrollTop(this.scrollTop - SPEED);
      else if (this.selectPointer.y > this.height - M) this.setScrollTop(this.scrollTop + SPEED);
      const pos = this.root ? hitTextPosition(this.root, this.selectPointer.x, this.selectPointer.y) : null;
      if (pos && this.anchor) this.updateSelection(pos);
      this.requestRender();
    }
    if (this.dirty) {
      // 先清标志再渲染：render 过程中 markDirty（图片缓存命中等）必须为下一帧保留
      this.dirty = false;
      this.render();
    }
    if (this.animating || this.selecting) this.requestRender();
  };

  private renderCount_ = 0;

  private render(): void {
    const root = this.root;
    if (!root) return;
    this.renderCount_++;
    const t = theme();
    // 宽度两遍法：先全宽排版，内容溢出 → 右侧留 10px 滚动条占位再排一遍
    const firstW = this.gutterApplied ? this.width - 10 : this.width;
    root._x = 0;
    root._y = 0;
    root._width = this.width;
    root._height = this.height;
    layout(root, 0, 0, firstW, this.height);
    const overflow = (root.children[0]?._contentHeight ?? root._contentHeight) > this.height + 1;
    if (overflow !== this.gutterApplied) {
      this.gutterApplied = overflow;
      for (const n of root.walk()) n._layoutDirty = true;
      layout(root, 0, 0, overflow ? this.width - 10 : this.width, this.height);
    }
    // scrollTop 可能因内容变矮而超出
    const maxTop = this.maxScrollTop();
    if (this.scrollTop > maxTop) {
      this.scrollTop = maxTop;
    }
    root._scrollY = this.scrollTop;
    // sticky 节点偏移
    for (const n of root.walk()) {
      if (n.style.position === "sticky") {
        const top = typeof n.style.top === "number" ? n.style.top : 0;
        const parent = n.parent;
        const limit = parent
          ? parent._y + parent._contentHeight - n._y - n._height
          : Infinity;
        n._stickyDy = Math.max(0, Math.min(limit, this.scrollTop + top - n._y));
      }
    }
    // 布局完成：此刻 contentHeight/节点几何才有效（吸底钉底、覆盖层定位依赖它）
    this.hooks.onAfterLayout?.();

    this.animating = false;
    // 图片节点异步加载（完成后 onload 会 markLayoutDirty + 重绘）
    for (const n of root.walk()) {
      if (n.tag === "img" && n.style.src && !n._img && !n._imgFailed) {
        loadNodeImage(n, () => this.markDirty());
      }
    }
    this.ctx.clearRect(0, 0, this.width, this.height);
    // 背景（DOM 的 body/chat 底色）
    this.ctx.fillStyle = t.bg;
    this.ctx.fillRect(0, 0, this.width, this.height);
    paint(this.ctx, root, 0, 0, this.width, this.height, { now: performance.now() });
    if (this.selection) paintSelection(this.ctx, root, this.selection);
    this.paintOuterScrollbar();
    // 收集动画节点
    for (const n of root.walk()) {
      if (n.animates && this.inViewport(n)) {
        this.animating = true;
        break;
      }
    }
  }

  private inViewport(n: Node): boolean {
    const r = this.nodeScreenRect(n);
    return r.y + r.h >= 0 && r.y <= this.height;
  }

  private paintOuterScrollbar(): void {
    if (!this.gutterApplied) return;
    const t = theme();
    const contentH = this.contentHeight();
    const maxTop = this.maxScrollTop();
    if (maxTop <= 0) return;
    const thumbH = Math.max(24, (this.height / contentH) * this.height);
    const range = this.height - thumbH;
    const thumbY = (this.scrollTop / maxTop) * range;
    const hover =
      this.lastPointer.x >= this.width - 10 || (this.scrollDrag && this.scrollDrag.node === null);
    this.ctx.fillStyle = hover ? t.scrollHover : t.scroll;
    const x = this.width - 10;
    this.ctx.beginPath();
    const r = 2;
    const rx = x + 3;
    const rw = 4;
    this.ctx.moveTo(rx + r, thumbY);
    this.ctx.arcTo(rx + rw, thumbY, rx + rw, thumbY + r, r);
    this.ctx.arcTo(rx + rw, thumbY + thumbH, rx + rw - r, thumbY + thumbH, r);
    this.ctx.arcTo(rx, thumbY + thumbH, rx, thumbY + thumbH - r, r);
    this.ctx.arcTo(rx, thumbY, rx + r, thumbY, r);
    this.ctx.fill();
  }

  // ---------- 事件 ----------

  private bound: Array<[EventTarget, string, EventListener, (AddEventListenerOptions | boolean)?]> = [];

  private on(
    target: EventTarget,
    type: string,
    fn: EventListener,
    opts?: AddEventListenerOptions | boolean,
  ): void {
    target.addEventListener(type, fn, opts);
    this.bound.push([target, type, fn, opts]);
  }

  private pos(e: MouseEvent): { x: number; y: number } {
    const r = this.canvas.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  }

  private bindEvents(): void {
    const c = this.canvas;
    this.on(c, "mousedown", this.onMouseDown as EventListener);
    this.on(window, "mousemove", this.onMouseMove as EventListener);
    this.on(window, "mouseup", this.onMouseUp as EventListener);
    this.on(c, "mouseleave", this.onMouseLeave as EventListener);
    this.on(c, "wheel", this.onWheel as EventListener, { passive: false });
    this.on(c, "contextmenu", this.onContextMenu as EventListener);
    this.on(window, "keydown", this.onKeyDown as EventListener, true);
    this.on(c, "copy", this.onCopy as EventListener);
  }

  private onMouseDown = (e: MouseEvent): void => {
    if (e.button !== 0) return;
    const { x, y } = this.pos(e);
    // 外层滚动条
    if (this.gutterApplied && x >= this.width - 10) {
      const contentH = this.contentHeight();
      const maxTop = this.maxScrollTop();
      const thumbH = Math.max(24, (this.height / contentH) * this.height);
      const range = this.height - thumbH;
      const thumbY = (this.scrollTop / maxTop) * range;
      if (y >= thumbY && y <= thumbY + thumbH) {
        this.scrollDrag = { node: null, axis: "v", grabOffset: y - thumbY };
      } else {
        // 点击轨道翻页
        this.setScrollTop(this.scrollTop + (y < thumbY ? -1 : 1) * this.height * PAGE_RATIO);
        this.scrollDrag = { node: null, axis: "v", grabOffset: thumbH / 2 };
      }
      this.clearSelection();
      return;
    }
    const hit = this.root ? hitTest(this.root, x, y) : null;
    if (!hit) {
      this.clearSelection();
      return;
    }
    // 内层滚动条
    let p: Node | null = hit;
    while (p) {
      if (p.style.overflow === "auto" || p.style.overflow === "scroll") {
        const lp = this.toLayoutCoords(p, x, y);
        const sb = scrollbarHit(p, lp.x, lp.y);
        if (sb) {
          const thumbPos = sb.axis === "v" ? lp.y - sb.thumbStart : lp.x - sb.thumbStart;
          if (thumbPos >= 0 && thumbPos <= sb.thumbLen) {
            this.scrollDrag = { node: p, axis: sb.axis, grabOffset: thumbPos };
          } else {
            // 点击轨道：thumb 居中到指针
            if (sb.axis === "v") {
              const range = p._height - sb.thumbLen - (p._maxScrollX > 0 ? 10 : 0);
              p._scrollY = Math.max(
                0,
                Math.min(p._maxScrollY, ((lp.y - p._y - sb.thumbLen / 2) / Math.max(1, range)) * p._maxScrollY),
              );
            } else {
              const range = p._width - sb.thumbLen - (p._maxScrollY > 0 ? 10 : 0);
              p._scrollX = Math.max(
                0,
                Math.min(p._maxScrollX, ((lp.x - p._x - sb.thumbLen / 2) / Math.max(1, range)) * p._maxScrollX),
              );
            }
            this.scrollDrag = { node: p, axis: sb.axis, grabOffset: sb.thumbLen / 2 };
            this.dirty = true;
            this.requestRender();
            this.hooks.onScrollChange?.();
          }
          return;
        }
      }
      p = p.parent;
    }
    this.pressedNode = hit;
    hit._active = true;
    // 文字选择
    if (this.isSelectable(hit)) {
      const pos = this.root ? hitTextPosition(this.root, x, y) : null;
      if (pos) {
        this.selecting = true;
        this.anchor = pos;
        this.selectPointer = { x, y };
        this.selection = {
          startNode: pos.node,
          startOffset: pos.offset,
          endNode: pos.node,
          endOffset: pos.offset,
        };
        this.canvas.focus();
        this.dirty = true;
        this.requestRender();
        this.hooks.onSelectionChange?.(true);
        return;
      }
    }
    this.clearSelection();
    this.dirty = true;
    this.requestRender();
  };

  private onMouseMove = (e: MouseEvent): void => {
    const { x, y } = this.pos(e);
    this.lastPointer = { x, y };
    // 滚动条拖拽
    if (this.scrollDrag) {
      const d = this.scrollDrag;
      if (d.node === null) {
        const contentH = this.contentHeight();
        const thumbH = Math.max(24, (this.height / contentH) * this.height);
        const range = this.height - thumbH;
        const maxTop = this.maxScrollTop();
        this.setScrollTop(((y - d.grabOffset) / Math.max(1, range)) * maxTop);
      } else {
        const layoutPt = this.toLayoutCoords(d.node, x, y);
        const n = d.node;
        if (d.axis === "v") {
          const thumbH = Math.max(24, (n._height / n._contentHeight) * n._height);
          const range = n._height - thumbH - (n._maxScrollX > 0 ? 10 : 0);
          n._scrollY = Math.max(
            0,
            Math.min(n._maxScrollY, ((layoutPt.y - n._y - d.grabOffset) / Math.max(1, range)) * n._maxScrollY),
          );
        } else {
          const thumbW = Math.max(24, (n._width / n._contentWidth) * n._width);
          const range = n._width - thumbW - (n._maxScrollY > 0 ? 10 : 0);
          n._scrollX = Math.max(
            0,
            Math.min(n._maxScrollX, ((layoutPt.x - n._x - d.grabOffset) / Math.max(1, range)) * n._maxScrollX),
          );
        }
        this.dirty = true;
        this.requestRender();
        this.hooks.onScrollChange?.();
      }
      return;
    }
    // 拖选
    if (this.selecting) {
      this.selectPointer = { x, y };
      const pos = this.root ? hitTextPosition(this.root, x, y) : null;
      if (pos) this.updateSelection(pos);
      return;
    }
    // hover
    const hit = this.root ? hitTest(this.root, x, y) : null;
    if (hit !== this.hoverNode) {
      if (this.hoverNode) this.setHoverChain(this.hoverNode, false);
      this.hoverNode = hit;
      if (hit) this.setHoverChain(hit, true);
      this.dirty = true;
      this.requestRender();
    }
    this.updateCursor(hit);
    this.updateTooltip(hit, x, y);
  };

  private onMouseUp = (e: MouseEvent): void => {
    if (this.scrollDrag) {
      this.scrollDrag = null;
      return;
    }
    if (this.selecting) {
      this.selecting = false;
      if (
        this.selection &&
        this.selection.startNode === this.selection.endNode &&
        this.selection.startOffset === this.selection.endOffset
      ) {
        this.clearSelection();
      }
      this.dirty = true;
      this.requestRender();
      // 折叠成点的选区不拦截 click
    }
    if (this.pressedNode) {
      this.pressedNode._active = false;
      this.pressedNode = null;
      this.dirty = true;
      this.requestRender();
    }
    // click：命中链上找 onClick
    if (e.button === 0 && !this.selection) {
      const { x, y } = this.pos(e);
      const hit = this.root ? hitTest(this.root, x, y) : null;
      let p: Node | null = hit;
      const ev: CanvasHitEvent = { x, y, native: e };
      while (p) {
        if (p.onClick) {
          p.onClick(p, ev);
          break;
        }
        p = p.parent;
      }
    }
  };

  private onMouseLeave = (): void => {
    this.lastPointer = { x: -1, y: -1 };
    if (this.hoverNode) {
      this.setHoverChain(this.hoverNode, false);
      this.hoverNode = null;
      this.dirty = true;
      this.requestRender();
    }
    this.updateTooltip(null, 0, 0);
  };

  private onWheel = (e: WheelEvent): void => {
    e.preventDefault();
    e.stopPropagation();
    let { deltaX, deltaY } = e;
    if (e.deltaMode === 1) {
      deltaX *= 40;
      deltaY *= 40;
    }
    if (e.shiftKey && deltaY !== 0 && deltaX === 0) {
      deltaX = deltaY;
      deltaY = 0;
    }
    const { x, y } = this.pos(e);
    const hit = this.root ? hitTest(this.root, x, y) : null;
    if (hit) {
      const route = routeScroll(hit, deltaX, deltaY);
      if (route.target) {
        this.scrollInner(route.target, deltaX, deltaY);
        return;
      }
      if (route.blocked) return;
    }
    this.scrollByOuter(deltaY || deltaX);
  };

  private onContextMenu = (e: MouseEvent): void => {
    const { x, y } = this.pos(e);
    const hit = this.root ? hitTest(this.root, x, y) : null;
    let p: Node | null = hit;
    const ev: CanvasHitEvent = { x, y, native: e };
    while (p) {
      if (p.onContextMenu) {
        e.preventDefault();
        p.onContextMenu(p, ev);
        return;
      }
      p = p.parent;
    }
  };

  private onKeyDown = (e: KeyboardEvent): void => {
    // Ctrl/Cmd+C：复制选区（不依赖 canvas 焦点，与 DOM 行为一致）
    if ((e.ctrlKey || e.metaKey) && (e.key === "c" || e.key === "C")) {
      const target = e.target;
      const inEditable =
        target instanceof HTMLElement &&
        (target.isContentEditable || target.tagName === "INPUT" || target.tagName === "TEXTAREA");
      if (this.selection && !inEditable) {
        this.copySelection();
        e.preventDefault();
      }
      return;
    }
    // 键盘滚动（镜像 ChatView 的排除规则）
    const target = e.target;
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    if (target instanceof HTMLElement) {
      if (
        target.isContentEditable ||
        target.tagName === "INPUT" ||
        target.tagName === "TEXTAREA" ||
        target.tagName === "SELECT"
      ) {
        return;
      }
      if (target !== this.canvas && target !== document.body && !this.canvas.contains(target)) return;
    }
    const maxTop = this.maxScrollTop();
    if (maxTop <= 0) return;
    switch (e.key) {
      case "ArrowUp":
        this.setScrollTop(this.scrollTop - ARROW_SCROLL);
        break;
      case "ArrowDown":
        this.setScrollTop(this.scrollTop + ARROW_SCROLL);
        break;
      case "PageUp":
        this.setScrollTop(this.scrollTop - this.height * PAGE_RATIO);
        break;
      case "PageDown":
        this.setScrollTop(this.scrollTop + this.height * PAGE_RATIO);
        break;
      case " ":
        this.setScrollTop(this.scrollTop + (e.shiftKey ? -1 : 1) * this.height * PAGE_RATIO);
        break;
      case "Home":
        this.setScrollTop(0);
        break;
      case "End":
        this.setScrollTop(maxTop);
        break;
      default:
        return;
    }
    e.preventDefault();
    this.hooks.onScrollChange?.();
  };

  private onCopy = (e: ClipboardEvent): void => {
    if (!this.selection || !this.root) return;
    const text = selectionText(this.root, this.selection);
    if (!text) return;
    e.preventDefault();
    e.clipboardData?.setData("text/plain", text);
  };

  // ---------- 内部 ----------

  private scrollInner(node: Node, dx: number, dy: number): void {
    if (scrollBy(node, dx, dy)) {
      this.dirty = true;
      this.requestRender();
      this.hooks.onScrollChange?.();
      // 滚动位置持久化（工具输出等，key 由 builder 挂在 data 上）
      const key = node.data?.scrollKey as string | undefined;
      if (key) innerScrollPositions.set(key, node._scrollY);
    }
  }

  /** 视口坐标 → 节点的布局坐标（自身 + 祖先滚动累计） */
  private toLayoutCoords(node: Node, x: number, y: number): { x: number; y: number } {
    let sx = node._scrollX || 0;
    let sy = node._scrollY || 0;
    let p = node.parent;
    while (p) {
      sx += p._scrollX || 0;
      sy += p._scrollY || 0;
      p = p.parent;
    }
    return { x: x + sx, y: y + sy };
  }

  private isSelectable(node: Node): boolean {
    if (node.tag === "img" || node.tag === "icon" || node.tag === "spinner") return false;
    return !!node.textContent && node.style.userSelect !== "none";
  }

  private updateSelection(focus: TextPos): void {
    if (!this.anchor || !this.root) return;
    const nodes = collectTextNodes(this.root);
    const ai = nodes.indexOf(this.anchor.node);
    const fi = nodes.indexOf(focus.node);
    if (ai < 0 || fi < 0) return;
    const forward = ai < fi || (ai === fi && this.anchor.offset <= focus.offset);
    this.selection = forward
      ? {
          startNode: this.anchor.node,
          startOffset: this.anchor.offset,
          endNode: focus.node,
          endOffset: focus.offset,
        }
      : {
          startNode: focus.node,
          startOffset: focus.offset,
          endNode: this.anchor.node,
          endOffset: this.anchor.offset,
        };
    this.dirty = true;
    this.requestRender();
  }

  private copySelection(): void {
    if (!this.selection || !this.root) return;
    const text = selectionText(this.root, this.selection);
    if (!text) return;
    void navigator.clipboard?.writeText(text).catch(() => {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      try {
        document.execCommand("copy");
      } catch {
        /* noop */
      }
      document.body.removeChild(ta);
    });
  }

  private setHoverChain(node: Node, on: boolean): void {
    let p: Node | null = node;
    while (p) {
      if (p === node) p._hover = on;
      p._hoverWithin = on;
      p = p.parent;
    }
  }

  private updateCursor(hit: Node | null): void {
    let cursor = "default";
    let p: Node | null = hit;
    while (p) {
      if (p.onClick || p.onContextMenu || (p.style.cursor && p.style.cursor !== "default")) {
        cursor = p.style.cursor !== "default" ? p.style.cursor : "pointer";
        break;
      }
      if (p.textContent && p.style.userSelect !== "none" && p.tag !== "img" && p.tag !== "icon" && p.tag !== "spinner") {
        cursor = "text";
        break;
      }
      p = p.parent;
    }
    if (this.gutterApplied && hit === null) cursor = "default";
    this.canvas.style.cursor = cursor;
  }

  private updateTooltip(hit: Node | null, x: number, y: number): void {
    let titled: Node | null = null;
    let p: Node | null = hit;
    while (p) {
      if (p.style.title) {
        titled = p;
        break;
      }
      p = p.parent;
    }
    if (titled !== this.tooltipNode) {
      this.tooltipNode = titled;
      if (this.tooltipTimer) {
        clearTimeout(this.tooltipTimer);
        this.tooltipTimer = undefined;
      }
      if (titled) {
        const node = titled;
        const tx = x;
        const ty = y;
        this.tooltipTimer = setTimeout(() => {
          this.tooltipTimer = undefined;
          if (this.tooltipNode === node) this.hooks.onHoverTitle?.(node, tx, ty);
        }, TOOLTIP_DELAY);
      } else {
        this.hooks.onHoverTitle?.(null, 0, 0);
      }
    }
  }

  destroy(): void {
    this.disposed = true;
    if (this.rafId) cancelAnimationFrame(this.rafId);
    if (this.tooltipTimer) clearTimeout(this.tooltipTimer);
    for (const [target, type, fn, opts] of this.bound) {
      target.removeEventListener(type, fn, opts);
    }
    this.removeThemeListener?.();
    this.removeFontLoadListener?.();
  }
}

/** 内层滚动位置持久化（key 规则与 DOM 版 toolScrollPositions 相同） */
export const innerScrollPositions = new Map<string, number>();

// ---------- 图片加载（img 节点 src → HTMLImageElement，加载完成后触发重排） ----------

interface ImgEntry {
  img: HTMLImageElement;
  loaded: boolean;
  failed: boolean;
  waiters: Set<() => void>;
}
const imageCache = new Map<string, ImgEntry>();

export function loadNodeImage(node: Node, onReady: () => void): void {
  const src = node.style.src;
  if (!src || node._img || node._imgFailed) return;
  let entry = imageCache.get(src);
  if (!entry) {
    const img = new Image();
    entry = { img, loaded: false, failed: false, waiters: new Set() };
    imageCache.set(src, entry);
    img.onload = () => {
      entry!.loaded = true;
      for (const fn of entry!.waiters) fn();
      entry!.waiters.clear();
    };
    img.onerror = () => {
      entry!.failed = true;
      for (const fn of entry!.waiters) fn();
      entry!.waiters.clear();
    };
    img.src = src;
  }
  if (entry.loaded) {
    node._img = entry.img;
    node._imgLoaded = true;
    node.markLayoutDirty();
    onReady();
    return;
  }
  if (entry.failed) {
    node._imgFailed = true;
    return;
  }
  entry.waiters.add(() => {
    if (entry!.loaded) {
      node._img = entry!.img;
      node._imgLoaded = true;
    } else {
      node._imgFailed = true;
    }
    node.markLayoutDirty();
    onReady();
  });
}
