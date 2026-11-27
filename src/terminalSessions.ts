import { Channel, invoke } from "@tauri-apps/api/core";
import { Terminal, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { createSignal, untrack } from "solid-js";
import {
  classifyTerminalOutboundBytes,
  computeTerminalWriteGuardDelayMs,
  encodeSoftMultilineTerminalChunks,
  TERMINAL_CONTROL_REPLY_GUARD_MS,
  TERMINAL_ESC_GUARD_MS,
  type TerminalKeyStep,
} from "./agyTerminalPrompt";

export type TerminalEvent =
  | { type: "windowsPty"; build: number | null }
  | { type: "data"; data: number[] }
  | { type: "exit"; code: number | null }
  | { type: "error"; message: string };
/** Tests replace only this native IPC boundary, not xterm or the component. */
export const terminalApi = {
  create: (
    id: string,
    threadId: string | null,
    cwd: string,
    cols: number,
    rows: number,
    onEvent: Channel<TerminalEvent>,
    initCommand?: string | null,
    allowResume?: boolean,
  ) =>
    invoke<{ initCommand?: string } | void>("terminal_create", {
      id,
      threadId,
      cwd,
      cols,
      rows,
      onEvent,
      initCommand: initCommand ?? null,
      allowResume: !!allowResume,
    }),
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
  allowResume?: boolean;
};

const clampFontSize = (n?: number) =>
  Math.min(24, Math.max(10, Math.round(n && Number.isFinite(n) ? n : 13)));

const TERMINAL_FONT_FAMILY =
  '"JetBrains Mono Variable", "Cascadia Mono", Consolas, "Microsoft YaHei UI", "PingFang SC", "Noto Sans Mono CJK SC", "Segoe UI Symbol", "Segoe UI Emoji", "Apple Color Emoji", monospace';

export type TerminalThemeMode = "ink-dark" | "ink-light";

export const TERMINAL_THEMES: Record<TerminalThemeMode, ITheme> = {
  "ink-dark": {
    background: "#141821",
    foreground: "#d6dce8",
    cursor: "#86a8ff",
    cursorAccent: "#141821",
    selectionBackground: "#2b3c5c",
    selectionInactiveBackground: "#222b3e",
    black: "#232938",
    red: "#e06c75",
    green: "#6cc895",
    yellow: "#deb968",
    blue: "#7aa6f7",
    magenta: "#b594ec",
    cyan: "#5cc2de",
    white: "#c7d0e0",
    brightBlack: "#6c778f",
    brightRed: "#eb868e",
    brightGreen: "#85d6a9",
    brightYellow: "#e8ca82",
    brightBlue: "#96baff",
    brightMagenta: "#c9aef5",
    brightCyan: "#7bd2ea",
    brightWhite: "#e6ebf5",
  },
  "ink-light": {
    background: "#eff3f9",
    foreground: "#242d3d",
    cursor: "#365fe0",
    cursorAccent: "#eff3f9",
    selectionBackground: "#cddcf7",
    selectionInactiveBackground: "#dce5f4",
    black: "#2a3345",
    red: "#c23d46",
    green: "#197a4f",
    yellow: "#966710",
    blue: "#2d58c8",
    magenta: "#6c4bb8",
    cyan: "#0d7a96",
    white: "#5d697e",
    brightBlack: "#6f7c91",
    brightRed: "#d44d56",
    brightGreen: "#208f5d",
    brightYellow: "#ad7916",
    brightBlue: "#3d6be0",
    brightMagenta: "#7f5ccf",
    brightCyan: "#148fae",
    brightWhite: "#3e4a5e",
  },
};

const TERMINAL_MIN_CONTRAST: Record<TerminalThemeMode, number> = {
  "ink-dark": 3,
  "ink-light": 4.5,
};

function currentTerminalThemeMode(): TerminalThemeMode {
  if (typeof document !== "undefined" && document.documentElement?.dataset?.theme === "ink-light") {
    return "ink-light";
  }
  return "ink-dark";
}

function applyTerminalTheme(terminal: Terminal, mode: TerminalThemeMode): void {
  terminal.options.theme = TERMINAL_THEMES[mode];
  terminal.options.minimumContrastRatio = TERMINAL_MIN_CONTRAST[mode];
}

function syncAllTerminalThemes(): void {
  const mode = currentTerminalThemeMode();
  for (const group of groups.values()) {
    for (const tab of untrack(() => group.tabs())) {
      if (!tab.disposed) applyTerminalTheme(tab.terminal, mode);
    }
  }
}

if (typeof document !== "undefined" && typeof MutationObserver !== "undefined" && document.documentElement) {
  const themeObserver = new MutationObserver(() => syncAllTerminalThemes());
  themeObserver.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["data-theme"],
  });
}

