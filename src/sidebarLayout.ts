import { createStore } from 'solid-js/store';

export const defaultSidebarLayout = { collapsed: false };
const key = 'fd:sidebarLayout';
function validated(value: Partial<typeof defaultSidebarLayout>): typeof defaultSidebarLayout {
  return { collapsed: value.collapsed === true };
}
let initial = defaultSidebarLayout;
try {
  initial = validated(JSON.parse(localStorage.getItem(key) ?? 'null') ?? {});
} catch { /* 存储不可用或损坏时使用默认布局。 */ }
export const [sidebarLayout, updateSidebarLayout] = createStore(initial);
export function setSidebarLayout(patch: Partial<typeof defaultSidebarLayout>) {
  const next = validated({ ...sidebarLayout, ...patch });
  updateSidebarLayout(next);
  try { localStorage.setItem(key, JSON.stringify(next)); } catch { /* 本次窗口内仍然生效。 */ }
}
