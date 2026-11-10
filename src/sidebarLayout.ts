import { createStore } from 'solid-js';

export const defaultSidebarLayout = { collapsedSessions: [] as string[], collapsedProjects: [] as string[] };
const key = 'fd:sidebarLayout';
function validated(value: Partial<typeof defaultSidebarLayout>): typeof defaultSidebarLayout {
  return {
    collapsedSessions: Array.isArray(value.collapsedSessions)
      ? value.collapsedSessions.filter((id): id is string => typeof id === 'string')
      : [],
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

/** 收起态按会话保存，空串代表首页/新会话。 */
export const isSidebarCollapsed = (sessionId: string | null) =>
  sidebarLayout.collapsedSessions.includes(sessionId ?? '');
export function setSidebarCollapsed(sessionId: string | null, collapsed: boolean) {
  const id = sessionId ?? '';
  const rest = sidebarLayout.collapsedSessions.filter(item => item !== id);
  setSidebarLayout({ collapsedSessions: collapsed ? [...rest, id] : rest });
}
/** 草稿会话拿到真实 id 后沿用其收起态，否则发首条消息时侧栏会自己展开。 */
export function moveSidebarSession(from: string, to: string) {
  if (from === to || !sidebarLayout.collapsedSessions.includes(from)) return;
  setSidebarLayout({ collapsedSessions: [...sidebarLayout.collapsedSessions.filter(id => id !== from && id !== to), to] });
}
