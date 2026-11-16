import type { Item, ToolItem } from "./types";

/** 后台任务：id 是 bash 的 job_id，command 是起它的命令。 */
export type BackgroundTask = { id: number; command: string; running: boolean };

// 文案契约来自 src-tauri/src/lyra/tools.rs（转后台、bash_poll 结果）与 bridge.rs（交付状态行）；改文案要同步这里。
const STARTED = /已转到后台：job_id=(\d+)/g;
const FINISHED = /后台任务\s*(\d+)\s*(?:已完成|已结束|未能取回结果)/g;

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
  // 交付状态行可能属于上一轮起的任务：照样留一条已结束（命令未知）；poll 结果本身就留在工具行里，不凭空多出一条。
  const settle = (text: string, addUnknown: boolean) => {
    for (const hit of text.matchAll(FINISHED)) {
      const id = Number(hit[1]);
      const task = tasks.get(id);
      if (task) task.running = false;
      else if (addUnknown) tasks.set(id, { id, command: "", running: false });
    }
  };
  const start = items.map(item => item.type).lastIndexOf("turn") + 1;
  for (const item of items.slice(start)) {
    if (item.type === "tool") {
      const text = output(item);
      for (const hit of text.matchAll(STARTED)) {
        const id = Number(hit[1]);
        tasks.set(id, { id, command: item.title, running: true });
      }
      settle(text, false);
    } else if (item.type === "system" && item.level === "background") {
      // 交付行可能是上一轮起的任务：交回后也要留痕，否则结果到了界面什么都没留下。
      settle(item.text, true);
    }
  }
  return [...tasks.values()].sort((a, b) => Number(b.running) - Number(a.running) || a.id - b.id);
}
