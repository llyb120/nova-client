import DOMPurify from "dompurify";
import { Marked } from "marked";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import type { PermissionRequest } from "../types";
import { agentLabel, stripAnsi } from "../utils";
import "./TaskbarBubble.css";

type Kind = "idle" | "running" | "waiting" | "done" | "error";
type Permission = PermissionRequest & { threadTitle?: string | null };
type Payload = {
  kind: Kind;
  text: string;
  trailing: string;
  threadId: string | null;
  title: string;
  body: string;
  permissions: Permission[];
};

const LABEL: Record<Kind, string> = { idle: "空闲", running: "进行中", waiting: "待确认", done: "已完成", error: "出错了" };
const marked = new Marked({ gfm: true, breaks: true });

function preview(raw: unknown): string {
  if (raw == null) return "";
  if (typeof raw === "object") {
    const o = raw as Record<string, unknown>;
    const value = o.command ?? o.cmd ?? o.path ?? o.file_path ?? o.url;
    if (typeof value === "string") return stripAnsi(value);
    try {
      const s = JSON.stringify(raw);
      return stripAnsi(s.length > 400 ? s.slice(0, 400) + "…" : s);
    } catch {
      return "";
    }
  }
  return stripAnsi(String(raw));
}

const tone = (kind: string) => (kind.startsWith("allow") ? "allow" : kind.startsWith("reject") ? "reject" : "");

function Ask(props: { req: Permission; onDone: () => void }) {
  const questions = props.req.questions ?? [];
  const [picked, setPicked] = createSignal<string[][]>(questions.map(() => []));
  const [custom, setCustom] = createSignal<string[]>(questions.map(() => ""));
  const [busy, setBusy] = createSignal(false);
  const answers = () => picked().map((sel, i) => (custom()[i]?.trim() ? [...sel, custom()[i].trim()] : sel));
  const respond = async (optionId: string) => {
    if (busy()) return;
    setBusy(true);
    try {
      await invoke("respond_permission", { requestKey: props.req.requestKey, optionId });
    } catch {
      // Already answered elsewhere; the next refresh drops it.
    }
    props.onDone();
  };
  const pick = (index: number, label: string, multiple: boolean) => {
    setPicked((all) => all.map((sel, i) => (i !== index ? sel : !multiple ? [label] : sel.includes(label) ? sel.filter((v) => v !== label) : [...sel, label])));
    if (!multiple) setCustom((all) => all.map((v, i) => (i === index ? "" : v)));
  };
  const text = () => preview(props.req.toolCall?.rawInput);
  return (
    <section class="tb-ask">
      <div class="tb-ask-head">
        <span class="tb-ask-badge">{props.req.questions ? "需要你回答" : "需要你确认"}</span>
        <span class="tb-ask-agent">{agentLabel(props.req.agentKind ?? "devin")}</span>
      </div>
      <Show when={props.req.threadTitle}>
        <div class="tb-ask-thread">{props.req.threadTitle}</div>
      </Show>
      <div class="tb-ask-title">{props.req.toolCall?.title ?? "执行工具"}</div>
      <Show
        when={props.req.questions}
        fallback={
          <>
            <Show when={text()}>
              <pre class="tb-code">{text()}</pre>
            </Show>
            <div class="tb-actions">
              <For each={props.req.options}>
                {(opt) => (
                  <button class={`tb-btn ${tone(opt.kind)}`} disabled={busy()} onClick={() => void respond(opt.optionId)}>
                    {opt.name}
                  </button>
                )}
              </For>
            </div>
          </>
        }
      >
        <For each={questions}>
          {(q, i) => (
            <div class="tb-question">
              <Show when={q.header}>
                <div class="tb-question-head">{q.header}</div>
              </Show>
              <div class="tb-question-text">{q.question}</div>
              <div class="tb-choices">
                <For each={q.options}>
                  {(o) => (
                    <button
                      class="tb-choice"
                      classList={{ selected: picked()[i()]?.includes(o.label) }}
                      onClick={() => pick(i(), o.label, Boolean(q.multiple))}
                    >
                      <span>{o.label}</span>
                      <Show when={o.description}>
                        <small>{o.description}</small>
                      </Show>
                    </button>
                  )}
                </For>
              </div>
              <Show when={q.custom}>
                <input
                  class="tb-input"
                  placeholder="输入其他答案"
                  value={custom()[i()] ?? ""}
                  onInput={(e) => {
                    const value = e.currentTarget.value;
                    setCustom((all) => all.map((v, j) => (j === i() ? value : v)));
                    if (!q.multiple && value.trim()) setPicked((all) => all.map((v, j) => (j === i() ? [] : v)));
                  }}
                />
              </Show>
            </div>
          )}
        </For>
        <div class="tb-actions">
          <button class="tb-btn allow" disabled={busy() || !answers().every((a) => a.length)} onClick={() => void respond(JSON.stringify(answers()))}>
            提交回答
          </button>
          <button class="tb-btn reject" disabled={busy()} onClick={() => void respond("")}>
            拒绝回答
          </button>
        </div>
      </Show>
    </section>
  );
}

