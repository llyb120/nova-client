import type { Item, ThoughtItem, ToolItem } from "./types";

type ProcessItem = ThoughtItem | ToolItem;
export type ProcessSegment = { type: "process"; id: number; items: ProcessItem[] } | { type: "item"; id: number; item: Item };

export function processSegments(items: Item[]): ProcessSegment[] {
  const segments: ProcessSegment[] = [];
  for (const item of items) {
    const last = segments.at(-1);
    if (item.type === "thought" || item.type === "tool") {
      if (last?.type === "process") last.items.push(item);
      else segments.push({ type: "process", id: item.id, items: [item] });
    } else segments.push({ type: "item", id: item.id, item });
  }
  return segments;
}

const ACTION_LABEL: Record<string, string> = { read: "读取文件", edit: "修改文件", delete: "删除文件", search: "搜索文件与资料", execute: "执行命令", think: "分析思路", fetch: "获取资料" };

/** 折叠行摘要：同类动作带上次数（执行命令 ×3），否则不同段只报类目、看不出做了多少 */
export function processSummary(items: ProcessItem[]): string {
  const tools = items.filter((item): item is ToolItem => item.type === "tool");
  const counts = new Map<string, number>();
  for (const tool of tools) {
    const label = ACTION_LABEL[tool.kind] ?? "调用工具";
    counts.set(label, (counts.get(label) ?? 0) + 1);
  }
  const actions = [...counts].map(([label, n]) => `${label} ×${n}`);
  const failed = tools.filter(item => item.status === "failed").length;
  return (actions.join("、") || "分析思路") + (failed ? `（${failed} 项失败）` : "");
}

export function processLiveLines(items: ProcessItem[], wrap = (text: string) => text.split("\n")): string[] {
  let lines: string[] = [];
  for (let i = items.length - 1; i >= 0 && lines.length < 2; i--) {
    const item = items[i];
    const status = item.type === "tool"
      ? item.status === "failed" ? "失败" : item.status === "completed" ? "已完成" : "进行中"
      : "";
    const text = item.type === "thought" ? item.text.trimEnd() || "思考中…"
      : `${status} · ${(item.title || item.kind).replace(/\s+/g, " ").trim()}`;
    lines = [...wrap(text).slice(-(2 - lines.length)), ...lines];
  }
  return lines;
}
