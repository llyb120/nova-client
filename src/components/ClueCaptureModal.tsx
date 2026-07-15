import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import { captureClue, clueCardById, clueCurrentVersion, state } from "../store";
import { IconClue, IconX } from "./icons";

type Placement = "update" | "parallel" | "new";

export function ClueCaptureModal(props: {
  threadId?: string | null;
  initialPlacement?: Placement;
  initialTargetCardId?: string | null;
  onClose: () => void;
}) {
  const allCards = createMemo(() => state.clueGroups.flatMap((group) => group.cards));
  const currentMeta = () => state.threads.find((thread) => thread.id === props.threadId);
  const activeCardId = () => props.initialTargetCardId ?? currentMeta()?.activeClueCardId ?? null;
  const lastAssistant = () =>
    [...state.items].reverse().find((item) => item.type === "assistant")?.text?.trim() ?? "";
  const defaultPlacement = (): Placement =>
    props.initialPlacement ?? (activeCardId() ? "update" : "new");

  const [placement, setPlacement] = createSignal<Placement>(defaultPlacement());
  const [targetCardId, setTargetCardId] = createSignal(activeCardId() ?? "");
  const [title, setTitle] = createSignal(props.threadId ? state.title : "");
  const [content, setContent] = createSignal(props.threadId ? lastAssistant() : "");
  const [busy, setBusy] = createSignal(false);
  const targetCard = createMemo(() => clueCardById(targetCardId()));
  const targetCardTitle = createMemo(() => {
    const card = targetCard();
    return card ? clueCurrentVersion(card)?.title || "未命名线索" : "线索不存在";
  });

  createEffect(() => {
    const mode = placement();
    if ((mode === "update" || mode === "parallel") && !targetCardId()) {
      setTargetCardId(activeCardId() ?? allCards()[0]?.id ?? "");
    }
  });

  const targetLabel = () => {
    switch (placement()) {
      case "update":
        return "更新哪条线索";
      case "parallel":
        return "与哪条线索平行";
      default:
        return "接在哪条线索之后（留空即新起点）";
    }
  };

  const submit = async () => {
    if (!title().trim() || !content().trim() || busy()) return;
    if ((placement() === "update" || placement() === "parallel") && !targetCardId()) return;
    setBusy(true);
    try {
      await captureClue(
        props.threadId ?? null,
        title(),
        content(),
        placement(),
        targetCardId() || null,
      );
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
            生成线索
          </span>
          <button class="icon-btn" title="关闭" disabled={busy()} onClick={props.onClose}>
            <IconX size={16} />
          </button>
        </div>
        <div class="modal-body">
          <div class="clue-placement">
            <button
              type="button"
              classList={{ "clue-placement-btn": true, active: placement() === "update" }}
              disabled={allCards().length === 0}
              onClick={() => setPlacement("update")}
            >
              更新旧线索
            </button>
            <button
              type="button"
              classList={{ "clue-placement-btn": true, active: placement() === "parallel" }}
              disabled={allCards().length === 0}
              onClick={() => setPlacement("parallel")}
            >
              平行后续线索
            </button>
            <button
              type="button"
              classList={{ "clue-placement-btn": true, active: placement() === "new" }}
              onClick={() => setPlacement("new")}
            >
              开启新线索
            </button>
          </div>

          <Show when={allCards().length > 0 && (placement() !== "new" || targetCardId())}>
            <label class="field">
              <span class="field-label">{targetLabel()}</span>
              <select
                class="field-input"
                value={targetCardId()}
                onChange={(event) => setTargetCardId(event.currentTarget.value)}
              >
                <Show when={placement() === "new"}>
                  <option value="">不接前置线索</option>
                </Show>
                <For each={allCards()}>
                  {(card) => (
                    <option value={card.id}>
                      {clueCurrentVersion(card)?.title || "未命名线索"}
                    </option>
                  )}
                </For>
              </select>
            </label>
          </Show>

          <label class="field">
            <span class="field-label">线索标题</span>
            <input
              class="field-input"
              value={title()}
              maxlength={100}
              placeholder="这条线索解决或确认了什么"
              onInput={(event) => setTitle(event.currentTarget.value)}
            />
          </label>

          <label class="field">
            <span class="field-label">线索内容</span>
            <textarea
              class="field-input clue-content-input"
              rows={10}
              value={content()}
              placeholder="记录结论、证据、产物、验证结果和下一步"
              onInput={(event) => setContent(event.currentTarget.value)}
            />
          </label>

          <Show when={targetCardId()}>
            <div class="field-hint">
              当前选择：{targetCardTitle()}
            </div>
          </Show>
        </div>
        <div class="modal-foot">
          <button class="btn secondary" disabled={busy()} onClick={props.onClose}>
            取消
          </button>
          <button
            class="btn primary"
            disabled={
              busy() ||
              !title().trim() ||
              !content().trim() ||
              ((placement() === "update" || placement() === "parallel") && !targetCardId())
            }
            onClick={() => void submit()}
          >
            {busy() ? "保存中…" : "保存线索"}
          </button>
        </div>
      </div>
    </div>
  );
}
