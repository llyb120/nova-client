import { createSignal, For, Show } from "solid-js";
import { respondPermission } from "../store";
import type { PermissionRequest } from "../types";
import { agentLabel, stripAnsi } from "../utils";
import { toolIcon } from "./icons";

function inputPreview(raw: unknown): string {
  if (raw == null) return "";
  if (typeof raw === "object") {
    const o = raw as Record<string, unknown>;
    const cmd = o.command ?? o.cmd ?? o.path ?? o.file_path ?? o.url;
    if (typeof cmd === "string") return stripAnsi(cmd);
    try {
      const s = JSON.stringify(raw);
      return stripAnsi(s.length > 200 ? s.slice(0, 200) + "…" : s);
    } catch {
      return "";
    }
  }
  return stripAnsi(String(raw));
}

function buttonClass(kind: string): string {
  if (kind.startsWith("allow")) return "perm-btn allow";
  if (kind.startsWith("reject")) return "perm-btn reject";
  return "perm-btn";
}

export function PermissionCard(props: { req: PermissionRequest }) {
  const preview = () => inputPreview(props.req.toolCall?.rawInput);
  const agent = () => agentLabel(props.req.agentKind ?? "devin");
  const [answers, setAnswers] = createSignal<string[][]>(
    props.req.questions?.map(() => []) ?? [],
  );
  const [customAnswers, setCustomAnswers] = createSignal<string[]>(
    props.req.questions?.map(() => "") ?? [],
  );
  const questionAnswers = () =>
    answers().map((selected, index) => {
      const custom = customAnswers()[index]?.trim();
      return custom ? [...selected, custom] : selected;
    });
  const canAnswer = () => questionAnswers().every((answer) => answer.length > 0);
  const selectAnswer = (index: number, label: string, multiple: boolean) => {
    setAnswers((current) =>
      current.map((answer, answerIndex) => {
        if (answerIndex !== index) return answer;
        if (!multiple) return [label];
        return answer.includes(label)
          ? answer.filter((value) => value !== label)
          : [...answer, label];
      }),
    );
    if (!multiple) {
      setCustomAnswers((current) =>
        current.map((value, answerIndex) => (answerIndex === index ? "" : value)),
      );
    }
  };
  const setCustomAnswer = (index: number, value: string, multiple: boolean) => {
    setCustomAnswers((current) =>
      current.map((answer, answerIndex) => (answerIndex === index ? value : answer)),
    );
    if (!multiple && value.trim()) {
      setAnswers((current) =>
        current.map((answer, answerIndex) => (answerIndex === index ? [] : answer)),
      );
    }
  };
  return (
    <div class="perm-card">
      <div class="perm-head">
        <span class="perm-icon">{toolIcon(props.req.toolCall?.kind ?? "other")}</span>
        <span class="perm-title">
          {agent()} {props.req.questions ? "需要确认" : "请求权限"}：
          {props.req.toolCall?.title ?? "执行工具"}
        </span>
      </div>
      <Show
        when={props.req.questions}
        fallback={
          <>
            <Show when={preview()}>
              <pre class="perm-preview">{preview()}</pre>
            </Show>
            <div class="perm-actions">
              <For each={props.req.options}>
                {(opt) => (
                  <button
                    class={buttonClass(opt.kind)}
                    onClick={() => void respondPermission(props.req.requestKey, opt.optionId)}
                  >
                    {opt.name}
                  </button>
                )}
              </For>
            </div>
          </>
        }
      >
        {(questions) => (
          <>
            <For each={questions()}>
              {(question, index) => (
                <div class="question-block">
                  <div class="question-header">{question.header}</div>
                  <div class="question-text">{question.question}</div>
                  <div class="question-options">
                    <For each={question.options}>
                      {(option) => (
                        <button
                          class={`question-option${answers()[index()]?.includes(option.label) ? " selected" : ""}`}
                          onClick={() => selectAnswer(index(), option.label, Boolean(question.multiple))}
                        >
                          <span>{option.label}</span>
                          <Show when={option.description}>
                            <small>{option.description}</small>
                          </Show>
                        </button>
                      )}
                    </For>
                  </div>
                  <Show when={question.custom}>
                    <input
                      class="question-custom"
                      value={customAnswers()[index()] ?? ""}
                      onInput={(event) =>
                        setCustomAnswer(index(), event.currentTarget.value, Boolean(question.multiple))
                      }
                      placeholder="输入其他答案"
                    />
                  </Show>
                </div>
              )}
            </For>
            <div class="perm-actions">
              <button
                class="perm-btn allow"
                disabled={!canAnswer()}
                onClick={() =>
                  void respondPermission(props.req.requestKey, JSON.stringify(questionAnswers()))
                }
              >
                提交回答
              </button>
              <button
                class="perm-btn reject"
                onClick={() => void respondPermission(props.req.requestKey, "")}
              >
                拒绝回答
              </button>
            </div>
          </>
        )}
      </Show>
    </div>
  );
}
