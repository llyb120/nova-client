import { onCleanup, onMount } from "solid-js";
import { api } from "./ipc";
import { ALL_AGENT_KINDS, state } from "./store";
import type { AgentKind, SessionShortcut, SessionShortcutAction } from "./types";

const MODIFIER_KEYS = new Set(["Control", "Shift", "Alt", "Meta", "OS"]);

/** 仅暂停页面内快捷键；普通弹窗不能禁用前台 Meteor。 */
let shortcutCaptureActive = false;
let shortcutRecordingActive = false;

export function setShortcutCaptureActive(active: boolean) {
  shortcutCaptureActive = active;
}

/** 只有真正录制组合键时才暂停原生 Meteor；false 也用于页面重载后清除残留状态。 */
export function setShortcutRecordingActive(active: boolean) {
  if (active && shortcutRecordingActive) return;
  shortcutRecordingActive = active;
  if (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window) {
    void api.setMeteorShortcutRecording(active).catch((error) => console.error("Meteor shortcut capture", error));
  }
}

export function newSessionShortcutId(): string {
  return typeof globalThis.crypto?.randomUUID === "function"
    ? globalThis.crypto.randomUUID()
    : `sc-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

export const DEFAULT_TERMINAL_SHORTCUT: SessionShortcut = {
  id: "default-toggle-terminal", keys: "Ctrl+`", action: "toggleTerminal", target: "",
};
/** 默认终端按键；已配置同动作或占用同按键时尊重用户设置。 */
export function withDefaultSessionShortcuts(
  shortcuts: SessionShortcut[],
): SessionShortcut[] {
  return [
    ...shortcuts,
    ...(!shortcuts.some(item => item.action === "toggleTerminal" ||
      ["ctrl+`", "ctrl+~", "ctrl+shift+~", "ctrl+shift+`"].includes(item.keys.trim().toLowerCase()))
      ? [{ ...DEFAULT_TERMINAL_SHORTCUT }] : []),
  ];
}

export function encodeModelTarget(agentKind: AgentKind, model: string): string {
  return `${agentKind}:${encodeURIComponent(model)}`;
}

/** 漫游目录：`roam:<token>:<folder>`，folder 经 encodeURIComponent。 */
export function encodeRoamTarget(peerToken: string, folder: string): string {
  return `roam:${encodeURIComponent(peerToken)}:${encodeURIComponent(folder)}`;
}

export function parseRoamTarget(
  target: string,
): { peerToken: string; folder: string } | null {
  if (!target.startsWith("roam:")) return null;
  const rest = target.slice("roam:".length);
  const i = rest.indexOf(":");
  if (i <= 0) return null;
  try {
    const peerToken = decodeURIComponent(rest.slice(0, i));
    const folder = decodeURIComponent(rest.slice(i + 1));
    if (!peerToken || !folder) return null;
    return { peerToken, folder };
  } catch {
    return null;
  }
}

export function parseModelTarget(
  target: string,
): { agentKind: AgentKind; model: string } | null {
  const i = target.indexOf(":");
  if (i <= 0) return null;
  const agentKind = target.slice(0, i) as AgentKind;
  if (!ALL_AGENT_KINDS.includes(agentKind)) return null;
  const raw = target.slice(i + 1);
  if (!raw) return null;
  try {
    return { agentKind, model: decodeURIComponent(raw) };
  } catch {
    return { agentKind, model: raw };
  }
}

/** 将 KeyboardEvent 规范为 Ctrl+Alt+Shift+Meta+Key；仅修饰键时返回 null。 */
export function formatShortcutKeys(event: KeyboardEvent): string | null {
  if (event.isComposing) return null;
  if (MODIFIER_KEYS.has(event.key)) return null;
  const parts: string[] = [];
  if (event.ctrlKey) parts.push("Ctrl");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");
  if (event.metaKey) parts.push("Meta");
  let label = event.key;
  if (label === " ") label = "Space";
  else if (label.length === 1) label = label.toUpperCase();
  parts.push(label);
  return parts.join("+");
}

export function shortcutHasModifier(keys: string): boolean {
  return /(?:^|\+)(Ctrl|Alt|Meta)(?:\+|$)/i.test(keys);
}

function normalizeShortcutKeys(keys: string): string {
  return keys.trim().toLowerCase();
}

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
}

