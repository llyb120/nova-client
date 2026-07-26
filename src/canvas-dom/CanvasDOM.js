// CanvasDOM.js - main renderer entry point.
// Renders a Node tree onto a canvas with interaction + simulated scrolling.

import { Node, h } from './Node.js';
import { layout } from './layout.js';
import { paint, hitTextPosition, paintSelection, selectionText, collectTextNodes, treeHasBusyStatus, treeHasBusyStatusVisible, collectBusyStatusVisible, hoverPaintTarget, nodeScreenBounds, findNodeTitle } from './painter.js';
import { hitTest, findScrollable, scrollBy } from './interaction.js';
import { parseMarkdown } from './markdown.js';

export { Node, h };
export { measureText, LAYOUT_REV } from './layout.js';
export { parseMarkdown };

export class CanvasDOM {
  constructor(canvas, options = {}) {
    this.canvas = canvas;
    // Keep focusable for copy shortcuts; hide browser focus ring.
    canvas.style.outline = 'none';
    this.ctx = canvas.getContext('2d', { alpha: true });
    this.root = null;
    this.dpr = options.dpr || (typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1);
    this.width = 0;
    this.height = 0;

    this._rafId = 0;
    this._dirty = true;
    this._needsLayout = true;
    this._hoverOnly = false;
    this._hoverDirtyNodes = null;
    this._hoverPaintKey = null;
    this._caretOn = true;
    this._caretTimer = 0;
    this._spinPhase = 0;
    this._lastSpinFrame = -1;
    this._busyProbeFrame = -1;
    this._hasBusy = false;
    this._fullPaint = false;
    this._paintScrollX = 0;
    this._paintScrollY = 0;
    this._paintScrollValid = false;
    this._blitCanvas = null;
    this._blitCtx = null;

    // interaction state
    this._hoverNode = null;
    this._activeNode = null;
    this._focusedNode = null;
    this._scrollTarget = null;
    this._velocityY = 0;
    this._velocityX = 0;
    this._lastWheelTime = 0;
    this._dragging = false;
    this._lastDragY = 0;
    this._lastDragX = 0;
    this._lastDragTime = 0;

    // text selection state
    this._selection = null;     // { startNode, startOffset, endNode, endOffset }
    this._selecting = false;    // currently dragging to select text
    this._streamingNodes = new Set(); // nodes with pending typewriter stream

    this._listeners = []; // user event listeners
    this._boundHandlers = {};

    this._bindEvents();
    this.resize();
  }

  setRoot(node) {
    this.root = node;
    this._loadImages(node);
    // 布局必须与树替换同步完成。否则点击展开到下一帧之间，新根的 maxScroll 仍为 0，
    // 吸底、时间线定位或紧接着发生的滚轮事件会把位置错误地夹到顶部。
    this._needsLayout = true;
    this._hoverOnly = false;
    this._hoverDirtyNodes = null;
    this._hasBusy = false;
    this._fullPaint = true;
    this._paintScrollValid = false;
    this._render();
    this._dirty = false;
  }

  /** @returns {boolean} true if the backing-store size changed */
  resize(w, h) {
    const width = Math.max(1, w || this.canvas.clientWidth || this.canvas.width || 1);
    const height = Math.max(1, h || this.canvas.clientHeight || this.canvas.height || 1);
    const bw = Math.max(1, Math.round(width * this.dpr));
    const bh = Math.max(1, Math.round(height * this.dpr));
    if (this.width === width && this.height === height && this.canvas.width === bw && this.canvas.height === bh) {
      return false;
    }
    this.width = width;
    this.height = height;
    // 赋值 canvas.width/height 会立刻清空位图；若等到下一帧 RAF 再画，合成器会闪一帧空白。
    this.canvas.width = bw;
    this.canvas.height = bh;
    this.ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    this._needsLayout = true;
    this._hoverOnly = false;
    this._hoverDirtyNodes = null;
    this._fullPaint = true;
    this._paintScrollValid = false;
    if (this.root) {
      this._render();
      this._dirty = false;
    } else {
      this._dirty = true;
    }
    return true;
  }

