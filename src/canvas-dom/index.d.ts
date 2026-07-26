export interface CanvasStyle {
  [key: string]: unknown;
  width?: number | string;
  height?: number | string;
  minHeight?: number | string;
  maxHeight?: number | string;
  overflow?: string;
  value?: string;
  scrollbarWidth?: number;
  scrollbarTrack?: string;
  scrollbarThumb?: string;
  hoverBackground?: string;
  activeBackground?: string;
  hoverColor?: string;
  hoverOpacity?: number | null;
  opacity?: number;
  leadingIcon?: string;
  leadingChevron?: boolean | null;
  iconColor?: string;
  trailingChevron?: boolean | null;
  chevronColor?: string;
}

export class Node {
  constructor(tag?: string, style?: CanvasStyle, textContent?: string);
  tag: string;
  style: CanvasStyle;
  textContent: string;
  children: Node[];
  parent: Node | null;
  _x: number;
  _y: number;
  _width: number;
  _height: number;
  _scrollX: number;
  _scrollY: number;
  _maxScrollX: number;
  _maxScrollY: number;
  _action?: () => void;
  _input?: (value: string) => void;
  _scrollKey?: string;
  _stickScrollBottom?: boolean;
  _groupIndex?: number;
  _controlLines?: string[];
  _wrappedLines?: string[];
  _layoutStable?: boolean;
  _editHoverRoot?: boolean;
  appendChild(child: Node): Node;
  removeChild(child: Node): Node;
  setStyle(patch: CanvasStyle): void;
  setText(text: string): void;
}

export function h(tag: string, style?: CanvasStyle, textContent?: string, children?: Node[]): Node;
export function measureText(text: string, fontSize?: number, fontFamily?: string, fontWeight?: string | number, fontStyle?: string): number;
export function parseMarkdown(markdown: string, baseStyle?: CanvasStyle): Node;

export class CanvasDOM {
  constructor(canvas: HTMLCanvasElement, options?: { dpr?: number });
  root: Node | null;
  width: number;
  height: number;
  setRoot(node: Node): void;
  resize(width?: number, height?: number): void;
  requestRender(): void;
  invalidate(needsLayout?: boolean): void;
  on(type: string, callback: (node: Node) => void): void;
  hasFocusedInput(): boolean;
  destroy(): void;
}
