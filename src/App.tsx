import { createEffect, createSignal, lazy, onSettled, Show, Loading } from "solid-js";
import { listen } from "@tauri-apps/api/event";
import { AchievementsModal } from "./components/AchievementsModal";
import { ChatView } from "./components/ChatView";
import { SkyBackdrop } from "./components/SkyBackdrop";
import { HomeView } from "./components/HomeView";
import { ExclusiveChatMark } from "./components/ExclusiveChatMark";
import { RoamRequestModal } from "./components/RoamRequestModal";
import { SettingsModal } from "./components/SettingsModal";
import { ShareInboxModal } from "./components/ShareInboxModal";
import { Sidebar } from "./components/Sidebar";
import { SignatureSplash } from "./components/SignatureSplash";
import { UpdateModal } from "./components/UpdateModal";
import { TitleBar } from "./components/TitleBar";
import "./promptQueue";
import { initLayoutEvents } from "./layoutStore";
import { scenesReady } from "./types";
import { selectedChatText } from "./chatSelection";
import { closeThread, initStore, openNewSession, openNextUnreadThread, setView, state, toastMessageSignal } from "./store";
import { mountSessionShortcuts, setShortcutRecordingActive } from "./sessionShortcuts";
import { createHomeTerminalState, setWorkspaceLayout, workspaceLayout } from "./workspaceLayout";
const HomeTerminalPanel = lazy(() => import("./components/HomeTerminalPanel"));
const KnowledgeGraphView = lazy(() => import("./components/KnowledgeGraphView"));
const JianpuView = lazy(() => import("./components/JianpuView"));
const EmployeeView = lazy(() => import("./components/EmployeeView"));
const MiniAppsView = lazy(() => import("./components/MiniAppsView"));

function SettingsLoadingModal(props: { onClose: () => void }) {
  return (
    <div class="modal-backdrop">
      <div class="modal settings-modal">
        <div class="modal-head">
          <span>设置</span>
          <button class="icon-btn" onClick={props.onClose}>
            ×
          </button>
        </div>
        <div class="modal-body">
          <div class="field-hint">正在读取已保存配置…</div>
        </div>
      </div>
    </div>
  );
}

/** 全局轻提示：优先贴着提示词输入框显示（发送后用户视线停留在输入框处），找不到输入框退回底部居中。 */
function AppToast() {
  return (
    <Show when={toastMessageSignal()} keyed>
      {(msg) => <AppToastChip text={msg} />}
    </Show>
  );
}

function AppToastChip(props: { text: string }) {
  let el: HTMLDivElement | undefined;
  onSettled(() => {
    if (!el) return;
    // 发送方随后才会清空输入框/附件/引用，首页居中布局随之收缩下移；
    // 先隐藏，等下一帧布局稳定后再测量定位，避免贴着「收缩前」的输入框。
    el.style.visibility = "hidden";
    requestAnimationFrame(() => {
      const composer = document.querySelector(".home-composer") ?? document.querySelector(".composer");
      if (!el) return;
      if (composer) {
        const rect = composer.getBoundingClientRect();
        const gap = 10;
        el.style.bottom = "auto";
        el.classList.add("bubble");
        // 左对齐到输入框左上角，不再水平居中。
        el.style.left = `${rect.left}px`;
        el.style.transform = "none";
        const above = rect.top - el.offsetHeight - gap;
        if (above >= 8) {
          el.style.top = `${above}px`;
        } else {
          // 上方放不下时退到输入框下方，气泡尾巴翻到上侧。
          el.classList.add("below");
          el.style.top = `${rect.bottom + gap}px`;
        }
      }
      el.style.visibility = "";
    });
  });
  return (
    <div ref={el} class="app-toast">
      {props.text}
    </div>
  );
}

