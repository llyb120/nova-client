import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For } from "solid-js";
import { associateClues, clueCurrentVersion, state } from "../store";
import { IconClue, IconX } from "./icons";

export function ClueAssociateModal(props: {
  initialBeforeCardId?: string | null;
  onClose: () => void;
}) {
  const cards = createMemo(() => state.clueGroups.flatMap((group) => group.cards));
  const initialBefore = () => props.initialBeforeCardId ?? cards()[0]?.id ?? "";
  const initialAfter = () => cards().find((card) => card.id !== initialBefore())?.id ?? "";
  const [beforeCardId, setBeforeCardId] = createSignal(initialBefore());
  const [afterCardId, setAfterCardId] = createSignal(initialAfter());
  const [busy, setBusy] = createSignal(false);

  createEffect(() => {
    if (beforeCardId() !== afterCardId()) return;
    setAfterCardId(cards().find((card) => card.id !== beforeCardId())?.id ?? "");
  });

  const submit = async () => {
    if (!beforeCardId() || !afterCardId() || beforeCardId() === afterCardId() || busy()) return;
    setBusy(true);
    try {
      await associateClues(beforeCardId(), afterCardId());
      props.onClose();
    } catch (error) {
      await message(String(error), { kind: "error" });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="modal-backdrop" onClick={(event) => event.target === event.currentTarget && props.onClose()}>
      <div class="modal clue-capture-modal" onClick={(event) => event.stopPropagation()}>
        <div class="modal-head">
          <span class="clue-modal-title">
            <IconClue size={15} />
            关联已有线索
          </span>
          <button class="icon-btn" title="关闭" disabled={busy()} onClick={props.onClose}>
            <IconX size={16} />
          </button>
        </div>
        <div class="modal-body">
          <label class="field">
            <span class="field-label">前置线索</span>
            <select
              class="field-input"
              value={beforeCardId()}
              onChange={(event) => setBeforeCardId(event.currentTarget.value)}
            >
              <For each={cards()}>
                {(card) => (
                  <option value={card.id}>{clueCurrentVersion(card)?.title || "未命名线索"}</option>
                )}
              </For>
            </select>
          </label>
          <label class="field">
            <span class="field-label">后续线索</span>
            <select
              class="field-input"
              value={afterCardId()}
              onChange={(event) => setAfterCardId(event.currentTarget.value)}
            >
              <For each={cards().filter((card) => card.id !== beforeCardId())}>
                {(card) => (
                  <option value={card.id}>{clueCurrentVersion(card)?.title || "未命名线索"}</option>
                )}
              </For>
            </select>
          </label>
          <div class="field-hint">只记录“前置 → 后续”的顺序。</div>
        </div>
        <div class="modal-foot">
          <button class="btn secondary" disabled={busy()} onClick={props.onClose}>
            取消
          </button>
          <button
            class="btn primary"
            disabled={!beforeCardId() || !afterCardId() || beforeCardId() === afterCardId() || busy()}
            onClick={() => void submit()}
          >
            {busy() ? "保存中…" : "建立前后顺序"}
          </button>
        </div>
      </div>
    </div>
  );
}
