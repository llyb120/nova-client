import { Channel, invoke } from "@tauri-apps/api/core";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { createSignal } from "solid-js";

export type TerminalEvent =
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
function makeTab(threadId: string | null, cwd: string, name: string) {
  // A stable label keeps shell OSC title/progress updates out of the tab bar.
  const title = () => name;
  const [status, setStatus] = createSignal<"starting" | "running" | "exited" | "error">("starting");
  const [error, setError] = createSignal("");
  const terminal = new Terminal({ cursorBlink: true, fontSize: 13, lineHeight: 1.2,
    fontFamily: '"JetBrains Mono Variable", Consolas, monospace', scrollback: 5000, screenReaderMode: true,
    // Apply before open(): neither the initial frame nor retained tabs inherit the app theme.
    theme: { background: "#000000", foreground: "#d4d8df", cursor: "#d4d8df", cursorAccent: "#000000", selectionBackground: "#344d70" } });
  const fit = new FitAddon(); terminal.loadAddon(fit);
  const tab = {
    id: crypto.randomUUID(), threadId, cwd, terminal, fit, host: document.createElement("div"),
    title, status, setStatus, error, setError,
    opened: false, disposed: false, ready: undefined as Promise<void> | undefined,
    channel: undefined as Channel<TerminalEvent> | undefined, input: Promise.resolve(), queuedInput: 0,
  };
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
  terminal.onData(data => send(new TextEncoder().encode(data)));
  terminal.onBinary(data => send(Uint8Array.from(data, char => char.charCodeAt(0))));
  terminal.onResize(({ cols, rows }) => {
    void tab.ready?.then(() => {
      if (!tab.disposed && status() === "running") return terminalApi.resize(tab.id, cols, rows);
    }).catch(error => { if (!tab.disposed && status() !== "exited") setError(String(error)); });
  });
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
export function createTerminalTab(group: TerminalGroup, threadId: string | null, cwd: string): TerminalTab {
  if ([...groups.values()].reduce((n, value) => n + value.tabs().length, 0) >= 32)
    throw new Error("最多保留 32 个终端标签，请先关闭不用的标签。");
  const names = new Set(group.tabs().map(tab => tab.title()));
  let index = 1; while (names.has(`终端 ${index}`)) index++;
  const tab = makeTab(threadId, cwd, `终端 ${index}`);
  group.setTabs(tabs => [...tabs, tab]); group.setActiveId(tab.id);
  return tab;
}
/** Remount the same instance; hiding the panel never restarts the shell. */
export function attachTerminal(tab: TerminalTab, parent: HTMLElement): () => void {
  parent.append(tab.host);
  if (!tab.opened) { tab.terminal.open(tab.host); tab.opened = true; }
  const fit = () => {
    if (!tab.disposed && tab.host.isConnected && parent.clientWidth > 0 && parent.clientHeight > 0) tab.fit.fit();
  };
  fit();
  if (!tab.ready) {
    const channel = new Channel<TerminalEvent>(); tab.channel = channel;
    channel.onmessage = event => {
      if (tab.disposed) return;
      if (event.type === "data") {
        // xterm's streaming decoder preserves UTF-8 characters split across reads.
        tab.terminal.write(Uint8Array.from(event.data), () => {
          if (!tab.disposed && tab.status() !== "exited") void terminalApi.ack(tab.id, event.data.length).catch(error => {
            if (!tab.disposed && tab.status() !== "exited") tab.setError(String(error));
          });
        });
      } else if (event.type === "exit") {
        tab.setStatus("exited"); tab.terminal.options.disableStdin = true;
        tab.terminal.write(`\r\n[进程已退出，退出码 ${event.code ?? "未知"}]\r\n`);
      } else tab.setError(event.message);
    };
    tab.ready = terminalApi.create(tab.id, tab.threadId, tab.cwd, tab.terminal.cols, tab.terminal.rows, channel).then(() => {
      if (tab.disposed || tab.status() === "exited") return;
      tab.setStatus("running");
      // Creation, not resize, gates input. ConPTY may need xterm's cursor
      // response to finish a resize; putting resize in ready deadlocks input.
      void terminalApi.resize(tab.id, tab.terminal.cols, tab.terminal.rows).catch(error => {
        if (!tab.disposed && tab.status() !== "exited") tab.setError(String(error));
      });
    }).catch(error => {
      if (!tab.disposed) { tab.setError(String(error)); tab.setStatus("error"); tab.terminal.options.disableStdin = true; }
      throw error;
    });
    void tab.ready.catch(() => {});
  }
  let frame = requestAnimationFrame(() => { fit(); tab.terminal.focus(); });
  const observer = new ResizeObserver(() => { cancelAnimationFrame(frame); frame = requestAnimationFrame(fit); });
  observer.observe(parent);
  let detached = false;
  void document.fonts?.ready.then(() => { if (!detached) fit(); });
  return () => { detached = true; cancelAnimationFrame(frame); observer.disconnect(); tab.host.remove(); };
}
export async function closeTerminalTab(group: TerminalGroup, tab: TerminalTab): Promise<void> {
  if (tab.disposed) return;
  tab.disposed = true;
  const tabs = group.tabs(), index = tabs.indexOf(tab);
  group.setTabs(tabs.filter(item => item !== tab));
  if (group.activeId() === tab.id) group.setActiveId(group.tabs()[Math.min(index, group.tabs().length - 1)]?.id ?? "");
  tab.terminal.dispose(); tab.host.remove();
  if (tab.ready) {
    // Close during spawn must not orphan a process that has not appeared yet.
    try { await tab.ready; } catch { /* create can fail after allocation */ }
    await terminalApi.close(tab.id);
  }
}
if (typeof window !== "undefined") window.addEventListener("pagehide", () => {
  for (const group of groups.values()) for (const tab of group.tabs()) void closeTerminalTab(group, tab).catch(() => {});
});
