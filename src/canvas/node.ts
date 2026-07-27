// node.ts — 轻量 DOM 风格节点模型（移植自参考项目 Node.js，按会话需求扩展）。
// 节点 = tag + style + children；布局结果由 layout.ts 写回 _ 开头字段。

let _uid = 0;

export type Edges = { t: number; r: number; b: number; l: number };

/** padding/margin/border 的 CSS 缩写展开（1/2/3/4 值） */
export function edgesOf(v: number | number[] | undefined): Edges {
  if (typeof v === "number") return { t: v, r: v, b: v, l: v };
  if (Array.isArray(v)) {
    if (v.length === 1) return { t: v[0], r: v[0], b: v[0], l: v[0] };
    if (v.length === 2) return { t: v[0], r: v[1], b: v[0], l: v[1] };
    if (v.length === 3) return { t: v[0], r: v[1], b: v[2], l: v[1] };
    if (v.length === 4) return { t: v[0], r: v[1], b: v[2], l: v[3] };
  }
  return { t: 0, r: 0, b: 0, l: 0 };
}

/** border-radius 的展开（1/2/3/4 值 → [tl, tr, br, bl]；2 值 = [左上右下, 右上左下]，3 值 = [左上, 右上左下, 右下]） */
export function radiiOf(v: number | number[] | undefined): [number, number, number, number] {
  if (typeof v === "number") return [v, v, v, v];
  if (Array.isArray(v)) {
    if (v.length === 1) return [v[0], v[0], v[0], v[0]];
    if (v.length === 2) return [v[0], v[1], v[0], v[1]];
    if (v.length === 3) return [v[0], v[1], v[2], v[1]];
    if (v.length === 4) return [v[0], v[1], v[2], v[3]];
  }
  return [0, 0, 0, 0];
}

export type WhiteSpace = "normal" | "nowrap" | "pre" | "pre-wrap";
export type WordBreak = "normal" | "break-all";

export interface Style {
  display: "block" | "inline" | "inline-block" | "flex" | "none";
  position: "static" | "relative" | "absolute" | "sticky";
  width: number | string; // px 数值 | "100%" | "auto" | "fit-content"
  height: number | string;
  minHeight?: number;
  maxHeight?: number;
  maxWidth?: number | string;
  padding: number | number[];
  margin: number | number[];
  color: string;
  background: string;
  fontSize: number;
  fontFamily: string;
  fontWeight: string | number;
  fontStyle: "normal" | "italic";
  textDecoration: "none" | "underline" | "line-through";
  lineHeight: number; // 倍数（CSS 无单位 line-height）
  textAlign: "left" | "center" | "right";
  whiteSpace: WhiteSpace;
  wordBreak: WordBreak;
  verticalAlign: "baseline" | "top" | "middle" | "bottom";
  borderRadius: number | number[];
  border: number | number[];
  borderColor: string | string[]; // 支持每边不同色 [t,r,b,l]
  overflow: "visible" | "hidden" | "scroll" | "auto";
  overflowX?: "visible" | "hidden" | "scroll" | "auto";
  overscrollBehavior: "auto" | "contain" | "none";
  scrollbarGutter?: "auto" | "stable";
  opacity: number;
  flexDirection: "row" | "column";
  flexWrap: "nowrap" | "wrap";
  justifyContent: "flex-start" | "center" | "flex-end" | "space-between" | "space-around";
  alignItems: "stretch" | "flex-start" | "center" | "flex-end" | "baseline";
  alignSelf?: "auto" | "stretch" | "flex-start" | "center" | "flex-end";
  gap: number;
  flex: number | string; // grow [shrink basis]
  textOverflow: "clip" | "ellipsis";
  letterSpacing: number; // px，0 = normal
  src: string | null;
  cursor: string;
  userSelect: "text" | "none";
  title?: string; // hover tooltip 文本（DOM 浮层呈现）
  top?: number | string; // sticky/absolute 偏移
  left?: number | string;
  right?: number | string;
  bottom?: number | string;
}

export const DEFAULT_STYLE: Style = {
  display: "block",
  position: "static",
  width: "auto",
  height: "auto",
  padding: 0,
  margin: 0,
  color: "#000000",
  background: "transparent",
  fontSize: 14,
  fontFamily: "sans-serif",
  fontWeight: "normal",
  fontStyle: "normal",
  textDecoration: "none",
  lineHeight: 1.4,
  textAlign: "left",
  whiteSpace: "normal",
  wordBreak: "normal",
  verticalAlign: "baseline",
  borderRadius: 0,
  border: 0,
  borderColor: "#000000",
  overflow: "visible",
  overscrollBehavior: "auto",
  opacity: 1,
  flexDirection: "row",
  flexWrap: "nowrap",
  justifyContent: "flex-start",
  alignItems: "stretch",
  gap: 0,
  flex: "0 0 auto",
  textOverflow: "clip",
  letterSpacing: 0,
  src: null,
  cursor: "default",
  userSelect: "text",
};

