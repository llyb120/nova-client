// Node.ts - lightweight DOM-like node model with style props.

let _uid = 0;

export type DisplayType = 'block' | 'inline' | 'inline-block' | 'flex' | 'none';
export type PositionType = 'static' | 'relative' | 'absolute';
export type OverflowType = 'visible' | 'hidden' | 'scroll' | 'auto';
export type WhiteSpaceType = 'normal' | 'nowrap' | 'pre';
export type UserSelectType = 'text' | 'none';

export interface StyleProps {
  display?: DisplayType;
  position?: PositionType;
  width?: number | string;
  height?: number | string;
  left?: number | string;
  top?: number | string;
  padding?: number | number[];
  margin?: number | number[];
  color?: string;
  background?: string;
  fontSize?: number;
  fontFamily?: string;
  fontWeight?: string | number;
  fontStyle?: 'normal' | 'italic';
  textDecoration?: 'none' | 'underline' | 'line-through';
  lineHeight?: number;
  textAlign?: 'left' | 'center' | 'right';
  whiteSpace?: WhiteSpaceType;
  verticalAlign?: 'baseline' | 'top' | 'middle' | 'bottom';
  borderRadius?: number;
  border?: number | number[];
  borderColor?: string;
  overflow?: OverflowType;
  opacity?: number;
  flexDirection?: 'row' | 'column';
  flexWrap?: 'nowrap' | 'wrap';
  justifyContent?: string;
  alignItems?: string;
  gap?: number;
  flex?: number | string;
  src?: string | null;
  alt?: string;
  value?: string;
  placeholder?: string;
  type?: string;
  cursor?: string;
  href?: string | null;
  userSelect?: UserSelectType;
}

export interface NodeMeta {
  cacheLayer?: boolean;
  onClick?: string | ((node: Node) => void);
  kind?: string;
  groupIndex?: number;
  [key: string]: unknown;
}

export interface TextLineFragment {
  text: string;
  x: number;
  y: number;
  w: number;
  fs: number;
  lh: number;
  offset: number;
}

const DEFAULT_STYLE: Required<
  Pick<
    StyleProps,
    | 'display'
    | 'position'
    | 'width'
    | 'height'
    | 'padding'
    | 'margin'
    | 'color'
    | 'background'
    | 'fontSize'
    | 'fontFamily'
    | 'fontWeight'
    | 'fontStyle'
    | 'textDecoration'
    | 'lineHeight'
    | 'textAlign'
    | 'whiteSpace'
    | 'verticalAlign'
    | 'borderRadius'
    | 'border'
    | 'borderColor'
    | 'overflow'
    | 'opacity'
    | 'flexDirection'
    | 'flexWrap'
    | 'justifyContent'
    | 'alignItems'
    | 'gap'
    | 'flex'
    | 'src'
    | 'alt'
    | 'value'
    | 'placeholder'
    | 'type'
    | 'cursor'
    | 'href'
    | 'userSelect'
  >
> = {
  display: 'block',
  position: 'static',
  width: 'auto',
  height: 'auto',
  padding: 0,
  margin: 0,
  color: '#000000',
  background: 'transparent',
  fontSize: 14,
  fontFamily: 'sans-serif',
  fontWeight: 'normal',
  fontStyle: 'normal',
  textDecoration: 'none',
  lineHeight: 1.4,
  textAlign: 'left',
  whiteSpace: 'normal',
  verticalAlign: 'baseline',
  borderRadius: 0,
  border: 0,
  borderColor: '#000000',
  overflow: 'visible',
  opacity: 1,
  flexDirection: 'row',
  flexWrap: 'nowrap',
  justifyContent: 'flex-start',
  alignItems: 'stretch',
  gap: 0,
  flex: '0 0 auto',
  src: null,
  alt: '',
  value: '',
  placeholder: '',
  type: 'text',
  cursor: 'default',
  href: null,
  userSelect: 'text',
};

export class Node {
  id: number;
  tag: string;
  style: StyleProps & typeof DEFAULT_STYLE;
  textContent: string;
  children: Node[];
  parent: Node | null;
  meta: Record<string, unknown> | null;

  _x = 0;
  _y = 0;
  _width = 0;
  _height = 0;
  _contentWidth = 0;
  _contentHeight = 0;
  _scrollX = 0;
  _scrollY = 0;
  _maxScrollX = 0;
  _maxScrollY = 0;

  _img: HTMLImageElement | null = null;
  _imgLoaded = false;

  _hover = false;
  _active = false;
  _focused = false;
  _dirty = true;
  _layoutDirty = true;

  _layer: OffscreenCanvas | HTMLCanvasElement | null = null;
  _layerValid = false;

  _textLines: TextLineFragment[] | null = null;
  _streamFull = '';
  _streamShown = 0;

  _mdBuffer = '';
  _mdStablePrefix = '';
  _mdStableNodes: Node[] | null = null;

  constructor(tag = 'div', style: StyleProps = {}, textContent = '') {
    this.id = ++_uid;
    this.tag = tag.toLowerCase();
    this.style = Object.assign({}, DEFAULT_STYLE, style);
    this.textContent = textContent;
    this.children = [];
    this.parent = null;
    this.meta = null;

    this._streamFull = this.textContent;
    this._streamShown = this.textContent.length;
  }

  markDirty(): void {
    this._dirty = true;
    this._layoutDirty = true;
    this._layerValid = false;
    let p = this.parent;
    while (p) {
      p._dirty = true;
      p._layoutDirty = true;
      p._layerValid = false;
      p = p.parent;
    }
  }

  appendChild(child: Node): Node {
    if (child.parent) child.parent.removeChild(child);
    child.parent = this;
    this.children.push(child);
    this.markDirty();
    return child;
  }

  removeChild(child: Node): Node {
    const i = this.children.indexOf(child);
    if (i >= 0) {
      this.children.splice(i, 1);
      child.parent = null;
      this.markDirty();
    }
    return child;
  }

  setStyle(patch: StyleProps): void {
    Object.assign(this.style, patch);
    this.markDirty();
  }

  setText(text: string): void {
    this.textContent = String(text);
    this._streamFull = this.textContent;
    this._streamShown = this.textContent.length;
    this.markDirty();
  }

  appendStream(chunk: string, { typewriter = false }: { typewriter?: boolean } = {}): void {
    this._streamFull += String(chunk);
    if (!typewriter) {
      this._streamShown = this._streamFull.length;
    }
    this.textContent = this._streamFull.slice(0, this._streamShown);
    this.markDirty();
  }

  revealStream(n = 1): boolean {
    if (this._streamShown >= this._streamFull.length) return false;
    this._streamShown = Math.min(this._streamFull.length, this._streamShown + n);
    this.textContent = this._streamFull.slice(0, this._streamShown);
    this.markDirty();
    return true;
  }

  hasStreamPending(): boolean {
    return this._streamShown < this._streamFull.length;
  }

  finishStream(): void {
    this._streamShown = this._streamFull.length;
    this.textContent = this._streamFull;
    this.markDirty();
  }
}

export function h(
  tag: string,
  style: StyleProps = {},
  textContent = '',
  children: Node[] = [],
): Node {
  const n = new Node(tag, style, textContent);
  for (const c of children) n.appendChild(c);
  return n;
}
