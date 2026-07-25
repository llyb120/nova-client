// CanvasDOM.ts - main renderer entry point.
// Renders a Node tree onto a canvas with interaction + simulated scrolling.

import { Node, h, type StyleProps } from './Node';
import { layout } from './layout';
import { paint, hitTextPosition, paintSelection, selectionText, collectTextNodes } from './painter';
import { hitTest, findScrollable, scrollBy } from './interaction';
import { parseMarkdown, findStableMarkdownPrefixEnd } from './markdown';

export { Node, h };
export { parseMarkdown, findStableMarkdownPrefixEnd };
export type { StyleProps, NodeMeta, TextLineFragment } from './Node';

export interface CanvasDOMOptions {
  dpr?: number;
  maxWidth?: number;
  columnPadX?: number;
  onScroll?: (scrollTop: number) => void;
}

export interface MarkdownTheme {
  codeBg?: string;
  codeColor?: string;
  linkColor?: string;
  quoteBg?: string;
  quoteBorder?: string;
  hrColor?: string;
  textColor?: string;
}

type CanvasListener = { type: string; fn: (payload: unknown) => void };
type SelectionState = {
  startNode: Node;
  startOffset: number;
  endNode: Node;
  endOffset: number;
};
type SelectionStart = { node: Node; offset: number };

export class CanvasDOM {
  canvas: HTMLCanvasElement;
  ctx: CanvasRenderingContext2D;
  root: Node | null = null;
  dpr: number;
  width = 0;
  height = 0;

  private _maxWidth: number;
  private _columnPadX: number;
  private _mdTheme: MarkdownTheme = {};
  private _content: Node | null = null;
  private _viewport: Node | null = null;
  private _column: Node | null = null;

  private _rafId = 0;
  private _dirty = true;
  private _layoutNeeded = true;
  private _caretOn = true;
  private _caretTimer = 0;

  private _hoverNode: Node | null = null;
  private _activeNode: Node | null = null;
  private _focusedNode: Node | null = null;
  private _scrollTarget: Node | null = null;
  private _velocityY = 0;
  private _velocityX = 0;
  private _dragging = false;
  private _lastDragY = 0;
  private _lastDragX = 0;
  private _lastDragTime = 0;

  private _selection: SelectionState | null = null;
  private _selecting = false;
  private _selectionStart: SelectionStart | null = null;
  private _streamingNodes = new Set<Node>();

  private _listeners: CanvasListener[] = [];
  private _scrollListeners: Array<(scrollTop: number) => void> = [];
  private _boundHandlers: Record<string, EventListener> = {};
  private _onKeyDownCopy: ((e: KeyboardEvent) => void) | null = null;
  private _streamSpeed = 2;
  private _destroyed = false;

  constructor(canvas: HTMLCanvasElement, options: CanvasDOMOptions = {}) {
    this.canvas = canvas;
    const ctx = canvas.getContext('2d', { alpha: true });
    if (!ctx) throw new Error('CanvasDOM: failed to get 2d context');
    this.ctx = ctx;
    this.dpr = options.dpr ?? (typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1);
    this._maxWidth = options.maxWidth ?? 980;
    this._columnPadX = options.columnPadX ?? 0;
    if (options.onScroll) this._scrollListeners.push(options.onScroll);

    this._bindEvents();
    this.resize();
  }

  /** Content column width: clamp(720, 78vw, maxWidth) minus horizontal padding. */
  private _computeColumnWidth(): number {
    const vw78 = this.width * 0.78;
    const clamped = Math.min(this._maxWidth, Math.max(720, vw78));
    return Math.max(0, Math.min(clamped, this.width - this._columnPadX * 2));
  }

  private _buildViewport(content: Node): Node {
    const colW = this._computeColumnWidth();
    const sideMargin = Math.max(this._columnPadX, (this.width - colW) / 2);

    const column = h('div', {
      width: colW,
      margin: [0, sideMargin],
    });
    column.appendChild(content);

    const viewport = h('div', {
      overflow: 'scroll',
      width: this.width,
      height: this.height,
    });
    viewport.appendChild(column);

    this._content = content;
    this._column = column;
    this._viewport = viewport;
    return viewport;
  }

  private _syncViewportSize(): void {
    if (!this._viewport || !this._column) return;
    this._viewport.style.width = this.width;
    this._viewport.style.height = this.height;
    const colW = this._computeColumnWidth();
    const sideMargin = Math.max(this._columnPadX, (this.width - colW) / 2);
    this._column.style.width = colW;
    this._column.style.margin = [0, sideMargin];
  }