  /** @param {boolean} [needsLayout=true] hover/busy/caret 等仅重绘，不重排。 */
  invalidate(needsLayout = true) {
    if (needsLayout) this._needsLayout = true;
    this._dirty = true;
    this._fullPaint = true;
    this._hoverOnly = false;
    this._hoverDirtyNodes = null;
    this.requestRender();
  }

  /** 仅局部视觉变化：合并脏节点，尽量脏矩形重绘，避免整画布 clear+paint。 */
  _invalidateHover(nodes) {
    const list = [];
    const seen = new Set();
    for (const node of nodes || []) {
      if (!node || seen.has(node.id)) continue;
      seen.add(node.id);
      list.push(node);
    }
    this._dirty = true;
    if (!this._rafId || this._hoverOnly || !this._fullPaint) {
      if (!this._fullPaint) this._hoverOnly = true;
      if (!this._hoverDirtyNodes || !this._hoverDirtyNodes.length) {
        this._hoverDirtyNodes = list;
      } else if (list.length) {
        for (const node of this._hoverDirtyNodes) seen.add(node.id);
        for (const node of list) {
          if (seen.has(node.id)) continue;
          seen.add(node.id);
          this._hoverDirtyNodes.push(node);
        }
      }
    } else {
      // 已有全量重绘在排队：保留/合并脏节点，供滚动 blit 后补绘。
      this._hoverOnly = false;
      if (!this._hoverDirtyNodes || !this._hoverDirtyNodes.length) {
        this._hoverDirtyNodes = list;
      } else if (list.length) {
        for (const node of this._hoverDirtyNodes) seen.add(node.id);
        for (const node of list) {
          if (seen.has(node.id)) continue;
          this._hoverDirtyNodes.push(node);
        }
      }
    }
    this.requestRender();
  }

  /** 在当前 tick 内把节点并入脏矩形路径（不再额外排队 RAF）。 */
  _markPaintNodes(nodes) {
    if (this._selection || this._needsLayout) {
      this._fullPaint = true;
      this._hoverOnly = false;
      this._hoverDirtyNodes = null;
      this._dirty = true;
      return;
    }
    const list = [];
    const seen = new Set();
    for (const node of nodes || []) {
      if (!node || seen.has(node.id)) continue;
      seen.add(node.id);
      list.push(node);
    }
    if (!list.length) return;
    this._dirty = true;
    if (!this._hoverDirtyNodes) this._hoverDirtyNodes = [];
    for (const node of this._hoverDirtyNodes) seen.add(node.id);
    for (const node of list) {
      if (seen.has(node.id)) continue;
      this._hoverDirtyNodes.push(node);
    }
    // 全量帧（滚动/流式/invalidate）不降级；否则走脏矩形。
    if (!this._fullPaint) this._hoverOnly = true;
  }

  requestRender() {
    if (this._rafId) return;
    this._rafId = requestAnimationFrame(this._tick);
  }

