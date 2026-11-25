import { Channel, invoke } from "@tauri-apps/api/core";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { createSignal, untrack } from "solid-js";

export type TerminalEvent =
  | { type: "windowsPty"; build: number | null }
  | { type: "data"; data: number[] }
  | { type: "exit"; code: number | null }
  | { type: "error"; message: string };
/** Tests replace only this native IPC boundary, not xterm or the component. */
export const terminalApi = {
  create: (id: string, threadId: string | null, cwd: string, cols: number, rows: number, onEvent: Channel<TerminalEvent>) => invoke<void>("terminal_create", { id, threadId, cwd, cols, rows, onEvent }),
  write: (id: string, data: number[]) => invoke<void>("terminal_write", { id, data }),
  resize: (id: string, cols: number, rows: number) => invoke<void>("terminal_resize", { id, cols, rows }),
  ack: (id: string, bytes: number) => invoke<void>("terminal_ack", { id, bytes }),
  close: (id: string) => invoke<void>("terminal_close", { id }),
};
function terminalGroup() {
  const [tabs, setTabs] = createSignal<TerminalTab[]>([]);
  const [activeId, setActiveId] = createSignal("");
  return { tabs, setTabs, activeId, setActiveId };
}
export type TerminalGroup = ReturnType<typeof terminalGroup>;
const groups = new Map<string, TerminalGroup>();
export function getTerminalGroup(key: string): TerminalGroup {
  let group = groups.get(key);
  if (!group) { group = terminalGroup(); groups.set(key, group); }
  return group;
}
export type CreateTerminalTabOptions = {
  initCommand?: string;
  fontSize?: number;
};

const clampFontSize = (n?: number) =>
  Math.min(24, Math.max(10, Math.round(n && Number.isFinite(n) ? n : 13)));

function normalizeInitCommand(command?: string): string {
  return (command ?? "").replace(/\r\n/g, "\n").replace(/\n+$/, "");
}

