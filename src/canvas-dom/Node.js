// Node.js - lightweight DOM-like node model with style props.

let _uid = 0;

const DEFAULT_STYLE = {
  display: 'block', // block | inline | inline-block | flex | none
  position: 'static', // static | relative | absolute
  width: 'auto',
  height: 'auto',
  minHeight: null,
  maxHeight: null,
  padding: 0,
  margin: 0,
  color: '#000000',
  background: 'transparent',
  hoverBackground: null,
  activeBackground: null,
  hoverColor: null,
  fontSize: 14,
  fontFamily: 'sans-serif',
  fontWeight: 'normal',
  fontStyle: 'normal', // normal | italic
  textDecoration: 'none', // none | underline | line-through
  lineHeight: 1.4,
  textAlign: 'left',
  whiteSpace: 'normal', // normal | nowrap | pre | pre-wrap
  verticalAlign: 'baseline', // baseline | top | middle | bottom
  borderRadius: 0,
  border: 0,
  borderColor: '#000000',
  overflow: 'visible', // visible | hidden | scroll | auto
  opacity: 1,
  // flex container
  flexDirection: 'row', // row | column
  flexWrap: 'nowrap', // nowrap | wrap
  justifyContent: 'flex-start', // flex-start | center | flex-end | space-between | space-around
  alignItems: 'stretch', // stretch | flex-start | center | flex-end
  gap: 0,
  // flex item
  flex: '0 1 auto', // CSS initial value: grow shrink basis
  src: null, // for image
  alt: '',
  value: '', // for input/button
  placeholder: '',
  type: 'text', // input type
  cursor: 'default',
  href: null, // for link
  userSelect: 'text', // text | none
  leadingIcon: null,
  leadingChevron: null,
  iconColor: null,
  trailingChevron: null,
  chevronColor: null,
  trailingStats: null,
  trailingStatus: null,
  statusColor: null,
  textOverflow: 'clip',
  hoverDecoration: null
};

export class Node {
  constructor(tag = 'div', style = {}, textContent = '') {
    this.id = ++_uid;
    this.tag = tag.toLowerCase();
    this.style = Object.assign({}, DEFAULT_STYLE, style);
    this.textContent = textContent;
    this.children = [];
    this.parent = null;

    // layout results (filled by layout engine)
    this._x = 0;
    this._y = 0;
    this._width = 0;
    this._height = 0;
    this._contentWidth = 0;
    this._contentHeight = 0;
    this._scrollX = 0;
    this._scrollY = 0;
    this._maxScrollX = 0;
    this._maxScrollY = 0;

    // image cache
    this._img = null;
    this._imgLoaded = false;

    // interaction state
    this._hover = false;
    this._active = false;
    this._focused = false;
    this._dirty = true;

    // text-line geometry cache (filled by painter for selection hit testing)
    // each entry: { text, x, y, w, fs, lh, offset }  (offset = char start offset within textContent)
    this._textLines = null;
    // streaming buffer (used by appendStream / typewriter)
    this._streamFull = '';   // full accumulated text
    this._streamShown = 0;   // number of chars currently revealed
  }

  appendChild(child) {
    if (child.parent) child.parent.removeChild(child);
    child.parent = this;
    this.children.push(child);
    this._dirty = true;
    return child;
  }

  removeChild(child) {
    const i = this.children.indexOf(child);
    if (i >= 0) {
      this.children.splice(i, 1);
      child.parent = null;
      this._dirty = true;
    }
    return child;
  }

  setStyle(patch) {
    Object.assign(this.style, patch);
    this._dirty = true;
    let p = this.parent;
    while (p) { p._dirty = true; p = p.parent; }
  }

  setText(text) {
    this.textContent = String(text);
    this._streamFull = this.textContent;
    this._streamShown = this.textContent.length;
    this._dirty = true;
  }

  // Streaming API: append a chunk to the underlying buffer.
  // If typewriter is true the visible text grows progressively via the renderer.
  appendStream(chunk, { typewriter = false } = {}) {
    this._streamFull += String(chunk);
    if (typewriter) {
      // keep visible portion unchanged; renderer will reveal more each frame
    } else {
      this._streamShown = this._streamFull.length;
    }
    this.textContent = this._streamFull.slice(0, this._streamShown);
    this._dirty = true;
    let p = this.parent;
    while (p) { p._dirty = true; p = p.parent; }
  }

  // Reveal n more chars from the streaming buffer (used by typewriter mode).
  revealStream(n = 1) {
    if (this._streamShown >= this._streamFull.length) return false;
    this._streamShown = Math.min(this._streamFull.length, this._streamShown + n);
    this.textContent = this._streamFull.slice(0, this._streamShown);
    this._dirty = true;
    let p = this.parent;
    while (p) { p._dirty = true; p = p.parent; }
    return true;
  }

  hasStreamPending() {
    return this._streamShown < this._streamFull.length;
  }

  finishStream() {
    this._streamShown = this._streamFull.length;
    this.textContent = this._streamFull;
    this._dirty = true;
    let p = this.parent;
    while (p) { p._dirty = true; p = p.parent; }
  }
}

export function h(tag, style = {}, textContent = '', children = []) {
  const n = new Node(tag, style, textContent);
  for (const c of children) n.appendChild(c);
  return n;
}
