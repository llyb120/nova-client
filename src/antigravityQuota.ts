import type { AntigravityQuotaBucket, AntigravityQuotaGroup, AntigravityQuotaSnapshot } from "./types";

const EFFORT_SUFFIX_RE = /(?:[:\s·-]+|\()(low|medium|high|xhigh|max)\)?$/i;

function normalizeModelToken(raw: string): string {
  return raw
    .trim()
    .replace(EFFORT_SUFFIX_RE, "")
    .replace(/[-_]+/g, " ")
    .replace(/\s+/g, " ")
    .toLowerCase()
    .trim();
}

function groupContainsModel(group: AntigravityQuotaGroup, modelId: string, modelLabel?: string): boolean {
  const desc = (group.description ?? "").toLowerCase();
  if (!desc) return false;
  const candidates = [modelLabel, modelId]
    .filter((v): v is string => !!v && v.trim().length > 0)
    .map(normalizeModelToken)
    .filter((v) => v.length >= 3);
  return candidates.some((token) => desc.includes(token));
}

function groupMatchesFamily(group: AntigravityQuotaGroup, combinedModel: string): boolean {
  const hay = `${group.name} ${group.buckets.map((b) => b.id).join(" ")}`.toLowerCase();
  if (/(claude|gpt|openai|anthropic)/i.test(combinedModel)) {
    return hay.includes("claude") || hay.includes("gpt") || hay.includes("3p");
  }
  if (/(gemini|orchestrator)/i.test(combinedModel)) {
    return hay.includes("gemini");
  }
  return false;
}

/** 按当前选择的 Antigravity 模型匹配所属配额分组（优先匹配 group.description，其次按模型家族关键词）。 */
export function matchAntigravityQuotaGroup(
  snapshot: AntigravityQuotaSnapshot | null | undefined,
  modelId: string,
  modelLabel?: string,
): AntigravityQuotaGroup | null {
  const groups = snapshot?.groups ?? [];
  if (groups.length === 0) return null;

  const byDesc = groups.find((g) => groupContainsModel(g, modelId, modelLabel));
  if (byDesc) return byDesc;

  const combined = `${modelId} ${modelLabel ?? ""}`.trim();
  if (combined) {
    const byFamily = groups.find((g) => groupMatchesFamily(g, combined));
    if (byFamily) return byFamily;
  }

  return groups[0] ?? null;
}

function isFiveHourBucket(bucket: AntigravityQuotaBucket): boolean {
  const w = bucket.window.trim().toLowerCase();
  const id = bucket.id.trim().toLowerCase();
  return w === "5h" || id.endsWith("-5h");
}

function isWeeklyBucket(bucket: AntigravityQuotaBucket): boolean {
  const w = bucket.window.trim().toLowerCase();
  const id = bucket.id.trim().toLowerCase();
  return w === "weekly" || id.endsWith("-weekly");
}

/** 从配额分组中提取 5 小时与周限额两个时间窗口桶。 */
export function pickQuotaBuckets(group: AntigravityQuotaGroup | null | undefined): {
  fiveHour: AntigravityQuotaBucket | null;
  weekly: AntigravityQuotaBucket | null;
} {
  const buckets = group?.buckets ?? [];
  return {
    fiveHour: buckets.find(isFiveHourBucket) ?? null,
    weekly: buckets.find(isWeeklyBucket) ?? null,
  };
}

/** 按剩余百分比映射视觉状态档位。 */
export function quotaTone(percent: number): "ok" | "warn" | "low" {
  if (percent > 30) return "ok";
  if (percent > 10) return "warn";
  return "low";
}

/** 将毫秒时间戳格式化为紧凑重置倒计时（如 `4h55m`、`6天`）；epoch 0 或空值返回 `null`。 */
export function formatQuotaResetCountdown(
  resetTimeMs: number | null | undefined,
  nowMs: number,
): string | null {
  if (resetTimeMs == null || !Number.isFinite(resetTimeMs) || resetTimeMs <= 0) {
    return null;
  }
  const diff = resetTimeMs - nowMs;
  if (diff <= 0) return "即将重置";

  const mins = Math.ceil(diff / 60_000);
  if (mins < 60) return `${mins}m`;

  const totalHours = Math.floor(mins / 60);
  const remMins = mins % 60;
  if (totalHours < 24) {
    return `${totalHours}h${remMins > 0 ? `${remMins}m` : ""}`;
  }

  const days = Math.floor(totalHours / 24);
  const remHours = totalHours % 24;
  return `${days}天${remHours > 0 && days < 3 ? `${remHours}h` : ""}`;
}

/** 将快照抓取时间格式化为相对更新时间。 */
export function formatQuotaUpdatedAgo(fetchedAt: number, nowMs: number): string {
  const diffSec = Math.max(0, Math.floor((nowMs - fetchedAt) / 1000));
  if (diffSec < 60) return "刚刚";
  const mins = Math.floor(diffSec / 60);
  if (mins < 60) return `${mins}分钟前`;
  const hours = Math.floor(mins / 60);
  return `${hours}小时前`;
}

/** 缩短分组名称以便紧凑展示。 */
export function shortQuotaGroupName(name: string): string {
  const trimmed = name.trim();
  const lower = trimmed.toLowerCase();
  if (lower === "gemini models") return "Gemini";
  if (lower === "claude and gpt models") return "Claude / GPT";
  return trimmed;
}
