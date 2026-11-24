import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import { Portal } from "@solidjs/web";
import {
  activeAntigravityAccount,
  loginAntigravityAccount,
  refreshAntigravityAccounts,
  state,
  switchAntigravityAccount,
  syncAntigravityAccount,
} from "../store";
import type { AntigravityAccount } from "../types";
import { IconAntigravity, IconCheck, IconChevron, IconPlus, IconRefresh } from "./icons";

function shortTierLabel(tierName?: string | null): string | null {
  const raw = tierName?.trim();
  if (!raw) return null;
  if (/ultra/i.test(raw)) return "Ultra";
  if (/pro/i.test(raw)) return "Pro";
  if (/enterprise|business/i.test(raw)) return "Enterprise";
  if (/free|standard/i.test(raw)) return "Free";
  return raw;
}

function accountDisplayName(acc: AntigravityAccount): string {
  return acc.label.trim() || acc.email.trim() || "Antigravity 账号";
}

/**
 * Antigravity CLI 一键切换账号浮层（用于新建会话页与聊天页的底部工具条）。
 * 点击任意已保存账号即可立即切换系统凭据、重启空闲 ACP 进程并刷新模型列表。
 */
export function AntigravityAccountPicker(props: {
  disabled?: boolean;
}) {
  const [opened, setOpened] = createSignal(false);
  const [statusMsg, setStatusMsg] = createSignal("");
  const [statusTone, setStatusTone] = createSignal<"info" | "ok" | "error">("info");
  const [coords, setCoords] = createSignal<{ left: number; top?: number; bottom?: number }>({
    left: 12,
    bottom: 60,
  });

  let triggerRef: HTMLButtonElement | undefined;
  let panelRef: HTMLDivElement | undefined;

  const snap = () => state.antigravityAccounts;
  const accounts = createMemo(() => snap()?.accounts ?? []);
  const active = createMemo(() => activeAntigravityAccount());
  const busy = () => state.antigravityAccountBusy;
  const canInteractiveLogin = () =>
    snap()?.canInteractiveLogin ?? /Windows/i.test(navigator.userAgent);

  // 首次挂载时懒拉取一次账号列表（纯本地凭据读取，零子进程开销）。
  createEffect(
    () => !!(state.settings?.antigravityEnabled && state.antigravityAccounts === null),
    (needLoad) => {
      if (needLoad) void refreshAntigravityAccounts();
    },
  );

  const triggerText = createMemo(() => {
    if (busy()) return "切换中…";
    const cur = active();
    if (cur) return accountDisplayName(cur);
    const email = snap()?.currentEmail;
    if (email) return email.split("@")[0] || email;
    return "账号";
  });

  const triggerTitle = createMemo(() => {
    if (props.disabled) return "当前会话运行中，结束后可切换 Antigravity 账号";
    const cur = active();
    if (cur) {
      const parts = [cur.email || cur.label];
      if (cur.tierName) parts.push(cur.tierName);
      return `Antigravity 账号：${parts.join(" · ")}\n点击一键切换账号`;
    }
    if (snap()?.currentEmail) {
      return `Antigravity 当前终端登录：${snap()!.currentEmail}\n点击管理或切换账号`;
    }
    return "Antigravity 账号：点击添加或一键切换账号";
  });

  const updateCoords = () => {
    const rect = triggerRef?.getBoundingClientRect();
    if (!rect) return;
    const panelWidth = 286;
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
    updateCoords();
    const onPointerDown = (event: MouseEvent) => {
      const target = event.target as Node | null;
      if (
        target &&
        (triggerRef?.contains(target) || panelRef?.contains(target))
      ) {
        return;
      }
      if (!busy()) setOpened(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !busy()) {
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
    if (props.disabled) return;
    const next = !opened();
    setOpened(next);
    if (next) {
      setStatusMsg("");
      void refreshAntigravityAccounts();
    }
  };

  const handleSwitch = async (acc: AntigravityAccount) => {
    if (busy()) return;
    if (snap()?.activeAccountId === acc.id) {
      setOpened(false);
      return;
    }
    setStatusTone("info");
    setStatusMsg(`正在切换到 ${accountDisplayName(acc)}…`);
    try {
      const outcome = await switchAntigravityAccount(acc.id);
      if (outcome.warning) {
        setStatusTone("error");
        setStatusMsg(outcome.warning);
      } else {
        setStatusTone("ok");
        setStatusMsg(`已切换到 ${accountDisplayName(acc)}`);
        setOpened(false);
      }
    } catch (error) {
      setStatusTone("error");
      setStatusMsg(String(error));
    }
  };

  const handleAddAccount = async () => {
    if (busy()) return;
    setStatusTone("info");
    setStatusMsg("请在弹出的 agy 登录窗口完成 Google 授权…");
    try {
      const outcome = await loginAntigravityAccount();
      if (outcome.warning) {
        setStatusTone("error");
        setStatusMsg(outcome.warning);
      } else {
        const added = outcome.snapshot.accounts.find(
          (a) => a.id === outcome.snapshot.activeAccountId,
        );
        setStatusTone("ok");
        setStatusMsg(added ? `已添加并切换到 ${accountDisplayName(added)}` : "已登录新账号");
      }
    } catch (error) {
      setStatusTone("error");
      setStatusMsg(String(error));
    }
  };

  const handleSyncTerminal = async () => {
    if (busy()) return;
    setStatusTone("info");
    setStatusMsg("正在同步终端登录…");
    try {
      const outcome = await syncAntigravityAccount();
      if (outcome.warning) {
        setStatusTone("error");
        setStatusMsg(outcome.warning);
      } else {
        setStatusTone("ok");
        setStatusMsg("已同步本机终端登录");
      }
    } catch (error) {
      setStatusTone("error");
      setStatusMsg(String(error));
    }
  };

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        class={["pill", "agy-account-pill", { active: opened(), busy: busy() }]}
        disabled={props.disabled}
        title={triggerTitle()}
        aria-haspopup="menu"
        aria-expanded={opened() ? "true" : "false"}
        onClick={toggleOpen}
      >
        <IconAntigravity size={13} />
        <span class="pill-text">{triggerText()}</span>
        <Show when={!busy() && shortTierLabel(active()?.tierName)}>
          {(tier) => <span class="agy-account-tier-chip">{tier()}</span>}
        </Show>
        <IconChevron size={11} open={opened()} />
      </button>

      <Show when={opened()}>
        <Portal>
          <div
            ref={panelRef}
            class="agy-account-popover"
            role="menu"
            style={{
              left: `${coords().left}px`,
              ...(coords().top !== undefined ? { top: `${coords().top}px` } : {}),
              ...(coords().bottom !== undefined ? { bottom: `${coords().bottom}px` } : {}),
            }}
          >
            <div class="agy-account-popover-head">
              <span class="agy-account-popover-title">
                Antigravity 账号
                <Show when={accounts().length > 0}>
                  <span class="agy-account-count">{accounts().length}</span>
                </Show>
              </span>
              <button
                type="button"
                class="agy-account-icon-btn"
                title="同步终端当前登录"
                disabled={busy()}
                onClick={() => void handleSyncTerminal()}
              >
                <IconRefresh size={13} />
              </button>
            </div>

            <div class="agy-account-popover-list">
              <Show
                when={accounts().length > 0}
                fallback={
                  <div class="agy-account-empty">
                    尚未检测到已保存账号，可点击下方登录或同步终端账号。
                  </div>
                }
              >
                <For each={accounts()}>
                  {(acc) => {
                    const isActive = () => snap()?.activeAccountId === acc.id;
                    const tier = () => shortTierLabel(acc.tierName);
                    return (
                      <button
                        type="button"
                        role="menuitemradio"
                        aria-checked={isActive() ? "true" : "false"}
                        class={["agy-account-option", { active: isActive() }]}
                        disabled={busy()}
                        onClick={() => void handleSwitch(acc)}
                      >
                        <span class="agy-account-option-check" aria-hidden="true">
                          <Show when={isActive()}>
                            <IconCheck size={13} />
                          </Show>
                        </span>
                        <span class="agy-account-option-body">
                          <span class="agy-account-option-top">
                            <span class="agy-account-option-label">{accountDisplayName(acc)}</span>
                            <Show when={tier()}>
                              {(t) => <span class="agy-account-tier-chip">{t()}</span>}
                            </Show>
                          </span>
                          <Show when={acc.email && acc.email !== acc.label}>
                            <span class="agy-account-option-email">{acc.email}</span>
                          </Show>
                        </span>
                      </button>
                    );
                  }}
                </For>
              </Show>
            </div>

            <Show when={statusMsg()}>
              <div class={`agy-account-status ${statusTone()}`} role="status">
                {statusMsg()}
              </div>
            </Show>

            <div class="agy-account-popover-foot">
              <Show when={canInteractiveLogin()}>
                <button
                  type="button"
                  class="agy-account-foot-btn primary"
                  disabled={busy()}
                  onClick={() => void handleAddAccount()}
                >
                  <IconPlus size={12} />
                  <span>添加账号</span>
                </button>
              </Show>
              <button
                type="button"
                class="agy-account-foot-btn"
                disabled={busy()}
                onClick={() => void handleSyncTerminal()}
              >
                <IconRefresh size={12} />
                <span>同步终端</span>
              </button>
            </div>
          </div>
        </Portal>
      </Show>
    </>
  );
}
