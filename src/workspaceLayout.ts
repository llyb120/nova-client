import { createSignal } from "solid-js";
import { createStore } from 'solid-js/store';

export type WorkspaceMode = 'files' | 'artifacts' | 'git' | 'browser' | 'terminal';
export const defaultWorkspaceLayout = { open: false, widthRatio: null as number | null, minimap: true, softWrap: true, mode: 'files' as WorkspaceMode };
const key = 'fd:workspaceLayout';
const isMode = (value: unknown): value is WorkspaceMode => value === 'files' || value === 'artifacts' || value === 'git' || value === 'browser' || value === 'terminal';
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
  const next = validated({ ...workspaceLayout, ...patch });
  updateWorkspaceLayout(next);
  try { localStorage.setItem(key, JSON.stringify(next)); } catch { /* 本次窗口内仍然生效。 */ }
}

export const [homeTerminalCwd, setHomeTerminalCwd] = createSignal("");