function makeTab(threadId: string | null, cwd: string, name: string, options?: CreateTerminalTabOptions) {
  // A stable label keeps shell OSC title/progress updates out of the tab bar.
  const title = () => name;
  const [status, setStatus] = createSignal<"starting" | "running" | "exited" | "error">("starting");
  const [error, setError] = createSignal("");
  const terminal = new Terminal({ cursorBlink: true, fontSize: clampFontSize(options?.fontSize), lineHeight: 1.2,
    fontFamily: '"JetBrains Mono Variable", Consolas, monospace', scrollback: 5000, screenReaderMode: true,
    // Apply before open(): neither the initial frame nor retained tabs inherit the app theme.
    theme: { background: "#000000", foreground: "#d4d8df", cursor: "#d4d8df", cursorAccent: "#000000", selectionBackground: "#344d70" } });
  const fit = new FitAddon(); terminal.loadAddon(fit);
  let ptySize: { cols: number; rows: number } | undefined;
  let resizing = false;
  const tab = {
    id: crypto.randomUUID(), threadId, cwd, terminal, fit, host: document.createElement("div"),
    title, status, setStatus, error, setError,
    initCommand: normalizeInitCommand(options?.initCommand),
    initSent: false,
    initTimer: undefined as ReturnType<typeof setTimeout> | undefined,
    opened: false, disposed: false, ready: undefined as Promise<void> | undefined,
    channel: undefined as Channel<TerminalEvent> | undefined, input: Promise.resolve(), queuedInput: 0,
    sendBytes: (_data: Uint8Array) => {},
    scheduleInitCommand(delayMs: number) {
      if (!tab.initCommand || tab.initSent || tab.disposed) return;
      if (tab.initTimer !== undefined) clearTimeout(tab.initTimer);
      tab.initTimer = setTimeout(() => {
        tab.initTimer = undefined;
        if (tab.disposed || tab.initSent || tab.status() !== "running") return;
        tab.initSent = true;
        const payload = tab.initCommand.replace(/\n/g, "\r") + "\r";
        tab.sendBytes(new TextEncoder().encode(payload));
      }, delayMs);
    },
    async startPty(channel: Channel<TerminalEvent>): Promise<void> {
      const { cols, rows } = terminal;
      await terminalApi.create(tab.id, threadId, cwd, cols, rows, channel);
      if (tab.disposed || terminal.options.disableStdin) return;
      ptySize = { cols, rows };
      setStatus("running");
      // Don't gate input on resize: ConPTY can wait for a cursor reply.
      void syncPtySize();
      tab.scheduleInitCommand(600);
    },
  };
  async function syncPtySize() {
    if (resizing) return;
    resizing = true;
    try {
      // Native resize calls can complete out of order; only send the latest size after each one finishes.
      while (ptySize && !tab.disposed && !terminal.options.disableStdin) {
        const { cols, rows } = terminal;
        if (cols === ptySize.cols && rows === ptySize.rows) break;
        try {
          await terminalApi.resize(tab.id, cols, rows);
          ptySize = { cols, rows };
        } catch (error) {
          if (!tab.disposed && !terminal.options.disableStdin) setError(String(error));
          if (cols === terminal.cols && rows === terminal.rows) break;
        }
      }
    } finally { resizing = false; }
  }
  tab.host.className = "workspace-terminal-instance";
  const send = (data: Uint8Array) => {
    if (tab.disposed || status() === "exited" || status() === "error") return;
    if (tab.queuedInput + data.length > 1024 * 1024) { setError("输入队列已满，请分段粘贴。"); return; }
    tab.queuedInput += data.length;
    // Serialize IPC to retain keystroke/paste order even when the native writer blocks.
    tab.input = tab.input.then(async () => {
      await tab.ready;
      for (let offset = 0; offset < data.length && !tab.disposed; offset += 32768)
        await terminalApi.write(tab.id, Array.from(data.subarray(offset, offset + 32768)));
    }).catch(error => { if (!tab.disposed && status() !== "exited") setError(String(error)); })
      .finally(() => { tab.queuedInput -= data.length; });
  };
  tab.sendBytes = send;
  terminal.onData(data => send(new TextEncoder().encode(data)));
  terminal.onBinary(data => send(Uint8Array.from(data, char => char.charCodeAt(0))));
  terminal.onResize(() => { void syncPtySize(); });
  terminal.attachCustomKeyEventHandler(event => {
    if (event.defaultPrevented) return false;
    const copyPaste = !event.altKey && ((event.ctrlKey && event.shiftKey) || event.metaKey);
    if (copyPaste && event.key.toLowerCase() === "c" && terminal.hasSelection() && navigator.clipboard) {
      if (event.type === "keydown") { event.preventDefault(); void navigator.clipboard.writeText(terminal.getSelection()).catch(error => setError(String(error))); }
      return false;
    }
    if (copyPaste && event.key.toLowerCase() === "v" && navigator.clipboard) {
      if (event.type === "keydown") {
        event.preventDefault();
        void navigator.clipboard.readText().then(text => { if (!tab.disposed) terminal.paste(text); }).catch(error => setError(String(error)));
      }
      return false;
    }
    return true;
  });
  return tab;
}
export type TerminalTab = ReturnType<typeof makeTab>;
export function createTerminalTab(
  group: TerminalGroup,
  threadId: string | null,
  cwd: string,
  options?: CreateTerminalTabOptions,
): TerminalTab {
  if ([...groups.values()].reduce((n, value) => n + value.tabs().length, 0) >= 32)
    throw new Error("最多保留 32 个终端标签，请先关闭不用的标签。");
  const names = new Set(group.tabs().map(tab => tab.title()));
  let index = 1; while (names.has(`终端 ${index}`)) index++;
  const tab = makeTab(threadId, cwd, `终端 ${index}`, options);
  group.setTabs(tabs => [...tabs, tab]); group.setActiveId(tab.id);
  return tab;
}
export function setTerminalGroupFontSize(group: TerminalGroup, fontSize: number): void {
  const next = clampFontSize(fontSize);
  for (const tab of untrack(() => group.tabs())) {
    if (tab.disposed || tab.terminal.options.fontSize === next) continue;
    tab.terminal.options.fontSize = next;
    if (!tab.opened || !tab.host.isConnected || !tab.host.clientWidth || !tab.host.clientHeight) continue;
    const size = tab.fit.proposeDimensions();
    if (size && Number.isInteger(size.cols) && Number.isInteger(size.rows) && size.cols > 2 && size.rows > 1) {
      tab.terminal.resize(size.cols, size.rows);
    }
  }
}
/** Remount the same instance; hiding the panel never restarts the shell. */
export function attachTerminal(tab: TerminalTab, parent: HTMLElement): () => void {
  parent.append(tab.host);
  if (!tab.opened) { tab.terminal.open(tab.host); tab.opened = true; }
  const fit = () => {
    if (tab.disposed || document.hidden || !tab.host.isConnected || !tab.host.clientWidth || !tab.host.clientHeight) return;
    const size = tab.fit.proposeDimensions();
    // shortcut: skip clamped 2-column/1-row fits, revisit when FitAddon exposes unclamped dimensions.
    if (!size || !Number.isInteger(size.cols) || !Number.isInteger(size.rows) || size.cols <= 2 || size.rows <= 1) return;
    // shortcut: xterm 5 has no public viewport resync for same-size restores, revisit this hook when upgrading xterm.
    const viewport = (tab.terminal as Terminal & {
      _core?: { viewport?: { _ignoreNextScrollEvent: boolean; syncScrollArea?(immediate: boolean): void } };
    })._core?.viewport;
    if (viewport?.syncScrollArea) viewport._ignoreNextScrollEvent = false;
    tab.terminal.resize(size.cols, size.rows);
    viewport?.syncScrollArea?.(true);
  };
  if (!tab.ready) {
    // Size a new PTY before spawn; on reattach, let the next frame settle the layout.
    fit();
    const channel = new Channel<TerminalEvent>(); tab.channel = channel;
    channel.onmessage = event => {
      if (tab.disposed) return;
      if (event.type === "windowsPty") {
        // ConPTY repaints on resize, and older builds need different wrap handling.
        tab.terminal.options.windowsPty = { backend: "conpty", buildNumber: event.build ?? undefined };
      } else if (event.type === "data") {
        // xterm's streaming decoder preserves UTF-8 characters split across reads.
        tab.terminal.write(Uint8Array.from(event.data), () => {
          if (!tab.disposed && tab.status() !== "exited") void terminalApi.ack(tab.id, event.data.length).catch(error => {
            if (!tab.disposed && tab.status() !== "exited") tab.setError(String(error));
          });
          if (tab.initCommand && !tab.initSent) tab.scheduleInitCommand(150);
        });
      } else if (event.type === "exit") {
        tab.setStatus("exited"); tab.terminal.options.disableStdin = true;
        tab.terminal.write(`\r\n[进程已退出，退出码 ${event.code ?? "未知"}]\r\n`);
      } else tab.setError(event.message);
    };
    tab.ready = tab.startPty(channel).catch(error => {
      if (!tab.disposed) { tab.setError(String(error)); tab.setStatus("error"); tab.terminal.options.disableStdin = true; }
      throw error;
    });
    void tab.ready.catch(() => {});
  }
  let frame = requestAnimationFrame(() => {
    fit();
    if (!document.hidden && !tab.disposed) tab.terminal.focus();
  });
  const scheduleFit = () => { cancelAnimationFrame(frame); frame = requestAnimationFrame(fit); };
  const observer = new ResizeObserver(scheduleFit);
  observer.observe(tab.host);
  document.addEventListener("visibilitychange", scheduleFit);
  window.addEventListener("focus", scheduleFit);
  let detached = false;
  void document.fonts?.ready.then(() => { if (!detached) fit(); });
  return () => {
    detached = true;
    cancelAnimationFrame(frame);
    observer.disconnect();
    document.removeEventListener("visibilitychange", scheduleFit);
    window.removeEventListener("focus", scheduleFit);
    tab.host.remove();
  };
}
export async function closeTerminalTab(group: TerminalGroup, tab: TerminalTab): Promise<void> {
  if (tab.disposed) return;
  tab.disposed = true;
  if (tab.initTimer !== undefined) {
    clearTimeout(tab.initTimer);
    tab.initTimer = undefined;
  }
  const tabs = group.tabs(), index = tabs.indexOf(tab);
  const remaining = tabs.filter(item => !item.disposed);
  group.setTabs(remaining);
  if (!remaining.some(item => item.id === group.activeId())) group.setActiveId(remaining[Math.min(index, remaining.length - 1)]?.id ?? "");
  tab.terminal.dispose(); tab.host.remove();
  if (tab.ready) {
    // Close during spawn must not orphan a process that has not appeared yet.
    try { await tab.ready; } catch { /* create can fail after allocation */ }
    await terminalApi.close(tab.id);
  }
}
export async function closeTerminalGroup(key: string): Promise<void> {
  const group = groups.get(key);
  if (!group) return;
  groups.delete(key);
  await Promise.all([...group.tabs()].map(tab => closeTerminalTab(group, tab)));
}
if (typeof window !== "undefined") window.addEventListener("pagehide", () => {
  for (const group of groups.values()) for (const tab of group.tabs()) void closeTerminalTab(group, tab).catch(() => {});
});