  setRoot(node: Node): void {
    const prevScroll = this._viewport?._scrollY ?? 0;
    this.root = this._buildViewport(node);
    this._markLayoutDirty();
    this._loadImages(this.root);
    this.requestRender();
    // Preserve scroll across full remounts when possible (applied after next layout).
    if (prevScroll > 0) {
      const restore = prevScroll;
      requestAnimationFrame(() => {
        if (this._destroyed || !this._viewport) return;
        layout(this.root!, 0, 0, this.width, this.height);
        this._layoutNeeded = false;
        this._viewport._scrollY = Math.max(0, Math.min(this._viewport._maxScrollY, restore));
        this._markPaintDirty();
        this.requestRender();
      });
    }
  }

  /** Replace transcript content without rebuilding the viewport shell (keeps scrollTop). */
  replaceContent(node: Node): void {
    if (!this._column || !this._viewport) {
      this.setRoot(node);
      return;
    }
    while (this._column.children.length > 0) {
      this._column.removeChild(this._column.children[0]);
    }
    this._column.appendChild(node);
    this._content = node;
    this._markLayoutDirty();
    this._loadImages(node);
    this.requestRender();
  }

  /** Sync layout immediately (for measuring group offsets before paint). */
  forceLayout(): void {
    if (!this.root) return;
    layout(this.root, 0, 0, this.width, this.height);
    this._captureLayerCaches(this.root);
    this._layoutNeeded = false;
  }

  /** Absolute layout Y of a group node (for scrollToGroup / time machine). */
  getGroupOffset(groupIndex: number): number | null {
    const node = this.findNode(
      (n) => n.meta?.kind === 'group' && n.meta.groupIndex === groupIndex,
    );
    if (!node || !this._viewport) return null;
    return Math.max(0, node._y - this._viewport._y);
  }

  findNode(pred: (n: Node) => boolean, root: Node | null = this.root): Node | null {
    if (!root) return null;
    if (pred(root)) return root;
    for (const c of root.children) {
      const hit = this.findNode(pred, c);
      if (hit) return hit;
    }
    return null;
  }

  /** Screen-space rect of a node (accounts for viewport scroll). */
  getNodeScreenRect(node: Node): { x: number; y: number; w: number; h: number } | null {
    if (!this._viewport) return null;
    return {
      x: node._x,
      y: node._y - this._viewport._scrollY,
      w: node._width,
      h: node._height,
    };
  }

  setMarkdownTheme(theme: MarkdownTheme): void {
    this._mdTheme = theme;
  }

  resize(w?: number, h?: number): void {
    this.width = w ?? this.canvas.clientWidth ?? this.canvas.width;
    this.height = h ?? this.canvas.clientHeight ?? this.canvas.height;
    this.canvas.width = this.width * this.dpr;
    this.canvas.height = this.height * this.dpr;
    this.ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    this._syncViewportSize();
    this._markLayoutDirty();
    this.requestRender();
  }

  get scrollTop(): number {
    return this._viewport?._scrollY ?? 0;
  }

  set scrollTop(v: number) {
    this.scrollTo(v);
  }

  get scrollHeight(): number {
    if (!this._viewport) return 0;
    return this._viewport._contentHeight || this._viewport._height;
  }

  scrollTo(y: number): void {
    if (!this._viewport) return;
    const max = this._viewport._maxScrollY;
    const next = Math.max(0, Math.min(max, y));
    if (next === this._viewport._scrollY) return;
    this._viewport._scrollY = next;
    this._onScrollChanged(this._viewport);
  }

  onScroll(fn: (scrollTop: number) => void): () => void {
    this._scrollListeners.push(fn);
    return () => {
      const i = this._scrollListeners.indexOf(fn);
      if (i >= 0) this._scrollListeners.splice(i, 1);
    };
  }

  private _emitScroll(scrollTop: number): void {
    for (const fn of this._scrollListeners) fn(scrollTop);
    this._emit('scroll', scrollTop);
  }

  private _onScrollChanged = (scroller: Node): void => {
    if (scroller === this._viewport) {
      this._emitScroll(scroller._scrollY);
    }
    this._markPaintDirty();
  };

  private _markPaintDirty(): void {
    this._dirty = true;
  }

  private _markLayoutDirty(): void {
    this._layoutNeeded = true;
    this._dirty = true;
  }

