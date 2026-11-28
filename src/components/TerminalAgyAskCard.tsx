import { createEffect, For, Show, createSignal, untrack } from "solid-js";
import type { AgyAskSubmissionItem } from "../agyTerminalPrompt";
import type { AgyAskBlock } from "../types";
import { Markdown } from "./Markdown";
import type { AgyAgentPhase } from "./TerminalAgyConversation";
import { IconCheck } from "./icons";

function splitRecommendedLabel(text: string): { label: string; recommended: boolean } {
  const match = text.match(/^\(Recommended\)\s*(.*)$/i);
  if (!match) return { label: text, recommended: false };
  const stripped = match[1].trim();
  return { label: stripped || text, recommended: true };
}

function formatAskTime(ts?: number | null): string {
  if (!ts) return "";
  const date = new Date(ts);
  if (Number.isNaN(date.getTime())) return "";
  const hh = String(date.getHours()).padStart(2, "0");
  const mm = String(date.getMinutes()).padStart(2, "0");
  return `${hh}:${mm}`;
}

export function TerminalAgyAskCard(props: {
  ask: AgyAskBlock;
  cliLabel?: string;
  interactive: boolean;
  submitting: boolean;
  touchedInTerminal: boolean;
  terminalReady: boolean;
  agyPhase: AgyAgentPhase | null;
  onSubmit?: (ask: AgyAskBlock, submissions: AgyAskSubmissionItem[]) => Promise<void> | void;
  onSkip?: (ask: AgyAskBlock) => Promise<void> | void;
  onFocusTerminal?: () => void;
}) {
  const cliLabel = () => props.cliLabel ?? "反重力 CLI";
  const [selected, setSelected] = createSignal<number[][]>(
    untrack(() => props.ask.questions.map(() => [])),
  );
  const [customs, setCustoms] = createSignal<string[]>(
    untrack(() => props.ask.questions.map(() => "")),
  );

  createEffect(
    () => `${props.ask.id}:${props.ask.questions.length}`,
    () => {
      const len = untrack(() => props.ask.questions.length);
      setSelected(Array.from({ length: len }, () => []));
      setCustoms(Array.from({ length: len }, () => ""));
    },
  );

  const controlsDisabled = () =>
    !props.interactive || props.submitting || props.touchedInTerminal;

  const canSubmit = () =>
    props.ask.questions.some(
      (_, idx) =>
        (selected()[idx]?.length ?? 0) > 0 || (customs()[idx]?.trim().length ?? 0) > 0,
    );

  const selectOption = (qIdx: number, optIdx: number, multiple: boolean) => {
    if (controlsDisabled()) return;
    setSelected((prev) =>
      prev.map((arr, idx) => {
        if (idx !== qIdx) return arr;
        if (!multiple) return [optIdx];
        return arr.includes(optIdx) ? arr.filter((v) => v !== optIdx) : [...arr, optIdx];
      }),
    );
    if (!multiple) {
      setCustoms((prev) => prev.map((val, idx) => (idx === qIdx ? "" : val)));
    }
  };

  const updateCustom = (qIdx: number, value: string, multiple: boolean) => {
    setCustoms((prev) => prev.map((val, idx) => (idx === qIdx ? value : val)));
    if (!multiple && value.trim()) {
      setSelected((prev) => prev.map((arr, idx) => (idx === qIdx ? [] : arr)));
    }
  };

  const submitAnswers = () => {
    if (!canSubmit() || controlsDisabled()) return;
    const submissions: AgyAskSubmissionItem[] = props.ask.questions.map((_, idx) => {
      const selectedIndices = selected()[idx] ?? [];
      const customText = customs()[idx] ?? "";
      return {
        selectedIndices,
        customText,
        skipped: selectedIndices.length === 0 && !customText.trim(),
      };
    });
    void props.onSubmit?.(props.ask, submissions);
  };

  const pendingBadge = () => {
    if (props.submitting) return "已提交，同步中…";
    if (props.touchedInTerminal) return "已在右侧操作";
    if (props.interactive) return "等待回答";
    if (!props.terminalReady) return "等待 CLI 就绪…";
    return "待右侧终端响应";
  };

  const totalQuestions = () => props.ask.questions.length;

  return (
    <Show
      when={props.ask.status === "pending"}
      fallback={
        <div
          class={[
            "terminal-agy-ask-card",
            props.ask.status === "cancelled" ? "is-cancelled" : "is-resolved",
          ]}
        >
          <div class="terminal-agy-ask-head">
            <span class="terminal-agy-ask-title-wrap">
              <Show when={props.ask.status === "answered"}>
                <span class="terminal-agy-ask-done-icon" aria-hidden="true">
                  <IconCheck size={13} />
                </span>
              </Show>
              <span class="terminal-agy-ask-title">
                {props.ask.status === "answered" ? "已回答提问" : "已取消提问"}
              </span>
              <span class="terminal-agy-ask-count">共 {totalQuestions()} 题</span>
            </span>
            <Show when={formatAskTime(props.ask.timestamp)}>
              {(t) => <span class="terminal-agy-time">{t()}</span>}
            </Show>
          </div>
          <Show when={props.ask.preamble}>
            {(pre) => (
              <div class="terminal-agy-ask-preamble">
                <Markdown text={pre()} markFiles />
              </div>
            )}
          </Show>
          <div class="terminal-agy-ask-summary">
            <For each={props.ask.questions}>
              {(q, idx) => (
                <div class="terminal-agy-ask-summary-item">
                  <div class="terminal-agy-ask-summary-q">
                    <Show when={totalQuestions() > 1}>
                      <span class="terminal-agy-ask-summary-idx">{idx() + 1}.</span>
                    </Show>
                    <span>{q.question}</span>
                  </div>
                  <div class="terminal-agy-ask-chips">
                    <Show
                      when={props.ask.status === "cancelled"}
                      fallback={
                        <Show
                          when={!q.skipped}
                          fallback={<span class="terminal-agy-ask-chip is-muted">已跳过</span>}
                        >
                          <Show
                            when={(q.answers?.length ?? 0) > 0}
                            fallback={
                              <Show when={totalQuestions() === 1 && props.ask.rawAnswer}>
                                {(raw) => <span class="terminal-agy-ask-chip">{raw()}</span>}
                              </Show>
                            }
                          >
                            <For each={q.answers}>
                              {(ans) => <span class="terminal-agy-ask-chip">{ans}</span>}
                            </For>
                          </Show>
                        </Show>
                      }
                    >
                      <span class="terminal-agy-ask-chip is-muted">已取消</span>
                    </Show>
                  </div>
                </div>
              )}
            </For>
          </div>
        </div>
      }
    >
      <div class="terminal-agy-ask-card is-pending" role="region" aria-label={`${cliLabel()} 提问`}>
        <div class="terminal-agy-ask-head">
          <span class="terminal-agy-ask-title-wrap">
            <span class="terminal-agy-ask-dot" aria-hidden="true" />
            <span class="terminal-agy-ask-title">
              {totalQuestions() > 1
                ? `${cliLabel()} 提问 · 共 ${totalQuestions()} 题`
                : `${cliLabel()} 提问`}
            </span>
          </span>
          <span
            class={[
              "terminal-agy-ask-badge",
              {
                "is-active": props.interactive && !props.touchedInTerminal && !props.submitting,
                "is-warn": props.touchedInTerminal,
              },
            ]}
          >
            {pendingBadge()}
          </span>
        </div>

        <Show when={props.ask.preamble}>
          {(pre) => (
            <div class="terminal-agy-ask-preamble">
              <Markdown text={pre()} markFiles />
            </div>
          )}
        </Show>

        <div class="terminal-agy-ask-questions">
          <For each={props.ask.questions}>
            {(q, qIdx) => {
              const selectedList = () => selected()[qIdx()] ?? [];
              const customVal = () => customs()[qIdx()] ?? "";
              return (
                <div class="terminal-agy-ask-qblock">
                  <div class="terminal-agy-ask-qhead">
                    <Show when={totalQuestions() > 1}>
                      <span class="terminal-agy-ask-qnum">
                        问题 {qIdx() + 1} / {totalQuestions()}
                      </span>
                    </Show>
                    <span class="terminal-agy-ask-qmode">{q.isMultiSelect ? "多选" : "单选"}</span>
                  </div>
                  <div class="terminal-agy-ask-qtext">{q.question}</div>
                  <Show when={q.options.length > 0}>
                    <div class="terminal-agy-ask-options">
                      <For each={q.options}>
                        {(opt, oIdx) => {
                          const parsed = () => splitRecommendedLabel(opt);
                          const isSelected = () => selectedList().includes(oIdx());
                          return (
                            <button
                              type="button"
                              class={[
                                "terminal-agy-ask-option",
                                { selected: isSelected(), multi: q.isMultiSelect },
                              ]}
                              disabled={controlsDisabled()}
                              aria-pressed={isSelected() ? "true" : "false"}
                              onClick={() => selectOption(qIdx(), oIdx(), q.isMultiSelect)}
                            >
                              <span
                                class={[
                                  "terminal-agy-ask-indicator",
                                  q.isMultiSelect ? "is-check" : "is-radio",
                                  { checked: isSelected() },
                                ]}
                                aria-hidden="true"
                              >
                                <Show when={q.isMultiSelect && isSelected()}>
                                  <IconCheck size={11} />
                                </Show>
                              </span>
                              <span class="terminal-agy-ask-opt-num">{oIdx() + 1}.</span>
                              <span class="terminal-agy-ask-opt-label">{parsed().label}</span>
                              <Show when={parsed().recommended}>
                                <span class="terminal-agy-ask-rec">推荐</span>
                              </Show>
                            </button>
                          );
                        }}
                      </For>
                    </div>
                  </Show>
                  <input
                    type="text"
                    class="terminal-agy-ask-custom"
                    value={customVal()}
                    disabled={controlsDisabled()}
                    placeholder="其他答案（可选，自定义输入）"
                    onInput={(e) =>
                      updateCustom(qIdx(), e.currentTarget.value, q.isMultiSelect)
                    }
                    onKeyDown={(e) => {
                      if (
                        totalQuestions() === 1 &&
                        e.key === "Enter" &&
                        !e.shiftKey &&
                        !e.isComposing &&
                        e.keyCode !== 229
                      ) {
                        e.preventDefault();
                        submitAnswers();
                      }
                    }}
                  />
                </div>
              );
            }}
          </For>
        </div>

        <Show when={props.touchedInTerminal}>
          <div class="terminal-agy-ask-touched" role="status">
            <span>检测到已在右侧终端键盘操作，为避免选项错位，请继续在右侧终端完成回答。</span>
            <button
              type="button"
              class="terminal-agy-ask-focus-btn"
              disabled={!props.terminalReady}
              onClick={() => props.onFocusTerminal?.()}
            >
              聚焦右侧终端
            </button>
          </div>
        </Show>

        <div class="terminal-agy-ask-actions">
          <button
            type="button"
            class="perm-btn allow"
            disabled={!canSubmit() || controlsDisabled()}
            onClick={submitAnswers}
          >
            {props.submitting ? "正在提交…" : "提交回答"}
          </button>
          <button
            type="button"
            class="perm-btn reject"
            disabled={controlsDisabled()}
            onClick={() => void props.onSkip?.(props.ask)}
          >
            {totalQuestions() > 1 ? "跳过全部" : "跳过"}
          </button>
          <button
            type="button"
            class="terminal-agy-ask-focus-btn"
            disabled={!props.terminalReady}
            title="将键盘焦点切换到右侧 CLI 终端"
            onClick={() => props.onFocusTerminal?.()}
          >
            聚焦终端
          </button>
        </div>
      </div>
    </Show>
  );
}