function normalizeInitCommand(command?: string): string {
  return (command ?? "").replace(/\r\n/g, "\n").replace(/\n+$/, "");
}

function makeTab(threadId: string | null, cwd: string, name: string, options?: CreateTerminalTabOptions) {
  // A stable label keeps shell OSC title/progress updates out of the tab bar.
  const title = () => name;
  const [status, setStatus] = createSignal<"starting" | "running" | "exited" | "error">("starting");
  const [error, setError] = createSignal("");
  const themeMode = currentTerminalThemeMode();
  const terminal = new Terminal({
    cursorBlink: true,
    cursorStyle: "block",
    cursorInactiveStyle: "outline",
    fontSize: clampFontSize(options?.fontSize),
    fontWeight: "400",
    fontWeightBold: "600",
    lineHeight: 1.12,
    letterSpacing: 0,
    fontFamily: TERMINAL_FONT_FAMILY,
    customGlyphs: true,
    rescaleOverlappingGlyphs: true,
    drawBoldTextInBrightColors: false,
    macOptionIsMeta: true,
    scrollback: 5000,
    screenReaderMode: false,
    minimumContrastRatio: TERMINAL_MIN_CONTRAST[themeMode],
    // Apply before open(): neither the initial frame nor retained tabs inherit stale theme colors.
    theme: TERMINAL_THEMES[themeMode],
  });
  const fit = new FitAddon(); terminal.loadAddon(fit);
  let ptySize: { cols: number; rows: number } | undefined;
  let resizePromise: Promise<void> | undefined;
  let inputTx: Promise<void> = Promise.resolve();
  let compGen = 0;
  let lastUserDataCompGen = 0;
  const normalizedInit = normalizeInitCommand(options?.initCommand);
  const tab = {
    id: crypto.randomUUID(), threadId, cwd, terminal, fit, host: document.createElement("div"),
    title, status, setStatus, error, setError,
    initCommand: normalizedInit,
    effectiveInitCommand: normalizedInit,
    allowResume: !!options?.allowResume,
    initSent: false,
    initTimer: undefined as ReturnType<typeof setTimeout> | undefined,
    opened: false, disposed: false, ready: undefined as Promise<void> | undefined,
    channel: undefined as Channel<TerminalEvent> | undefined, input: Promise.resolve(), queuedInput: 0,
    composing: false,
    deferredFit: false,
    lastOutputAt: 0,
    lastResizeAt: 0,
    lastCancelAt: 0,
    lastControlReplyAt: 0,
    markCancelSent() {
      tab.lastCancelAt = Date.now();
    },
    getInputDiagnostics() {
      return {
        now: Date.now(),
        lastOutputAt: tab.lastOutputAt,
        lastResizeAt: tab.lastResizeAt,
        lastCancelAt: tab.lastCancelAt,
        lastControlReplyAt: tab.lastControlReplyAt,
        isConpty: tab.terminal.options.windowsPty?.backend === "conpty",
        sendFocusMode: !!tab.terminal.modes.sendFocusMode,
        bracketedPasteMode: !!tab.terminal.modes.bracketedPasteMode,
      };
    },
    runInputTx<T>(task: () => Promise<T>): Promise<T> {
      const next = inputTx.then(task, task);
      inputTx = next.then(() => undefined, () => undefined);
      return next;
    },
    async awaitWriteGuard(): Promise<void> {
      try {
        await tab.ready;
      } catch {
        return;
      }
      if (tab.disposed) return;
      if (resizePromise) {
        await Promise.race([
          resizePromise,
          new Promise<void>((resolve) => window.setTimeout(resolve, 200)),
        ]);
      }
      if (tab.disposed) return;
      const delayMs = computeTerminalWriteGuardDelayMs({
        now: Date.now(),
        lastCancelAt: tab.lastCancelAt,
        lastResizeAt: tab.lastResizeAt,
        lastControlReplyAt: tab.lastControlReplyAt,
      });
      if (delayMs > 0) {
        await new Promise<void>((resolve) => window.setTimeout(resolve, delayMs));
      }
    },
    beginComposition() {
      if (tab.disposed) return 0;
      tab.composing = true;
      compGen += 1;
      return compGen;
    },
    endComposition(gen: number, committed: string, onAfterFinish: () => void) {
      window.setTimeout(() => {
        if (tab.disposed) return;
        if (gen === compGen || gen === 0) {
          tab.composing = false;
          if (tab.deferredFit) {
            tab.deferredFit = false;
            onAfterFinish();
          }
        }
        if (
          gen > 0 &&
          committed.length > 0 &&
          lastUserDataCompGen < gen &&
          tab.status() === "running" &&
          !tab.terminal.options.disableStdin
        ) {
          send(new TextEncoder().encode(committed));
          if (tab.terminal.textarea && tab.terminal.textarea.value === committed) {
            tab.terminal.textarea.value = "";
          }
        }
      }, 0);
    },
    sendBytes: (_data: Uint8Array) => {},
    scheduleInitCommand(delayMs: number) {
      if (!tab.effectiveInitCommand || tab.initSent || tab.disposed) return;
      if (tab.initTimer !== undefined) clearTimeout(tab.initTimer);
      tab.initTimer = setTimeout(() => {
        tab.initTimer = undefined;
        if (tab.disposed || tab.initSent || tab.status() !== "running") return;
        tab.initSent = true;
        const payload = tab.effectiveInitCommand.replace(/\n/g, "\r") + "\r";
        tab.sendBytes(new TextEncoder().encode(payload));
      }, delayMs);
    },
    async startPty(channel: Channel<TerminalEvent>): Promise<void> {
      const { cols, rows } = terminal;
      const created = await terminalApi.create(
        tab.id,
        threadId,
        cwd,
        cols,
        rows,
        channel,
        tab.initCommand || null,
        tab.allowResume,
      );
      if (tab.disposed || terminal.options.disableStdin) return;
      if (created && typeof created.initCommand === "string") {
        tab.effectiveInitCommand = normalizeInitCommand(created.initCommand);
      }
      ptySize = { cols, rows };
      setStatus("running");
      // Don't gate input on resize: ConPTY can wait for a cursor reply.
      void syncPtySize();
      tab.scheduleInitCommand(600);
    },
  };
  function syncPtySize(): Promise<void> {
    if (resizePromise) return resizePromise;
    resizePromise = (async () => {
      // Native resize calls can complete out of order; only send the latest size after each one finishes.
      while (ptySize && !tab.disposed && !terminal.options.disableStdin) {
        const { cols, rows } = terminal;
        if (cols === ptySize.cols && rows === ptySize.rows) break;
        try {
          await terminalApi.resize(tab.id, cols, rows);
          ptySize = { cols, rows };
          tab.lastResizeAt = Date.now();
        } catch (error) {
          if (!tab.disposed && !terminal.options.disableStdin) setError(String(error));
          if (cols === terminal.cols && rows === terminal.rows) break;
        }
      }
    })().finally(() => {
      resizePromise = undefined;
    });
    return resizePromise;
  }
  tab.host.className = "workspace-terminal-instance";
  const send = (data: Uint8Array) => {
    if (tab.disposed || status() === "exited" || status() === "error") return;
    if (tab.queuedInput + data.length > 1024 * 1024) { setError("输入队列已满，请分段粘贴。"); return; }
    const kind = classifyTerminalOutboundBytes(data);
    if (kind === "bareEsc") tab.lastCancelAt = Date.now();
    else if (kind === "controlReply") tab.lastControlReplyAt = Date.now();
    else if (kind === "userText") lastUserDataCompGen = compGen;
    tab.queuedInput += data.length;
    // Serialize IPC to retain keystroke/paste order even when the native writer blocks.
    tab.input = tab.input.then(async () => {
      await tab.ready;
      if (kind === "userText" && !tab.disposed) {
        const now = Date.now();
        const escWait =
          tab.lastCancelAt > 0
            ? Math.min(120, Math.max(0, tab.lastCancelAt + Math.min(120, TERMINAL_ESC_GUARD_MS) - now))
            : 0;
        const replyWait =
          tab.lastControlReplyAt > 0
            ? Math.min(35, Math.max(0, tab.lastControlReplyAt + Math.min(35, TERMINAL_CONTROL_REPLY_GUARD_MS) - now))
            : 0;
        const inlineWait = Math.max(escWait, replyWait);
        if (inlineWait > 0) {
          await new Promise<void>((resolve) => window.setTimeout(resolve, inlineWait));
        }
      }
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
    // Never intercept IME composition keys (e.g. CJK input Enter/Backspace/candidates).
    if (event.isComposing || event.keyCode === 229) return true;
    const key = event.key.toLowerCase();
    const copyMod = !event.altKey && ((event.ctrlKey && !event.metaKey) || (event.metaKey && !event.ctrlKey && !event.shiftKey));
    if (copyMod && key === "c" && terminal.hasSelection() && navigator.clipboard) {
      if (event.type === "keydown") {
        event.preventDefault();
        void navigator.clipboard.writeText(terminal.getSelection())
          .then(() => terminal.clearSelection())
          .catch(error => setError(String(error)));
      }
      return false;
    }
    const pasteMod = !event.altKey && ((event.ctrlKey && !event.metaKey) || (event.metaKey && !event.ctrlKey && !event.shiftKey));
    if (pasteMod && key === "v" && navigator.clipboard) {
      if (event.type === "keydown") {
        event.preventDefault();
        const plainCtrlV = event.ctrlKey && !event.shiftKey && !event.metaKey;
        void navigator.clipboard.readText()
          .then(text => {
            if (tab.disposed) return;
            if (text) terminal.paste(text);
            else if (plainCtrlV) send(new Uint8Array([0x16]));
          })
          .catch(error => setError(String(error)));
      }
      return false;
    }
    // Shift+Enter inserts a soft newline (ESC + CR) in TUI inputs like agy / claude / codex.
    if (!event.ctrlKey && !event.altKey && !event.metaKey && event.shiftKey && event.key === "Enter") {
      if (event.type === "keydown") {
        event.preventDefault();
        send(new Uint8Array([0x1b, 0x0d]));
      }
      return false;
    }
    // Ctrl+Backspace deletes the previous word (Ctrl+W) in TUI/shell prompts.
    if (event.ctrlKey && !event.shiftKey && !event.altKey && !event.metaKey && event.key === "Backspace") {
      if (event.type === "keydown") {
        event.preventDefault();
        send(new Uint8Array([0x17]));
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
function fitTerminal(tab: TerminalTab): void {
  if (tab.disposed || !tab.opened || document.hidden || !tab.host.isConnected || !tab.host.clientWidth || !tab.host.clientHeight) return;
  if (tab.composing) {
    tab.deferredFit = true;
    return;
  }
  const size = tab.fit.proposeDimensions();
  // shortcut: skip clamped 2-column/1-row fits, revisit when FitAddon exposes unclamped dimensions.
  if (!size || !Number.isInteger(size.cols) || !Number.isInteger(size.rows) || size.cols <= 2 || size.rows <= 1) return;
  const { terminal } = tab;
  // shortcut: xterm 5 has no public viewport resync for same-size restores, revisit this hook when upgrading xterm.
  const viewport = (terminal as Terminal & {
    _core?: { viewport?: { _ignoreNextScrollEvent: boolean; syncScrollArea?(immediate: boolean): void } };
  })._core?.viewport;
  if (viewport?.syncScrollArea) viewport._ignoreNextScrollEvent = false;
  if (size.cols !== terminal.cols || size.rows !== terminal.rows) {
    const buffer = terminal.buffer.active;
    const top = buffer.viewportY;
    const following = top >= buffer.baseY;
    const cursor = buffer.baseY + buffer.cursorY;
    // Resizing can shift the viewport even when the line being read is still in scrollback.
    const anchor = following ? undefined : terminal.registerMarker(top - cursor);
    // If wider reflow removes a continuation row, fall back to the start of its wrapped line.
    let start = top;
    if (anchor && size.cols > terminal.cols) {
      while (start > 0 && buffer.getLine(start)?.isWrapped) start--;
    }
    const wrapAnchor = start === top ? undefined : terminal.registerMarker(start - cursor);
    terminal.resize(size.cols, size.rows);
    if (following) terminal.scrollToBottom();
    else if (anchor) terminal.scrollToLine(Math.max(0, anchor.isDisposed ? wrapAnchor?.line ?? 0 : anchor.line));
    anchor?.dispose();
    wrapAnchor?.dispose();
  }
  viewport?.syncScrollArea?.(true);
}

export function setTerminalGroupFontSize(group: TerminalGroup, fontSize: number): void {
  const next = clampFontSize(fontSize);
  for (const tab of untrack(() => group.tabs())) {
    if (tab.disposed || tab.terminal.options.fontSize === next) continue;
    tab.terminal.options.fontSize = next;
    fitTerminal(tab);
  }
}
/** Remount the same instance; hiding the panel never restarts the shell. */
export function attachTerminal(tab: TerminalTab, parent: HTMLElement, autoFocus = true): () => void {
  parent.append(tab.host);
  applyTerminalTheme(tab.terminal, currentTerminalThemeMode());
  if (!tab.opened) {
    tab.terminal.open(tab.host);
    tab.opened = true;
    const textarea = tab.terminal.textarea;
    if (textarea) {
      let activeCompGen = 0;
      const onCompositionStart = () => {
        activeCompGen = tab.beginComposition();
      };
      const onCompositionEnd = (event: CompositionEvent) => {
        const gen = activeCompGen;
        activeCompGen = 0;
        const committed = typeof event.data === "string" ? event.data : "";
        tab.endComposition(gen, committed, () => fitTerminal(tab));
      };
      textarea.addEventListener("compositionstart", onCompositionStart);
      textarea.addEventListener("compositionend", onCompositionEnd);
    }
  }
  const resetHorizontalScroll = () => {
    if (parent.scrollLeft !== 0) parent.scrollLeft = 0;
    if (tab.host.scrollLeft !== 0) tab.host.scrollLeft = 0;
    const xtermEl = tab.host.querySelector<HTMLElement>(".xterm");
    if (xtermEl && xtermEl.scrollLeft !== 0) xtermEl.scrollLeft = 0;
    const viewportEl = tab.host.querySelector<HTMLElement>(".xterm-viewport");
    if (viewportEl && viewportEl.scrollLeft !== 0) viewportEl.scrollLeft = 0;
  };
  const fit = () => {
    resetHorizontalScroll();
    fitTerminal(tab);
    resetHorizontalScroll();
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
        tab.lastOutputAt = Date.now();
        // xterm's streaming decoder preserves UTF-8 characters split across reads.
        tab.terminal.write(Uint8Array.from(event.data), () => {
          if (!tab.disposed && tab.status() !== "exited") void terminalApi.ack(tab.id, event.data.length).catch(error => {
            if (!tab.disposed && tab.status() !== "exited") tab.setError(String(error));
          });
          if (tab.effectiveInitCommand && !tab.initSent) tab.scheduleInitCommand(150);
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
    if (autoFocus && !document.hidden && !tab.disposed) tab.terminal.focus();
  });
  const scheduleFit = () => { cancelAnimationFrame(frame); frame = requestAnimationFrame(fit); };
  const observer = new ResizeObserver(scheduleFit);
  observer.observe(tab.host);
  parent.addEventListener("scroll", resetHorizontalScroll);
  tab.host.addEventListener("scroll", resetHorizontalScroll);
  document.addEventListener("visibilitychange", scheduleFit);
  window.addEventListener("focus", scheduleFit);
  let detached = false;
  void document.fonts?.ready.then(() => {
    if (detached || tab.disposed) return;
    const core = (tab.terminal as Terminal & {
      _core?: { _charSizeService?: { measure?: () => void } };
    })._core;
    core?._charSizeService?.measure?.();
    fit();
  });
  return () => {
    detached = true;
    cancelAnimationFrame(frame);
    observer.disconnect();
    parent.removeEventListener("scroll", resetHorizontalScroll);
    tab.host.removeEventListener("scroll", resetHorizontalScroll);
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

/**
 * 向运行中的终端发送一段文本：
 * - 非 Windows ConPTY 且终端开启 bracketed paste（如 Unix PTY）时，走原生 `terminal.paste`；
 * - Windows ConPTY（ReadConsoleInputW 会把 bracketed paste 内的 CR 误判为直接回车提交）
 *   或未开启 bracketed paste 时，将换行编码为独立原子 `[0x1b, 0x0d]` 软换行分块写入，
 *   避免多行与附件路径被首行换行提前提交；
 * - 可选在写入队列排空并稍作缓冲后自动发送回车提交。
 */
export async function sendTerminalInput(
  tab: TerminalTab,
  text: string,
  options?: { submit?: boolean; useTerminalPaste?: boolean; skipTx?: boolean },
): Promise<boolean> {
  const execute = async () => {
    const ready = () => !tab.disposed && tab.status() === "running" && !tab.terminal.options.disableStdin && !tab.error();
    if (!ready()) return false;
    if (!options?.skipTx && tab.awaitWriteGuard) {
      await tab.awaitWriteGuard();
      if (!ready()) return false;
    }
    const normalized = text.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
    if (normalized.length > 0) {
      const isWindowsConpty = tab.terminal.options.windowsPty?.backend === "conpty";
      if (options?.useTerminalPaste || (!isWindowsConpty && tab.terminal.modes.bracketedPasteMode)) {
        tab.terminal.paste(normalized);
      } else {
        for (const chunk of encodeSoftMultilineTerminalChunks(normalized)) {
          tab.sendBytes(chunk);
        }
      }
    }
    await tab.input;
    if (!ready()) return false;
    if (options?.submit !== false) {
      const settleDelayMs = Math.min(200, 80 + Math.floor(normalized.length / 4000) * 20);
      await new Promise<void>((resolve) => window.setTimeout(resolve, settleDelayMs));
      if (!ready()) return false;
      tab.sendBytes(new Uint8Array([0x0d]));
      await tab.input;
    }
    return ready();
  };
  if (options?.skipTx || !tab.runInputTx) {
    return execute();
  }
  return tab.runInputTx(execute);
}

/** 向运行中的终端发送 Esc（0x1b）以取消当前 CLI 轮次。 */
export function sendTerminalCancel(tab: TerminalTab): boolean {
  if (tab.disposed || tab.status() !== "running" || tab.terminal.options.disableStdin) {
    return false;
  }
  tab.markCancelSent?.();
  tab.sendBytes(new Uint8Array([0x1b]));
  return true;
}

/** 按序向运行中的终端发送按键步骤序列（用于驱动 CLI 交互式提问等 TUI 状态机）。 */
export async function sendTerminalKeySteps(
  tab: TerminalTab,
  steps: TerminalKeyStep[],
): Promise<boolean> {
  return tab.runInputTx(async () => {
    const ready = () =>
      !tab.disposed && tab.status() === "running" && !tab.terminal.options.disableStdin && !tab.error();
    if (!ready()) return false;
    await tab.awaitWriteGuard();
    if (!ready()) return false;
    for (let i = 0; i < steps.length; i++) {
      const step = steps[i];
      if (!ready()) return false;
      if (step.kind === "bytes") {
        if (step.bytes.length > 0) {
          tab.sendBytes(step.bytes);
          await tab.input;
        }
      } else {
        const ok = await sendTerminalInput(tab, step.text, {
          submit: false,
          useTerminalPaste: false,
          skipTx: true,
        });
        if (!ok) return false;
      }
      if (!ready()) return false;
      const delayMs = step.delayAfterMs ?? 0;
      if (delayMs > 0 && i < steps.length - 1) {
        await new Promise<void>((resolve) => window.setTimeout(resolve, delayMs));
        if (!ready()) return false;
      }
    }
    return ready();
  });
}

/** 等待 CLI 进入空闲后达到输出静默稳态，再允许自动投递首条提示词。 */
export async function waitForTerminalIdleSettle(
  tab: TerminalTab,
  options?: { minDelayMs?: number; quietMs?: number; maxWaitMs?: number; isCancelled?: () => boolean },
): Promise<boolean> {
  const minDelayMs = options?.minDelayMs ?? 320;
  const quietMs = options?.quietMs ?? 180;
  const maxWaitMs = options?.maxWaitMs ?? 1200;
  const isStopped = () =>
    !!options?.isCancelled?.() || tab.disposed || tab.status() !== "running" || !!tab.terminal.options.disableStdin;
  if (isStopped()) return false;
  try {
    await tab.ready;
  } catch {
    return false;
  }
  const start = Date.now();
  while (!isStopped()) {
    const now = Date.now();
    const elapsed = now - start;
    const quietFor = tab.lastOutputAt > 0 ? now - tab.lastOutputAt : elapsed;
    if (elapsed >= minDelayMs && (quietFor >= quietMs || elapsed >= maxWaitMs)) {
      return true;
    }
    await new Promise<void>((resolve) => window.setTimeout(resolve, 40));
  }
  return false;
}
if (typeof window !== "undefined") window.addEventListener("pagehide", () => {
  for (const group of groups.values()) for (const tab of group.tabs()) void closeTerminalTab(group, tab).catch(() => {});
});
