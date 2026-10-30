import { createEffect, createSignal, createStore } from "solid-js";

export type WorkspaceMode = 'layout' | 'files' | 'artifacts' | 'git' | 'terminal';
export const defaultWorkspaceLayout = { open: false, widthRatio: null as number | null, minimap: true, softWrap: true, mode: 'files' as WorkspaceMode };
const key = 'fd:workspaceLayout';
const isMode = (value: unknown): value is WorkspaceMode => value === 'layout' || value === 'files' || value === 'artifacts' || value === 'git' || value === 'terminal';
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
  initial = validated(JSON.parse(localStorage.getItem(key) ?? 'null') ?? {
    open: localStorage.getItem('fd:workspaceOpen') === 'true',
    widthRatio: Number(localStorage.getItem('fd:workspaceWidthRatio')),
  });
} catch { /* 存储不可用或损坏时使用默认布局。 */ }
export const [workspaceLayout, updateWorkspaceLayout] = createStore(initial);
export function setWorkspaceLayout(patch: Partial<typeof defaultWorkspaceLayout>) {
  updateWorkspaceLayout(draft => {
    const next = validated({ ...draft, ...patch });
    Object.assign(draft, next);
    try { localStorage.setItem(key, JSON.stringify(next)); } catch { /* 本次窗口内仍然生效。 */ }
  });
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
