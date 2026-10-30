import { createStore } from 'solid-js';

export const defaultSidebarLayout = { collapsed: false, collapsedProjects: [] as string[] };
const key = 'fd:sidebarLayout';
function validated(value: Partial<typeof defaultSidebarLayout>): typeof defaultSidebarLayout {
  return {
    collapsed: value.collapsed === true,
    collapsedProjects: Array.isArray(value.collapsedProjects)
      ? value.collapsedProjects.filter((path): path is string => typeof path === 'string')
      : [],
  };
}
let initial = defaultSidebarLayout;
try {
  initial = validated(JSON.parse(localStorage.getItem(key) ?? 'null') ?? {});
} catch { /* 存储不可用或损坏时使用默认布局。 */ }
export const [sidebarLayout, updateSidebarLayout] = createStore(initial);
export function setSidebarLayout(patch: Partial<typeof defaultSidebarLayout>) {
  updateSidebarLayout(draft => {
    const next = validated({ ...draft, ...patch });
    Object.assign(draft, next);
    try { localStorage.setItem(key, JSON.stringify(next)); } catch { /* 本次窗口内仍然生效。 */ }
  });
}
