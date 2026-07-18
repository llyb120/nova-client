import { createEffect, createSignal, onMount, Show } from "solid-js";
import { AmbientScene } from "./components/AmbientScene";
import { ChatView } from "./components/ChatView";
import { CliOperationModal } from "./components/CliOperationModal";
import { DecisionWorkbench } from "./components/DecisionWorkbench";
import { EmployeesView } from "./components/EmployeesView";
import { EvidenceChainView } from "./components/EvidenceChainView";
import { HomeView } from "./components/HomeView";
import { RoamRequestModal } from "./components/RoamRequestModal";
import { SettingsModal } from "./components/SettingsModal";
import { ShareInboxModal } from "./components/ShareInboxModal";
import { Sidebar } from "./components/Sidebar";
import { UpdateModal } from "./components/UpdateModal";
import { initStore, state } from "./store";

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
  const [showUpdate, setShowUpdate] = createSignal(false);
  const [showInbox, setShowInbox] = createSignal(false);

  onMount(() => {
    void initStore();
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
      <Show when={state.uiStyle === "classic"}>
        <AmbientScene />
      </Show>
      <Sidebar
        onOpenSettings={() => setShowSettings(true)}
        onOpenUpdate={() => setShowUpdate(true)}
        onOpenInbox={() => setShowInbox(true)}
      />
      <Show
        when={state.currentId}
        fallback={
          <Show when={state.view === "clues"} fallback={
            <Show when={state.view === "employees"} fallback={
              <Show when={state.view === "workbench"} fallback={<HomeView />}>
                <DecisionWorkbench />
              </Show>
            }>
              <EmployeesView />
            </Show>
          }>
            <EvidenceChainView />
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
      <Show when={showInbox()}>
        <ShareInboxModal onClose={() => setShowInbox(false)} />
      </Show>
      <RoamRequestModal />
      <UpdateModal show={showUpdate()} onClose={() => setShowUpdate(false)} />
      <CliOperationModal />
    </div>
  );
}
