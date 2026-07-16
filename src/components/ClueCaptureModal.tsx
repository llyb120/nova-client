import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import {
  captureClue,
  clueCardById,
  clueCurrentVersion,
  clueMentionPeers,
  state,
  summarizeClue,
} from "../store";
import { IconClue, IconX } from "./icons";
import { MentionPicker } from "./MentionPicker";

type Placement = "update" | "parallel" | "new";

export function ClueCaptureModal(props: {
  threadId?: string | null;
  initialPlacement?: Placement;
  initialTargetCardId?: string | null;
  /** 会话内简化：仅添加新线索 + 可选堆叠 + AI 总结 */
  sessionMode?: boolean;
  /** 嵌入到其它弹窗：不渲染独立 backdrop / 标题栏 */
  embedded?: boolean;
  onClose: () => void;
}) {
  const allCards = createMemo(() => state.clueGroups.flatMap((group) => group.cards));
  const currentMeta = () => state.threads.find((thread) => thread.id === props.threadId);
  const activeCardId = () => props.initialTargetCardId ?? currentMeta()?.activeClueCardId ?? null;
  // 打开时锁定会话起点线索，避免保存后 activeClueCardId 被改写影响「不堆叠」时的父节点
  const baseParentId = activeCardId();
  const lastAssistant = () =>
    [...state.items].reverse().find((item) => item.type === "assistant")?.text?.trim() ?? "";
  const defaultPlacement = (): Placement =>
    props.sessionMode
      ? "new"
      : (props.initialPlacement ?? (activeCardId() ? "update" : "new"));
  const initialVersion = () => {
    if (props.threadId || props.sessionMode || defaultPlacement() !== "update") return undefined;
    const card = clueCardById(activeCardId());
    return card ? clueCurrentVersion(card) : undefined;
  };

  const [placement, setPlacement] = createSignal<Placement>(defaultPlacement());
  const [targetCardId, setTargetCardId] = createSignal(activeCardId() ?? "");
  const [stackSameSession, setStackSameSession] = createSignal(false);
  const [title, setTitle] = createSignal(initialVersion()?.title ?? (props.threadId ? state.title : ""));
  const [content, setContent] = createSignal(
    initialVersion()?.content ?? (props.threadId ? lastAssistant() : ""),
  );
  const [busy, setBusy] = createSignal(false);
  const [summarizing, setSummarizing] = createSignal(false);
  const [mentionTokens, setMentionTokens] = createSignal<string[]>([]);
  const mentionPeers = createMemo(clueMentionPeers);
  const targetCard = createMemo(() => clueCardById(targetCardId()));
  const targetCardTitle = createMemo(() => {
    const card = targetCard();
    return card ? clueCurrentVersion(card)?.title || "未命名线索" : "线索不存在";
  });

  const sameSessionCardId = createMemo(() => {
    const threadId = props.threadId;
    if (!threadId) return null;
    const matches = allCards().filter((card) =>
      card.versions.some((version) => version.sourceThreadId === threadId),
    );
    if (matches.length === 0) return null;
    return [...matches].sort((a, b) => b.updatedAt - a.updatedAt)[0]?.id ?? null;
  });

  createEffect(() => {
    if (props.sessionMode) return;
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
        return "堆叠到哪组线索";
      default:
        return "接在哪条线索之后（留空即新起点）";
    }
  };

  const resolveSessionCapture = (): { placement: Placement; targetCardId: string | null } => {
    if (stackSameSession()) {
      const stackTarget = sameSessionCardId();
      if (stackTarget) {
        return { placement: "parallel", targetCardId: stackTarget };
      }
    }
    return { placement: "new", targetCardId: baseParentId };
  };

  const runAiSummary = async () => {
    if (!props.threadId || summarizing() || busy()) return;
    setSummarizing(true);
    try {
      const summary = await summarizeClue(props.threadId);
      if (summary.title.trim()) setTitle(summary.title.trim());
      if (summary.content.trim()) setContent(summary.content.trim());
    } catch (error) {
      await message(String(error), { kind: "error" });
    } finally {
      setSummarizing(false);
    }
  };

  const submit = async () => {
    if (!title().trim() || !content().trim() || busy() || summarizing()) return;
    let nextPlacement = placement();
    let nextTarget = targetCardId() || null;
    if (props.sessionMode) {
      const resolved = resolveSessionCapture();
      nextPlacement = resolved.placement;
      nextTarget = resolved.targetCardId;
    } else if ((nextPlacement === "update" || nextPlacement === "parallel") && !nextTarget) {
      return;
    }
    setBusy(true);
    try {
      await captureClue(
        props.threadId ?? null,
        title(),
        content(),
        nextPlacement,
        nextTarget,
        mentionTokens(),
      );
      props.onClose();
    } catch (error) {
      await message(String(error), { kind: "error" });
    } finally {
      setBusy(false);
    }
  };

  const body = (
    <>
      <div class="modal-body">
        <Show when={!props.sessionMode}>
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
              堆叠线索
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
        </Show>

        <Show when={props.sessionMode}>
          <p class="field-hint">把本次会话结论保存到证据链。默认新增一条线索；勾选后会与本会话已产生的线索堆叠（可溯源时）。</p>
          <label class="setting-check clue-stack-check">
            <input
              type="checkbox"
              checked={stackSameSession()}
              disabled={busy() || summarizing()}
              onChange={(event) => setStackSameSession(event.currentTarget.checked)}
            />
            <span>
              <span class="field-label">与同会话线索堆叠</span>
              <span class="field-hint">
                {sameSessionCardId()
                  ? "已找到本会话产生的线索，勾选后将堆叠到同一组。"
                  : "当前还没有可溯源的同会话线索；勾选后将先创建新线索，后续同会话线索可继续堆叠。"}
              </span>
            </span>
          </label>
        </Show>

        <label class="field">
          <span class="field-label">线索标题</span>
          <input
            class="field-input"
            value={title()}
            maxlength={100}
            placeholder="这条线索解决或确认了什么"
            disabled={busy() || summarizing()}
            onInput={(event) => setTitle(event.currentTarget.value)}
          />
        </label>

        <label class="field">
          <div class="clue-content-label-row">
            <span class="field-label">线索内容</span>
            <Show when={props.sessionMode && props.threadId}>
              <button
                type="button"
                class="btn secondary clue-ai-summary-btn"
                disabled={busy() || summarizing()}
                title="使用设置里的高级分享模型总结本会话核心内容"
                onClick={() => void runAiSummary()}
              >
                {summarizing() ? "总结中…" : "AI 总结"}
              </button>
            </Show>
          </div>
          <textarea
            class="field-input clue-content-input"
            rows={10}
            value={content()}
            placeholder="记录结论、证据、产物、验证结果和下一步"
            disabled={busy() || summarizing()}
            onInput={(event) => setContent(event.currentTarget.value)}
          />
        </label>

        <div class="field">
          <span class="field-label">@ 提醒谁注意</span>
          <MentionPicker
            peers={mentionPeers()}
            selectedTokens={mentionTokens()}
            disabled={busy() || summarizing() || mentionPeers().length === 0}
            placeholder={mentionPeers().length > 0 ? "@ 提醒团队成员" : "暂无可提醒的团队成员"}
            onChange={setMentionTokens}
          />
          <span class="field-hint">被 @ 的成员会收到可点击的线索提醒，离线时会在重连后补发。</span>
        </div>

        <Show when={!props.sessionMode && targetCardId()}>
          <div class="field-hint">当前选择：{targetCardTitle()}</div>
        </Show>
      </div>
      <div class="modal-foot">
        <button class="btn secondary" disabled={busy() || summarizing()} onClick={props.onClose}>
          取消
        </button>
        <button
          class="btn primary"
          disabled={
            busy() ||
            summarizing() ||
            !title().trim() ||
            !content().trim() ||
            (!props.sessionMode &&
              (placement() === "update" || placement() === "parallel") &&
              !targetCardId())
          }
          onClick={() => void submit()}
        >
          {busy() ? "保存中…" : "保存线索"}
        </button>
      </div>
    </>
  );

  if (props.embedded) {
    return <div class="clue-capture-embedded">{body}</div>;
  }

  return (
    <div class="modal-backdrop" onClick={(event) => event.target === event.currentTarget && props.onClose()}>
      <div class="modal clue-capture-modal" onClick={(event) => event.stopPropagation()}>
        <div class="modal-head">
          <span class="clue-modal-title">
            <IconClue size={15} />
            生成线索
          </span>
          <button class="icon-btn" title="关闭" disabled={busy() || summarizing()} onClick={props.onClose}>
            <IconX size={16} />
          </button>
        </div>
        {body}
      </div>
    </div>
  );
}
