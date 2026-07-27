// icons.ts — transcript 内所需图标的形状数据（从 src/components/icons.tsx 的 SVG
// 提取，24x24 viewBox、stroke currentColor、stroke-width 2、round caps）。
// painter 按 size/24 缩放绘制，与 SVG 渲染一致。

import { Node } from "./node.js";
import type { IconShape } from "./painter.js";

const p = (d: string): IconShape => ({ kind: "path", d });
const c = (cx: number, cy: number, r: number): IconShape => ({ kind: "circle", cx, cy, r });
const r = (x: number, y: number, w: number, h: number, rx = 0): IconShape => ({
  kind: "rect",
  x,
  y,
  w,
  h,
  rx,
});

export const IconFile: IconShape[] = [
  p("M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"),
  p("M14 2v4a2 2 0 0 0 2 2h4"),
];
export const IconPencil: IconShape[] = [
  p("M21.17 6.83a2.83 2.83 0 0 0-4-4L3 17v4h4Z"),
  p("m15 5 4 4"),
];
export const IconTrash: IconShape[] = [
  p("M3 6h18"),
  p("M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"),
  p("M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"),
];
export const IconMove: IconShape[] = [
  p("M5 9l-3 3 3 3M9 5l3-3 3 3M15 19l-3 3-3-3M19 9l3 3-3 3M2 12h20M12 2v20"),
];
export const IconSearch: IconShape[] = [c(11, 11, 8), p("m21 21-4.3-4.3")];
export const IconTerminal: IconShape[] = [p("m4 17 6-6-6-6"), p("M12 19h8")];
export const IconBrain: IconShape[] = [
  p("M12 5a3 3 0 1 0-5.997.125 4 4 0 0 0-2.526 5.77 4 4 0 0 0 .556 6.588A4 4 0 1 0 12 18Z"),
  p("M12 5a3 3 0 1 1 5.997.125 4 4 0 0 1 2.526 5.77 4 4 0 0 1-.556 6.588A4 4 0 1 1 12 18Z"),
];
export const IconGlobe: IconShape[] = [
  c(12, 12, 10),
  p("M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20M2 12h20"),
];
export const IconWrench: IconShape[] = [
  p("M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z"),
];
export const IconChevronOpen: IconShape[] = [p("m6 9 6 6 6-6")];
export const IconChevronClosed: IconShape[] = [p("m9 6 6 6-6 6")];
export const IconCopy: IconShape[] = [
  r(9, 9, 13, 13, 2),
  p("M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"),
];
export const IconCheck: IconShape[] = [p("M20 6 9 17l-5-5")];
export const IconUndo: IconShape[] = [p("M3 7v6h6"), p("M21 17a9 9 0 0 0-9-9 9 9 0 0 0-6.7 3L3 13")];
export const IconFolder: IconShape[] = [
  p("M4 20h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13c0 1.1.9 2 2 2Z"),
];

/** 与 icons.tsx 的 toolIcon(kind) 一致 */
export function toolIconShapes(kind: string): IconShape[] {
  switch (kind) {
    case "read":
      return IconFile;
    case "edit":
      return IconPencil;
    case "delete":
      return IconTrash;
    case "move":
      return IconMove;
    case "search":
      return IconSearch;
    case "execute":
      return IconTerminal;
    case "think":
      return IconBrain;
    case "fetch":
      return IconGlobe;
    default:
      return IconWrench;
  }
}

/** 便捷构造：图标节点 */
export function iconNode(
  shapes: IconShape[],
  size: number,
  style: Parameters<typeof Object.assign>[1] = {},
): Node {
  const n = new Node("icon", {
    width: size,
    height: size,
    fontSize: size,
    userSelect: "none",
    ...(style as object),
  });
  n.data = { ...(n.data ?? {}), iconShapes: shapes };
  return n;
}

/** 便捷构造：chevron（open 决定是否展开态） */
export function chevronNode(open: boolean, size = 12, style = {}): Node {
  return iconNode(open ? IconChevronOpen : IconChevronClosed, size, style);
}

/** 便捷构造：spinner 节点（small = 11px/1.5px，否则 12px/2px） */
export function spinnerNode(small = false, style = {}): Node {
  const size = small ? 11 : 12;
  const n = new Node("spinner", {
    width: size,
    height: size,
    fontSize: size,
    userSelect: "none",
    ...(style as object),
  });
  n.data = { ...(n.data ?? {}), lineWidth: small ? 1.5 : 2 };
  n.animates = true;
  return n;
}