export default function TaskbarBubble() {
  const [data, setData] = createSignal<Payload | null>(null);
  const [permissions, setPermissions] = createSignal<Permission[]>([]);
  let shell!: HTMLDivElement;
  let card: HTMLDivElement | undefined;
  let timer: number | undefined;

  const html = createMemo(() => {
    const body = data()?.body ?? "";
    return body ? DOMPurify.sanitize(marked.parse(body, { async: false }) as string) : "";
  });
  const place = () => invoke<boolean>("taskbar_bubble_place", { height: shell.offsetHeight }).catch(() => false);
  const hide = () => void invoke("taskbar_bubble_hide").catch(() => {});

  const load = async () => {
    const next = await invoke<Payload | null>("taskbar_bubble").catch(() => null);
    // Nothing left to show (e.g. the result was viewed elsewhere): close instead of an empty card.
    if (!next || (next.kind === "idle" && !next.permissions.length)) return hide();
    setData(next);
    // Keep in-progress answers: only rebuild cards when the set of requests changes.
    const keys = (list: Permission[]) => list.map((p) => p.requestKey).join("\n");
    if (keys(next.permissions) !== keys(permissions())) setPermissions(next.permissions);
  };
  const open = async () => {
    clearInterval(timer);
    await load();
    card?.classList.remove("enter");
    void card?.offsetWidth; // restart the entrance animation
    card?.classList.add("enter");
    await new Promise(requestAnimationFrame);
    // Live refresh only while shown (a prewarmed page is not).
    if (await place()) timer = window.setInterval(() => void load(), 1000);
  };
  const answered = async () => {
    await load();
    if (!permissions().length) hide();
  };

  onMount(() => {
    const observer = new ResizeObserver(() => void place());
    observer.observe(shell);
    const keys = (e: KeyboardEvent) => e.key === "Escape" && hide();
    window.addEventListener("keydown", keys);
    const offs = [listen("taskbar-bubble:open", () => void open()), listen("taskbar-bubble:hidden", () => clearInterval(timer))];
    // Listen before the first place(): a click landing in between is then either seen by
    // place() or delivered as an open event, never lost.
    void Promise.all(offs).then(() => open());
    onCleanup(() => {
      observer.disconnect();
      window.removeEventListener("keydown", keys);
      clearInterval(timer);
      offs.forEach((p) => void p.then((off) => off()));
    });
  });

  return (
    <div class="tb-shell" ref={shell}>
      <Show when={data()}>
        {(d) => (
          <div class={`tb-card enter ${d().kind}`} ref={card}>
            <header class="tb-head">
              <span class="tb-dot" />
              <span class="tb-state">{LABEL[d().kind]}</span>
              <Show when={d().trailing}>
                <span class="tb-time">{d().trailing}</span>
              </Show>
              <button class="tb-close" title="关闭 (Esc)" aria-label="关闭" onClick={hide}>
                <svg viewBox="0 0 12 12" width="10" height="10"><path d="M2 2l8 8M10 2l-8 8" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" /></svg>
              </button>
            </header>
            <Show when={d().title}>
              <div class="tb-title">{d().title}</div>
            </Show>
            <Show when={d().kind === "running" || !d().body}>
              <div class="tb-activity">{d().text}</div>
            </Show>
            <For each={permissions()}>{(req) => <Ask req={req} onDone={() => void answered()} />}</For>
            <Show when={html()}>
              <div
                class="tb-body"
                innerHTML={html()}
                onClick={(e) => (e.target as HTMLElement).closest("a") && e.preventDefault()}
              />
            </Show>
            <footer class="tb-foot">
              <span class="tb-hint">双击灵动岛直接打开</span>
              <button class="tb-open" onClick={() => void invoke("taskbar_bubble_open", { threadId: d().threadId })}>
                {d().threadId ? "打开会话" : "打开 Nova"}
                <svg viewBox="0 0 12 12" width="10" height="10"><path d="M4.5 2.5L8 6l-3.5 3.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
              </button>
            </footer>
          </div>
        )}
      </Show>
    </div>
  );
}
