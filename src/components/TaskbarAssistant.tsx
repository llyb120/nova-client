import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import DOMPurify from "dompurify";
import { marked } from "marked";
import { createEffect, createSignal, For, onCleanup, onMount, Show, untrack } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { fileToAttachment } from "./ImageAttachmentStrip";
import { titleOf, type Capsule } from "./ThoughtCapsules";
import "./TaskbarAssistant.css";

type Kind = "running" | "waiting" | "done" | "error";
/** `status`: live state of the dispatched session task; the backend rewrites this message in place.
 * `thinking` (assistant) / `running` with empty text (session) render as a typing indicator. */
type Message = { id: string; role: "user" | "assistant"; text: string; createdAt: number; threadId?: string; status?: Kind | "thinking" };
type History = { messages: Message[]; hasMore: boolean; busy?: boolean };
type Session = { threadId: string; title: string; kind: Kind; text: string; updatedAt: number };
type View = { id?: string; offset: number; top: number; bottom: boolean };
const LABEL = { running: "进行中", waiting: "待确认", done: "已完成", error: "出错了" };
const PAGE_SIZE = 30;
const MAX_FILE = 20 * 1024 * 1024;
type Upload = { name: string; data: string; mimeType: string };
/** Not the shared Markdown component: it pulls in main-window store/IPC this light page avoids. */
const markdown = (text: string) => DOMPurify.sanitize(marked.parse(text, { gfm: true, breaks: true, async: false }));
/** Links open in the browser; never navigate the panel's own page. */
const openLink = (event: MouseEvent) => {
  const href = (event.target as HTMLElement).closest("a")?.getAttribute("href");
  if (!href) return;
  event.preventDefault();
  if (/^https?:\/\//i.test(href)) void invoke("open_url", { url: href }).catch(() => {});
};

/** The unsent draft outlives the panel and Nova restarts. IndexedDB, not localStorage:
 * pasted attachments can reach 80 MiB. Keys: "text", "uploads", "picked". */
const draftStore = <T,>(run: (store: IDBObjectStore) => IDBRequest<T>, mode: IDBTransactionMode = "readonly") =>
  new Promise<T>((resolve, reject) => {
    const open = indexedDB.open("taskbar-assistant-draft", 1);
    open.onupgradeneeded = () => open.result.createObjectStore("draft");
    open.onerror = () => reject(open.error);
    open.onsuccess = () => {
      const db = open.result;
      const request = run(db.transaction("draft", mode).objectStore("draft"));
      request.onsuccess = () => { resolve(request.result); db.close(); };
      request.onerror = () => { reject(request.error); db.close(); };
    };
  });

/** `active`: assistant tab visible; `shown`: panel visible. The compose box serves every tab. */
/** 会话动态 for the 会话 tab: the user's own running / recently finished sessions. */
export function TaskbarSessions(props: { active: boolean }) {
  const [sessions, setSessions] = createStore<Session[]>([]);
  const [error, setError] = createSignal("");
  const refresh = async () => {
    if (!props.active) return;
    try {
      const current = await invoke<Session[]>("taskbar_assistant_sessions");
      const rank = (session: Session) => session.kind === "running" || session.kind === "waiting" ? 0 : 1;
      setSessions(reconcile([...current].sort((a, b) => rank(a) - rank(b) || b.updatedAt - a.updatedAt), { key: "threadId" }));
      setError("");
    } catch (e) { setError(String(e)); }
  };
  createEffect(() => {
    if (!props.active) return;
    untrack(() => void refresh());
    const timer = window.setInterval(() => void refresh(), 3000);
    onCleanup(() => clearInterval(timer));
  });
  const open = (threadId: string) => void invoke("taskbar_bubble_open", { threadId }).catch((e) => setError(String(e)));
  return (
    <section class="ta-sessions" aria-label="运行中和最近完成的会话">
      <div class="ta-section-title">会话动态 <span>最新步骤 / 结论</span></div>
      <div class="ta-session-list">
        <For each={sessions} fallback={<div class="ta-empty">暂无运行中或最近完成的会话</div>}>
          {(session) => (
            <button class="ta-session" onClick={() => open(session.threadId)}>
              <span class="ta-session-heading"><strong>{session.title || "未命名会话"}</strong><span class={`ta-status ${session.kind}`}>{LABEL[session.kind]}</span></span>
              <span class="ta-session-text">{session.text || "暂无更新"}</span>
            </button>
          )}
        </For>
      </div>
      <Show when={error()}><div class="ta-error" role="alert">{error()}</div></Show>
    </section>
  );
}

export default function TaskbarAssistant(props: { active: boolean; shown: boolean; quick: boolean; focusRequest: number }) {
  const [messages, setMessages] = createStore<Message[]>([]);
  const [text, setText] = createSignal("");
  const [sending, setSending] = createSignal(false);
  const [backendBusy, setBackendBusy] = createSignal(false);
  const [hasMore, setHasMore] = createSignal(false);
  const [paging, setPaging] = createSignal(false);
  const [loading, setLoading] = createSignal(true);
  const [loadError, setLoadError] = createSignal("");
  const [sendError, setSendError] = createSignal("");
  const [away, setAway] = createSignal(false);
  const [uploads, setUploads] = createSignal<Upload[]>([]);
  const [picked, setPicked] = createSignal<Capsule[]>([]);
  const [menu, setMenu] = createSignal<"" | "menu" | "capsules">("");
  const [capsules, setCapsules] = createSignal<Capsule[]>([]);
  let imagePicker!: HTMLInputElement;
  let filePicker!: HTMLInputElement;
  let compose!: HTMLFormElement;
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
  const hasDraft = () => !!text().trim() || uploads().length > 0 || picked().length > 0;
  const addFiles = async (files: File[]) => {
    if (!files.length) return;
    setSendError("");
    try {
      // Checked before reading: a huge file must not be base64-encoded in the webview.
      if (files.some((file) => file.size > MAX_FILE)) throw new Error("单个附件上限 20 MiB");
      const read = await Promise.all(files.map(fileToAttachment));
      setUploads((all) => [...all, ...read.map(({ name, data, mimeType }) => ({ name, data: data ?? "", mimeType }))]);
    } catch (error) { setSendError(String(error)); }
  };
  const openCapsules = async () => {
    setMenu("capsules");
    try { setCapsules(await invoke<Capsule[]>("capsule_list")); }
    catch (error) { setSendError(String(error)); setMenu(""); }
  };
  const toggleCapsule = (capsule: Capsule) =>
    setPicked((all) => all.some((c) => c.id === capsule.id) ? all.filter((c) => c.id !== capsule.id) : [...all, capsule]);
  let restored = false;
  const persist = (key: string, value: unknown) => {
    if (restored) draftStore((s) => s.put(value, key), "readwrite").catch((error) => setSendError(`草稿未能保存到本机：${String(error)}`));
  };
  createEffect(() => persist("text", text()));
  createEffect(() => persist("uploads", uploads()));
  createEffect(() => persist("picked", picked()));
  /** Closing the panel on the quick tab files the draft as 星尘 (text and each attachment separately) and keeps no draft.
   * Closing on any other tab keeps the draft as is. */
  const stash = async () => {
    const value = text();
    const files = uploads();
    if (sending()) return;
    setPicked([]);
    if (!value.trim() && !files.length) { setText(""); return; }
    setSending(true);
    setSendError("");
    setMenu("");
    try {
      await invoke("capsule_capture", { text: value.trim() ? value : "", paths: [], uploads: files.map(({ name, data }) => ({ name, data })) });
      // Clear only what was captured; anything added meanwhile stays in the draft.
      if (text() === value) setText("");
      if (uploads() === files) setUploads([]);
    } catch (error) { if (!disposed) setSendError(`未能存为星尘，已保留草稿：${String(error)}`); }
    finally { setSending(false); }
  };
  let wasShown = false;
  createEffect(() => {
    const shown = props.shown;
    if (wasShown && !shown && untrack(() => props.quick)) void stash();
    wasShown = shown;
  });

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
    // The first page also loads while hidden, so the first click opens on ready history.
    if (disposed || (!props.active && initialized)) return;
    if (refreshing || paging()) { refreshAgain = true; return; }
    refreshing = true;
    refreshAgain = false;
    const revision = messageRevision;
    const [recent] = await Promise.allSettled([invoke<History>("taskbar_assistant_history", { limit: PAGE_SIZE })]);
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
    const files = uploads();
    const chosen = picked();
    if (!hasDraft() || busy() || composing) return;
    setSending(true);
    setSendError("");
    setMenu("");
    // Clear now: dispatch waits for the model, which can take seconds.
    setText("");
    setUploads([]);
    setPicked([]);
    try {
      // The assistant only dispatches. Never send this text via taskbar_bubble_reply.
      const message = await invoke<Message>("taskbar_assistant_send", {
        text: value, capsules: chosen.map((c) => c.id), uploads: files.map(({ name, data }) => ({ name, data })),
      });
      if (disposed) return;
      merge([message]);
    } catch (error) {
      if (disposed) return;
      setSendError(String(error));
      // Restore the draft unless something new was added meanwhile.
      if (!text()) setText(draft);
      if (!uploads().length) setUploads(files);
      if (!picked().length) setPicked(chosen);
    }
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
    if (!props.shown || !request || request === focusedRequest) return;
    focusedRequest = request;
    // Parent increments only after successful native placement of a click-opened panel.
    const focus = () => input.focus({ preventScroll: true });
    focus();
    // The first native activation can reach the WebView after this; focus again once it does.
    if (!document.hasFocus()) window.addEventListener("focus", focus, { once: true });
  });
  onMount(() => {
    void Promise.all(["text", "uploads", "picked"].map((key) => draftStore((s) => s.get(key))))
      .then(([savedText, savedUploads, savedPicked]) => {
        if (disposed) return;
        // Typing that raced the restore wins; it is persisted once `restored` is set.
        if (typeof savedText === "string" && !text()) setText(savedText);
        if (Array.isArray(savedUploads) && !uploads().length) setUploads(savedUploads as Upload[]);
        if (Array.isArray(savedPicked) && !picked().length) setPicked(savedPicked as Capsule[]);
        // Only after a successful read: never overwrite a saved draft we could not load.
        restored = true;
        persist("text", text()); persist("uploads", uploads()); persist("picked", picked());
      })
      .catch((error) => setSendError(`未能读取上次的草稿：${String(error)}`));
    const off = listen("taskbar-assistant:changed", () => void refresh());
    const resize = new ResizeObserver(restoreView);
    resize.observe(history);
    void off.then(() => void refresh()).catch((error) => setLoadError(String(error)));
    // Typing anywhere in the panel that is not already a field goes to the assistant, on any tab.
    const redirect = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (!props.shown || event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey || event.key.length !== 1 || event.key === " ") return;
      if (target?.closest?.("input, textarea, select, [contenteditable], [role=dialog], [aria-modal]")) return;
      input.focus({ preventScroll: true }); // The keystroke then lands in the focused textarea.
    };
    window.addEventListener("keydown", redirect);
    const dismiss = (event: PointerEvent) => { if (menu() && !compose.contains(event.target as Node)) setMenu(""); };
    window.addEventListener("pointerdown", dismiss);
    onCleanup(() => {
      disposed = true;
      window.removeEventListener("keydown", redirect);
      window.removeEventListener("pointerdown", dismiss);
      resize.disconnect();
      void off.then((unlisten) => unlisten()).catch(() => {});
    });
  });

  return (<>
    <section id="tb-pane-assistant" role="tabpanel" aria-labelledby="tb-tab-assistant" class="ta" hidden={!props.active}>
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
                    <div class="ta-bubble" classList={{ typing: !message.text, md: message.role === "assistant" && !!message.text }}>
                      <Show when={message.text} keyed fallback={<>{message.status === "thinking" ? "正在思考中" : "会话正在输入中"}<span class="ta-dots" aria-hidden="true"><i /><i /><i /></span></>}>
                        {(text) => message.role === "assistant" ? <div class="ta-md" innerHTML={markdown(text)} onClick={openLink} /> : text}
                      </Show>
                    </div>
                    <Show when={message.threadId || (message.status && message.text)}>
                      <div class="ta-message-meta">
                        <Show when={message.text && message.status && message.status !== "thinking"}><span class={`ta-status ${message.status}`} title="会话状态">会话{LABEL[message.status as Kind]}</span></Show>
                        <Show when={message.threadId}><button class="ta-thread" onClick={() => openThread(message.threadId!)}>打开会话 ↗</button></Show>
                      </div>
                    </Show>
                  </div>
                </article>
              )}
            </For>
          </div>
        </div>
        <Show when={away()}><button class="ta-latest" onClick={() => { view = { offset: 0, top: 0, bottom: true }; restoreView(); }}>回到最新 ↓</button></Show>
      </div>
      <Show when={loadError()}><div class="ta-error" role="alert">{loadError()} <button onClick={() => void refresh()}>刷新</button></div></Show>
    </section>
    <form class="ta-compose" ref={compose} onSubmit={(event) => { event.preventDefault(); void send(); }}>
        <Show when={uploads().length || picked().length}>
          <div class="ta-attachments">
            <For each={picked()}>
              {(capsule) => (
                <span class="ta-chip capsule" title={titleOf(capsule)}>
                  <span class="ta-chip-icon" aria-hidden="true">✦</span><span class="ta-chip-name">{titleOf(capsule)}</span>
                  <button type="button" aria-label={`移除星尘 ${titleOf(capsule)}`} onClick={() => toggleCapsule(capsule)}>×</button>
                </span>
              )}
            </For>
            <For each={uploads()}>
              {(file, index) => (
                <span class="ta-chip" title={file.name}>
                  <Show when={file.mimeType.startsWith("image/")} fallback={<svg class="ta-chip-icon" viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><path d="M14 3H7a2 2 0 00-2 2v14a2 2 0 002 2h10a2 2 0 002-2V8l-5-5zm0 0v5h5" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linejoin="round" /></svg>}>
                    <img src={`data:${file.mimeType};base64,${file.data}`} alt="" />
                  </Show>
                  <span class="ta-chip-name">{file.name}</span>
                  <button type="button" aria-label={`移除 ${file.name}`} onClick={() => setUploads((all) => all.filter((_, i) => i !== index()))}>×</button>
                </span>
              )}
            </For>
          </div>
        </Show>
        <button type="button" class="ta-plus" classList={{ open: !!menu() }} title="添加图片、文件或星尘" aria-label="添加附件" aria-expanded={!!menu()} onClick={() => setMenu(menu() ? "" : "menu")}>
          <svg viewBox="0 0 12 12" width="13" height="13" aria-hidden="true"><path d="M6 2v8M2 6h8" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" /></svg>
        </button>
        <Show when={menu() === "menu"}>
          <div class="ta-menu" role="menu">
            <button type="button" role="menuitem" onClick={() => { setMenu(""); imagePicker.click(); }}>图片</button>
            <button type="button" role="menuitem" onClick={() => { setMenu(""); filePicker.click(); }}>文件</button>
            <button type="button" role="menuitem" onClick={() => void openCapsules()}>星尘</button>
          </div>
        </Show>
        <Show when={menu() === "capsules"}>
          <div class="ta-menu ta-capsule-menu" role="listbox" aria-multiselectable="true" aria-label="选择星尘">
            <For each={capsules()} fallback={<div class="ta-empty">还没有星尘，可以拖入文字或文件收集</div>}>
              {(capsule) => (
                <button type="button" role="option" aria-selected={picked().some((c) => c.id === capsule.id)} onClick={() => toggleCapsule(capsule)}>
                  <span class="ta-check" aria-hidden="true" />{titleOf(capsule)}
                </button>
              )}
            </For>
          </div>
        </Show>
        <input ref={imagePicker} type="file" accept="image/*" multiple hidden onChange={(event) => { void addFiles(Array.from(event.currentTarget.files ?? [])); event.currentTarget.value = ""; }} />
        <input ref={filePicker} type="file" multiple hidden onChange={(event) => { void addFiles(Array.from(event.currentTarget.files ?? [])); event.currentTarget.value = ""; }} />
        <textarea ref={input} class="tb-input ta-input" rows={1} aria-label="给助理的消息" placeholder="告诉助理要安排什么…" title="Enter 发送 · Shift+Enter 换行 · 可粘贴图片和文件" value={text()}
          onInput={(event) => setText(event.currentTarget.value)}
          onPaste={(event) => {
            const files = Array.from(event.clipboardData?.files ?? []);
            if (!files.length) return;
            event.preventDefault();
            void addFiles(files);
          }}
          onFocus={() => void invoke("taskbar_bubble_activate").catch((error) => setSendError(String(error)))}
          onCompositionStart={() => { composing = true; }} onCompositionEnd={() => { composing = false; }}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey && !event.isComposing && event.keyCode !== 229 && !composing) {
              event.preventDefault();
              void send();
            }
          }}
        />
        <button class="ta-submit" type="submit" title="发送" aria-label="发送" disabled={busy() || !hasDraft()}>
          <svg viewBox="0 0 12 12" width="13" height="13" aria-hidden="true"><path d="M6 10V2.5M2.5 6L6 2.5 9.5 6" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" /></svg>
        </button>
        <Show when={sendError()}><div class="ta-error" role="alert">{sendError()}</div></Show>
      </form>
  </>);
}
