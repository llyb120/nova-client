export const STREAM_PREBUFFER_MS = 100;

const TARGET_DRAIN_MS = 220;
const MIN_CHARS_PER_MS = 0.06;
const MAX_CHARS_PER_MS = 0.8;
const JUMP_AT = 3000;

export interface StreamRevealStep {
  text: string;
  remainder: number;
}

/**
 * Consume a buffered stream according to backlog and real frame time.
 * The fractional remainder prevents low rates from being rounded up or lost each frame.
 */
export function advanceStreamText(
  current: string,
  target: string,
  elapsedMs: number,
  remainder = 0,
): StreamRevealStep {
  if (!target.startsWith(current) || target.length - current.length > JUMP_AT) {
    return { text: target, remainder: 0 };
  }
  const backlog = target.length - current.length;
  if (backlog <= 0) return { text: current, remainder };

  const speed = Math.min(
    MAX_CHARS_PER_MS,
    Math.max(MIN_CHARS_PER_MS, backlog / TARGET_DRAIN_MS),
  );
  const budget = Math.max(0, elapsedMs) * speed + Math.max(0, remainder);
  const count = Math.min(backlog, Math.floor(budget));
  if (count === 0) return { text: current, remainder: budget };

  let end = current.length + count;
  // Do not expose half of a UTF-16 surrogate pair (emoji, some rare CJK characters).
  const last = target.charCodeAt(end - 1);
  if (last >= 0xd800 && last <= 0xdbff && end < target.length) end++;
  return {
    text: target.slice(0, end),
    remainder: end >= target.length ? 0 : budget - count,
  };
}
