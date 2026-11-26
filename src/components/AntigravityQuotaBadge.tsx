import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { Portal } from "@solidjs/web";
import {
  formatQuotaResetCountdown,
  formatQuotaUpdatedAgo,
  matchAntigravityQuotaGroup,
  pickQuotaBuckets,
  quotaTone,
  shortQuotaGroupName,
} from "../antigravityQuota";
import { ensureAntigravityQuota, modelChoices, refreshAntigravityQuota, state } from "../store";
import { IconRefresh } from "./icons";

const CLOCK_TICK_MS = 30_000;
const BG_REFRESH_MS = 180_000;

/**
 * Antigravity（反重力）紧凑配额胶囊与详情浮层（展示当前模型家族的 5h 与周剩余用量）。
 */
export function AntigravityQuotaBadge(props: {
  model: string;
  class?: string;
}) {
  const [opened, setOpened] = createSignal(false);
  const [nowMs, setNowMs] = createSignal(Date.now());
  const [coords, setCoords] = createSignal<{ left: number; top?: number; bottom?: number }>({
    left: 12,
    bottom: 60,
  });

  let triggerRef: HTMLButtonElement | undefined;
  let panelRef: HTMLDivElement | undefined;

  const snapshot = () => state.antigravityQuota;
  const loading = () => state.antigravityQuotaLoading;
  const errorMsg = () => state.antigravityQuotaError;

  // 挂载或切换账号时懒拉取一次配额（60s 内走后端/前端缓存）。
  createEffect(
    () => state.antigravityAccounts?.activeAccountId,
    () => {
      void ensureAntigravityQuota();
    },
  );

  // 定时刷新倒计时显示与后台静默同步（仅页面可见时刷新）。
  const clockTimer = window.setInterval(() => setNowMs(Date.now()), CLOCK_TICK_MS);
  const bgTimer = window.setInterval(() => {
    if (document.visibilityState === "visible") {
      void refreshAntigravityQuota(false);
    }
  }, BG_REFRESH_MS);
  onCleanup(() => {
    window.clearInterval(clockTimer);
    window.clearInterval(bgTimer);
  });

  const modelLabel = createMemo(() => {
    const raw = props.model.trim();
    if (!raw) return undefined;
    const choices = modelChoices("antigravity");
    return choices.find((c) => c.value === raw)?.name;
  });

  const activeGroup = createMemo(() =>
    matchAntigravityQuotaGroup(snapshot(), props.model, modelLabel()),
  );

  const activeBuckets = createMemo(() => pickQuotaBuckets(activeGroup()));

  const worstTone = createMemo<"ok" | "warn" | "low">(() => {
    const { fiveHour, weekly } = activeBuckets();
    const values = [fiveHour?.remainingPercent, weekly?.remainingPercent].filter(
      (v): v is number => typeof v === "number",
    );
    if (values.length === 0) return "ok";
    return quotaTone(Math.min(...values));
  });

  const fiveHourDisplay = createMemo(() => {
    const b = activeBuckets().fiveHour;
    if (!b) return null;
    return {
      percent: b.remainingPercent,
      tone: quotaTone(b.remainingPercent),
    };
  });

  const weeklyDisplay = createMemo(() => {
    const b = activeBuckets().weekly;
    if (!b) return null;
    return {
      percent: b.remainingPercent,
      tone: quotaTone(b.remainingPercent),
    };
  });

  const triggerTitle = createMemo(() => {
    const group = activeGroup();
    if (!group) {
      if (loading()) return "正在查询 Antigravity 用量…";
      if (errorMsg()) return `Antigravity 用量获取失败：${errorMsg()}\n点击重试或查看详情`;
      return "Antigravity 用量：点击查询剩余额度";
    }
    const { fiveHour, weekly } = activeBuckets();
    const lines = [`Antigravity · ${shortQuotaGroupName(group.name)}`];
    if (fiveHour) {
      const cd = formatQuotaResetCountdown(fiveHour.resetTime, nowMs());
      lines.push(`5h 剩余 ${fiveHour.remainingPercent}%${cd ? `（${cd}后重置）` : ""}`);
    }
    if (weekly) {
      const cd = formatQuotaResetCountdown(weekly.resetTime, nowMs());
      lines.push(`周剩余 ${weekly.remainingPercent}%${cd ? `（${cd}后重置）` : ""}`);
    }
    lines.push("点击查看全部分组或刷新");
    return lines.join("\n");
  });

  const updateCoords = () => {
    const rect = triggerRef?.getBoundingClientRect();
    if (!rect) return;
    const panelWidth = 268;
    const margin = 10;
    const left = Math.max(margin, Math.min(rect.left, window.innerWidth - panelWidth - margin));
    const spaceAbove = rect.top;
    if (spaceAbove >= 220) {
      setCoords({ left, bottom: window.innerHeight - rect.top + 8 });
    } else {
      setCoords({ left, top: rect.bottom + 8 });
    }
  };

  createEffect(opened, (isOpen) => {
    if (!isOpen) return;
    setNowMs(Date.now());
    updateCoords();
    const onPointerDown = (event: MouseEvent) => {
      const target = event.target as Node | null;
      if (target && (triggerRef?.contains(target) || panelRef?.contains(target))) {
        return;
      }
      setOpened(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        setOpened(false);
      }
    };
    const onResize = () => updateCoords();
    window.addEventListener("mousedown", onPointerDown, true);
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("resize", onResize);
    return () => {
      window.removeEventListener("mousedown", onPointerDown, true);
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("resize", onResize);
    };
  });

  const toggleOpen = () => {
    const next = !opened();
    setOpened(next);
    if (next && !snapshot() && !loading()) {
      void refreshAntigravityQuota(true);
    }
  };

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        class={[
          "pill",
          "agy-quota-pill",
          props.class,
          {
            active: opened(),
            loading: loading(),
            warn: worstTone() === "warn",
            low: worstTone() === "low",
          },
        ]}
        title={triggerTitle()}
        aria-haspopup="dialog"
        aria-expanded={opened() ? "true" : "false"}
        onClick={toggleOpen}
      >
        <Show
          when={fiveHourDisplay() || weeklyDisplay()}
          fallback={
            <Show
              when={loading()}
              fallback={<span class="agy-quota-placeholder">用量—</span>}
            >
              <span class="agy-quota-cell">
                <span class="agy-quota-tag">5h</span>
                <span class="agy-quota-val muted">—</span>
              </span>
              <span class="agy-quota-sep" aria-hidden="true" />
              <span class="agy-quota-cell">
                <span class="agy-quota-tag">周</span>
                <span class="agy-quota-val muted">—</span>
              </span>
            </Show>
          }
        >
          <Show when={fiveHourDisplay()}>
            {(item) => (
              <span class="agy-quota-cell">
                <span class="agy-quota-tag">5h</span>
                <span class={`agy-quota-val ${item().tone}`}>
                  {item().percent}<span class="agy-quota-unit">%</span>
                </span>
                <span class="agy-quota-bar" aria-hidden="true">
                  <span
                    class={`agy-quota-fill ${item().tone}`}
                    style={{ width: `${item().percent}%` }}
                  />
                </span>
              </span>
            )}
          </Show>
          <Show when={fiveHourDisplay() && weeklyDisplay()}>
            <span class="agy-quota-sep" aria-hidden="true" />
          </Show>
          <Show when={weeklyDisplay()}>
            {(item) => (
              <span class="agy-quota-cell">
                <span class="agy-quota-tag">周</span>
                <span class={`agy-quota-val ${item().tone}`}>
                  {item().percent}<span class="agy-quota-unit">%</span>
                </span>
                <span class="agy-quota-bar" aria-hidden="true">
                  <span
                    class={`agy-quota-fill ${item().tone}`}
                    style={{ width: `${item().percent}%` }}
                  />
                </span>
              </span>
            )}
          </Show>
        </Show>
      </button>

      <Show when={opened()}>
        <Portal>
          <div
            ref={panelRef}
            class="agy-quota-popover"
            role="dialog"
            aria-label="Antigravity 用量详情"
            style={{
              left: `${coords().left}px`,
              ...(coords().top !== undefined ? { top: `${coords().top}px` } : {}),
              ...(coords().bottom !== undefined ? { bottom: `${coords().bottom}px` } : {}),
            }}
          >
            <div class="agy-quota-popover-head">
              <div class="agy-quota-popover-title-wrap">
                <span class="agy-quota-popover-title">Antigravity 用量</span>
                <Show when={snapshot()}>
                  {(snap) => (
                    <span class="agy-quota-updated">
                      {formatQuotaUpdatedAgo(snap().fetchedAt, nowMs())}更新
                    </span>
                  )}
                </Show>
              </div>
              <button
                type="button"
                class={["agy-account-icon-btn", { "agy-quota-spin": loading() }]}
                title="立即刷新用量"
                disabled={loading()}
                onClick={() => void refreshAntigravityQuota(true)}
              >
                <IconRefresh size={13} />
              </button>
            </div>

            <div class="agy-quota-popover-body">
              <Show
                when={snapshot() && snapshot()!.groups.length > 0}
                fallback={
                  <div class="agy-quota-empty">
                    {loading() ? "正在查询剩余额度…" : "暂无用量数据，点击右上角刷新重试。"}
                  </div>
                }
              >
                <For each={snapshot()!.groups}>
                  {(group) => {
                    const isCurrent = () => activeGroup()?.name === group.name;
                    const buckets = () => pickQuotaBuckets(group);
                    const rows = () =>
                      [
                        buckets().fiveHour
                          ? { label: "5h 限额", bucket: buckets().fiveHour! }
                          : null,
                        buckets().weekly
                          ? { label: "周限额", bucket: buckets().weekly! }
                          : null,
                      ].filter((r): r is NonNullable<typeof r> => r !== null);

                    return (
                      <div class={["agy-quota-group", { current: isCurrent() }]}>
                        <div class="agy-quota-group-head" title={group.description ?? undefined}>
                          <span class="agy-quota-group-name">
                            {shortQuotaGroupName(group.name)}
                          </span>
                          <Show when={isCurrent()}>
                            <span class="agy-quota-current-chip">当前</span>
                          </Show>
                        </div>
                        <For each={rows()}>
                          {(row) => {
                            const tone = () => quotaTone(row.bucket.remainingPercent);
                            const resetText = () => {
                              const cd = formatQuotaResetCountdown(row.bucket.resetTime, nowMs());
                              if (cd) return cd === "即将重置" ? cd : `${cd}后重置`;
                              return row.bucket.remainingPercent >= 100 ? "满额" : "";
                            };
                            return (
                              <div class="agy-quota-row" title={row.bucket.description ?? undefined}>
                                <div class="agy-quota-row-top">
                                  <span class="agy-quota-row-label">{row.label}</span>
                                  <span class="agy-quota-row-meta">
                                    <Show when={resetText()}>
                                      {(hint) => <span class="agy-quota-reset">{hint()}</span>}
                                    </Show>
                                    <span class={`agy-quota-row-pct ${tone()}`}>
                                      {row.bucket.remainingPercent}%
                                    </span>
                                  </span>
                                </div>
                                <div class="agy-quota-track" aria-hidden="true">
                                  <div
                                    class={`agy-quota-fill ${tone()}`}
                                    style={{ width: `${row.bucket.remainingPercent}%` }}
                                  />
                                </div>
                              </div>
                            );
                          }}
                        </For>
                      </div>
                    );
                  }}
                </For>
              </Show>
            </div>

            <Show when={errorMsg()}>
              {(err) => (
                <div class="agy-account-status error" role="status">
                  {err()}
                </div>
              )}
            </Show>
          </div>
        </Portal>
      </Show>
    </>
  );
}
