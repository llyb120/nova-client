import { message } from "@tauri-apps/plugin-dialog";
import { createSignal, Show } from "solid-js";
import { enabledAgentKinds, state } from "../store";
import type { AgentKind } from "../types";
import { ConfigSelects } from "./ConfigSelects";

/**
 * 「时光笔记」启动弹窗：输入 skill 名称、选择训练后端与模型后，
 * 新建一个训练会话自行阅读世界线材料并逐级分析。
 */
export function TimeNotesModal(props: {
  defaultName: string;
  onConfirm: (skillName: string, agentKind: AgentKind, model: string) => Promise<void>;
  onClose: () => void;
}) {
  const [name, setName] = createSignal(props.defaultName);
  const [agentKind, setAgentKind] = createSignal<AgentKind>(state.agentKind);
  const [model, setModel] = createSignal(state.model);
  const [busy, setBusy] = createSignal(false);

  const confirm = async () => {
    const trimmed = name().trim();
    if (!trimmed || busy()) return;
    setBusy(true);
    try {
      await props.onConfirm(trimmed, agentKind(), model());
    } catch (error) {
      await message(String(error), { kind: "error" });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="modal-backdrop" onMouseDown={() => !busy() && props.onClose()}>
      <div class="modal time-notes-modal" onMouseDown={(event) => event.stopPropagation()}>
        <div class="modal-head">
          <span>时光笔记</span>
        </div>
        <div class="modal-body">
          <p class="field-hint">
            新开一个训练会话，自行阅读这条世界线的成败历史，逐级分析并沉淀为可复用的 skill。
          </p>
          <label class="field">
            <span class="field-label">skill 名称</span>
            <input
              class="field-input"
              value={name()}
              placeholder="建议短横线小写英文，如 retry-with-git-reset"
              onInput={(event) => setName(event.currentTarget.value)}
              onKeyDown={(event) => event.key === "Enter" && void confirm()}
            />
          </label>
          <label class="field">
            <span class="field-label">训练模型</span>
            <ConfigSelects
              agentKind={agentKind()}
              agentKinds={enabledAgentKinds()}
              model={model()}
              portal
              favorites
              onPickModel={(kind, value) => {
                setAgentKind(kind);
                setModel(value);
              }}
            />
          </label>
        </div>
        <div class="modal-foot">
          <button class="btn" disabled={busy()} onClick={props.onClose}>
            取消
          </button>
          <button
            class="btn primary"
            disabled={busy() || !name().trim()}
            onClick={() => void confirm()}
          >
            <Show when={busy()} fallback="开始分析">
              准备中…
            </Show>
          </button>
        </div>
      </div>
    </div>
  );
}