  _tick = () => {
    this._rafId = 0;
    // inertia
    if (Math.abs(this._velocityY) > 0.5 || Math.abs(this._velocityX) > 0.5) {
      const t = this._scrollTarget;
      if (t) {
        const moved = scrollBy(t, this._velocityX, this._velocityY);
        this._velocityX *= 0.92;
        this._velocityY *= 0.92;
        if (!moved || (Math.abs(this._velocityY) < 0.5 && Math.abs(this._velocityX) < 0.5)) {
          this._velocityX = 0;
          this._velocityY = 0;
        }
        this._dirty = true;
        this._fullPaint = true;
        this._hoverOnly = false;
        if (moved) this._emit('scroll', t);
      }
    }
    // typewriter streaming: reveal a few chars per frame
    if (this._streamingNodes.size > 0) {
      const speed = this._streamSpeed || 2;
      for (const n of this._streamingNodes) {
        if (n.hasStreamPending()) n.revealStream(speed);
        else this._streamingNodes.delete(n);
      }
      this._needsLayout = true;
      this._dirty = true;
      this._fullPaint = true;
      this._hoverOnly = false;
    }
    // caret blink：只重绘输入框区域
    const now = performance.now();
    if (this._focusedNode && now - this._caretTimer > 530) {
      this._caretOn = !this._caretOn;
      this._caretTimer = now;
      this._markPaintNodes([this._focusedNode]);
    }
    // busy 转圈降到约 20fps；仅视口内 busy 才触发绘制，避免长会话空转全量重绘。
    let keepBusyTick = false;
    let visibleBusy = false;
    let viewTop = 0;
    let viewBottom = 0;
    if (this.root) {
      const sy = this.root._scrollY || 0;
      viewTop = sy - 80;
      viewBottom = sy + this.height + 80;
      visibleBusy = treeHasBusyStatusVisible(this.root, viewTop, viewBottom);
      if (visibleBusy) {
        this._hasBusy = true;
        keepBusyTick = true;
      } else {
        const probe = Math.floor(now / 250);
        if (probe !== this._busyProbeFrame) {
          this._busyProbeFrame = probe;
          this._hasBusy = treeHasBusyStatus(this.root);
        }
        keepBusyTick = this._hasBusy;
      }
    } else {
      this._hasBusy = false;
    }
    if (visibleBusy) {
      const frame = Math.floor(now / 50);
      if (frame !== this._lastSpinFrame) {
        this._lastSpinFrame = frame;
        this._spinPhase = (now / 800) % 1;
        this._markPaintNodes(collectBusyStatusVisible(this.root, viewTop, viewBottom));
      }
    }
    if (this._dirty) {
      this._render();
      this._dirty = false;
      this._fullPaint = false;
      this._hoverOnly = false;
      this._hoverDirtyNodes = null;
    }
    if (this._velocityX || this._velocityY || this._focusedNode || this._streamingNodes.size > 0 || keepBusyTick) {
      this.requestRender();
    }
  };

  _render() {
    if (!this.root) return;
    if (this._needsLayout) {
      layout(this.root, 0, 0, this.width, this.height);
      this._clampScrollTree(this.root);
      this._needsLayout = false;
      this._hoverOnly = false;
      this._fullPaint = true;
      this._paintScrollValid = false;
    }
    const sx = this.root._scrollX || 0;
    const sy = this.root._scrollY || 0;
    const dirtyNodes = !this._selection
      && this._hoverDirtyNodes && this._hoverDirtyNodes.length
      ? this._hoverDirtyNodes : null;
    const dsy = sy - this._paintScrollY;
    const dsx = sx - this._paintScrollX;
    const canBlitScroll = !this._selection && this._paintScrollValid
      && dsx === 0 && dsy !== 0 && Math.abs(dsy) < this.height;
    const localOnly = !!(dirtyNodes && this._hoverOnly && !this._fullPaint);

    if (canBlitScroll) {
      this._blitRootScroll(dsy);
      if (dirtyNodes) this._paintDirtyNodes(dirtyNodes);
    } else if (localOnly) {
      this._paintDirtyNodes(dirtyNodes);
    } else {
      this.ctx.clearRect(0, 0, this.width, this.height);
      paint(this.ctx, this.root, 0, 0, this.width, this.height, this);
    }
    // selection overlay (drawn after text so highlight sits on top)
    if (this._selection) {
      paintSelection(this.ctx, this.root, this._selection);
    }
    this._paintScrollX = sx;
    this._paintScrollY = sy;
    this._paintScrollValid = true;
  }

  _paintDirtyNodes(nodes) {
    const pad = 3;
    for (const node of nodes || []) {
      const b = nodeScreenBounds(node);
      if (!b) continue;
      const x = Math.max(0, Math.floor(b.x - pad));
      const y = Math.max(0, Math.floor(b.y - pad));
      const w = Math.min(this.width - x, Math.ceil(b.w + pad * 2));
      const h = Math.min(this.height - y, Math.ceil(b.h + pad * 2));
      if (w <= 0 || h <= 0) continue;
      this.ctx.save();
      this.ctx.beginPath();
      this.ctx.rect(x, y, w, h);
      this.ctx.clip();
      this.ctx.clearRect(x, y, w, h);
      paint(this.ctx, this.root, x, y, w, h, this);
      this.ctx.restore();
    }
  }

  _ensureBlitCanvas() {
    const bw = this.canvas.width;
    const bh = this.canvas.height;
    if (!this._blitCanvas) {
      this._blitCanvas = document.createElement('canvas');
      this._blitCtx = this._blitCanvas.getContext('2d');
    }
    if (this._blitCanvas.width !== bw || this._blitCanvas.height !== bh) {
      this._blitCanvas.width = bw;
      this._blitCanvas.height = bh;
    }
    return this._blitCtx;
  }