/** 一行排好版的文本（layout 产出；paint/选区/命中共用）。 */
export interface TextLine {
  text: string;
  x: number; // 绝对（布局）坐标
  y: number; // 片段行盒顶（含半 leading）
  w: number;
  lh: number; // 片段行盒高（fs * lineHeight）
  baseline: number; // y 到 alphabetic 基线的距离
  fs: number;
  ascent: number;
  descent: number;
  offset: number; // 起始字符在 owner.textContent 中的下标
  owner: Node; // 文本所属的节点（inline 片段指向各自 inline 子节点）
  font: string; // ctx.font 字符串（缓存）
  /** 长词硬断产生的续片：断裂处不重复 inline 背景 padding */
  continuesFromPrev?: boolean;
  continuesToNext?: boolean;
}

export interface CanvasHitEvent {
  x: number;
  y: number; // 视口（canvas CSS 像素）坐标
  native: MouseEvent;
}

export class Node {
  id = ++_uid;
  key: string | number | undefined;
  tag: string;
  style: Style;
  textContent: string;
  children: Node[] = [];
  parent: Node | null = null;

  /** 任意附属数据（item id、文件路径、scrollKey …） */
  data: Record<string, unknown> | undefined;
  onClick: ((node: Node, e: CanvasHitEvent) => void) | undefined;
  onContextMenu: ((node: Node, e: CanvasHitEvent) => void) | undefined;
  /** hover/active 时的样式补丁（替代 CSS :hover/:active） */
  hoverStyle: Partial<Style> | undefined;
  activeStyle: Partial<Style> | undefined;
  /** 仅在祖先链有 hover 时才显示（.codeblock:hover .code-copy 语义） */
  revealOnHover = false;
  /** hover 容器：revealOnHover 子节点只认最近一个此类祖先的 hover 状态 */
  hoverContainer = false;
  /** tag === "icon" 时的 svg path 数据（24x24 viewBox，stroke currentColor 风格） */
  icon: string | undefined;
  /** 标记节点是否需要逐帧动画（spinner） */
  animates = false;

  // —— 布局结果（layout.ts 写） ——
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
  _textLines: TextLine[] | null = null;

  // —— 增量布局缓存 ——
  _layoutDirty = true;
  _layoutAvailW = Number.NaN;

  // —— 图片 ——
  _img: HTMLImageElement | null = null;
  _imgLoaded = false;
  _imgFailed = false;

  // —— 交互状态（renderer 写） ——
  _hover = false;
  _hoverWithin = false;
  _active = false;
  /** sticky 定位的逐帧偏移（renderer 根据滚动位置计算） */
  _stickyDy = 0;

  constructor(tag = "div", style: Partial<Style> = {}, textContent = "") {
    this.tag = tag.toLowerCase();
    this.style = Object.assign({}, DEFAULT_STYLE, style);
    this.textContent = textContent;
  }

  appendChild(child: Node): Node {
    if (child.parent) child.parent.removeChild(child);
    child.parent = this;
    this.children.push(child);
    this.markLayoutDirty();
    return child;
  }

  removeChild(child: Node): Node {
    const i = this.children.indexOf(child);
    if (i >= 0) {
      this.children.splice(i, 1);
      child.parent = null;
      this.markLayoutDirty();
    }
    return child;
  }

  replaceChildren(children: Node[]): void {
    for (const c of this.children) c.parent = null;
    this.children = children;
    for (const c of children) c.parent = this;
    this.markLayoutDirty();
  }

  setStyle(patch: Partial<Style>): void {
    Object.assign(this.style, patch);
    this.markLayoutDirty();
  }

  setText(text: string): void {
    this.textContent = String(text);
    this.markLayoutDirty();
  }

  /** 内容变化 → 自身与祖先的布局缓存失效（不重排，等下一帧） */
  markLayoutDirty(): void {
    let p: Node | null = this;
    while (p) {
      p._layoutDirty = true;
      p = p.parent;
    }
  }

  /** 深度优先查找（含自身） */
  find(pred: (n: Node) => boolean): Node | null {
    if (pred(this)) return this;
    for (const c of this.children) {
      const r = c.find(pred);
      if (r) return r;
    }
    return null;
  }

  *walk(): Generator<Node> {
    yield this;
    for (const c of this.children) yield* c.walk();
  }
}

export function h(
  tag: string,
  style: Partial<Style> = {},
  textContent = "",
  children: Node[] = [],
): Node {
  const n = new Node(tag, style, textContent);
  for (const c of children) n.appendChild(c);
  return n;
}
