import { createEffect, createSignal, Show, snapshot, untrack } from "solid-js";
import { api } from "../ipc";
import {
  ensureServerSupportProvider,
  selectServerSceneModels,
} from "../simpleMode";
import {
  installChromeExtensionWithJianlai,
  lastUsed,
  refreshRelayStatus,
  setState,
  state,
} from "../store";
import type { Settings } from "../types";

export type OnboardingStep = "choose" | "simple" | "chrome";

export function OnboardingWizard(props: {
  step: OnboardingStep;
  onStepChange: (step: OnboardingStep) => void;
  onSelectManual: () => void;
  onFinish: () => void;
}) {
  const initialSettings = untrack(() => snapshot(state.settings));
  const [relayServer, setRelayServer] = createSignal(initialSettings?.relayServer ?? "");
  const [relayToken, setRelayToken] = createSignal(initialSettings?.relayToken ?? "");
  const [busy, setBusy] = createSignal(false);
  const [statusText, setStatusText] = createSignal("");
  const [errorText, setErrorText] = createSignal("");
  const [configuredModelName, setConfiguredModelName] = createSignal("");

  const [chromeDep, setChromeDep] = createSignal<Awaited<ReturnType<typeof api.chromeDependencyStatus>> | null>(null);
  const [chromeChecking, setChromeChecking] = createSignal(false);
  const [chromeInstalling, setChromeInstalling] = createSignal(false);
  const [chromeError, setChromeError] = createSignal("");

  createEffect(
    () => props.step === "chrome",
    (isChrome) => {
      if (!isChrome) return;
      let cancelled = false;
      setChromeChecking(true);
      setChromeError("");
      void api
        .chromeDependencyStatus()
        .then((dep) => {
          if (!cancelled) setChromeDep(dep);
        })
        .catch((error) => {
          if (!cancelled) setChromeError(String(error));
        })
        .finally(() => {
          if (!cancelled) setChromeChecking(false);
        });
      return () => {
        cancelled = true;
      };
    },
  );

  const chromeReady = () => {
    const dep = chromeDep();
    return !!dep && dep.connected && !dep.outdated;
  };

  const configureSimpleMode = async () => {
    const server = relayServer().trim();
    const token = relayToken().trim();
    if (!server) {
      setErrorText("请填写团队中转服务地址（URL）");
      return;
    }
    if (!token) {
      setErrorText("请填写身份 Token");
      return;
    }
    const base = state.settings;
    if (!base) {
      setErrorText("配置尚未加载完成，请稍后重试");
      return;
    }

    setBusy(true);
    setErrorText("");
    setStatusText("正在校验中转站地址与 Token…");
    try {
      await api.verifyRelay(server, token, base.relayGroups ?? "");

      setStatusText("校验通过，正在拉取服务端模型…");
      // 后端 server-support 预设从磁盘上的 settings.json 读取中转地址与 token，需先落盘；
      // 保持 simpleMode 与 remoteControlEnabled 为原值，直到模型拉取和校验全部成功再一起提交。
      const preSettings: Settings = {
        ...snapshot(base),
        relayServer: server,
        relayToken: token,
        lyraEnabled: true,
      };
      await api.setSettings(preSettings);

      const rawLyra = await api.getLyraConfig();
      const { config: nextLyra, providerId, provider } = ensureServerSupportProvider(rawLyra);
      await api.saveLyraConfig(nextLyra);

      const fetchedModels = await api.fetchLyraModels(providerId, provider);
      const lyraOpts = await api.getModelOptions("lyra", true).catch(() => null);
      const selected = selectServerSceneModels(providerId, fetchedModels, lyraOpts);

      setStatusText("正在自动配置场景模型并开启远程控制…");
      const finalSettings: Settings = {
        ...preSettings,
        simpleMode: true,
        remoteControlEnabled: true,
        sceneModels: {
          lightweight: [selected.lightweight],
          coding: [selected.coding],
          computer: [selected.computer],
        },
      };
      await api.setSettings(finalSettings);

      setState((s) => {
        s.settings = finalSettings;
        s.connected = true;
        s.agentKind = "lyra";
        if (lyraOpts && !(lyraOpts as { pending?: boolean }).pending) {
          s.modelOptions.lyra = { pending: false, ...lyraOpts };
        }
      });
      lastUsed.setAgentKind("lyra");
      lastUsed.setModel("lyra", selected.lightweight.model);
      setTimeout(() => void refreshRelayStatus(), 500);

      setConfiguredModelName(selected.displayName);
      setStatusText("");
      props.onStepChange("chrome");
    } catch (error) {
      setStatusText("");
      setErrorText(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };

  const startChromeInstall = async () => {
    setChromeInstalling(true);
    setChromeError("");
    try {
      const status = await api.chromeDependencyStatus(true);
      await installChromeExtensionWithJianlai(status);
      props.onFinish();
    } catch (error) {
      setChromeError(error instanceof Error ? error.message : String(error));
    } finally {
      setChromeInstalling(false);
    }
  };

  return (
    <div class="modal-backdrop">
      <div class="modal onboarding-modal" role="dialog" aria-modal="true" aria-labelledby="onboarding-title">
        <div class="modal-head">
          <span id="onboarding-title">
            {props.step === "choose"
              ? "欢迎使用 Nova · 选择配置方式"
              : props.step === "simple"
                ? "简易模式 · 连接团队中转站"
                : "简易模式已就绪 · 安装 Chrome 扩展"}
          </span>
        </div>

        <div class="modal-body">
          <Show when={props.step === "choose"}>
            <p class="settings-group-desc">
              请选择适合你的使用方式。以后随时可以在「设置 → 通用」中切换简易模式或调整配置。
            </p>
            <div class="onboarding-mode-grid">
              <button
                type="button"
                class="onboarding-mode-card recommended"
                onClick={() => {
                  setErrorText("");
                  setStatusText("");
                  props.onStepChange("simple");
                }}
              >
                <div class="onboarding-mode-head">
                  <span class="onboarding-mode-title">1. 简易模式</span>
                  <span class="onboarding-badge">推荐</span>
                </div>
                <p class="onboarding-mode-sub">面向普通用户 · 填写地址和 Token 即可开箱使用</p>
                <ul class="onboarding-mode-list">
                  <li>自动连接团队中转站并配置服务端模型</li>
                  <li>自动开启远程控制，隐藏 Butterfly 桌面浮标</li>
                  <li>简化界面：无需选择模型与项目，会话按时间展示</li>
                </ul>
              </button>

              <button
                type="button"
                class="onboarding-mode-card"
                onClick={props.onSelectManual}
              >
                <div class="onboarding-mode-head">
                  <span class="onboarding-mode-title">2. 手动模式</span>
                </div>
                <p class="onboarding-mode-sub">面向开发者 · 自由配置服务商与各项细节</p>
                <ul class="onboarding-mode-list">
                  <li>手动配置 Lyra 模型服务商与三个场景模型</li>
                  <li>保留项目选择器、模型选择器与 Git Worktree</li>
                  <li>保留完整的 Butterfly 与高级工具控制</li>
                </ul>
              </button>
            </div>
          </Show>

          <Show when={props.step === "simple"}>
            <p class="settings-group-desc">
              请填写团队管理员提供的中转服务地址和个人 Token。校验通过后会自动拉取服务端模型、开启远程控制并启用简易模式。
            </p>
            <label class="field">
              <span class="field-label">中转服务地址（URL）</span>
              <input
                class="field-input"
                type="url"
                placeholder="http://127.0.0.1:8320"
                value={relayServer()}
                disabled={busy()}
                onInput={(e) => setRelayServer(e.currentTarget.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.isComposing && !busy()) {
                    e.preventDefault();
                    void configureSimpleMode();
                  }
                }}
              />
            </label>
            <label class="field">
              <span class="field-label">身份 Token</span>
              <input
                class="field-input"
                type="text"
                autocomplete="off"
                placeholder="填写服务端配置的 token"
                value={relayToken()}
                disabled={busy()}
                onInput={(e) => setRelayToken(e.currentTarget.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.isComposing && !busy()) {
                    e.preventDefault();
                    void configureSimpleMode();
                  }
                }}
              />
            </label>
            <Show when={statusText()}>
              <div class="onboarding-status" role="status">{statusText()}</div>
            </Show>
            <Show when={errorText()}>
              <div class="onboarding-error" role="alert">{errorText()}</div>
            </Show>
          </Show>

          <Show when={props.step === "chrome"}>
            <div class="onboarding-summary">
              <div>✓ 已启用简易模式（Butterfly 浮标已隐藏，会话按时间展示）</div>
              <div>✓ 已连接团队中转站并自动开启远程控制</div>
              <div>✓ 已自动配置场景模型：{configuredModelName() || "服务端默认模型"}</div>
            </div>
            <div class="onboarding-chrome-box">
              <h3 class="settings-group-title">安装 Nova Chrome 扩展</h3>
              <p class="settings-group-desc">
                安装 Chrome 扩展后，Nova 可以直接在你的 Chrome 浏览器中帮你浏览网页、填写表单或完成线上操作。点击下方按钮将自动开启安装会话。
              </p>
              <div class="field-hint">
                <Show when={!chromeChecking()} fallback={<span>正在检测 Chrome 与扩展状态…</span>}>
                  <Show when={chromeDep()}>
                    {(dep) => (
                      <>
                        {dep().chromePath
                          ? chromeReady()
                            ? "检测到 Nova Chrome 扩展已安装且已连接 ✓"
                            : dep().connected && dep().outdated
                              ? `检测到旧版扩展（当前 ${dep().installedVersion ?? "旧版"}，最新 ${dep().bundledVersion}），建议一键更新。`
                              : "已检测到 Google Chrome，尚未连接 Nova 扩展。"
                          : "未检测到 Google Chrome，请先安装 Chrome 浏览器，之后可在「设置 → 剑来」中随时安装扩展。"}
                      </>
                    )}
                  </Show>
                </Show>
              </div>
              <Show when={chromeError()}>
                <div class="onboarding-error" role="alert">{chromeError()}</div>
              </Show>
            </div>
          </Show>
        </div>

        <div class="modal-foot">
          <Show when={props.step === "choose"}>
            <span class="field-hint">选择任一模式继续</span>
          </Show>
          <Show when={props.step === "simple"}>
            <button
              type="button"
              class="btn secondary"
              disabled={busy()}
              onClick={() => props.onStepChange("choose")}
            >
              返回
            </button>
            <button
              type="button"
              class="btn primary"
              disabled={busy() || !relayServer().trim() || !relayToken().trim()}
              onClick={() => void configureSimpleMode()}
            >
              {busy() ? "配置中…" : "校验并配置"}
            </button>
          </Show>
          <Show when={props.step === "chrome"}>
            <button
              type="button"
              class="btn secondary"
              disabled={chromeInstalling()}
              onClick={props.onFinish}
            >
              {chromeReady() ? "完成，开始使用" : "暂不安装，稍后再说"}
            </button>
            <Show when={!chromeReady()}>
              <button
                type="button"
                class="btn primary"
                disabled={chromeInstalling() || chromeDep()?.chromePath === null}
                onClick={() => void startChromeInstall()}
              >
                {chromeInstalling() ? "正在启动安装会话…" : "一键自动安装 Chrome 扩展"}
              </button>
            </Show>
          </Show>
        </div>
      </div>
    </div>
  );
}
