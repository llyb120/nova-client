import type { AgyTurn } from "./types";

const SPLIT_OPEN_KEY = "nova:agy-split-open";
const SPLIT_RATIO_KEY = "nova:agy-split-ratio";
export const DEFAULT_AGY_SPLIT_RATIO = 0.5;

export interface OptimisticAgyTurn {
  id: number;
  prompt: string;
  timestamp: number;
  baseTurnCount: number;
}

/** 过滤掉已被真实轮次覆盖的乐观轮次。 */
export function reconcileOptimisticTurns(
  realTurns: AgyTurn[],
  optimistic: OptimisticAgyTurn[],
): OptimisticAgyTurn[] {
  if (optimistic.length === 0) return optimistic;
  return optimistic.filter((item) => realTurns.length < item.baseTurnCount + 1);
}

/** 将真实轮次与未落盘的乐观用户输入合并，供世界线即时渲染。 */
export function mergeAgyTurnsWithOptimistic(
  realTurns: AgyTurn[],
  optimistic: OptimisticAgyTurn[],
): AgyTurn[] {
  const pending = reconcileOptimisticTurns(realTurns, optimistic);
  if (pending.length === 0) return realTurns;
  return [
    ...realTurns,
    ...pending.map((item, i) => ({
      index: realTurns.length + i + 1,
      prompt: item.prompt,
      images: [],
      promptTimestamp: item.timestamp,
      response: null,
      responseTimestamp: null,
      model: null,
    })),
  ];
}

/** 将用户输入与附件路径合成为适合终端 CLI 的多行提示词。 */
export function formatTerminalPromptWithPaths(text: string, paths: string[]): string {
  const trimmed = text.replace(/\r\n/g, "\n").trim();
  const uniquePaths: string[] = [];
  for (const raw of paths) {
    const path = raw.trim();
    if (path && !uniquePaths.includes(path)) {
      uniquePaths.push(path);
    }
  }
  if (uniquePaths.length === 0) return trimmed;
  const lines = uniquePaths.map((p) => `- ${/\s/.test(p) ? `"${p}"` : p}`);
  if (!trimmed) {
    return `请查看以下附件文件：\n${lines.join("\n")}`;
  }
  return `${trimmed}\n\n附件文件：\n${lines.join("\n")}`;
}

export function clampAgySplitRatio(ratio: number): number {
  if (!Number.isFinite(ratio)) return DEFAULT_AGY_SPLIT_RATIO;
  return Math.min(0.72, Math.max(0.25, ratio));
}

export function loadAgySplitOpen(): boolean {
  try {
    if (typeof localStorage === "undefined") return true;
    return localStorage.getItem(SPLIT_OPEN_KEY) !== "false";
  } catch {
    return true;
  }
}

export function saveAgySplitOpen(open: boolean): void {
  try {
    if (typeof localStorage === "undefined") return;
    localStorage.setItem(SPLIT_OPEN_KEY, open ? "true" : "false");
  } catch {
    // 忽略隐私模式或禁用存储场景
  }
}

export function loadAgySplitRatio(): number {
  try {
    if (typeof localStorage === "undefined") return DEFAULT_AGY_SPLIT_RATIO;
    const raw = localStorage.getItem(SPLIT_RATIO_KEY);
    if (!raw) return DEFAULT_AGY_SPLIT_RATIO;
    return clampAgySplitRatio(Number.parseFloat(raw));
  } catch {
    return DEFAULT_AGY_SPLIT_RATIO;
  }
}

export function saveAgySplitRatio(ratio: number): void {
  try {
    if (typeof localStorage === "undefined") return;
    localStorage.setItem(SPLIT_RATIO_KEY, clampAgySplitRatio(ratio).toFixed(4));
  } catch {
    // 忽略隐私模式或禁用存储场景
  }
}

/**
 * 将多行文本编码为适合非 bracketed-paste（如 Windows ConPTY）TUI 的字节分块：
 * 每行之间使用软换行序列 ESC + CR (`0x1b 0x0d`)，避免中途提前触发回车提交，
 * 并保证不在多字节 UTF-8 字符或 ESC+CR 对中间切分。
 */
export function encodeSoftMultilineTerminalChunks(
  text: string,
  maxChunkBytes = 32768,
): Uint8Array[] {
  const normalized = text.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  if (!normalized) return [];
  const limit = Math.max(4, Math.floor(maxChunkBytes));
  const encoder = new TextEncoder();
  const chunks: Uint8Array[] = [];
  let buf: number[] = [];

  const flush = () => {
    if (buf.length > 0) {
      chunks.push(Uint8Array.from(buf));
      buf = [];
    }
  };

  const pushBytes = (bytes: ArrayLike<number>) => {
    if (buf.length + bytes.length > limit) {
      flush();
    }
    for (let i = 0; i < bytes.length; i++) {
      buf.push(bytes[i]);
    }
  };

  const lines = normalized.split("\n");
  for (let i = 0; i < lines.length; i++) {
    if (i > 0) {
      pushBytes([0x1b, 0x0d]);
    }
    const line = lines[i];
    for (const ch of line) {
      pushBytes(encoder.encode(ch));
    }
  }
  flush();
  return chunks;
}
