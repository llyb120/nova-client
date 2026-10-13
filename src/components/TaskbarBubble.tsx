import DOMPurify from "dompurify";
import { Marked } from "marked";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import type { PermissionRequest } from "../types";
import { agentLabel, stripAnsi } from "../utils";
import "./TaskbarBubble.css";

type Kind = "idle" | "running" | "waiting" | "done" | "error";
type Permission = PermissionRequest & { threadTitle?: string | null };
/** One bubble: a result popped when its turn finished, or the live card a click opened. */
type Card = {
  key: string;
  live: boolean;
  kind: Kind;
  text: string;
  trailing: string;
  threadId: string | null;
  title: string;
  body: string;
};
/** `light`: the Windows taskbar theme, so cards match the Butterfly they pop out of. */
type Payload = { cards: Card[]; permissions: Permission[]; light: boolean };

const LABEL: Record<Kind, string> = { idle: "空闲", running: "进行中", waiting: "待确认", done: "已完成", error: "出错了" };
const marked = new Marked({ gfm: true, breaks: true });
const markdown = (body: string) => (body ? DOMPurify.sanitize(marked.parse(body, { async: false }) as string) : "");

/** Same mark as the taskbar chip; the wings beat while the card is live. */
const WING = "M0.5-0.5L1.8-4.4L4.4-6L7-5.2L7-2.6L0.5 0.6ZM0.5 0.6L4.6 1L5 3.4L2.8 5.2L0.5 2.8Z";
const Butterfly = () => (
  <svg class="tb-fly" viewBox="-8 -7 16 13" width="16" height="13" aria-hidden="true">
    <g class="tb-wings">
      <path d={WING} />
      <path d={WING} transform="scale(-1 1)" />
    </g>
    <rect class="tb-fly-body" x="-0.9" y="-4.6" width="1.8" height="9.2" rx="0.9" />
  </svg>
);

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

/** Quick reply to the card's thread; Enter sends, Shift+Enter breaks the line. */
function Reply(props: { card: Card; onSent: () => void }) {
  const [text, setText] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const send = async () => {
    const value = text().trim();
    if (!value || busy()) return;
    setBusy(true);
    setError("");
    try {
      await invoke("taskbar_bubble_reply", { threadId: props.card.threadId, text: value });
      setText("");
      props.onSent();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form class="tb-reply" onSubmit={(e) => { e.preventDefault(); void send(); }}>
      <textarea
        class="tb-input tb-reply-input"
        rows={1}
        value={text()}
        disabled={busy()}
        aria-label={props.card.kind === "idle" ? "发起新会话" : "快速回复"}
        placeholder={props.card.kind === "idle" ? "输入消息，在临时目录发起新会话" : props.card.kind === "running" ? "追加消息，本轮结束后发送" : "快速回复，Enter 发送，Shift+Enter 换行"}
        onInput={(e) => setText(e.currentTarget.value)}
        onKeyDown={(e) => {
          // isComposing: Enter that confirms an IME candidate is not a send.
          if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
            e.preventDefault();
            void send();
          }
        }}
      />
      <button class="tb-send" type="submit" title="发送" aria-label="发送" disabled={busy() || !text().trim()}>
        <svg viewBox="0 0 12 12" width="12" height="12"><path d="M6 10V2.5M2.5 6L6 2.5 9.5 6" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
      </button>
      <Show when={error()}>
        <div class="tb-reply-error" role="alert">{error()}</div>
      </Show>
    </form>
  );
}

export default function TaskbarBubble() {
  const [cards, setCards] = createStore<Card[]>([]);
  const [permissions, setPermissions] = createSignal<Permission[]>([]);
  let shell!: HTMLDivElement;
  let timer: number | undefined;

  const place = () => invoke<boolean>("taskbar_bubble_place", { height: shell.offsetHeight }).catch(() => false);
  const hide = () => void invoke("taskbar_bubble_hide").catch(() => {});
  const dismiss = (card: Card) => void invoke("taskbar_bubble_dismiss", { threadId: card.live ? null : card.threadId }).catch(() => {});

  const load = async () => {
    const next = await invoke<Payload | null>("taskbar_bubble").catch(() => null);
    // Nothing left to show (e.g. the result was viewed elsewhere): close instead of an empty bubble.
    if (!next?.cards.length) return hide();
    document.documentElement.classList.toggle("light", next.light);
    // Keyed: a refresh updates the current card in place.
    setCards(reconcile(next.cards, { key: "key" }));
    // Keep in-progress answers: only rebuild cards when the set of requests changes.
    const keys = (list: Permission[]) => list.map((p) => p.requestKey).join("\n");
    if (keys(next.permissions) !== keys(permissions())) setPermissions(next.permissions);
  };
  const open = async () => {
    clearInterval(timer);
    await load();
    await new Promise(requestAnimationFrame);
    // Live refresh only while shown (a prewarmed page is not).
    if (await place()) timer = window.setInterval(() => void load(), 1000);
  };
  const answered = async () => {
    await load();
    // Last confirmation answered: close the live card.
    const live = cards.find((c) => c.live);
    if (!permissions().length && live) dismiss(live);
  };

  onMount(() => {
    const observer = new ResizeObserver(() => void place());
    observer.observe(shell);
    const keys = (e: KeyboardEvent) => e.key === "Escape" && hide();
    window.addEventListener("keydown", keys);
    const offs = [
      listen("taskbar-bubble:open", () => void open()),
      // Drop the cards so the next bubble mounts fresh and plays its entrance again.
      listen("taskbar-bubble:hidden", () => {
        clearInterval(timer);
        setCards([]);
      }),
    ];
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
    <div class="tb-shell" classList={{ stack: cards.length > 1 }} ref={shell}>
      <For each={cards}>
        {(card) => {
          const html = createMemo(() => markdown(card.body));
          return (
            <div class={`tb-card ${card.kind}`}>
              <header class="tb-head">
                <Butterfly />
                <span class="tb-state">{LABEL[card.kind]}</span>
                <Show when={card.trailing}>
                  <span class="tb-time">{card.trailing}</span>
                </Show>
                <button class="tb-close" title="关闭" aria-label="关闭" onClick={() => dismiss(card)}>
                  <svg viewBox="0 0 12 12" width="10" height="10"><path d="M2 2l8 8M10 2l-8 8" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" /></svg>
                </button>
              </header>
              <Show when={card.title}>
                <div class="tb-title">{card.title}</div>
              </Show>
              <Show when={card.kind === "running" || !card.body}>
                <div class="tb-activity">{card.text}</div>
              </Show>
              <Show when={card.live}>
                <For each={permissions()}>{(req) => <Ask req={req} onDone={() => void answered()} />}</For>
              </Show>
              <Show when={html()}>
                <div
                  class="tb-body"
                  innerHTML={html()}
                  onClick={(e) => (e.target as HTMLElement).closest("a") && e.preventDefault()}
                />
              </Show>
              {/* Waiting cards answer through the confirmation above, not a new prompt. */}
              <Show when={(card.threadId || card.kind === "idle") && card.kind !== "waiting"}>
                <Reply card={card} onSent={hide} />
              </Show>
              <footer class="tb-foot">
                <span class="tb-hint">双击 Butterfly 直接打开</span>
                <button class="tb-open" onClick={() => void invoke("taskbar_bubble_open", { threadId: card.threadId })}>
                  {card.threadId ? "打开会话" : "打开 Nova"}
                  <svg viewBox="0 0 12 12" width="10" height="10"><path d="M4.5 2.5L8 6l-3.5 3.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
                </button>
              </footer>
            </div>
          );
        }}
      </For>
    </div>
  );
}