export default function App() {
  const homeTerminal = createHomeTerminalState(() => ({
    currentId: state.currentId,
    view: state.view,
    homeComposerFocusAt: state.homeComposerFocusAt,
  }));
  const [showSettings, setShowSettings] = createSignal(false);
  // 先决条件：三个场景都至少配置一个模型，否则启动即弹出不可关闭的首次引导（后端 → 场景模型）。
  const scenesMissing = () => !!state.settings && !scenesReady(state.settings.sceneModels);
  const [showAchievements, setShowAchievements] = createSignal(false);
  const [showUpdate, setShowUpdate] = createSignal(false);
  const [showInbox, setShowInbox] = createSignal(false);
  createEffect(
    () => state.view === "knowledge" && !state.settings?.knowledgeGraphEnabled,
    disabled => { if (disabled) setView("home"); },
  );

  onSettled(() => {
    setShortcutRecordingActive(false);
    void initStore();
    initLayoutEvents();
    // Chrome page launchers follow the published library from startup, not only after visiting the studio.
    void import("./miniapps").then(m => m.syncBrowserTriggers()).catch(() => {});

    // Native global shortcut events also arrive while the WebView is unfocused/minimized.
    let disposed = false;
    const unlistens: Array<() => void> = [];
    const onGlobalShortcut = (event: string, handler: () => void) =>
      void listen(event, handler).then((remove) => {
        if (disposed) remove();
        else unlistens.push(remove);
      });
    onGlobalShortcut("session-shortcut:new-session", () => openNewSession(selectedChatText()));
    onGlobalShortcut("session-shortcut:open-unread", () => void openNextUnreadThread());
    // Butterfly 快捷入口：关闭当前会话并切换主区域。
    void listen<{ view: "knowledge" | "miniapps"; appId?: string; run?: boolean; create?: boolean }>("nova:open-view", event => {
      const view = event.payload?.view;
      if (view !== "knowledge" && view !== "miniapps") return;
      if (view === "miniapps") sessionStorage.setItem("nova:miniapps:open", JSON.stringify(event.payload));
      closeThread();
      setView(view);
      if (view === "miniapps") window.dispatchEvent(new Event("nova:miniapps-open"));
    })
      .then(remove => { if (disposed) remove(); else unlistens.push(remove); });
    return () => {
      disposed = true;
      for (const unlisten of unlistens) unlisten();
    };
  });

  // 「打开未读消息」在应用聚焦时的按键处理；全局注册（Rust 侧）覆盖最小化/失焦场景。
  mountSessionShortcuts({
    allowedActions: ["openUnread", "toggleTerminal"],
    onOpenUnread: () => void openNextUnreadThread(),
    onToggleTerminal: () => {
      if (!state.currentId) { homeTerminal.toggle(); return; }
      if (state.threads.find(thread => thread.id === state.currentId)?.roamingRole === "guest") return;
      setWorkspaceLayout({ open: !(workspaceLayout.open && workspaceLayout.mode === "terminal"), mode: "terminal" });
    },
  });

  // 空闲时后端请求更新（update:prompt）→ 自动弹出更新对话框，由用户选择是否现在更新。
  createEffect(() => state.updatePromptAt, at => {
    if (at > 0) setShowUpdate(true);
  });

  // 漫游召回的快照到达 → 自动弹出收件箱，用户直接选项目接收
  createEffect(() => state.inboxPromptAt, at => {
    if (at > 0) setShowInbox(true);
  });

  return (
    <div class="app">
      {/* 全局星野：整扇窗口一张画布，压在应用层之下，暗色主题才出银河 */}
      <SkyBackdrop />
      <ExclusiveChatMark token={state.settings?.relayToken || ""} />
      <TitleBar />
      <div class="app-body">
        <Sidebar
        onOpenSettings={() => setShowSettings(true)}
        onOpenAchievements={() => setShowAchievements(true)}
        onOpenUpdate={() => setShowUpdate(true)}
        onOpenInbox={() => setShowInbox(true)}
      />
      <Show
        when={state.currentId}
        fallback={
          <Show when={state.view === "knowledge" && state.settings?.knowledgeGraphEnabled} fallback={
            <Show when={state.view === "employee"} fallback={
              <Show when={state.view === "jianpu"} fallback={
                <Show when={state.view === "miniapps"} fallback={<HomeView />}>
                  <Loading><MiniAppsView /></Loading>
                </Show>
              }>
                <Loading><JianpuView /></Loading>
              </Show>
            }>
              <Loading><EmployeeView /></Loading>
            </Show>
          }>
            <Loading><KnowledgeGraphView /></Loading>
          </Show>
        }
      >
        <ChatView />
      </Show>
      <Show when={homeTerminal.open()}>
        <Loading><HomeTerminalPanel onClose={homeTerminal.close} /></Loading>
      </Show>
      </div>
      <Show when={showSettings() || scenesMissing()}>
        <Show
          when={state.settings}
          fallback={<SettingsLoadingModal onClose={() => setShowSettings(false)} />}
        >
          <Show when={scenesMissing()} fallback={<SettingsModal onClose={() => setShowSettings(false)} />}>
            <SettingsModal onboarding onClose={() => setShowSettings(false)} />
          </Show>
        </Show>
      </Show>
      <Show when={showAchievements()}>
        <AchievementsModal onClose={() => setShowAchievements(false)} />
      </Show>
      <Show when={showInbox()}>
        <ShareInboxModal onClose={() => setShowInbox(false)} />
      </Show>
      <RoamRequestModal />
      <UpdateModal show={showUpdate()} onClose={() => setShowUpdate(false)} />
      <SignatureSplash />
      <AppToast />
    </div>
  );
}
