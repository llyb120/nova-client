import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { listen } from "@tauri-apps/api/event";
import { AchievementsModal } from "./components/AchievementsModal";
import { ChatView } from "./components/ChatView";
import { EvidenceChainView } from "./components/EvidenceChainView";
import { HomeView } from "./components/HomeView";
import BrowserView from "./components/BrowserView";
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
import { initStore, openNewSession, state, toastMessageSignal, zenDropLanded, zenDropSignal } from "./store";

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

/** 减少焦虑：提示词化作星尘沿弧线飞进侧栏「室女座」tab，落地后轻脉冲并弹轻提示。 */
function ZenDropOverlay() {
  return (
    <Show when={zenDropSignal()} keyed>
      {(drop) => <ZenDropChip text={drop.text} />}
    </Show>
  );
}

function ZenDropChip(props: { text: string }) {
  let el: HTMLDivElement | undefined;
  let raf = 0;
  onMount(() => {
    if (!el) return;
    const tab = document.getElementById("virgo-tab");
    const rect = tab?.getBoundingClientRect();
    // 起点取提示词输入框中心（发送后视图已回到首页），找不到时退回屏幕下方。
    const inputRect = document.querySelector(".composer-input")?.getBoundingClientRect();
    const start = inputRect
      ? { x: inputRect.left + inputRect.width / 2, y: inputRect.top + inputRect.height / 2 }
      : { x: window.innerWidth / 2, y: window.innerHeight - 150 };
    const end = rect
      ? { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 }
      : { x: 130, y: 180 };
    const dist = Math.hypot(end.x - start.x, end.y - start.y);
    const duration = Math.min(1050, Math.max(720, 620 + dist * 0.3));
    // 二次贝塞尔：中点上抬控制点，走出柔和弧线。
    const ctrl = {
      x: (start.x + end.x) / 2,
      y: Math.min(start.y, end.y) - Math.min(200, dist * 0.22),
    };
    const dot = el.querySelector(".zen-drop-dot") as HTMLElement | null;
    const label = el.querySelector(".zen-drop-label") as HTMLElement | null;
    const easeInOut = (t: number) => (t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2);
    const t0 = performance.now();
    const step = (now: number) => {
      const t = Math.min(1, (now - t0) / duration);
      const e = easeInOut(t);
      const u = 1 - e;
      const x = u * u * start.x + 2 * u * e * ctrl.x + e * e * end.x;
      const y = u * u * start.y + 2 * u * e * ctrl.y + e * e * end.y;
      // 起步轻弹放大，中段巡航，末段缩小「被吸进去」；标签在中途逐渐消散。
      const scale =
        t < 0.14
          ? 0.4 + 0.6 * (t / 0.14)
          : 1 - 0.65 * Math.pow(Math.max(0, (t - 0.55) / 0.45), 1.6);
      const fadeIn = Math.min(1, t / 0.1);
      el!.style.transform = `translate(${x}px, ${y}px)`;
      if (dot) {
        dot.style.transform = `scale(${scale})`;
        dot.style.opacity = String(fadeIn * (t > 0.9 ? (1 - t) / 0.1 : 1));
      }
      if (label) {
        label.style.opacity = String(Math.max(0, fadeIn * (1 - Math.max(0, (t - 0.32) / 0.33))));
      }
      if (t < 1) {
        raf = requestAnimationFrame(step);
      } else {
        if (tab) {
          tab.classList.add("virgo-landed");
          setTimeout(() => tab.classList.remove("virgo-landed"), 900);
        }
        zenDropLanded();
      }
    };
    raf = requestAnimationFrame(step);
  });
  onCleanup(() => cancelAnimationFrame(raf));
  return (
    <div ref={el} class="zen-drop">
      <div class="zen-drop-dot" />
      <div class="zen-drop-label">{props.text}</div>
    </div>
  );
}

/** 全局轻提示：优先贴着提示词输入框显示（减少焦虑发送后用户视线停留在输入框处），找不到输入框退回底部居中。 */
function AppToast() {
  return (
    <Show when={toastMessageSignal()} keyed>
      {(msg) => <AppToastChip text={msg} />}
    </Show>
  );
}

function AppToastChip(props: { text: string }) {
  let el: HTMLDivElement | undefined;
  onMount(() => {
    const composer = document.querySelector(".home-composer") ?? document.querySelector(".composer");
    if (!el || !composer) return;
    const rect = composer.getBoundingClientRect();
    const below = rect.bottom + 12;
    el.style.bottom = "auto";
    el.style.top =
      below + el.offsetHeight <= window.innerHeight - 8
        ? `${below}px`
        : `${Math.max(8, rect.top - el.offsetHeight - 12)}px`;
  });
  return (
    <div ref={el} class="app-toast">
      {props.text}
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
          <Show when={state.view === "browser"} fallback={
            <Show when={state.view === "training"} fallback={<HomeView />}>
              <TrainingGroundView />
            </Show>
          }>
            <BrowserView />
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
      <SignatureSplash />
      <ZenDropOverlay />
      <AppToast />
    </div>
  );
}
