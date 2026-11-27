import type { AgyAskBlock, AgyAskQuestion, AgyTurn } from "./types";

const SPLIT_RATIO_KEY = "nova:agy-split-ratio";
export const DEFAULT_AGY_SPLIT_RATIO = 2 / 3;

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
      asks: [],
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

export interface AgyAskSubmissionItem {
  selectedIndices: number[];
  customText: string;
  skipped?: boolean;
}

export type TerminalKeyStep =
  | { kind: "bytes"; bytes: Uint8Array; delayAfterMs?: number }
  | { kind: "text"; text: string; delayAfterMs?: number };

const KEY_UP = new Uint8Array([0x1b, 0x5b, 0x41]);
const KEY_DOWN = new Uint8Array([0x1b, 0x5b, 0x42]);
const KEY_ENTER = new Uint8Array([0x0d]);
const KEY_ESC = new Uint8Array([0x1b]);
const KEY_SPACE = new Uint8Array([0x20]);

function digitBytes(oneBased: number): Uint8Array {
  return new Uint8Array([0x30 + oneBased]);
}

function pushMoveSteps(steps: TerminalKeyStep[], from: number, to: number): void {
  const delta = to - from;
  if (delta === 0) return;
  const bytes = delta > 0 ? KEY_DOWN : KEY_UP;
  const count = Math.abs(delta);
  for (let i = 0; i < count; i++) {
    steps.push({ kind: "bytes", bytes, delayAfterMs: 25 });
  }
}

/** 仅在最后一轮尚无最终文本结论时，返回其中最后一个处于待回答状态的提问块。 */
export function findActivePendingAgyAsk(turns: AgyTurn[]): AgyAskBlock | null {
  if (turns.length === 0) return null;
  const last = turns[turns.length - 1];
  if (!last || last.response) return null;
  const asks = last.asks ?? [];
  for (let i = asks.length - 1; i >= 0; i--) {
    if (asks[i]?.status === "pending") return asks[i];
  }
  return null;
}

/** 为全部问题生成逐题按 Esc 跳过的终端按键序列（间隔 > 50ms 以避开 BubbleTea ESC 序列合并）。 */
export function buildAgyAskSkipSteps(questionCount: number): TerminalKeyStep[] {
  const count = Math.max(0, Math.floor(questionCount));
  return Array.from({ length: count }, () => ({
    kind: "bytes" as const,
    bytes: KEY_ESC,
    delayAfterMs: 90,
  }));
}

/** 根据 `agy` TUI `AskQuestionModel` 的状态机将结构化回答编码为有序 PTY 键位步骤。 */
export function buildAgyAskKeySteps(
  questions: Pick<AgyAskQuestion, "options" | "isMultiSelect">[],
  submissions: AgyAskSubmissionItem[],
): TerminalKeyStep[] {
  const steps: TerminalKeyStep[] = [];
  for (let qi = 0; qi < questions.length; qi++) {
    const q = questions[qi];
    const sub = submissions[qi];
    const optionCount = q.options.length;
    const validSelected: number[] = [];
    for (const idx of sub?.selectedIndices ?? []) {
      if (Number.isInteger(idx) && idx >= 0 && idx < optionCount && !validSelected.includes(idx)) {
        validSelected.push(idx);
      }
    }
    const custom = (sub?.customText ?? "").replace(/\r\n/g, "\n").replace(/\r/g, "\n").trim();

    if (sub?.skipped || (validSelected.length === 0 && !custom)) {
      steps.push({ kind: "bytes", bytes: KEY_ESC, delayAfterMs: 90 });
      continue;
    }

    if (!q.isMultiSelect) {
      if (validSelected.length > 0) {
        const idx = validSelected[0];
        if (idx < 9) {
          steps.push({ kind: "bytes", bytes: digitBytes(idx + 1), delayAfterMs: 85 });
        } else {
          pushMoveSteps(steps, 0, idx);
          steps.push({ kind: "bytes", bytes: KEY_ENTER, delayAfterMs: 85 });
        }
      } else {
        if (optionCount < 9) {
          steps.push({ kind: "bytes", bytes: digitBytes(optionCount + 1), delayAfterMs: 70 });
        } else {
          pushMoveSteps(steps, 0, optionCount);
          steps.push({ kind: "bytes", bytes: KEY_ENTER, delayAfterMs: 70 });
        }
        steps.push({ kind: "text", text: custom, delayAfterMs: 80 });
        steps.push({ kind: "bytes", bytes: KEY_ENTER, delayAfterMs: 90 });
      }
      continue;
    }

    const sorted = [...validSelected].sort((a, b) => a - b);
    let cursor = 0;
    for (const idx of sorted) {
      if (idx < 9) {
        steps.push({ kind: "bytes", bytes: digitBytes(idx + 1), delayAfterMs: 45 });
      } else {
        pushMoveSteps(steps, cursor, idx);
        steps.push({ kind: "bytes", bytes: KEY_SPACE, delayAfterMs: 45 });
      }
      cursor = idx;
    }
    if (custom) {
      if (optionCount < 9) {
        steps.push({ kind: "bytes", bytes: digitBytes(optionCount + 1), delayAfterMs: 70 });
      } else {
        pushMoveSteps(steps, cursor, optionCount);
        steps.push({ kind: "bytes", bytes: KEY_ENTER, delayAfterMs: 70 });
      }
      steps.push({ kind: "text", text: custom, delayAfterMs: 80 });
      steps.push({ kind: "bytes", bytes: KEY_ENTER, delayAfterMs: 90 });
    } else {
      steps.push({ kind: "bytes", bytes: KEY_ENTER, delayAfterMs: 90 });
    }
  }
  return steps;
}
