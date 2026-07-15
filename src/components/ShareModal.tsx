import { message } from "@tauri-apps/plugin-dialog";
import { createMemo, createSignal, For, Show } from "solid-js";
import { api } from "../ipc";
import { enabledAgentKinds, openThread, refreshThreads, state } from "../store";
import type { AgentKind, Peer } from "../types";
import { ClueCaptureModal } from "./ClueCaptureModal";
import { ModelPicker } from "./ConfigSelects";
import { IconBroadcast, IconX } from "./icons";

type Mode = "clue" | "quick" | "advanced";

/** Flow 对话框：线索（默认）/ 快速分享 / 高级分享 */
export function ShareModal(props: {
  threadId: string;
  /** 进入时默认 tab；默认线索 */
  initialMode?: Mode;
  onClose: () => void;
}) {
  const [mode, setMode] = createSignal<Mode>(props.initialMode ?? "clue");
  const [target, setTarget] = createSignal<string>("");
  const [prompt, setPrompt] = createSignal("总结这段会话的要点");
  // 高级分享在本机处理，后端 + 模型从下拉选择（默认取设置里的「分享后端 / 分享模型」）
  const [agent, setAgent] = createSignal<AgentKind>(
    (state.settings?.shareModelAgent as AgentKind) || "devin",
  );
  const [model, setModel] = createSignal(state.settings?.shareModel || "swe-1.6");
  const [busy, setBusy] = createSignal(false);
  const [done, setDone] = createSignal<string | null>(null);

  const myToken = () => state.settings?.relayToken ?? "";
  const peers = createMemo<Peer[]>(() =>
    state.peers.filter((p) => p.token !== myToken()),
  );
  const canShare = () => state.relay.connected;

  const submitShare = async () => {
    const to = target();
    if (!to) {
      await message("请先选择要分享给谁", { kind: "warning" });
      return;
    }
    setBusy(true);
    try {
      if (mode() === "quick") {
        await api.shareThread(props.threadId, to);
        setDone(to);
        setTimeout(() => props.onClose(), 900);
      } else {
        const t = await api.advancedShare(
          props.threadId,
          to,
          prompt().trim(),
          agent(),
          model().trim() || null,
        );
        await refreshThreads();
        await openThread(t.id);
        props.onClose();
      }
    } catch (e) {
      await message(String(e), { kind: "error" });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="modal-backdrop" onClick={(e) => e.target === e.currentTarget && props.onClose()}>
      <div
        class="modal"
        classList={{ "clue-capture-modal": mode() === "clue" }}
        onClick={(e) => e.stopPropagation()}
      >
        <div class="modal-head">
          <span>Flow</span>
          <button class="icon-btn" onClick={props.onClose}>
            <IconX size={16} />
          </button>
        </div>

        <div class="share-modal-tabbar">
          <div class="share-seg">
            <button
              class={`share-seg-btn ${mode() === "clue" ? "active" : ""}`}
              onClick={() => setMode("clue")}
            >
              线索
            </button>
            <button
              class={`share-seg-btn ${mode() === "quick" ? "active" : ""}`}
              onClick={() => setMode("quick")}
            >
              快速分享
            </button>
            <button
              class={`share-seg-btn ${mode() === "advanced" ? "active" : ""}`}
              onClick={() => setMode("advanced")}
            >
              高级分享
            </button>
          </div>
        </div>

        <Show when={mode() === "clue"}>
          <ClueCaptureModal
            threadId={props.threadId}
            sessionMode
            embedded
            onClose={props.onClose}
          />
        </Show>

        <Show when={mode() !== "clue"}>
          <div class="modal-body">
            <Show
              when={canShare()}
              fallback={
                <div class="inbox-empty">
                  <IconBroadcast size={26} />
                  <p>未连接到团队中转站</p>
                  <p class="field-hint">先在设置里填写 token，再分享给队友。</p>
                </div>
              }
            >
              <p class="field-hint">
                {mode() === "quick"
                  ? "直接把当前对话原样分享给队友。"
                  : "先用模型按你的提示词处理会话（如总结要点），把结果分享给队友。处理过程会在本机新开一个会话，跑完自动发送。"}
              </p>

              <Show when={mode() === "advanced"}>
                <label class="field">
                  <span class="field-label">处理提示词</span>
                  <textarea
                    class="field-input"
                    rows={3}
                    value={prompt()}
                    onInput={(e) => setPrompt(e.currentTarget.value)}
                    placeholder="例如：总结会话要点 / 提炼待办 / 翻译成英文"
                  />
                </label>
                <div class="field">
                  <span class="field-label">处理模型</span>
                  <ModelPicker
                    agentKind={agent()}
                    agentKinds={enabledAgentKinds()}
                    model={model()}
                    onPickModel={(a, m) => {
                      setAgent(a);
                      setModel(m);
                    }}
                    prefix="处理模型"
                    title="高级分享处理模型"
                    portal
                  />
                </div>
              </Show>

              <div class="field">
                <span class="field-label">分享给</span>
                <Show
                  when={peers().length > 0}
                  fallback={<p class="field-hint">暂时没有其他成员。</p>}
                >
                  <div class="share-peers">
                    <For each={peers()}>
                      {(p) => (
                        <button
                          class={`peer-row ${target() === p.token ? "active" : ""}`}
                          onClick={() => setTarget(p.token)}
                        >
                          <span class={`peer-dot ${p.online ? "on" : "off"}`} />
                          <span class="peer-name">{p.name}</span>
                          <span class="peer-action">{p.online ? "在线" : "离线"}</span>
                        </button>
                      )}
                    </For>
                  </div>
                </Show>
              </div>
            </Show>
          </div>
          <div class="modal-foot">
            <button class="btn secondary" disabled={busy()} onClick={props.onClose}>
              取消
            </button>
            <button
              class="btn primary"
              disabled={busy() || !canShare() || !target() || done() !== null}
              onClick={() => void submitShare()}
            >
              {done()
                ? "已发送 ✓"
                : busy()
                  ? mode() === "advanced"
                    ? "处理并分享…"
                    : "发送中…"
                  : mode() === "advanced"
                    ? "处理并分享"
                    : "分享"}
            </button>
          </div>
        </Show>
      </div>
    </div>
  );
}
