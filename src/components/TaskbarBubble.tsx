import DOMPurify from "dompurify";
import { Marked } from "marked";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createMemo, createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import type { PermissionRequest } from "../types";
import { agentLabel, stripAnsi } from "../utils";
import TaskbarAssistant, { TaskbarSessions } from "./TaskbarAssistant";
import ThoughtCapsules, { captureCapsule } from "./ThoughtCapsules";
import { fileToAttachment } from "./ImageAttachmentStrip";
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
/** `light`: the Windows taskbar theme, so cards match the Butterfly they pop out of.
 *  `panel`: hover must not focus; a click focuses the assistant after placement. */
type Payload = { cards: Card[]; permissions: Permission[]; light: boolean; panel?: boolean; captureError?: string | null } & Partial<PanelData>;

type PanelData = { jianpu: { recording: boolean; id?: string; elapsedMs?: number } };
type Tab = "assistant" | "session" | "capsules" | "quick";
const TABS: [Tab, string, string][] = [
  ["assistant", "助理", "M12 3.5l1.9 5.1 5.1 1.9-5.1 1.9L12 17.5l-1.9-5.1L5 10.5l5.1-1.9zM18.5 15.5l.7 1.8 1.8.7-1.8.7-.7 1.8-.7-1.8-1.8-.7 1.8-.7z"],
  ["session", "会话", "M5.5 5h13A1.5 1.5 0 0120 6.5v8a1.5 1.5 0 01-1.5 1.5H11l-4 3.5V16H5.5A1.5 1.5 0 014 14.5v-8A1.5 1.5 0 015.5 5zM8.5 10.5h7"],
  ["capsules", "星屑", "M12 3.8l2.5 5.1 5.6.8-4 4 .9 5.6-5-2.6-5 2.6.9-5.6-4-4 5.6-.8z"],
  ["quick", "快捷", "M5 4.5h4.5A.5.5 0 0110 5v4.5a.5.5 0 01-.5.5H5a.5.5 0 01-.5-.5V5a.5.5 0 01.5-.5zM14.5 4.5H19a.5.5 0 01.5.5v4.5a.5.5 0 01-.5.5h-4.5a.5.5 0 01-.5-.5V5a.5.5 0 01.5-.5zM5 14h4.5a.5.5 0 01.5.5V19a.5.5 0 01-.5.5H5a.5.5 0 01-.5-.5v-4.5A.5.5 0 015 14zM14.5 14H19a.5.5 0 01.5.5V19a.5.5 0 01-.5.5h-4.5a.5.5 0 01-.5-.5v-4.5a.5.5 0 01.5-.5z"],
];
const clock = (ms = 0) => `${Math.floor(ms / 60000)}:${String(Math.floor(ms / 1000) % 60).padStart(2, "0")}`;
const icon = (d: string, size = 22) => (
  <svg viewBox="0 0 24 24" width={size} height={size} aria-hidden="true"><path d={d} fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" /></svg>
);
const ICONS = {
  brush: "M4 20l4.5-1.2L19 8.3a2.1 2.1 0 00-3-3L5.5 15.8 4 20zM14.5 6.8l3 3",
  stop: "M7 7h10v10H7z",
  graph: "M6 7a2 2 0 100-.01M18 6a2 2 0 100-.01M12 18a2 2 0 100-.01M7.6 8.2l3.2 7.8M16.4 7.2l-3.2 8.8M8 6.6h8",
};

