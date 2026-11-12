import { createEffect, createSignal, createStore } from "solid-js";

export type WorkspaceMode = 'layout' | 'files' | 'artifacts' | 'git' | 'terminal' | 'browser' | 'context' | 'memory';
export const defaultWorkspaceLayout = { open: false, widthRatio: null as number | null, minimap: true, softWrap: true, mode: 'files' as WorkspaceMode };
const key = 'fd:workspaceLayout';
const isMode = (value: unknown): value is WorkspaceMode => value === 'layout' || value === 'files' || value === 'artifacts' || value === 'git' || value === 'terminal' || value === 'browser' || value === 'context' || value === 'memory';
function validated(value: Partial<typeof defaultWorkspaceLayout>): typeof defaultWorkspaceLayout {
  return {
    open: value.open === true,
    widthRatio: typeof value.widthRatio === 'number' && Number.isFinite(value.widthRatio) && value.widthRatio > 0 && value.widthRatio <= .7 ? value.widthRatio : null,
    minimap: value.minimap !== false,
    softWrap: value.softWrap !== false,
    mode: isMode(value.mode) ? value.mode : 'files',
  };
}
let initial = defaultWorkspaceLayout;
try {
  // 开合按会话记（syncWorkspaceOpen），不跨会话或重启保存；其余是全局偏好。
  initial = { ...validated(JSON.parse(localStorage.getItem(key) ?? 'null') ?? {}), open: false };
} catch { /* 存储不可用或损坏时使用默认布局。 */ }
export const [workspaceLayout, updateWorkspaceLayout] = createStore(initial);
export function setWorkspaceLayout(patch: Partial<typeof defaultWorkspaceLayout>) {
  updateWorkspaceLayout(draft => {
    const next = validated({ ...draft, ...patch });
    Object.assign(draft, next);
    try { localStorage.setItem(key, JSON.stringify({ ...next, open: false })); } catch { /* 本次窗口内仍然生效。 */ }
  });
}

/** 侧栏开合按会话记：一个会话打开不代表别的会话也打开；首页没有侧栏，切过去只保存不恢复。 */
const opens = new Map<string, boolean>();
let shownThread: string | null = null;
export function syncWorkspaceOpen(threadId: string | null) {
  if (threadId === shownThread) return;
  if (shownThread) {
    // ponytail: 只记最近 64 个会话的开合，更早的会话下次重新点开。
    if (opens.size > 64) opens.clear();
    opens.set(shownThread, workspaceLayout.open);
  }
  shownThread = threadId;
  if (threadId) setWorkspaceLayout({ open: opens.get(threadId) === true });
}

export const [homeTerminalCwd, setHomeTerminalCwd] = createSignal("");

/** Home never inherits a saved/chat panel-open flag. Only an explicit shortcut
 * opens it for the current visit; leaving/re-entering or requesting a new
 * session closes the panel without disposing the retained terminal group. */
export function createHomeTerminalState(context: () => {
  currentId: string | null;
  view: string;
  homeComposerFocusAt: number;
}) {
  const [opened, setOpened] = createSignal(false);
  const available = () => {
    const { currentId, view } = context();
    return !currentId && view !== "knowledge";
  };
  // Track navigation/new-session requests only, never the opened signal.
  createEffect(context, () => { setOpened(false); });
  return {
    open: () => available() && opened(),
    toggle: () => { if (available()) setOpened(value => !value); },
    close: () => setOpened(false),
  };
}
