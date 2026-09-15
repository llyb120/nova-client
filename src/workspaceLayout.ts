import { createStore } from 'solid-js/store';

export const defaultWorkspaceLayout = { open: false, widthRatio: null as number | null, minimap: true, softWrap: true };
const key = 'fd:workspaceLayout';
function validated(value: Partial<typeof defaultWorkspaceLayout>) {
  return {
    open: value.open === true,
    widthRatio: typeof value.widthRatio === 'number' && Number.isFinite(value.widthRatio) && value.widthRatio > 0 && value.widthRatio <= .7 ? value.widthRatio : null,
    minimap: value.minimap !== false,
    softWrap: value.softWrap !== false,
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
