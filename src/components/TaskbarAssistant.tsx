import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createEffect, createSignal, For, onCleanup, onMount, Show, untrack } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import "./TaskbarAssistant.css";

type Message = { id: string; role: "user" | "assistant"; text: string; createdAt: number; threadId?: string };
type History = { messages: Message[]; hasMore: boolean; busy?: boolean };
type Session = { threadId: string; title: string; kind: "running" | "waiting" | "done" | "error"; text: string; updatedAt: number };
type View = { id?: string; offset: number; top: number; bottom: boolean };
const LABEL = { running: "进行中", waiting: "待确认", done: "已完成", error: "出错了" };
const PAGE_SIZE = 30;

export default function TaskbarAssistant(props: { active: boolean; focusRequest: number }) {
  const [messages, setMessages] = createStore<Message[]>([]);
  const [sessions, setSessions] = createStore<Session[]>([]);
  const [text, setText] = createSignal("");
  const [sending, setSending] = createSignal(false);
  const [backendBusy, setBackendBusy] = createSignal(false);
  const [hasMore, setHasMore] = createSignal(false);
  const [paging, setPaging] = createSignal(false);
  const [loading, setLoading] = createSignal(true);
  const [loadError, setLoadError] = createSignal("");
  const [sendError, setSendError] = createSignal("");
  const [away, setAway] = createSignal(false);
  let history!: HTMLDivElement;
  let messageList!: HTMLDivElement;
  let input!: HTMLTextAreaElement;
  let view: View = { offset: 0, top: 0, bottom: true };
  let initialized = false;
  let disposed = false;
  let refreshing = false;
  let refreshAgain = false;
  let composing = false;
  let focusedRequest = 0;
  let messageRevision = 0;
  let latestHistoryId: string | undefined;
  const busy = () => sending() || backendBusy();

  const readView = (): View => {
    if (!history.clientHeight) return view;
    const top = history.getBoundingClientRect().top;
    const first = Array.from(messageList.children).find((row) => row.getBoundingClientRect().bottom > top) as HTMLElement | undefined;
    return {
      id: first?.dataset.messageId,
      offset: first ? first.getBoundingClientRect().top - top : 0,
      top: history.scrollTop,
      bottom: history.scrollHeight - history.clientHeight - history.scrollTop < 32,
    };
  };
  const restoreView = () => {
    if (disposed || !history.clientHeight) return;
    if (view.bottom) history.scrollTop = history.scrollHeight;
    else {
      const row = view.id ? messageList.querySelector<HTMLElement>(`[data-message-id="${CSS.escape(view.id)}"]`) : null;
      history.scrollTop = row ? history.scrollTop + row.getBoundingClientRect().top - history.getBoundingClientRect().top - view.offset : view.top;
    }
    view = readView();
    setAway(!view.bottom);
  };
  const merge = (incoming: Message[], older = false) => {
    view = readView();
    if (older) view.bottom = false; // Prepending is never a request to jump to the newest message.
    // Preserve backend ordering, including messages created in the same millisecond.
    const ids = new Set(incoming.map((message) => message.id));
    const retained = messages.filter((message) => !ids.has(message.id));
    ++messageRevision;
    setMessages(reconcile(older ? [...incoming, ...retained] : [...retained, ...incoming], { key: "id" }));
    queueMicrotask(restoreView);
  };

  const refresh = async () => {
    if (disposed || !props.active) return;
    if (refreshing || paging()) { refreshAgain = true; return; }
    refreshing = true;
    refreshAgain = false;
    const revision = messageRevision;
    const [recent, current] = await Promise.allSettled([
      invoke<History>("taskbar_assistant_history", { limit: PAGE_SIZE }),
      invoke<Session[]>("taskbar_assistant_sessions"),
    ]);
    if (disposed) return;
    const errors: string[] = [];
    if (revision !== messageRevision) refreshAgain = true;
    else if (recent.status === "fulfilled") {
      const page = recent.value;
      // On reopen, bridge any missed events before merging, so retained older history has no gap.
      // Initial opening still fetches only one page, not the whole conversation.
      let incoming = page.messages;
      let more = page.hasMore;
      const boundary = latestHistoryId;
      try {
        while (boundary && more && incoming.length && !incoming.some((message) => message.id === boundary)) {
          const before = incoming[0].id;
          const previous = await invoke<History>("taskbar_assistant_history", { before, limit: PAGE_SIZE });
          if (disposed) return;
          if (!previous.messages.length || previous.messages[0].id === before) throw new Error("历史分页未前进，请重试");
          incoming = [...previous.messages, ...incoming];
          more = previous.hasMore;
        }
        if (revision !== messageRevision) refreshAgain = true;
        else {
          if (!initialized) { setHasMore(more); initialized = true; }
          merge(incoming);
          latestHistoryId = page.messages.at(-1)?.id;
          setBackendBusy(Boolean(page.busy));
        }
      } catch (error) { errors.push(`聊天记录：${String(error)}`); }
    } else errors.push(`聊天记录：${String(recent.reason)}`);
    if (current.status === "fulfilled") {
      const rank = (session: Session) => session.kind === "running" || session.kind === "waiting" ? 0 : 1;
      setSessions(reconcile([...current.value].sort((a, b) => rank(a) - rank(b) || b.updatedAt - a.updatedAt), { key: "threadId" }));
    } else errors.push(`会话动态：${String(current.reason)}`);
    setLoadError(errors.join("；"));
    setLoading(false);
    refreshing = false;
    if (refreshAgain) void refresh();
  };
  const loadOlder = async () => {
    if (!hasMore() || paging() || refreshing || !messages.length) return;
    setPaging(true);
    try {
      // before is an exclusive, opaque message id, never a timestamp or an array index.
      const before = messages[0].id;
      const page = await invoke<History>("taskbar_assistant_history", { before, limit: PAGE_SIZE });
      if (disposed) return;
      if (page.hasMore && (!page.messages.length || page.messages[0].id === before)) throw new Error("历史分页未前进，请重试");
      merge(page.messages, true);
      setHasMore(page.hasMore);
      setLoadError("");
    } catch (error) { if (!disposed) setLoadError(`更早消息：${String(error)}`); }
    finally {
      setPaging(false);
      if (refreshAgain) void refresh();
    }
  };
  const send = async () => {
    const draft = text();
    const value = draft.trim();
    if (!value || busy() || composing) return;
    setSending(true);
    setSendError("");
    try {
      // The assistant only dispatches. Never send this text via taskbar_bubble_reply.
      const message = await invoke<Message>("taskbar_assistant_send", { text: value });
      if (disposed) return;
      merge([message]);
      if (text() === draft) setText(""); // Preserve anything typed while dispatch was in flight.
    } catch (error) { if (!disposed) setSendError(String(error)); }
    finally {
      setSending(false);
      if (!disposed) void refresh();
    }
  };
  const openThread = (threadId: string) => {
    void invoke("taskbar_bubble_open", { threadId }).catch((error) => setSendError(String(error)));
  };

  createEffect(() => {
    if (!props.active) return;
    queueMicrotask(restoreView);
    untrack(() => void refresh());
    const timer = window.setInterval(() => void refresh(), 3000);
    onCleanup(() => clearInterval(timer));
  });
  createEffect(() => {
    const request = props.focusRequest;
    if (!props.active || !request || request === focusedRequest) return;
    focusedRequest = request;
    // Parent increments only after successful native placement of a click-opened panel.
    input.focus({ preventScroll: true });
  });
  onMount(() => {
    const off = listen("taskbar-assistant:changed", () => void refresh());
    const resize = new ResizeObserver(restoreView);
    resize.observe(history);
    void off.then(() => { if (props.active) void refresh(); }).catch((error) => setLoadError(String(error)));
    onCleanup(() => {
      disposed = true;
      resize.disconnect();
      void off.then((unlisten) => unlisten()).catch(() => {});
    });
  });

  return (
    <section id="tb-pane-assistant" role="tabpanel" aria-labelledby="tb-tab-assistant" class="ta" hidden={!props.active}>
      <section class="ta-sessions" aria-label="运行中和最近完成的会话">
        <div class="ta-section-title">会话动态 <span>最新步骤 / 结论</span></div>
        <div class="ta-session-list">
          <For each={sessions} fallback={<div class="ta-empty">暂无运行中或最近完成的会话</div>}>
            {(session) => (
              <button class="ta-session" onClick={() => openThread(session.threadId)}>
                <span class="ta-session-heading"><strong>{session.title || "未命名会话"}</strong><span class={`ta-status ${session.kind}`}>{LABEL[session.kind]}</span></span>
                <span class="ta-session-text">{session.text || "暂无更新"}</span>
              </button>
            )}
          </For>
        </div>
      </section>
      <div class="ta-chat">
        <div class="ta-history" ref={history} role="log" aria-label="助理聊天记录" aria-live="polite" tabIndex={0} onScroll={() => {
          if (!history.clientHeight) return;
          const previousTop = view.top;
          view = readView();
          setAway(!view.bottom);
          if (view.top < previousTop && view.top < 60) void loadOlder();
        }}>
          <div class="ta-history-head">
            <Show when={hasMore()} fallback={<span>{loading() ? "正在读取最近消息…" : "只负责调度，执行进展见会话"}</span>}>
              <button disabled={paging()} onClick={() => void loadOlder()}>{paging() ? "正在读取…" : "查看更早消息"}</button>
            </Show>
          </div>
          <div ref={messageList}>
            <For each={messages} fallback={<Show when={!loading()}><div class="ta-welcome">有什么事需要我安排？<small>告诉我目标，我来分派任务、跟进会话。</small></div></Show>}>
              {(message) => (
                <article class={`ta-message ${message.role}`} data-message-id={message.id} aria-label={message.role === "user" ? "你" : "Nova 助理"}>
                  <span class="ta-avatar" aria-hidden="true">{message.role === "user" ? "我" : "N"}</span>
                  <div class="ta-message-content">
                    <div class="ta-bubble">{message.text}</div>
                    <Show when={message.threadId}><button class="ta-thread" onClick={() => openThread(message.threadId!)}>打开会话 ↗</button></Show>
                  </div>
                </article>
              )}
            </For>
          </div>
        </div>
        <Show when={away()}><button class="ta-latest" onClick={() => { view = { offset: 0, top: 0, bottom: true }; restoreView(); }}>回到最新 ↓</button></Show>
      </div>
      <Show when={loadError()}><div class="ta-error" role="alert">{loadError()} <button onClick={() => void refresh()}>刷新</button></div></Show>
      <form class="ta-compose" onSubmit={(event) => { event.preventDefault(); void send(); }}>
        <textarea ref={input} class="tb-input ta-input" rows={2} aria-label="给助理的消息" placeholder="告诉助理要安排什么…" value={text()}
          onInput={(event) => setText(event.currentTarget.value)}
          onFocus={() => void invoke("taskbar_bubble_activate").catch((error) => setSendError(String(error)))}
          onCompositionStart={() => { composing = true; }} onCompositionEnd={() => { composing = false; }}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey && !event.isComposing && event.keyCode !== 229 && !composing) {
              event.preventDefault();
              void send();
            }
          }}
        />
        <div class="ta-compose-foot"><span role="status">{busy() ? "助理正在调度…" : "Enter 发送 · Shift+Enter 换行"}</span><button class="ta-submit" type="submit" disabled={busy() || !text().trim()}>发送</button></div>
        <Show when={sendError()}><div class="ta-error" role="alert">{sendError()}</div></Show>
      </form>
    </section>
  );
}
