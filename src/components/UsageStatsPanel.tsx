import { createMemo, createSignal, For, onSettled, Show } from "solid-js";
import { api } from "../ipc";
import { showToast } from "../store";
import type { UsageBucket, UsageGroup, UsageStats } from "../types";
import { fmtTokens } from "./TurnGroup";
import "./UsageStatsPanel.css";

/** 缓存命中率：命中读取占全部输入（新增 + 缓存读写）的比例。 */
function hitRate(b: UsageBucket): number {
  const prompt = b.input + b.cacheRead + b.cacheWrite;
  return prompt > 0 ? b.cacheRead / prompt : 0;
}

/** 项目记忆前缀的命中率：前缀排在每次请求最前面，命中读取就是它被缓存的部分。 */
function memoryRate(b: UsageBucket): number {
  return b.memory > 0 ? b.memoryCacheRead / b.memory : 0;
}

const pct = (rate: number) => (rate > 0 ? `${(rate * 100).toFixed(1)}%` : "—");
/** 悬浮说明用精确值，卡片本体用紧凑的 k/M。 */
const exact = (n: number) => n.toLocaleString("zh-CN");

/** 时间窗：`days` 是含今天在内的天数，null = 全部。 */
const RANGES = [
  { id: "today", label: "今天", days: 1 },
  { id: "7d", label: "7 天", days: 7 },
  { id: "14d", label: "14 天", days: 14 },
  { id: "30d", label: "30 天", days: 30 },
  { id: "90d", label: "近三个月", days: 90 },
  { id: "180d", label: "近半年", days: 180 },
  { id: "all", label: "全部", days: null },
] as const;

type RangeId = (typeof RANGES)[number]["id"];

/** 天数多的窗口按自然月汇总：几百根日柱既看不清也翻不完。 */
function byMonth(days: UsageGroup[]): UsageGroup[] {
  const months = new Map<string, UsageGroup>();
  for (const day of days) {
    const name = day.name.slice(0, 7);
    const prev = months.get(name);
    months.set(
      name,
      prev
        ? {
            name,
            turns: prev.turns + day.turns,
            input: prev.input + day.input,
            output: prev.output + day.output,
            cacheRead: prev.cacheRead + day.cacheRead,
            cacheWrite: prev.cacheWrite + day.cacheWrite,
            total: prev.total + day.total,
            memory: prev.memory + day.memory,
            memoryCacheRead: prev.memoryCacheRead + day.memoryCacheRead,
          }
        : { ...day, name },
    );
  }
  return [...months.values()];
}

