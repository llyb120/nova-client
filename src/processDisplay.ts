import type { Item, ThoughtItem, ToolItem } from "./types";

type ProcessItem = ThoughtItem | ToolItem;

// 后台交付的状态行。level 是契约；「等待后台任务」只覆盖改 level 之前存下的记录。
function isBackgroundStatus(item: Item): boolean {
  return item.type === "system" && (item.level === "background" || item.text.startsWith("等待后台任务"));
}

/** 旧记录把后台交付存成了用户消息。返回状态行就不要再当成用户提示词；正文只给模型。 */
export function backgroundDeliveryNotice(text: string): string | undefined {
  if (!text.startsWith("[后台命令交付]\n")) return undefined;
  const line = text.split("\n").find(part => part.startsWith("[后台任务"))?.trim() ?? "";
  return line.replace(/^\[/, "").replace("] ", " · ") || "后台任务已结束";
}

/** 分身派发或 wait_agent 会等外部结果：交回前后的回答都属于本轮答复。
 *  title 形如 「Lyra / spawn_agent」；只认工具名位置，避免文件路径里碰巧带同名而误判。 */
function waitingTool(item: Item): boolean {
  if (item.type !== "tool") return false;
  return item.title.includes("/ spawn_agent") || item.title.includes("/ wait_agent");
}

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
  // 兼容旧记录：首个执行模型只是基线，不是切换；后续不同模型才展示，不修改存储记录。
  let previousModel = "";
  body = body.flatMap(item => {
    if (item.type !== "system" || !item.text.startsWith("本段执行模型：")) return [item];
    const model = item.text.slice("本段执行模型：".length).trim();
    if (!model) return [];
    const from = previousModel;
    previousModel = model;
    return from && from !== model ? [{ ...item, text: `模型切换：${from} → ${model}` }] : [];
  });
  // 空思考与空/None 回复不渲染，却会切断过程折叠，导致连续工具被拆成多行“调用工具 ×1”。
  body = body.filter(item => (item.type !== "thought" && item.type !== "assistant") || !["", "None"].includes(item.text.trim()));
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
  if (body.slice(last + 1).every(it => it.type === "thought" || (it.type === "tool" && memoryTool(it)))) {
    // 分身结果只写进模型历史，界面上没有新的状态行。派发或后台状态在更早的位置时，其后已经写给用户的结论不能因为交回后又思考一次就折进过程区。
    let seenWait = false;
    const afterWait = body.map(item => {
      const before = seenWait;
      if (isBackgroundStatus(item) || waitingTool(item)) seenWait = true;
      return before;
    });
    let start = first;
    let wrote = false;
    let delivered = false;
    while (start > 0) {
      const item = body[start - 1];
      // 跨过状态行、思考和分身调用：等外部结果之前的回复已经写给用户，不能跟着折进过程区。
      if (item.type === "thought" || isBackgroundStatus(item) || waitingTool(item)) {
        delivered ||= isBackgroundStatus(item) || waitingTool(item);
        start--;
        continue;
      }
      const kind = item.type === "tool" ? memoryTool(item) : null;
      if (kind) {
        wrote ||= kind === "write";
        start--;
        continue;
      }
      if ((delivered || afterWait[start - 1]) && isText(item) && !isBackgroundStatus(item)) {
        while (start > 0 && isText(body[start - 1]) && !isBackgroundStatus(body[start - 1])) selected.add(body[--start]);
        delivered = false;
        continue;
      }
      break;
    }
    // 交付后的短确认不是新结论；而等待过外部结果（后台任务、分身）时，交回前后的回答都要给用户看到。
    if (delivered || (wrote && shortAck)) {
      while (start > 0 && isText(body[start - 1]) && !isBackgroundStatus(body[start - 1])) selected.add(body[--start]);
    }
  }
  if ([...selected].some(item => item.type === "assistant")) {
    for (const item of [...selected]) if (isBackgroundStatus(item)) selected.delete(item);
  }
  return { process: body.filter(it => !selected.has(it)), conclusion: body.filter(it => selected.has(it)) };
}
export type ProcessSegment = { type: "process"; id: number; items: ProcessItem[] } | { type: "item"; id: number; item: Item };

export function processSegments(items: Item[]): ProcessSegment[] {
  const segments: ProcessSegment[] = [];
  for (const item of items) {
    // 与 pi 一致：签名仍留在原生会话中，历史空思考不生成折叠行。
    if (item.type === "thought" && !item.text.trim()) continue;
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
    lines = [...wrap(text).filter(line => line.trim()).slice(-(2 - lines.length)), ...lines];
  }
  return lines;
}

/** 本轮使用的模型（取最近的切换标记目标，遇到上一轮的收尾项即停）；无切换返回空串。 */
export function liveModel(items: Item[]): string {
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i];
    // 轮中追加的 steer 也是 user 项，不能当作本轮起点；只在上一轮的 turn 收尾项处截止（末尾本轮自己的 turn 项除外）。
    if (item.type === "turn" && i < items.length - 1) return "";
    if (item.type !== "system") continue;
    // 循环内推理预算降档：模型没变、只是强度降了，尾巴括号里是本轮生效的模型串。
    if (item.text.startsWith("思考强度：")) {
      const applied = item.text.match(/（(.+)）$/)?.[1]?.trim();
      if (applied) return applied;
    }
    if (item.text.startsWith("模型切换：")) return item.text.split("→").at(-1)!.trim();
    if (item.text.startsWith("本段执行模型：")) return item.text.slice("本段执行模型：".length).trim();
  }
  return "";
}
