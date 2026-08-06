import { createSignal } from "solid-js";
import type { PromptImage } from "./types";

export type PromptHistoryItem = {
  id: string;
  text: string;
  ts: number;
  images: PromptImage[];
};

const MAX_PROMPT_HISTORY = 5;
let nextHistoryId = 0;

/** 最近的用户输入在所有会话和输入框之间共享。 */
export const [promptHistory, setPromptHistory] = createSignal<PromptHistoryItem[]>([]);

/** 记录用户输入；相同文本只保留最近一次，避免历史菜单被重复内容占满。 */
export function rememberPromptHistory(
  text: string,
  images: PromptImage[] = [],
  ts = Date.now(),
  id = `prompt:${ts}:${nextHistoryId++}`,
) {
  const normalized = text.trim();
  if (!normalized) return;
  const snapshot = images.map((image) => ({ ...image }));
  setPromptHistory((items) => {
    const previous = items.find((item) => item.text === normalized);
    if (previous && previous.ts >= ts) return items;
    const next = [
      { id, text: normalized, ts, images: snapshot },
      ...items.filter((item) => item.text !== normalized && item.id !== id),
    ];
    return next.sort((a, b) => b.ts - a.ts).slice(0, MAX_PROMPT_HISTORY);
  });
}
