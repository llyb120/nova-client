import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { listen } from "@tauri-apps/api/event";
import { AchievementsModal } from "./components/AchievementsModal";
import { ChatView } from "./components/ChatView";
import { CliOperationModal } from "./components/CliOperationModal";
import { EvidenceChainView } from "./components/EvidenceChainView";
import { HomeView } from "./components/HomeView";
import { RoamRequestModal } from "./components/RoamRequestModal";
import { SettingsModal } from "./components/SettingsModal";
import { ShareInboxModal } from "./components/ShareInboxModal";
import { Sidebar } from "./components/Sidebar";
import { SignatureSplash } from "./components/SignatureSplash";
import { UpdateModal } from "./components/UpdateModal";
import { TrainingGroundView } from "./components/TrainingGroundView";
import { WorkflowsView } from "./components/WorkflowsView";
import "./promptQueue";
import "./training-ground.css";
import { selectedChatText } from "./chatSelection";
import { initStore, openNewSession, state } from "./store";

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

export default function App() {
  const [showSettings, setShowSettings] = createSignal(false);
  const [showAchievements, setShowAchievements] = createSignal(false);
  const [showUpdate, setShowUpdate] = createSignal(false);
  const [showInbox, setShowInbox] = createSignal(false);

  onMount(() => {
    void initStore();

    // Native global shortcut events also arrive while the WebView is unfocused/minimized.
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen("session-shortcut:new-session", () => {
      openNewSession(selectedChatText());
    }).then((remove) => {
      if (disposed) remove();
      else unlisten = remove;
    });
    onCleanup(() => {
      disposed = true;
      unlisten?.();
    });
  });

  // 空闲时后端请求更新（update:prompt）→ 自动弹出更新对话框，由用户选择是否现在更新。
  createEffect(() => {
    if (state.updatePromptAt > 0) setShowUpdate(true);
  });

  // 漫游召回的快照到达 → 自动弹出收件箱，用户直接选项目接收
  createEffect(() => {
    if (state.inboxPromptAt > 0) setShowInbox(true);
  });

  return (
    <div class="app">
      <Sidebar
        onOpenSettings={() => setShowSettings(true)}
        onOpenAchievements={() => setShowAchievements(true)}
        onOpenUpdate={() => setShowUpdate(true)}
        onOpenInbox={() => setShowInbox(true)}
      />
      <Show
        when={state.currentId}
        fallback={
          <Show when={state.view === "workflows"} fallback={
          <Show when={state.view === "clues"} fallback={
            <Show when={state.view === "training"} fallback={<HomeView />}>
              <TrainingGroundView />
            </Show>
          }>
            <EvidenceChainView />
          </Show>
          }>
            <WorkflowsView />
          </Show>
        }
      >
        <ChatView />
      </Show>
      <Show when={showSettings()}>
        <Show
          when={state.settings}
          fallback={<SettingsLoadingModal onClose={() => setShowSettings(false)} />}
        >
          <SettingsModal onClose={() => setShowSettings(false)} />
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
      <CliOperationModal />
      <SignatureSplash />
    </div>
  );
}