/** 设置 · 用量统计：全部会话的 token 用量，重点看缓存命中。 */
export default function UsageStatsPanel() {
  // 切换范围就发一次请求，后端并发处理：慢的那次可能后到，不能盖掉后选的范围。
  const [loaded, setLoaded] = createSignal<{ range: RangeId; data: UsageStats } | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  const [range, setRange] = createSignal<RangeId>("all");
  let request = 0;
  const windowDays = (id: RangeId) => RANGES.find((r) => r.id === id)?.days ?? null;
  const load = async (id: RangeId) => {
    const ticket = ++request;
    setBusy(true);
    setFailed(false);
    try {
      const data = await api.usageStats(windowDays(id));
      if (ticket === request) setLoaded({ range: id, data });
    } catch (error) {
      if (ticket === request) {
        setFailed(true);
        showToast(`读取用量失败：${String(error)}`);
      }
    } finally {
      if (ticket === request) setBusy(false);
    }
  };
  onSettled(() => void load(range()));
  const pickRange = (id: RangeId) => {
    setRange(id);
    void load(id);
  };
  // 只认当前范围的数据：旧范围的慢响应不会挂在别的页签下面。
  const stats = createMemo(() => (loaded()?.range === range() ? loaded()!.data : null));

  // 按天或按月；柱宽按区间内最大值归一，避免长期历史把小值压成一条线。
  const periods = createMemo(() => {
    const list = stats()?.days ?? [];
    return list.length > 31 ? { monthly: true, rows: byMonth(list) } : { monthly: false, rows: list };
  });
  const peakDay = createMemo(() => Math.max(1, ...periods().rows.map((d) => d.total)));
  const models = createMemo(() => (stats()?.models ?? []).slice(0, 10));
  const peakModel = createMemo(() => Math.max(1, ...models().map((m) => m.total)));

  return (
    <div class="usage-stats">
      <p class="field-hint usage-stats-hint">
        统计本机轮次用量（含数字员工、分身与子会话；会话删除后已用量仍计入）。「项目记忆」是每次请求都重发的前缀在输入里占的 token，是输入的子集，不重复计入总量。
      </p>

      <div class="usage-toolbar">
        <div class="theme-seg" role="tablist" aria-label="用量时间范围">
          <For each={RANGES}>
            {(r) => (
              <button
                type="button"
                role="tab"
                aria-selected={range() === r.id ? "true" : "false"}
                class={{ "theme-seg-btn": true, active: range() === r.id }}
                onClick={() => pickRange(r.id)}
              >
                {r.label}
              </button>
            )}
          </For>
        </div>
        <button type="button" class="link-btn usage-refresh" disabled={busy()} onClick={() => void load(range())}>
          {busy() ? "刷新中…" : "刷新"}
        </button>
      </div>

      <Show
        when={stats()}
        fallback={<div class="sel-empty">{busy() ? "统计中…" : failed() ? "读取用量失败，点刷新重试" : "暂无用量记录"}</div>}
      >
        {(s) => (
          <>
            <div class="usage-cards">
              <div class="usage-card accent">
                <span class="usage-card-label">总 token</span>
                <strong class="usage-card-value" title={exact(s().total.total)}>
                  {fmtTokens(s().total.total)}
                </strong>
                <span class="usage-card-sub">{exact(s().total.turns)} 轮</span>
              </div>
              <div class="usage-card accent">
                <span class="usage-card-label">缓存命中率</span>
                <strong
                  class="usage-card-value"
                  title={`缓存读取 ${exact(s().total.cacheRead)}；命中率 = 缓存读取 /（新增输入 + 缓存读取 + 缓存写入）`}
                >
                  {pct(hitRate(s().total))}
                </strong>
                <span class="usage-card-sub">读取 {fmtTokens(s().total.cacheRead)}</span>
              </div>
              <div class="usage-card">
                <span class="usage-card-label">项目记忆</span>
                <strong
                  class="usage-card-value"
                  title={`项目记忆前缀共 ${exact(s().total.memory)} tokens（输入的子集，不重复计入总量）；其中命中缓存 ${exact(s().total.memoryCacheRead)}`}
                >
                  {fmtTokens(s().total.memory)}
                </strong>
                <span class="usage-card-sub">
                  {s().total.memory > 0 ? `命中 ${(memoryRate(s().total) * 100).toFixed(1)}% · ${fmtTokens(s().total.memoryCacheRead)}` : "未启用"}
                </span>
              </div>
              <div class="usage-card">
                <span class="usage-card-label">新增输入</span>
                <strong class="usage-card-value" title={exact(s().total.input)}>
                  {fmtTokens(s().total.input)}
                </strong>
                <span class="usage-card-sub">未命中缓存</span>
              </div>
              <div class="usage-card">
                <span class="usage-card-label">缓存写入</span>
                <strong class="usage-card-value" title={exact(s().total.cacheWrite)}>
                  {fmtTokens(s().total.cacheWrite)}
                </strong>
              </div>
              <div class="usage-card">
                <span class="usage-card-label">输出</span>
                <strong class="usage-card-value" title={exact(s().total.output)}>
                  {fmtTokens(s().total.output)}
                </strong>
              </div>
            </div>

            <div class="usage-section">
              <h4>{periods().monthly ? "按月" : "按天"}</h4>
              <Show when={periods().rows.length} fallback={<div class="sel-empty">该时间段没有用量</div>}>
                <For each={periods().rows}>
                  {(d) => (
                    <div class="usage-bar-row">
                      <span class="usage-bar-day">{periods().monthly ? d.name : d.name.slice(5)}</span>
                      <span class="usage-bar-track">
                        <span class="usage-bar-fill" style={{ width: `${(d.total / peakDay()) * 100}%` }} />
                      </span>
                      <span class="usage-bar-total">{fmtTokens(d.total)}</span>
                      <span
                        class="usage-bar-hit"
                        title={`命中 ${exact(d.cacheRead)} / 输入 ${exact(d.input + d.cacheRead + d.cacheWrite)}`}
                      >
                        {pct(hitRate(d))}
                      </span>
                    </div>
                  )}
                </For>
              </Show>
            </div>

            <div class="usage-section">
              <h4>按模型</h4>
              <Show when={models().length} fallback={<div class="sel-empty">该时间段没有用量</div>}>
                <For each={models()}>
                  {(m) => (
                    <div class="usage-bar-row model">
                      <span class="usage-bar-day" title={m.name}>
                        {m.name}
                      </span>
                      <span class="usage-bar-track">
                        <span class="usage-bar-fill" style={{ width: `${(m.total / peakModel()) * 100}%` }} />
                      </span>
                      <span class="usage-bar-total">{fmtTokens(m.total)}</span>
                      <span
                        class="usage-bar-hit"
                        title={`命中 ${exact(m.cacheRead)} / 输入 ${exact(m.input + m.cacheRead + m.cacheWrite)}`}
                      >
                        {pct(hitRate(m))}
                      </span>
                    </div>
                  )}
                </For>
              </Show>
            </div>
          </>
        )}
      </Show>
    </div>
  );
}
