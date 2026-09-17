import type { Item, ThoughtItem, ToolItem } from "./types";

type ProcessItem = ThoughtItem | ToolItem;

function memoryTool(item: ToolItem): "read" | "write" | null {
  if (item.status !== "completed") return null;
  // Only explicit memory tools / known memory files; never infer from prose or shell commands.
  const name = item.title.trim().toLowerCase();
  if (/^(?:mcp__\w+__)?(?:write_memory|save_memory|update_memory|memory_write|memory_update)$/.test(name)) return "write";
  const input = item.rawInput && typeof item.rawInput === "object" ? item.rawInput as Record<string, unknown> : {};
  const paths = [...(item.locations ?? []).map(l => l.path), ...["path", "file_path", "filePath", "filename"].map(k => input[k])]
    .filter((p): p is string => typeof p === "string" && !!p.trim());
  const isMemoryPath = (path: string) => {
    const p = path.replace(/\\/g, "/").toLowerCase();
    return /(?:^|\/)memory\.md$/.test(p) || /(?:^|\/)\.(?:codebuddy|claude)\/(?:[^/]+\/)*memor(?:y|ies)\//.test(p);
  };
  if (!paths.length || !paths.every(isMemoryPath)) return null;
  if (["edit", "write"].includes(item.kind)) return "write";
  return item.kind === "read" ? "read" : null;
}

/** Preserve the usual final-block folding, except for a short acknowledgement after memory maintenance. */
export function splitTurnBody(body: Item[], finished: boolean): { process: Item[]; conclusion: Item[] } {
  if (!finished) return { process: body, conclusion: [] };
  const isText = (item: Item) => item.type === "assistant" || item.type === "system";
  const last = body.findLastIndex(isText);
  if (last < 0) return { process: body, conclusion: [] };
  let first = last;
  while (first > 0 && isText(body[first - 1])) first--;
  const selected = new Set(body.slice(first, last + 1));
  const tail = body.slice(first, last + 1);
  const shortAck = tail.every(it => it.type === "assistant") && tail.map(it => "text" in it ? it.text : "").join("\n").length <= 400;
  // ponytail: a conservative 400-character acknowledgement ceiling; use explicit provider metadata if available later.
  if (shortAck && body.slice(last + 1).every(it => it.type === "thought" || (it.type === "tool" && memoryTool(it)))) {
    let start = first;
    let wrote = false;
    while (start > 0) {
      const item = body[start - 1];
      if (item.type === "thought") { start--; continue; }
      const kind = item.type === "tool" ? memoryTool(item) : null;
      if (!kind) break;
      wrote ||= kind === "write";
      start--;
    }
    if (wrote) {
      while (start > 0 && isText(body[start - 1])) selected.add(body[--start]);
    }
  }
  return { process: body.filter(it => !selected.has(it)), conclusion: body.filter(it => selected.has(it)) };
}
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
