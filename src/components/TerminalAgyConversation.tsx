import { convertFileSrc } from "@tauri-apps/api/core";
import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createSignal, For, onCleanup, Show, untrack } from "solid-js";
import { api } from "../ipc";
import type { AgyTurn } from "../types";
import { Markdown } from "./Markdown";
import { IconCheck, IconChevron, IconCopy, IconStopwatch } from "./icons";

export type AgyAgentPhase = "idle" | "working" | "tool_use" | "reviewing" | "error";

export function isRunningAgyPhase(phase: AgyAgentPhase | null): boolean {
  return phase === "working" || phase === "tool_use" || phase === "reviewing";
}

function formatTurnTime(ts?: number | null): string {
  if (!ts) return "";
  const date = new Date(ts);
  if (Number.isNaN(date.getTime())) return "";
  const now = new Date();
  const hh = String(date.getHours()).padStart(2, "0");
  const mm = String(date.getMinutes()).padStart(2, "0");
  if (
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate()
  ) {
    return `${hh}:${mm}`;
  }
  const mon = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${mon}-${day} ${hh}:${mm}`;
}

function isLongPrompt(text: string): boolean {
  if (text.length > 360) return true;
  let lines = 1;
  for (let i = 0; i < text.length; i++) {
    if (text.charCodeAt(i) === 10 && ++lines > 6) return true;
  }
  return false;
}

function phaseBadge(phase: AgyAgentPhase | null): { label: string; tone: "running" | "waiting" | "error" } | null {
  switch (phase) {
    case "working":
      return { label: "执行中", tone: "running" };
    case "tool_use":
      return { label: "调用工具中", tone: "running" };
    case "reviewing":
      return { label: "等待确认", tone: "waiting" };
    case "error":
      return { label: "执行出错", tone: "error" };
    default:
      return null;
  }
}

export function TerminalAgyConversation(props: {
  threadId: string;
  turns: AgyTurn[];
  agyPhase: AgyAgentPhase | null;
  hasConversationId: boolean;
  hasPendingOptimistic?: boolean;
  onStop?: () => void;
  onCollapse: () => void;
}) {
  const [stickToBottom, setStickToBottom] = createSignal(true);
  const [expandedPrompts, setExpandedPrompts] = createSignal<Record<number, boolean>>({});
  const [copiedKey, setCopiedKey] = createSignal<string | null>(null);
  let feedRef: HTMLDivElement | undefined;
  let copyTimer: number | undefined;

  onCleanup(() => {
    if (copyTimer !== undefined) window.clearTimeout(copyTimer);
  });

  const scrollToBottom = (smooth = false) => {
    if (!feedRef) return;
    if (smooth) {
      feedRef.scrollTo({ top: feedRef.scrollHeight, behavior: "smooth" });
    } else {
      feedRef.scrollTop = feedRef.scrollHeight;
    }
    setStickToBottom(true);
  };

  createEffect(
    () => props.threadId,
    () => {
      setExpandedPrompts({});
      setStickToBottom(true);
      queueMicrotask(() => scrollToBottom(false));
    },
  );

  createEffect(
    () => {
      const len = props.turns.length;
      const last = props.turns[len - 1];
      return `${len}:${last?.response?.length ?? 0}:${props.agyPhase ?? ""}`;
    },
    () => {
      if (!untrack(stickToBottom)) return;
      requestAnimationFrame(() => {
        if (untrack(stickToBottom)) scrollToBottom(false);
      });
    },
  );

  const onFeedScroll = () => {
    if (!feedRef) return;
    const distanceToBottom = feedRef.scrollHeight - feedRef.scrollTop - feedRef.clientHeight;
    setStickToBottom(distanceToBottom < 48);
  };

  const togglePromptExpanded = (index: number) => {
    setExpandedPrompts((prev) => ({ ...prev, [index]: !prev[index] }));
  };

  const copyText = async (key: string, text: string) => {
    if (!navigator.clipboard || !text) return;
    try {
      await navigator.clipboard.writeText(text);
      setCopiedKey(key);
      if (copyTimer !== undefined) window.clearTimeout(copyTimer);
      copyTimer = window.setTimeout(() => {
        copyTimer = undefined;
        setCopiedKey(null);
      }, 1500);
    } catch (error) {
      void message(String(error), { kind: "error" });
    }
  };

  const badge = () =>
    phaseBadge(props.agyPhase) ??
    (props.hasPendingOptimistic ? { label: "执行中", tone: "running" as const } : null);

  return (
    <div class="terminal-agy-conv" aria-label="Nova 对话式汇总">
      <div class="repo-time-machine-label terminal-agy-head">
        <span class="terminal-worldline-title">世界线</span>
        <span class="terminal-worldline-meta">{props.turns.length} 轮</span>
        <Show when={badge()}>
          {(b) => (
            <span class={["terminal-agy-phase-badge", `tone-${b().tone}`]}>
              <Show when={b().tone === "running"}>
                <span class="terminal-agy-pulse" aria-hidden="true" />
              </Show>
              <span>{b().label}</span>
            </span>
          )}
        </Show>
        <button
          type="button"
          class="repo-time-magic-clock"
          title="收起世界线分屏"
          aria-label="收起世界线分屏"
          onClick={() => props.onCollapse()}
        >
          <span class="repo-time-clock-face" aria-hidden="true">
            <IconStopwatch size={17} />
            <span class="repo-time-clock-hand" />
          </span>
        </button>
      </div>

      <div class="terminal-agy-feed-wrap">
        <div ref={feedRef} class="terminal-agy-feed" onScroll={onFeedScroll}>
          <Show
            when={props.turns.length > 0}
            fallback={
              <div class="terminal-agy-empty">
                <p>
                  {props.hasConversationId
                    ? "暂无对话轮次记录"
                    : "发送首条消息后，这里将只汇总你的输入与 AI 的最终结论"}
                </p>
                <span>左侧保留 Antigravity CLI 的实时运行过程与工具调用</span>
              </div>
            }
          >
            <For each={props.turns}>
              {(turn, idx) => {
                const isLast = () => idx() === props.turns.length - 1;
                const long = () => isLongPrompt(turn.prompt);
                const expanded = () => !!expandedPrompts()[turn.index];
                const promptCopyKey = `u:${turn.index}`;
                const respCopyKey = `a:${turn.index}`;
                const promptCopied = () => copiedKey() === promptCopyKey;
                const respCopied = () => copiedKey() === respCopyKey;
                return (
                  <section class="html-turn terminal-agy-turn" aria-label={`第 ${turn.index} 轮对话`}>
                    <div class="terminal-agy-user">
                      <div class="terminal-agy-user-col">
                        <div class="terminal-agy-user-head">
                          <span class="terminal-agy-turn-index">#{turn.index}</span>
                          <Show when={formatTurnTime(turn.promptTimestamp)}>
                            {(t) => <span class="terminal-agy-time">{t()}</span>}
                          </Show>
                        </div>
                        <div class="ht-bubble-wrap terminal-agy-bubble-wrap">
                          <div class="ht-user-bubble terminal-agy-bubble">
                            <Show when={(turn.images?.length ?? 0) > 0}>
                              <div class="ht-bubble-images terminal-agy-images">
                                <For each={turn.images}>
                                  {(imgPath) => (
                                    <img
                                      src={convertFileSrc(imgPath)}
                                      alt="上传图片"
                                      title={`点击打开图片：${imgPath}`}
                                      class="ht-clickable"
                                      onClick={() => void api.openFileDefault(props.threadId, imgPath)}
                                    />
                                  )}
                                </For>
                              </div>
                            </Show>
                            <Show when={turn.prompt.trim().length > 0}>
                              <div
                                class={[
                                  "ht-user-text",
                                  "terminal-agy-user-text",
                                  { folded: long() && !expanded() },
                                ]}
                              >
                                {turn.prompt}
                              </div>
                            </Show>
                            <Show when={long()}>
                              <button
                                type="button"
                                class="terminal-agy-fold-btn"
                                onClick={() => togglePromptExpanded(turn.index)}
                              >
                                <IconChevron size={12} open={expanded()} />
                                <span>{expanded() ? "收起输入" : "展开全文"}</span>
                              </button>
                            </Show>
                          </div>
                          <Show when={turn.prompt.trim().length > 0}>
                            <button
                              type="button"
                              class={["terminal-agy-copy-btn", "user", { copied: promptCopied() }]}
                              title="复制用户输入"
                              aria-label={`复制第 ${turn.index} 轮输入`}
                              onClick={() => void copyText(promptCopyKey, turn.prompt)}
                            >
                              <Show when={promptCopied()} fallback={<IconCopy size={13} />}>
                                <IconCheck size={13} />
                              </Show>
                            </button>
                          </Show>
                        </div>
                      </div>
                    </div>

                    <div class="terminal-agy-answer">
                      <Show
                        when={turn.response}
                        fallback={
                          <Show
                            when={isLast() && (isRunningAgyPhase(props.agyPhase) || !!props.hasPendingOptimistic)}
                            fallback={
                              <div class="terminal-agy-status-note">
                                {!isLast()
                                  ? "已随后续消息一并回复"
                                  : props.agyPhase === "error"
                                    ? "本轮执行出错，请查看左侧终端输出"
                                    : "本轮未产生最终文本结论"}
                              </div>
                            }
                          >
                            <div class="terminal-agy-pending" role="status">
                              <span class="terminal-agy-pulse" aria-hidden="true" />
                              <span>反重力 CLI 正在左侧执行中，完成后自动汇总最终结论…</span>
                              <Show when={props.onStop}>
                                <button
                                  type="button"
                                  class="terminal-agy-pending-stop"
                                  title="停止当前任务（向 CLI 发送 Esc）"
                                  onClick={() => props.onStop?.()}
                                >
                                  停止
                                </button>
                              </Show>
                            </div>
                          </Show>
                        }
                      >
                        {(responseText) => (
                          <div class="terminal-agy-answer-body">
                            <Markdown text={responseText()} markFiles />
                            <div class="terminal-agy-answer-foot">
                              <Show when={formatTurnTime(turn.responseTimestamp)}>
                                {(t) => <span class="terminal-agy-time">{t()}</span>}
                              </Show>
                              <Show when={turn.model}>
                                {(m) => <span class="terminal-agy-model">{m()}</span>}
                              </Show>
                              <button
                                type="button"
                                class={["terminal-agy-copy-btn", "answer", { copied: respCopied() }]}
                                title="复制最终结论"
                                aria-label={`复制第 ${turn.index} 轮结论`}
                                onClick={() => void copyText(respCopyKey, responseText())}
                              >
                                <Show when={respCopied()} fallback={<IconCopy size={13} />}>
                                  <IconCheck size={13} />
                                </Show>
                                <span>{respCopied() ? "已复制" : "复制结论"}</span>
                              </button>
                            </div>
                          </div>
                        )}
                      </Show>
                    </div>
                  </section>
                );
              }}
            </For>
          </Show>
        </div>

        <Show when={!stickToBottom() && props.turns.length > 0}>
          <button
            type="button"
            class="terminal-agy-scroll-bottom"
            title="回到底部并恢复自动跟随"
            onClick={() => scrollToBottom(true)}
          >
            回到底部
          </button>
        </Show>
      </div>
    </div>
  );
}
