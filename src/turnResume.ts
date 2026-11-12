import type { Item } from "./types";

/**
 * 最后一轮是否停在中断处：正常收尾会在最后一条用户消息之后写 end_turn 轮次标记，
 * 用户停止/请求出错留下别的 stopReason，进程被杀则根本没来得及写标记。
 * 只看最后一条用户消息之后，收尾后追加的系统提示不算中断。
 */
export function interruptedTurn(items: readonly Item[]): boolean {
  const start = items.map((item) => item.type).lastIndexOf("user");
  if (start < 0) return false;
  for (let i = items.length - 1; i > start; i--) {
    const item = items[i];
    if (item.type === "turn") {
      return item.stopReason !== "end_turn" && item.stopReason !== "max_turn_requests";
    }
  }
  return true;
}