export function findSessionShortcut(
  event: KeyboardEvent,
  shortcuts: SessionShortcut[],
): SessionShortcut | null {
  const formatted = formatShortcutKeys(event);
  if (!formatted) return null;
  const needle = normalizeShortcutKeys(formatted);
  const exact = shortcuts.find((item) => normalizeShortcutKeys(item.keys) === needle);
  if (exact) return exact;
  // The ~ keycap is commonly used to describe Ctrl+Backquote, with or without Shift.
  if (event.ctrlKey && !event.altKey && !event.metaKey &&
      (event.code === "Backquote" || event.key === "`" || event.key === "~")) {
    return shortcuts.find(item => item.id === DEFAULT_TERMINAL_SHORTCUT.id &&
      item.action === "toggleTerminal" && item.keys === DEFAULT_TERMINAL_SHORTCUT.keys) ?? null;
  }
  return null;
}

/** 在组件 setup 阶段调用：挂全局 capture keydown，按 allowedActions 执行回调。 */
export function mountSessionShortcuts(options: {
  allowedActions: readonly SessionShortcutAction[];
  onSelectProject?: (
    path: string,
    roam?: { peerToken: string; folder: string } | null,
  ) => void;
  onSelectModel?: (agentKind: AgentKind, model: string) => void;
  onNewSession?: () => void;
  /** 循环打开普通模式下有未读轮次的会话。 */
  onOpenUnread?: () => void;
  onToggleTerminal?: () => void;
  /**
   * 返回 true 表示已插入到当前会话输入框。
   * mayFocus=true 表示允许先聚焦输入框再插入（焦点在别处时的全局触发）；
   * false 表示仅在输入框已聚焦时处理（避免抢走其它可编辑区的无修饰键输入）。
   */
  onInsertText?: (text: string, mayFocus: boolean) => boolean;
}): void {
  const allowed = new Set(options.allowedActions);
  const onKeyDown = (event: KeyboardEvent) => {
    if (shortcutCaptureActive || shortcutRecordingActive || event.defaultPrevented || event.isComposing || event.repeat) {
      return;
    }
    const shortcuts = withDefaultSessionShortcuts(state.settings?.sessionShortcuts ?? []);
    if (shortcuts.length === 0) return;
    const hit = findSessionShortcut(event, shortcuts);
    if (!hit?.keys) return;
    if (!allowed.has(hit.action)) return;
    if (event.target instanceof HTMLElement && event.target.closest(".workspace-terminal") &&
        !["toggleTerminal", "openUnread", "newSession"].includes(hit.action)) return;
    if (
      hit.action !== "newSession" &&
      hit.action !== "openUnread" &&
      hit.action !== "toggleTerminal" &&
      !hit.target
    ) {
      return;
    }
    // insertText 允许无修饰键；其它动作在可编辑区需 Ctrl/Alt/Meta。
    if (
      hit.action !== "insertText" &&
      isEditableTarget(event.target) &&
      !shortcutHasModifier(hit.keys)
    ) {
      return;
    }

    if (hit.action === "insertText") {
      // 任意位置可触发：焦点不在会话输入框时先聚焦再插入。
      // 无修饰键的快捷键只作用于已聚焦的输入框，避免抢走其它可编辑区的正常输入。
      const mayFocus = !(isEditableTarget(event.target) && !shortcutHasModifier(hit.keys));
      const handled = options.onInsertText?.(hit.target, mayFocus) ?? false;
      if (!handled) return;
      event.preventDefault();
      event.stopPropagation();
      return;
    }
    if (hit.action === "newSession") {
      event.preventDefault();
      event.stopPropagation();
      options.onNewSession?.();
      return;
    }
    if (hit.action === "toggleTerminal") {
      event.preventDefault(); event.stopPropagation(); options.onToggleTerminal?.(); return;
    }
    if (hit.action === "openUnread") {
      event.preventDefault();
      event.stopPropagation();
      options.onOpenUnread?.();
      return;
    }
    if (hit.action === "selectProject") {
      event.preventDefault();
      event.stopPropagation();
      const roam = parseRoamTarget(hit.target);
      if (roam) {
        options.onSelectProject?.(roam.folder, roam);
        return;
      }
      options.onSelectProject?.(hit.target, null);
      return;
    }
    if (hit.action === "selectModel") {
      const parsed = parseModelTarget(hit.target);
      if (!parsed) return;
      event.preventDefault();
      event.stopPropagation();
      options.onSelectModel?.(parsed.agentKind, parsed.model);
    }
  };

  onMount(() => {
    window.addEventListener("keydown", onKeyDown, true);
    onCleanup(() => window.removeEventListener("keydown", onKeyDown, true));
  });
}