  requestRender(): void {
    if (this._destroyed || this._rafId) return;
    this._rafId = requestAnimationFrame(this._tick);
  }

  private _tick = (): void => {
    this._rafId = 0;
    if (this._destroyed) return;

    if (Math.abs(this._velocityY) > 0.5 || Math.abs(this._velocityX) > 0.5) {
      const t = this._scrollTarget;
      if (t) {
        const moved = scrollBy(t, this._velocityX, this._velocityY, this._onScrollChanged);
        this._velocityX *= 0.92;
        this._velocityY *= 0.92;
        if (!moved || (Math.abs(this._velocityY) < 0.5 && Math.abs(this._velocityX) < 0.5)) {
          this._velocityX = 0;
          this._velocityY = 0;
        }
        if (moved) this._markPaintDirty();
      }
    }

    if (this._streamingNodes.size > 0) {
      for (const n of this._streamingNodes) {
        if (n.hasStreamPending()) n.revealStream(this._streamSpeed);
        else this._streamingNodes.delete(n);
      }
      this._markLayoutDirty();
    }

    const now = performance.now();
    if (this._focusedNode && now - this._caretTimer > 530) {
      this._caretOn = !this._caretOn;
      this._caretTimer = now;
      this._markPaintDirty();
    }

    if (this._dirty) {
      this._render();
      this._dirty = false;
    }

    if (this._velocityX || this._velocityY || this._focusedNode || this._streamingNodes.size > 0) {
      this.requestRender();
    }
  };

  private _render(): void {
    if (!this.root) return;

    if (this._layoutNeeded) {
      layout(this.root, 0, 0, this.width, this.height);
      this._captureLayerCaches(this.root);
      this._layoutNeeded = false;
    }

    this.ctx.clearRect(0, 0, this.width, this.height);
    paint(this.ctx, this.root, 0, 0, this.width, this.height, this);

    if (this._selection) {
      paintSelection(this.ctx, this.root, this._selection);
    }
  }

  private _captureLayerCaches(node: Node): void {
    if (node.meta?.cacheLayer && (!node._layerValid || node._dirty)) {
      this._captureNodeLayer(node);
    }
    for (const c of node.children) this._captureLayerCaches(c);
  }

  private _captureNodeLayer(node: Node): void {
    const w = Math.ceil(node._width);
    const h = Math.ceil(node._height);
    if (w <= 0 || h <= 0) return;

    let layer = node._layer;
    const pw = w * this.dpr;
    const ph = h * this.dpr;

    if (!layer || layer.width !== pw || layer.height !== ph) {
      if (typeof OffscreenCanvas !== 'undefined') {
        layer = new OffscreenCanvas(pw, ph);
      } else {
        const c = document.createElement('canvas');
        c.width = pw;
        c.height = ph;
        layer = c;
      }
      node._layer = layer;
    }

    const lctx = layer.getContext('2d');
    if (!lctx) return;
    lctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    lctx.clearRect(0, 0, w, h);
    lctx.save();
    lctx.translate(-node._x, -node._y);
    paint(lctx, node, node._x, node._y, node._x + w, node._y + h, this, { forceRepaint: true });
    lctx.restore();

    node._layerValid = true;
    node._dirty = false;
  }

  private _loadImages(node: Node): void {
    if (node.tag === 'img' && node.style.src && !node._img) {
      const img = new Image();
      img.onload = () => {
        node._img = img;
        node._imgLoaded = true;
        this._markLayoutDirty();
        this.requestRender();
      };
      img.onerror = () => { node._imgLoaded = false; };
      img.src = node.style.src;
    }
    for (const c of node.children) this._loadImages(c);
  }

