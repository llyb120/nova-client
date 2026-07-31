import { onCleanup, onMount } from "solid-js";
import { ALL_AGENT_KINDS, state } from "./store";
import type { AgentKind, SessionShortcut, SessionShortcutAction } from "./types";

const MODIFIER_KEYS = new Set(["Control", "Shift", "Alt", "Meta", "OS"]);

/** Settings 正在录制快捷键时置位，避免运行时监听抢先处理。 */
let shortcutCaptureActive = false;

export function setShortcutCaptureActive(active: boolean) {
  shortcutCaptureActive = active;
}

export function newSessionShortcutId(): string {
  return typeof globalThis.crypto?.randomUUID === "function"
    ? globalThis.crypto.randomUUID()
    : `sc-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

export function encodeModelTarget(agentKind: AgentKind, model: string): string {
  return `${agentKind}:${encodeURIComponent(model)}`;
}

/** 额度租借模型：与 ModelPicker 的 quota: 编码一致。 */
export function encodeQuotaModelTarget(
  peerToken: string,
  agentKind: AgentKind,
  model: string,
): string {
  return `quota:${encodeURIComponent(peerToken)}:${agentKind}:${encodeURIComponent(model)}`;
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
): { agentKind: AgentKind; model: string; peerToken?: string } | null {
  if (target.startsWith("quota:")) {
    const parts = target.split(":");
    if (parts.length !== 4) return null;
    const agentKind = parts[2] as AgentKind;
    if (!ALL_AGENT_KINDS.includes(agentKind)) return null;
    try {
      const peerToken = decodeURIComponent(parts[1]);
      const model = decodeURIComponent(parts[3]);
      if (!peerToken || !model) return null;
      return { peerToken, agentKind, model };
    } catch {
      return null;
    }
  }
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
  return shortcuts.find((item) => normalizeShortcutKeys(item.keys) === needle) ?? null;
}

/** 在组件 setup 阶段调用：挂全局 capture keydown，按 allowedActions 执行回调。 */
export function mountSessionShortcuts(options: {
  allowedActions: readonly SessionShortcutAction[];
  onSelectProject?: (
    path: string,
    roam?: { peerToken: string; folder: string } | null,
  ) => void;
  onSelectModel?: (
    agentKind: AgentKind,
    model: string,
    quotaPeer?: { token: string; name: string } | null,
  ) => void;
  onNewSession?: () => void;
  /** 返回 true 表示已插入到当前会话输入框。 */
  onInsertText?: (text: string) => boolean;
}): void {
  const allowed = new Set(options.allowedActions);
  const onKeyDown = (event: KeyboardEvent) => {
    if (shortcutCaptureActive || event.defaultPrevented || event.isComposing || event.repeat) {
      return;
    }
    const shortcuts = state.settings?.sessionShortcuts ?? [];
    if (shortcuts.length === 0) return;
    const hit = findSessionShortcut(event, shortcuts);
    if (!hit?.keys) return;
    if (!allowed.has(hit.action)) return;
    if (hit.action !== "newSession" && !hit.target) return;
    // insertText 仅在输入框内生效，允许无修饰键；其它动作在可编辑区需 Ctrl/Alt/Meta。
    if (
      hit.action !== "insertText" &&
      isEditableTarget(event.target) &&
      !shortcutHasModifier(hit.keys)
    ) {
      return;
    }

    if (hit.action === "insertText") {
      if (!isEditableTarget(event.target)) return;
      const handled = options.onInsertText?.(hit.target) ?? false;
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
      const quotaPeer = parsed.peerToken
        ? {
            token: parsed.peerToken,
            name: state.peers.find((peer) => peer.token === parsed.peerToken)?.name ?? "队友",
          }
        : null;
      options.onSelectModel?.(parsed.agentKind, parsed.model, quotaPeer);
    }
  };

  onMount(() => {
    window.addEventListener("keydown", onKeyDown, true);
    onCleanup(() => window.removeEventListener("keydown", onKeyDown, true));
  });
}