  /**
   * 根滚动：把上一帧位图平移，只重绘露出的水平条带 + 右侧滚动条槽。
   * 避免滚轮/惯性时每帧整树 clear+paint。
   */
  _blitRootScroll(dsy) {
    if (!this._blitCtx && typeof document === 'undefined') return false;
    const blit = this._ensureBlitCanvas();
    if (!blit) return false;
    const bw = this.canvas.width;
    const bh = this.canvas.height;
    const dpr = this.dpr || 1;
    // 经离屏缓冲拷贝，避免同源 canvas 重叠 drawImage 的未定义行为。
    blit.setTransform(1, 0, 0, 1, 0, 0);
    blit.clearRect(0, 0, bw, bh);
    blit.drawImage(this.canvas, 0, 0);
    this.ctx.setTransform(1, 0, 0, 1, 0, 0);
    this.ctx.clearRect(0, 0, bw, bh);
    // scrollY 增大 → 内容上移
    this.ctx.drawImage(this._blitCanvas, 0, Math.round(-dsy * dpr));
    this.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

    const bandY = dsy > 0 ? this.height - dsy : 0;
    const bandH = Math.abs(dsy);
    this.ctx.save();
    this.ctx.beginPath();
    this.ctx.rect(0, bandY, this.width, bandH);
    this.ctx.clip();
    this.ctx.clearRect(0, bandY, this.width, bandH);
    paint(this.ctx, this.root, 0, bandY, this.width, bandH, this);
    this.ctx.restore();

    // 滚动条槽是屏坐标固定的，平移后必须重绘，否则拇指位置错位。
    const trackW = (this.root.style.scrollbarWidth || 4) + 8;
    const gx = Math.max(0, this.width - trackW);
    if (trackW > 0 && this.root._maxScrollY > 0) {
      this.ctx.save();
      this.ctx.beginPath();
      this.ctx.rect(gx, 0, trackW, this.height);
      this.ctx.clip();
      this.ctx.clearRect(gx, 0, trackW, this.height);
      paint(this.ctx, this.root, gx, 0, trackW, this.height, this);
      this.ctx.restore();
    }
    return true;
  }

  _clampScrollTree(node) {
    node._scrollX = Math.max(0, Math.min(node._maxScrollX, node._scrollX || 0));
    node._scrollY = node._stickScrollBottom
      ? node._maxScrollY
      : Math.max(0, Math.min(node._maxScrollY, node._scrollY || 0));
    node._stickScrollBottom = false;
    for (const child of node.children) this._clampScrollTree(child);
  }

  _loadImages(node) {
    if (node.tag === 'img' && node.style.src && !node._img) {
      const img = new Image();
      img.onload = () => {
        node._img = img;
        node._imgLoaded = true;
        this.invalidate(true);
      };
      img.onerror = () => { node._imgLoaded = false; };
      img.src = node.style.src;
    }
    for (const c of node.children) this._loadImages(c);
  }

  // ---- events ----
  _bindEvents() {
    const on = (type, fn, opts) => {
      this.canvas.addEventListener(type, fn, opts);
      this._boundHandlers[type] = fn;
    };
    on('mousemove', this._onMouseMove);
    on('mousedown', this._onMouseDown);
    on('mouseup', this._onMouseUp);
    on('mouseleave', this._onMouseLeave);
    on('wheel', this._onWheel, { passive: false });
    on('click', this._onClick);
    on('keydown', this._onKeyDown);
    on('keypress', this._onKeyPress);
    on('compositionend', this._onCompositionEnd);
    on('paste', this._onPaste);
    on('blur', this._onBlur);
    on('copy', this._onCopy);
    // keyboard: also listen on window when canvas has focus so Ctrl+C works
    this._onKeyDownCopy = (e) => {
      if ((e.ctrlKey || e.metaKey) && (e.key === 'c' || e.key === 'C')) {
        if (this._selection && document.activeElement === this.canvas) {
          this._copySelection();
        }
      }
    };
    window.addEventListener('keydown', this._onKeyDownCopy);
    this._boundHandlers._windowKeydown = this._onKeyDownCopy;
  }

