import { createMemo, createSignal, For, onSettled, Show } from "solid-js";
import { api } from "../ipc";
import { showToast } from "../store";
import type { UsageBucket, UsageStats } from "../types";
import { fmtTokens } from "./TurnGroup";
import "./UsageStatsPanel.css";

/** 缓存命中率：命中读取占全部输入（新增 + 缓存读写）的比例。 */
function hitRate(b: UsageBucket): number {
  const prompt = b.input + b.cacheRead + b.cacheWrite;
  return prompt > 0 ? b.cacheRead / prompt : 0;
}

const pct = (rate: number) => (rate > 0 ? `${(rate * 100).toFixed(1)}%` : "—");
/** 悬浮说明用精确值，卡片本体用紧凑的 k/M。 */
const exact = (n: number) => n.toLocaleString("zh-CN");

/** 设置 · 用量统计：全部会话的 token 用量，重点看缓存命中。 */
export default function UsageStatsPanel() {
  const [stats, setStats] = createSignal<UsageStats | null>(null);
  const [busy, setBusy] = createSignal(false);
  const load = async () => {
    setBusy(true);
    try {
      setStats(await api.usageStats());
    } catch (error) {
      showToast(`读取用量失败：${String(error)}`);
    } finally {
      setBusy(false);
    }
  };
  onSettled(() => void load());

  // 只展示最近 14 天；柱宽按区间内最大值归一，避免长期历史把小值压成一条线。
  const days = createMemo(() => (stats()?.days ?? []).slice(-14));
  const peakDay = createMemo(() => Math.max(1, ...days().map((d) => d.total)));
  const models = createMemo(() => (stats()?.models ?? []).slice(0, 10));
  const peakModel = createMemo(() => Math.max(1, ...models().map((m) => m.total)));

  return (
    <div class="usage-stats">
      <div class="usage-stats-head">
        <p class="field-hint">
          统计本机全部会话的轮次用量（含数字员工、分身与子会话）。缓存命中率 = 缓存读取 /（新增输入 + 缓存读取 + 缓存写入）。
        </p>
        <button type="button" class="link-btn" disabled={busy()} onClick={() => void load()}>
          {busy() ? "刷新中…" : "刷新"}
        </button>
      </div>

      <Show
        when={stats()}
        fallback={<div class="sel-empty">{busy() ? "统计中…" : "暂无用量记录"}</div>}
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
                <strong class="usage-card-value" title={`缓存读取 ${exact(s().total.cacheRead)}`}>
                  {pct(hitRate(s().total))}
                </strong>
                <span class="usage-card-sub">读取 {fmtTokens(s().total.cacheRead)}</span>
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
              <h4>最近 14 天</h4>
              <Show when={days().length} fallback={<div class="sel-empty">最近没有用量</div>}>
                <For each={days()}>
                  {(d) => (
                    <div class="usage-bar-row">
                      <span class="usage-bar-day">{d.name.slice(5)}</span>
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
              <Show when={models().length} fallback={<div class="sel-empty">暂无用量记录</div>}>
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