  private _bindEvents(): void {
    const on = (type: string, fn: EventListener, opts?: AddEventListenerOptions) => {
      this.canvas.addEventListener(type, fn, opts);
      this._boundHandlers[type] = fn;
    };
    on('mousemove', this._onMouseMove as EventListener);
    on('mousedown', this._onMouseDown as EventListener);
    on('mouseup', this._onMouseUp as EventListener);
    on('mouseleave', this._onMouseLeave as EventListener);
    on('wheel', this._onWheel as EventListener, { passive: false });
    on('click', this._onClick as EventListener);
    on('keydown', this._onKeyDown as EventListener);
    on('keypress', this._onKeyPress as EventListener);
    on('blur', this._onBlur as EventListener);
    on('copy', this._onCopy as EventListener);

    this._onKeyDownCopy = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && (e.key === 'c' || e.key === 'C')) {
        if (this._selection && document.activeElement === this.canvas) {
          this._copySelection();
        }
      }
    };
    window.addEventListener('keydown', this._onKeyDownCopy);
    this._boundHandlers._windowKeydown = this._onKeyDownCopy as EventListener;
  }

  private _pos(e: MouseEvent | WheelEvent): { x: number; y: number } {
    const r = this.canvas.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  }

  private _onMouseMove = (e: MouseEvent): void => {
    const { x, y } = this._pos(e);
    if (this._selecting && this._selectionStart) {
      const pos = this.root ? hitTextPosition(this.root, x, y) : null;
      if (pos) {
        this._selection = this._normalizeSelection({
          startNode: this._selectionStart.node,
          startOffset: this._selectionStart.offset,
          endNode: pos.node,
          endOffset: pos.offset,
        });
        this._markPaintDirty();
        this.requestRender();
      }
      return;
    }

    if (this._dragging && this._scrollTarget) {
      const dy = y - this._lastDragY;
      const dx = x - this._lastDragX;
      const now = performance.now();
      const dt = Math.max(1, now - this._lastDragTime);
      this._velocityY = dy / dt * 16;
      this._velocityX = dx / dt * 16;
      scrollBy(this._scrollTarget, -dx, -dy, this._onScrollChanged);
      this._lastDragY = y;
      this._lastDragX = x;
      this._lastDragTime = now;
      this._markPaintDirty();
      this.requestRender();
      return;
    }

    const hit = this.root ? hitTest(this.root, x, y) : null;
    if (hit !== this._hoverNode) {
      if (this._hoverNode) this._hoverNode._hover = false;
      this._hoverNode = hit;
      if (hit) {
        hit._hover = true;
        const isInteractive = hit.tag === 'button' || hit.tag === 'input' || (hit.tag === 'a' && hit.style.href);
        const cursor = (hit.style.cursor && hit.style.cursor !== 'default')
          ? hit.style.cursor
          : (isInteractive ? 'pointer' : 'default');
        this.canvas.style.cursor = cursor;
      } else {
        this.canvas.style.cursor = 'default';
      }
      this._markPaintDirty();
      this.requestRender();
    } else if (hit) {
      const isInteractive = hit.tag === 'button' || hit.tag === 'input' || (hit.tag === 'a' && hit.style.href);
      const cursor = (hit.style.cursor && hit.style.cursor !== 'default') ? hit.style.cursor
        : (isInteractive ? 'pointer'
          : (hit.textContent && hit.style.userSelect !== 'none' ? 'text' : 'default'));
      this.canvas.style.cursor = cursor;
    }
  };

  private _isSelectableText(node: Node | null): boolean {
    if (!node) return false;
    if (node.tag === 'input' || node.tag === 'button' || node.tag === 'img') return false;
    if (!node.textContent) return false;
    if (node.style.userSelect === 'none') return false;
    return true;
  }

  private _onMouseDown = (e: MouseEvent): void => {
    const { x, y } = this._pos(e);
    const hit = this.root ? hitTest(this.root, x, y) : null;
    if (hit) {
      hit._active = true;
      this._activeNode = hit;

      if (hit.tag === 'input' || hit.tag === 'button') {
        if (this._focusedNode && this._focusedNode !== hit) this._focusedNode._focused = false;
        this._focusedNode = hit;
        hit._focused = true;
        this._caretOn = true;
        this._caretTimer = performance.now();
        this.canvas.setAttribute('tabindex', '0');
        this.canvas.focus();
      } else if (this._focusedNode) {
        this._focusedNode._focused = false;
        this._focusedNode = null;
      }

      if (this._isSelectableText(hit)) {
        const pos = hitTextPosition(this.root!, x, y);
        if (pos) {
          this._selecting = true;
          this._selectionStart = { node: pos.node, offset: pos.offset };
          this._selection = {
            startNode: pos.node, startOffset: pos.offset,
            endNode: pos.node, endOffset: pos.offset,
          };
          this.canvas.setAttribute('tabindex', '0');
          this.canvas.focus();
          this._markPaintDirty();
          this.requestRender();
          return;
        }
      }

      const scroller = findScrollable(hit, 0, 1);
      if (scroller) {
        this._scrollTarget = scroller;
        this._dragging = true;
        this._lastDragX = x;
        this._lastDragY = y;
        this._lastDragTime = performance.now();
        this._velocityX = 0;
        this._velocityY = 0;
        this._selection = null;
      } else {
        this._selection = null;
      }
      this._markPaintDirty();
      this.requestRender();
    } else {
      this._selection = null;
      this._markPaintDirty();
      this.requestRender();
    }
  };

  private _onMouseUp = (): void => {
    if (this._activeNode) {
      this._activeNode._active = false;
      this._activeNode = null;
    }
    if (this._selecting) {
      this._selecting = false;
      if (this._selection && this._selection.startNode === this._selection.endNode
          && this._selection.startOffset === this._selection.endOffset) {
        this._selection = null;
      }
      this._markPaintDirty();
      this.requestRender();
    }
    if (this._dragging) {
      this._dragging = false;
      if (Math.abs(this._velocityY) < 0.5 && Math.abs(this._velocityX) < 0.5) {
        this._scrollTarget = null;
      }
    }
    this._markPaintDirty();
    this.requestRender();
  };

  private _onMouseLeave = (): void => {
    if (this._hoverNode) {
      this._hoverNode._hover = false;
      this._hoverNode = null;
      this._markPaintDirty();
      this.requestRender();
    }
  };

  private _onWheel = (e: WheelEvent): void => {
    e.preventDefault();
    const { x, y } = this._pos(e);
    const hit = this.root ? hitTest(this.root, x, y) : null;
    if (!hit) return;
    const scroller = findScrollable(hit, e.deltaX, e.deltaY);
    if (scroller) {
      const moved = scrollBy(scroller, e.deltaX, e.deltaY, this._onScrollChanged);
      if (moved) {
        this._scrollTarget = scroller;
        this._velocityX = e.deltaX * 0.5;
        this._velocityY = e.deltaY * 0.5;
        this._markPaintDirty();
        this.requestRender();
      }
    }
  };

  private _findClickAction(node: Node | null): { node: Node; action: string | ((node: Node) => void) } | null {
    let n: Node | null = node;
    while (n) {
      const onClick = n.meta?.onClick;
      if (typeof onClick === 'string') {
        return { node: n, action: onClick };
      }
      if (typeof onClick === 'function') {
        return { node: n, action: onClick as (node: Node) => void };
      }
      n = n.parent;
    }
    return null;
  }

  private _onClick = (e: MouseEvent): void => {
    const { x, y } = this._pos(e);
    const hit = this.root ? hitTest(this.root, x, y) : null;
    if (!hit) return;

    const clickAction = this._findClickAction(hit);
    if (clickAction) {
      if (typeof clickAction.action === 'string') {
        this._emit('action', { node: clickAction.node, action: clickAction.action });
      } else {
        clickAction.action(clickAction.node);
      }
    }

    if (hit.tag === 'button') {
      this._emit('click', hit);
    }
    if (hit.tag === 'a' && hit.style.href) {
      try { window.open(hit.style.href, '_blank', 'noopener'); } catch { /* ignore */ }
      this._emit('click', hit);
    } else if (hit.tag !== 'button') {
      this._emit('click', hit);
    }
  };

  private _onKeyDown = (e: KeyboardEvent): void => {
    if (!this._focusedNode || this._focusedNode.tag !== 'input') return;
    if (e.key === 'Backspace') {
      e.preventDefault();
      const v = String(this._focusedNode.style.value || '');
      if (v.length) {
        this._focusedNode.style.value = v.slice(0, -1);
        this._focusedNode.markDirty();
        this._markLayoutDirty();
        this._emit('input', this._focusedNode);
        this.requestRender();
      }
    } else if (e.key === 'Enter') {
      e.preventDefault();
      this._emit('submit', this._focusedNode);
    }
  };

  private _onKeyPress = (e: KeyboardEvent): void => {
    if (!this._focusedNode || this._focusedNode.tag !== 'input') return;
    if (!e.key || e.key.length !== 1) return;
    e.preventDefault();
    this._focusedNode.style.value = (this._focusedNode.style.value || '') + e.key;
    this._focusedNode.markDirty();
    this._markLayoutDirty();
    this._caretOn = true;
    this._caretTimer = performance.now();
    this._emit('input', this._focusedNode);
    this.requestRender();
  };

  private _onBlur = (): void => {
    if (this._focusedNode) {
      this._focusedNode._focused = false;
      this._focusedNode = null;
      this._markPaintDirty();
      this.requestRender();
    }
  };

  on(type: string, fn: (payload: unknown) => void): void {
    this._listeners.push({ type, fn });
  }

  private _emit(type: string, payload: unknown): void {
    for (const l of this._listeners) if (l.type === type) l.fn(payload);
  }

  private _normalizeSelection(sel: SelectionState): SelectionState {
    if (!this.root) return sel;
    const nodes = collectTextNodes(this.root) as Node[];
    const si = nodes.indexOf(sel.startNode);
    const ei = nodes.indexOf(sel.endNode);
    if (si === -1 || ei === -1) return sel;
    if (si < ei) return sel;
    if (si === ei && sel.startOffset <= sel.endOffset) return sel;
    return {
      startNode: sel.endNode, startOffset: sel.endOffset,
      endNode: sel.startNode, endOffset: sel.startOffset,
    };
  }

  private _onCopy = (e: ClipboardEvent): void => {
    if (!this._selection) return;
    const text = selectionText(this.root!, this._selection);
    if (!text) return;
    e.preventDefault();
    if (e.clipboardData) {
      e.clipboardData.setData('text/plain', text);
    } else if (navigator.clipboard) {
      navigator.clipboard.writeText(text);
    }
  };

  private _copySelection(): void {
    if (!this._selection || !this.root) return;
    const text = selectionText(this.root, this._selection);
    if (!text) return;
    if (navigator.clipboard?.writeText) {
      navigator.clipboard.writeText(text).catch(() => {});
    } else {
      const ta = document.createElement('textarea');
      ta.value = text;
      ta.style.position = 'fixed';
      ta.style.opacity = '0';
      document.body.appendChild(ta);
      ta.select();
      try { document.execCommand('copy'); } catch { /* ignore */ }
      document.body.removeChild(ta);
    }
  }

  getSelectionText(): string {
    return this._selection && this.root ? selectionText(this.root, this._selection) : '';
  }

  clearSelection(): void {
    if (this._selection) {
      this._selection = null;
      this._markPaintDirty();
      this.requestRender();
    }
  }

  streamTo(node: Node, chunk: string, { typewriter = false }: { typewriter?: boolean } = {}): Node {
    node.appendStream(chunk, { typewriter });
    if (typewriter) this._streamingNodes.add(node);
    this._markLayoutDirty();
    this.requestRender();
    return node;
  }

  streamMarkdown(
    container: Node,
    chunk: string,
    { typewriter = false }: { typewriter?: boolean } = {},
  ): Node {
    container._mdBuffer = (container._mdBuffer ?? '') + String(chunk);
    const buf = container._mdBuffer;
    const stableEnd = findStableMarkdownPrefixEnd(buf);
    const stablePrefix = buf.slice(0, stableEnd);
    const unstableTail = buf.slice(stableEnd);

    let stableChildren: Node[];
    if (stablePrefix === container._mdStablePrefix && container._mdStableNodes) {
      stableChildren = container._mdStableNodes;
    } else {
      const stableTree = stablePrefix
        ? parseMarkdown(stablePrefix, {}, this._mdTheme)
        : h('div', { width: '100%' }, '', []);
      stableChildren = [...stableTree.children];
      container._mdStablePrefix = stablePrefix;
      container._mdStableNodes = stableChildren;
    }

    const tailTree = unstableTail
      ? parseMarkdown(unstableTail, {}, this._mdTheme)
      : h('div', { width: '100%' }, '', []);

    container.children.length = 0;
    for (const c of stableChildren) container.appendChild(c);
    for (const c of tailTree.children) container.appendChild(c);

    container.markDirty();
    this._markLayoutDirty();
    this.requestRender();
    return container;
  }

  setStreamSpeed(charsPerFrame: number): void {
    this._streamSpeed = charsPerFrame;
  }

  finishAllStreams(): void {
    for (const n of this._streamingNodes) n.finishStream();
    this._streamingNodes.clear();
    this._markLayoutDirty();
    this.requestRender();
  }

  destroy(): void {
    this._destroyed = true;
    if (this._rafId) cancelAnimationFrame(this._rafId);
    this._rafId = 0;

    for (const [type, fn] of Object.entries(this._boundHandlers)) {
      if (type === '_windowKeydown') {
        window.removeEventListener('keydown', fn);
      } else {
        this.canvas.removeEventListener(type, fn);
      }
    }
    this._boundHandlers = {};

    this._listeners = [];
    this._scrollListeners = [];
    this._streamingNodes.clear();
    this._selection = null;
    this._rootCleanup();
  }

  private _rootCleanup(): void {
    this.root = null;
    this._viewport = null;
    this._column = null;
    this._content = null;
  }
}