  _pos(e) {
    const r = this.canvas.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  }

  _onMouseMove = (e) => {
    const { x, y } = this._pos(e);
    // text selection drag
    if (this._selecting) {
      const pos = this.root ? hitTextPosition(this.root, x, y) : null;
      if (pos) {
        this._selection = this._normalizeSelection({
          startNode: this._selectionStart.node,
          startOffset: this._selectionStart.offset,
          endNode: pos.node,
          endOffset: pos.offset
        });
        this.invalidate(false);
      }
      return;
    }
    // drag scroll
    if (this._dragging && this._scrollTarget) {
      const dy = y - this._lastDragY;
      const dx = x - this._lastDragX;
      const now = performance.now();
      const dt = Math.max(1, now - this._lastDragTime);
      this._velocityY = dy / dt * 16;
      this._velocityX = dx / dt * 16;
      scrollBy(this._scrollTarget, -dx, -dy);
      this._lastDragY = y;
      this._lastDragX = x;
      this._lastDragTime = now;
      this.invalidate(false);
      return;
    }
    const hit = this.root ? hitTest(this.root, x, y) : null;
    if (hit !== this._hoverNode) {
      const prevTarget = hoverPaintTarget(this._hoverNode);
      if (this._hoverNode) this._hoverNode._hover = false;
      this._hoverNode = hit;
      if (hit) {
        hit._hover = true;
        const isInteractive = hit.tag === 'button' || hit.tag === 'input' || (hit.tag === 'a' && hit.style.href);
        const cursor = (hit.style.cursor && hit.style.cursor !== 'default') ? hit.style.cursor : (isInteractive ? 'pointer' : 'default');
        this.canvas.style.cursor = cursor;
      } else {
        this.canvas.style.cursor = 'default';
      }
      const nextTarget = hoverPaintTarget(hit);
      const title = findNodeTitle(hit);
      if (this.canvas.title !== title) this.canvas.title = title;
      if ((prevTarget?.key || null) !== (nextTarget?.key || null)) {
        this._hoverPaintKey = nextTarget?.key || null;
        this._invalidateHover([prevTarget?.dirty, nextTarget?.dirty]);
      }
    } else if (hit) {
      // update cursor for text selection
      const isInteractive = hit.tag === 'button' || hit.tag === 'input' || (hit.tag === 'a' && hit.style.href);
      const cursor = (hit.style.cursor && hit.style.cursor !== 'default') ? hit.style.cursor : (isInteractive ? 'pointer'
        : (hit.textContent && hit.style.userSelect !== 'none' ? 'text' : 'default'));
      this.canvas.style.cursor = cursor;
    }
  };

  _isSelectableText(node) {
    if (!node) return false;
    if (node.tag === 'input' || node.tag === 'button' || node.tag === 'img') return false;
    if (!node.textContent) return false;
    if (node.style.userSelect === 'none') return false;
    return true;
  }

  _onMouseDown = (e) => {
    // 新的直接操作应立即终止上一段滚动惯性，避免点击工具后旧滚动目标继续移动。
    this._velocityX = 0;
    this._velocityY = 0;
    this._scrollTarget = null;
    const { x, y } = this._pos(e);
    const hit = this.root ? hitTest(this.root, x, y) : null;
    if (hit) {
      hit._active = true;
      this._activeNode = hit;
      // focus input/button
      if (hit.tag === 'input' || hit.tag === 'button') {
        if (this._focusedNode && this._focusedNode !== hit) this._focusedNode._focused = false;
        this._focusedNode = hit;
        hit._focused = true;
        this._caretOn = true;
        this._caretTimer = performance.now();
        // ensure canvas has keyboard focus
        this.canvas.setAttribute('tabindex', '0');
        this.canvas.focus();
      } else if (this._focusedNode) {
        this._focusedNode._focused = false;
        this._focusedNode = null;
      }

      // text selection: if landed on selectable text, start selection
      if (this._isSelectableText(hit)) {
        const pos = hitTextPosition(this.root, x, y);
        if (pos) {
          this._selecting = true;
          this._selectionStart = { node: pos.node, offset: pos.offset };
          this._selection = {
            startNode: pos.node, startOffset: pos.offset,
            endNode: pos.node, endOffset: pos.offset
          };
          this.canvas.setAttribute('tabindex', '0');
          this.canvas.focus();
          this.invalidate(false);
          return;
        }
      }

      // start drag scroll on scrollable
      const scroller = findScrollable(hit, 0, 1);
      if (scroller) {
        this._scrollTarget = scroller;
        this._dragging = true;
        this._lastDragX = x;
        this._lastDragY = y;
        this._lastDragTime = performance.now();
        this._velocityX = 0;
        this._velocityY = 0;
        // clicking inside scrollable text clears selection
        this._selection = null;
      } else {
        this._selection = null;
      }
      this.invalidate(false);
    } else {
      this._selection = null;
      this.invalidate(false);
    }
  };