/** One persistent panel, independent of session keys and native show/hide cycles. */
function Panel(props: { card?: Card; data: PanelData; tab: Tab; setTab: (tab: Tab) => void; session: JSX.Element; active: boolean; focusRequest: number; permissionCount: number; onClose: () => void; reload: () => void }) {
  const [error, setError] = createSignal("");
  const run = async (action: string) => {
    setError("");
    try {
      await invoke("taskbar_quick", { action });
    } catch (e) {
      setError(String(e));
    }
    props.reload();
  };
  const rec = () => props.data.jianpu.recording;
  // Fixed button nodes with reactive labels: the 1s refresh (recording clock) must not rebuild the DOM
  // under the pointer, or a click spanning the refresh is lost.
  const tiles = [
    { action: "jianpu", label: () => (rec() ? `停止 ${clock(props.data.jianpu.elapsedMs)}` : "剑谱录制"), icon: () => (rec() ? ICONS.stop : ICONS.brush), hot: rec },
    { action: "knowledge", label: () => "知识图谱", icon: () => ICONS.graph, hot: () => false },
  ];
  return (
    <div class={`tb-card tb-panel ${props.card?.kind ?? "idle"}`}>
      <nav class="tb-tabs" role="tablist" aria-label="蝴蝶面板" aria-orientation="vertical">
        <For each={TABS}>
          {([id, label, d]) => (
            <button id={`tb-tab-${id}`} role="tab" aria-controls={`tb-pane-${id}`} aria-selected={props.tab === id} tabIndex={props.tab === id ? 0 : -1} classList={{ active: props.tab === id, rec: id === "quick" && rec() }} onClick={() => props.setTab(id)} onKeyDown={(event) => {
              if (!["ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) return;
              event.preventDefault();
              const index = TABS.findIndex(([tab]) => tab === id);
              const next = event.key === "Home" ? 0 : event.key === "End" ? TABS.length - 1 : (index + (event.key === "ArrowDown" ? 1 : -1) + TABS.length) % TABS.length;
              props.setTab(TABS[next][0]);
              document.getElementById(`tb-tab-${TABS[next][0]}`)?.focus();
            }}>
              {icon(d, 19)}<span>{label}</span><Show when={id === "session" && props.permissionCount}><span class="tb-tab-count">{props.permissionCount}</span></Show>
            </button>
          )}
        </For>
      </nav>
      <div class="tb-panel-main">
        <header class="tb-head">
          <Butterfly />
          <span class="tb-state">{props.tab === "assistant" ? "Nova 助理" : props.tab === "capsules" ? "星屑" : LABEL[props.card?.kind ?? "idle"]}</span>
          <Show when={props.tab === "session" && props.card?.trailing}>
            <span class="tb-time">{props.card?.trailing}</span>
          </Show>
          <button class="tb-close" title="关闭" aria-label="关闭" onClick={props.onClose}>
            <svg viewBox="0 0 12 12" width="10" height="10"><path d="M2 2l8 8M10 2l-8 8" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" /></svg>
          </button>
        </header>
        <TaskbarAssistant active={props.active && props.tab === "assistant"} shown={props.active} focusRequest={props.focusRequest} />
        <div id="tb-pane-capsules" role="tabpanel" aria-labelledby="tb-tab-capsules" class="tb-capsules-pane" hidden={props.tab !== "capsules"}>
          <ThoughtCapsules active={props.active && props.tab === "capsules"} onCaptured={() => props.setTab("capsules")} />
        </div>
        <div id="tb-pane-session" role="tabpanel" aria-labelledby="tb-tab-session" class="tb-session-list" hidden={props.tab !== "session"}>{props.session}<TaskbarSessions active={props.active && props.tab === "session"} /></div>
        <div id="tb-pane-quick" role="tabpanel" aria-labelledby="tb-tab-quick" hidden={props.tab !== "quick"}>
          <div class="tb-grid">
            <For each={tiles}>
              {(t) => (
                <button class="tb-tile" classList={{ hot: t.hot() }} onClick={() => void run(t.action)}>
                  {icon(t.icon())}
                  <span>{t.label()}</span>
                </button>
              )}
            </For>
          </div>
        </div>
        <Show when={error()}>
          <div class="tb-reply-error" role="alert">{error()}</div>
        </Show>
      </div>
    </div>
  );
}

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
          if (e.key === "Enter" && !e.shiftKey && !e.isComposing && e.keyCode !== 229) {
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
  const [panel, setPanel] = createSignal<PanelData>({ jianpu: { recording: false } });
  const [tab, setTab] = createSignal<Tab>("assistant");
  const [active, setActive] = createSignal(false);
  const [focusRequest, setFocusRequest] = createSignal(0);
  const [dragging, setDragging] = createSignal(false);
  const [captureError, setCaptureError] = createSignal("");
  const [capturing, setCapturing] = createSignal(false);
  const capture = async (event: DragEvent) => {
    event.preventDefault();
    setDragging(false);
    const data = event.dataTransfer;
    if (!data) return;
    if (capturing()) { setCaptureError("上一份星屑仍在保存，请完成后重新拖入。"); return; }
    // Snapshot while the drag data is readable. Wry's native handler is disabled on this
    // WebView so text, links and files all arrive here once; the chip still uses native OLE.
    const files = Array.from(data.files);
    const directory = Array.from(data.items).some(item => item.webkitGetAsEntry?.()?.isDirectory);
    const text = files.length ? "" : data.getData("text/plain") || data.getData("text/uri-list").split(/\r?\n/).filter(line => line && !line.startsWith("#")).join("\n")
      || new DOMParser().parseFromString(data.getData("text/html"), "text/html").body.textContent || "";
    setCaptureError("");
    setCapturing(true);
    try {
      await invoke("taskbar_bubble_activate");
      if (directory) throw new Error("请拖入文件，不支持文件夹；不会递归读取。");
      if (files.length > 16 || files.some(file => file.size > 20 * 1024 * 1024) || files.reduce((total, file) => total + file.size, 0) > 80 * 1024 * 1024) {
        throw new Error("一次最多收取 16 个文件，单文件 20 MiB，合计 80 MiB；本次未保存。");
      }
      if (files.length) {
        const uploads = [];
        for (const file of files) { const attachment = await fileToAttachment(file); uploads.push({ name: attachment.name, data: attachment.data }); }
        await invoke("capsule_capture", { text: "", paths: [], uploads });
      } else {
        if (!text.trim()) throw new Error("这份拖入内容没有可保存的文字，请拖入文件、链接或选中的文本。");
        await captureCapsule(text);
      }
    } catch (error) { setCaptureError(String(error)); }
    finally { setCapturing(false); }
  };
  let fresh = true;
  let focusPending = false;
  let revision = 0;
  let shell!: HTMLDivElement;
  let timer: number | undefined;

  // Ask for the desired height, not the viewport-clamped DOM height: otherwise each native
  // resize would subtract the transparent padding again. Native clamps this to the work area.
  const place = () => invoke<boolean>("taskbar_bubble_place", { height: 640 }).catch(() => false);
  const hide = () => void invoke("taskbar_bubble_hide").catch(() => {});
  const dismiss = (card: Card) => void invoke("taskbar_bubble_dismiss", { threadId: card.live ? null : card.threadId }).catch(() => {});

  const load = async () => {
    const request = ++revision;
    const next = await invoke<Payload | null>("taskbar_bubble").catch(() => null);
    if (request !== revision) return false;
    // Empty payload means hidden/prewarmed; it must not acknowledge a result or take focus.
    if (!next?.cards.length) { setActive(false); return hide(); }
    document.documentElement.classList.toggle("light", next.light);
    setPanel({ jianpu: next.jianpu ?? { recording: false } });
    if (fresh) {
      focusPending = next.panel === false;
      setTab("assistant");
    }
    if (next.captureError) setCaptureError(next.captureError);
    fresh = false;
    // Keyed: a refresh updates the current card in place.
    setCards(reconcile(next.cards, { key: "key" }));
    // Keep in-progress answers: only rebuild cards when the set of requests changes.
    const keys = (list: Permission[]) => list.map((p) => p.requestKey).join("\n");
    if (keys(next.permissions) !== keys(permissions())) setPermissions(next.permissions);
    // A same-size update does not fire ResizeObserver; acknowledge it after rendering too.
    setActive(true);
    await new Promise(requestAnimationFrame);
    if (request !== revision) return false;
    const placed = await place();
    if (request !== revision) return false;
    if (placed && focusPending) {
      focusPending = false;
      setFocusRequest((value) => value + 1);
    }
    return placed;
  };
  const open = async () => {
    clearInterval(timer);
    // Live refresh only while shown (a prewarmed page is not).
    if (await load()) timer = window.setInterval(() => void load(), 1000);
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
      listen<boolean>("capsules:dragging", (event) => setDragging(event.payload)),
      // Drops are captured in place: never switch the user to the capsules tab.
      listen("capsules:captured", () => { setDragging(false); setCaptureError(""); }),
      listen<{ error: string }>("capsules:error", (event) => { setDragging(false); setCaptureError(event.payload.error); }),
      listen("taskbar-bubble:open", () => {
        fresh = true;
        void open();
      }),
      listen("taskbar-bubble:hidden", () => {
        ++revision;
        fresh = true;
        focusPending = false;
        clearInterval(timer);
        setActive(false);
        setDragging(false);
        setCaptureError("");
        setCards([]);
      }),
    ];
    // Listen before the first place(): a click landing in between is then either seen by
    // place() or delivered as an open event, never lost.
    void Promise.all(offs).then(() => open());
    onCleanup(() => {
      ++revision;
      observer.disconnect();
      window.removeEventListener("keydown", keys);
      clearInterval(timer);
      offs.forEach((p) => void p.then((off) => off()));
    });
  });

  return (
    <div class="tb-shell" ref={shell}
      onPointerDown={() => void invoke("taskbar_bubble_activate").catch(() => {})}
      onDragOver={(event) => { event.preventDefault(); if (event.dataTransfer) event.dataTransfer.dropEffect = "copy"; setDragging(true); }}
      onDragLeave={(event) => { if (!shell.contains(event.relatedTarget as Node | null)) setDragging(false); }}
      onDrop={(event) => void capture(event)}>
      <Show when={dragging()}><div class="tb-capsule-drop" role="status"><span>✦</span><strong>松手，收好这一刻</strong><small>文字、链接、文件 · 收集成星屑</small></div></Show>
      <Show when={capturing()}><div class="tb-capture-progress" role="status">正在收好星屑…</div></Show>
      <Show when={captureError()}><div class="tb-capture-error" role="alert">{captureError()}<button aria-label="关闭错误提示" onClick={() => setCaptureError("")}>×</button></div></Show>
      <Panel card={cards[0]} data={panel()} tab={tab()} setTab={setTab} active={active()} focusRequest={focusRequest()} permissionCount={permissions().length} onClose={() => cards[0] ? dismiss(cards[0]) : hide()} reload={() => void load()} session={
      <For each={cards} fallback={<div class="tb-activity">暂无会话</div>}>
        {(card) => {
          const html = createMemo(() => markdown(card.body));
          return (
            <div class="tb-session">
              <div class="tb-session-head">
                <div class="tb-title">{card.title || (card.threadId ? "未命名会话" : "新会话")}</div>
                <button class="tb-open" title="双击 Butterfly 也可直接打开" onClick={() => void invoke("taskbar_bubble_open", { threadId: card.threadId })}>
                  {card.threadId ? "打开会话" : "打开 Nova"}
                  <svg viewBox="0 0 12 12" width="10" height="10"><path d="M4.5 2.5L8 6l-3.5 3.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
                </button>
              </div>
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
            </div>
          );
        }}
      </For>
      } />
    </div>
  );
}
