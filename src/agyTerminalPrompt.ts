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
 * 将多行文本编码为适合 Windows ConPTY / 非 bracketed-paste TUI 的字节分块：
 * 每行文本按 UTF-8 字符安全切分，行与行之间输出独立的 2 字节软换行块 `ESC + CR`
 * （`[0x1b, 0x0d]`，与终端键盘 Shift+Enter 的单次写入完全一致），避免中途提前回车提交。
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

  const pushLine = (line: string) => {
    if (!line) return;
    let buf: number[] = [];
    for (const ch of line) {
      const encoded = encoder.encode(ch);
      if (buf.length + encoded.length > limit && buf.length > 0) {
        chunks.push(Uint8Array.from(buf));
        buf = [];
      }
      for (let i = 0; i < encoded.length; i++) {
        buf.push(encoded[i]);
      }
    }
    if (buf.length > 0) {
      chunks.push(Uint8Array.from(buf));
    }
  };

  const lines = normalized.split("\n");
  for (let i = 0; i < lines.length; i++) {
    if (i > 0) {
      chunks.push(new Uint8Array([0x1b, 0x0d]));
    }
    pushLine(lines[i]);
  }
  return chunks;
}
