import type { Item, ToolItem } from "./types";

/** 后台任务：id 是 bash 的 job_id，startedAt 是命令开始时间。 */
export type BackgroundTask = { id: number; command: string; running: boolean; startedAt: number | null };

// 新记录优先用后端 jobId；旧记录仅识别完整的转后台提示，不能从普通命令输出里搜索片段。
const STARTED = /^命令超过 \d+s 仍未结束，已转到后台：job_id=(\d+)(?:[。.]|$)/;
const FINISHED = /后台任务\s*(\d+)\s*(?:已完成|已结束|未能取回结果|已停止)/g;

function output(item: ToolItem): string {
  return item.content
    .map(part => (part.type === "content" ? String((part.content as { text?: unknown }).text ?? "") : ""))
    .join("\n");
}

/**
 * 当前一轮的后台任务。只看最后一条 turn 项之后：轮次一收尾卡片就消失，
 * 历史里早就交付过的任务不会被当成本轮还在跑。
 * shortcut: 只认本轮起过的任务；上一轮 detach 的长驻服务、以及上一轮留下续 poll 的任务不显示。
 */
export function backgroundTasks(items: readonly Item[]): BackgroundTask[] {
  const tasks = new Map<number, BackgroundTask>();
  // 交付状态行可能属于上一轮起的任务：照样留一条已结束（命令未知）。
  const settle = (text: string) => {
    for (const hit of text.matchAll(FINISHED)) {
      const id = Number(hit[1]);
      const task = tasks.get(id);
      if (task) task.running = false;
      else tasks.set(id, { id, command: "", running: false, startedAt: null });
    }
  };
  const start = items.map(item => item.type).lastIndexOf("turn") + 1;
  for (const item of items.slice(start)) {
    if (item.type === "tool") {
      const text = output(item);
      if (item.kind === "execute") {
        const meta = item.rawOutput && typeof item.rawOutput === "object" ? item.rawOutput as Record<string, unknown> : null;
        const legacy = !meta || !("exitCode" in meta) ? text.match(STARTED)?.[1] : undefined;
        const id = typeof meta?.jobId === "number" ? meta.jobId : Number(legacy);
        if (item.status === "completed" && Number.isSafeInteger(id) && id > 0) {
          tasks.set(id, { id, command: item.title, running: true, startedAt: item.ts });
        }
      }
      if (item.title.startsWith("Lyra / bash_poll") && item.status !== "pending" && item.status !== "in_progress") {
        const args = item.rawInput && typeof item.rawInput === "object" ? item.rawInput as Record<string, unknown> : null;
        const polled = typeof args?.job_id === "number" ? args.job_id : Number(text.match(/^\[后台任务\s*(\d+)\s*(?:已完成|已结束|未能取回结果)/)?.[1]);
        if (Number.isSafeInteger(polled) && !text.startsWith(`后台任务 ${polled} 仍在运行`)) {
          const task = tasks.get(polled);
          if (task) task.running = false;
        }
      }
    } else if (item.type === "system" && item.level === "background") {
      // 交付行可能是上一轮起的任务：交回后也要留痕，否则结果到了界面什么都没留下。
      settle(item.text);
    }
  }
  return [...tasks.values()].sort((a, b) => Number(b.running) - Number(a.running) || a.id - b.id);
}
