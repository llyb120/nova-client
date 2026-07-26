// CanvasDOM.js - main renderer entry point.
// Renders a Node tree onto a canvas with interaction + simulated scrolling.

import { Node, h } from './Node.js';
import { layout } from './layout.js';
import { paint, hitTextPosition, paintSelection, selectionText, collectTextNodes } from './painter.js';
import { hitTest, findScrollable, scrollBy } from './interaction.js';
import { parseMarkdown } from './markdown.js';

export { Node, h };
export { measureText } from './layout.js';
export { parseMarkdown };

export class CanvasDOM {
  constructor(canvas, options = {}) {
    this.canvas = canvas;
    this.ctx = canvas.getContext('2d', { alpha: true });
    this.root = null;
    this.dpr = options.dpr || (typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1);
    this.width = 0;
    this.height = 0;

    this._rafId = 0;
    this._dirty = true;
    this._caretOn = true;
    this._caretTimer = 0;

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
    this._render();
    this._dirty = false;
  }

  resize(w, h) {
    this.width = w || this.canvas.clientWidth || this.canvas.width;
    this.height = h || this.canvas.clientHeight || this.canvas.height;
    this.canvas.width = this.width * this.dpr;
    this.canvas.height = this.height * this.dpr;
    this.ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    this._dirty = true;
    this.requestRender();
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
      this._dirty = true;
    }
    // caret blink
    const now = performance.now();
    if (this._focusedNode && now - this._caretTimer > 530) {
      this._caretOn = !this._caretOn;
      this._caretTimer = now;
      this._dirty = true;
    }
    if (this._dirty) {
      this._render();
      this._dirty = false;
    }
    if (this._velocityX || this._velocityY || this._focusedNode || this._streamingNodes.size > 0) {
      this.requestRender();
    }
  };

  _render() {
    if (!this.root) return;
    layout(this.root, 0, 0, this.width, this.height);
    this._clampScrollTree(this.root);
    this.ctx.clearRect(0, 0, this.width, this.height);
    paint(this.ctx, this.root, 0, 0, this.width, this.height, this);
    // selection overlay (drawn after text so highlight sits on top)
    if (this._selection) {
      paintSelection(this.ctx, this.root, this._selection);
    }
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
        this._dirty = true;
        this.requestRender();
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
        this._dirty = true;
        this.requestRender();
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
      this._dirty = true;
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
        const cursor = (hit.style.cursor && hit.style.cursor !== 'default') ? hit.style.cursor : (isInteractive ? 'pointer' : 'default');
        this.canvas.style.cursor = cursor;
      } else {
        this.canvas.style.cursor = 'default';
      }
      this._dirty = true;
      this.requestRender();
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
          this._dirty = true;
          this.requestRender();
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
      this._dirty = true;
      this.requestRender();
    } else {
      this._selection = null;
      this._dirty = true;
      this.requestRender();
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
      this._dirty = true;
      this.requestRender();
    }
    if (this._dragging) {
      this._dragging = false;
      // keep scrollTarget for inertia
      if (Math.abs(this._velocityY) < 0.5 && Math.abs(this._velocityX) < 0.5) {
        this._scrollTarget = null;
      }
    }
    this._dirty = true;
    this.requestRender();
  };

  _onMouseLeave = () => {
    if (this._hoverNode) {
      this._hoverNode._hover = false;
      this._hoverNode = null;
      this._dirty = true;
      this.requestRender();
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
        this._dirty = true;
        this._emit('scroll', scroller);
        this.requestRender();
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
        this._dirty = true;
        this._emit('input', this._focusedNode);
        this.requestRender();
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
    this._dirty = true;
    this._caretOn = true;
    this._caretTimer = performance.now();
    this._emit('input', this._focusedNode);
    this.requestRender();
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
      this._dirty = true;
      this.requestRender();
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
      this._dirty = true;
      this.requestRender();
    }
  }

  // ---- streaming API ----
  // Append a chunk to a node's text buffer. If typewriter is true, the visible
  // text grows progressively each frame (set speed via setStreamSpeed).
  streamTo(node, chunk, { typewriter = false } = {}) {
    node.appendStream(chunk, { typewriter });
    if (typewriter) this._streamingNodes.add(node);
    this._dirty = true;
    this.requestRender();
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
    this._dirty = true;
    this.requestRender();
    return container;
  }

  setStreamSpeed(charsPerFrame) {
    this._streamSpeed = charsPerFrame;
  }

  finishAllStreams() {
    for (const n of this._streamingNodes) n.finishStream();
    this._streamingNodes.clear();
    this._dirty = true;
    this.requestRender();
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