  _onMouseUp = (e) => {
    if (this._activeNode) {
      this._activeNode._active = false;
      this._activeNode = null;
    }
    if (this._selecting) {
      this._selecting = false;
      // if selection collapsed to a single point, clear it
      if (this._selection && this._selection.startNode === this._selection.endNode
          && this._selection.startOffset === this._selection.endOffset) {
        this._selection = null;
      }
      this.invalidate(false);
    }
    if (this._dragging) {
      this._dragging = false;
      // keep scrollTarget for inertia
      if (Math.abs(this._velocityY) < 0.5 && Math.abs(this._velocityX) < 0.5) {
        this._scrollTarget = null;
      }
    }
    this.invalidate(false);
  };

  _onMouseLeave = () => {
    if (this._hoverNode) {
      const prevTarget = hoverPaintTarget(this._hoverNode);
      this._hoverNode._hover = false;
      this._hoverNode = null;
      this._hoverPaintKey = null;
      if (this.canvas.title) this.canvas.title = '';
      if (prevTarget) this._invalidateHover([prevTarget.dirty]);
    } else if (this.canvas.title) {
      this.canvas.title = '';
    }
  };

  _onWheel = (e) => {
    e.preventDefault();
    const { x, y } = this._pos(e);
    const hit = this.root ? hitTest(this.root, x, y) : null;
    if (!hit) return;
    const scroller = findScrollable(hit, e.deltaX, e.deltaY);
    if (scroller) {
      const moved = scrollBy(scroller, e.deltaX, e.deltaY);
      if (moved) {
        this._scrollTarget = scroller;
        this._velocityX = e.deltaX * 0.5;
        this._velocityY = e.deltaY * 0.5;
        this._emit('scroll', scroller);
        this.invalidate(false);
      }
    }
  };

  _onClick = (e) => {
    const { x, y } = this._pos(e);
    const hit = this.root ? hitTest(this.root, x, y) : null;
    if (hit && hit.tag === 'button') {
      this._emit('click', hit);
    }
    if (hit && hit.tag === 'a' && hit.style.href) {
      try { window.open(hit.style.href, '_blank', 'noopener'); } catch (err) {}
      this._emit('click', hit);
    }
  };

  _onKeyDown = (e) => {
    if (!this._focusedNode || this._focusedNode.tag !== 'input') return;
    if (e.key === 'Backspace') {
      e.preventDefault();
      const v = String(this._focusedNode.style.value || '');
      if (v.length) {
        this._focusedNode.style.value = v.slice(0, -1);
        this._focusedNode._dirty = true;
        this._emit('input', this._focusedNode);
        this.invalidate(true);
      }
    } else if (e.key === 'Enter') {
      e.preventDefault();
      this._emit('submit', this._focusedNode);
    }
  };

  _appendInputText(text) {
    if (!this._focusedNode || this._focusedNode.tag !== 'input' || !text) return;
    this._focusedNode.style.value = (this._focusedNode.style.value || '') + text;
    this._focusedNode._dirty = true;
    this._caretOn = true;
    this._caretTimer = performance.now();
    this._emit('input', this._focusedNode);
    this.invalidate(true);
  }

  _onKeyPress = (e) => {
    if (!this._focusedNode || this._focusedNode.tag !== 'input') return;
    if (!e.key || e.key.length !== 1 || e.isComposing) return;
    e.preventDefault();
    this._appendInputText(e.key);
  };

  _onCompositionEnd = (e) => {
    if (!this._focusedNode || this._focusedNode.tag !== 'input') return;
    e.preventDefault();
    this._appendInputText(e.data || '');
  };

  _onPaste = (e) => {
    if (!this._focusedNode || this._focusedNode.tag !== 'input') return;
    const text = e.clipboardData?.getData('text/plain') || '';
    if (!text) return;
    e.preventDefault();
    this._appendInputText(text.replace(/[\r\n]+/g, ' '));
  };

  _onBlur = () => {
    if (this._focusedNode) {
      this._focusedNode._focused = false;
      this._focusedNode = null;
      this.invalidate(false);
    }
  };

  on(type, fn) { this._listeners.push({ type, fn }); }
  _emit(type, node) {
    for (const l of this._listeners) if (l.type === type) l.fn(node);
  }

  // ---- selection helpers ----
  _normalizeSelection(sel) {
    const nodes = collectTextNodes(this.root);
    const si = nodes.indexOf(sel.startNode);
    const ei = nodes.indexOf(sel.endNode);
    if (si === -1 || ei === -1) return sel;
    if (si < ei) return sel;
    if (si === ei && sel.startOffset <= sel.endOffset) return sel;
    // swap
    return {
      startNode: sel.endNode, startOffset: sel.endOffset,
      endNode: sel.startNode, endOffset: sel.startOffset
    };
  }

  _onCopy = (e) => {
    if (!this._selection) return;
    const text = selectionText(this.root, this._selection);
    if (!text) return;
    e.preventDefault();
    if (e.clipboardData) {
      e.clipboardData.setData('text/plain', text);
    } else if (navigator.clipboard) {
      navigator.clipboard.writeText(text);
    }
  };

  _copySelection() {
    if (!this._selection) return;
    const text = selectionText(this.root, this._selection);
    if (!text) return;
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).catch(() => {});
    } else {
      // fallback: temporary textarea
      const ta = document.createElement('textarea');
      ta.value = text;
      ta.style.position = 'fixed';
      ta.style.opacity = '0';
      document.body.appendChild(ta);
      ta.select();
      try { document.execCommand('copy'); } catch (e) {}
      document.body.removeChild(ta);
    }
  }

  getSelectionText() {
    return this._selection ? selectionText(this.root, this._selection) : '';
  }

  hasFocusedInput() {
    return !!this._focusedNode && this._focusedNode.tag === 'input';
  }

  clearSelection() {
    if (this._selection) {
      this._selection = null;
      this.invalidate(false);
    }
  }

  // ---- streaming API ----
  // Append a chunk to a node's text buffer. If typewriter is true, the visible
  // text grows progressively each frame (set speed via setStreamSpeed).
  streamTo(node, chunk, { typewriter = false } = {}) {
    node.appendStream(chunk, { typewriter });
    if (typewriter) this._streamingNodes.add(node);
    this.invalidate(true);
    return node;
  }

  // Append a markdown chunk: re-parse the accumulated buffer into the target
  // container's children. Useful for streaming LLM markdown responses.
  streamMarkdown(container, chunk, { typewriter = false } = {}) {
    if (container._mdBuffer == null) container._mdBuffer = '';
    container._mdBuffer += String(chunk);
    const tree = parseMarkdown(container._mdBuffer, {});
    // replace children
    container.children.length = 0;
    for (const c of [...tree.children]) container.appendChild(c);
    container._dirty = true;
    let p = container.parent;
    while (p) { p._dirty = true; p = p.parent; }
    this.invalidate(true);
    return container;
  }

  setStreamSpeed(charsPerFrame) {
    this._streamSpeed = charsPerFrame;
  }

  finishAllStreams() {
    for (const n of this._streamingNodes) n.finishStream();
    this._streamingNodes.clear();
    this.invalidate(true);
  }

  destroy() {
    if (this._rafId) cancelAnimationFrame(this._rafId);
    for (const [type, fn] of Object.entries(this._boundHandlers)) {
      if (type === '_windowKeydown') {
        window.removeEventListener('keydown', fn);
      } else {
        this.canvas.removeEventListener(type, fn);
      }
    }
  }
}
