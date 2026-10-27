import { createSignal } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type LayoutItem = {
  id: string;
  name: string;
  html: string;
  schema: string;
  data: Record<string, unknown>;
  designing: boolean;
};
export type LayoutPreset = { id: string; name: string; html: string; schema: string; designing: boolean };
type ThreadLayouts = { active: string; layouts: LayoutItem[] };

const cmd = <T = unknown>(op: string, args: Record<string, unknown> = {}) => invoke<T>("layout_cmd", { op, args });

export const layoutApi = {
  design: (threadId: string, text: string, opts: { layoutId?: string; new?: boolean } = {}) => cmd("design", { threadId, text, ...opts }),
  switch: (threadId: string, layoutId: string) => cmd("switch", { threadId, layoutId }),
  remove: (threadId: string, layoutId: string) => cmd("delete", { threadId, layoutId }),
  rename: (threadId: string, layoutId: string, name: string) => cmd("rename", { threadId, layoutId, name }),
  usePreset: (threadId: string, presetId: string) => cmd("use_preset", { threadId, presetId }),
  saveAsPreset: (threadId: string, layoutId: string) => cmd("save_preset", { threadId, layoutId }),
  clearData: (threadId: string, layoutId: string) => cmd("clear_data", { threadId, layoutId }),
  presetDesign: (text: string, opts: { presetId?: string; name?: string } = {}) => cmd("preset_design", { text, ...opts }),
  presetRemove: (presetId: string) => cmd("preset_delete", { presetId }),
  presetRename: (presetId: string, name: string) => cmd("preset_rename", { presetId, name }),
};

export const [layoutState, setLayoutState] = createStore<{
  byThread: Record<string, ThreadLayouts>;
  presets: LayoutPreset[];
}>({ byThread: {}, presets: [] });

async function fetchState(threadId: string) {
  try {
    const got = await cmd<ThreadLayouts & { presets: LayoutPreset[] }>("get", { threadId });
    if (threadId) setLayoutState("byThread", threadId, reconcile({ active: got.active, layouts: got.layouts }));
    setLayoutState("presets", reconcile(got.presets));
  } catch {
    // 布局是增强能力，读取失败不打扰会话。
  }
}

/** 载入某会话的布局（含预设）；会话切换时调用。 */
export const loadLayouts = fetchState;

// 每个会话各自的设计模式 / 设计目标（"new" = 下一条设计提示词新建布局）。
const [designModes, setDesignModes] = createSignal<Record<string, boolean>>({});
const [designTargets, setDesignTargets] = createSignal<Record<string, "new" | undefined>>({});
export const designMode = (threadId: string | null | undefined) => !!threadId && !!designModes()[threadId];
export const toggleDesignMode = (threadId: string | null | undefined) => {
  if (!threadId) return;
  setDesignModes((m) => ({ ...m, [threadId]: !m[threadId] }));
  void fetchState(threadId);
};
export const designTarget = (threadId: string) => designTargets()[threadId];
export const setDesignTarget = (threadId: string, target: "new" | undefined) => setDesignTargets((m) => ({ ...m, [threadId]: target }));

/** 设计模式下的提示词：交给设计会话，而不是主会话。 */
export async function sendDesign(threadId: string, text: string) {
  const make = designTarget(threadId) === "new";
  setDesignTarget(threadId, undefined);
  await layoutApi.design(threadId, text, make ? { new: true } : {});
}

let started = false;
export function initLayoutEvents() {
  if (started) return;
  started = true;
  void listen<{ kind: "meta" | "data" | "presets"; threadId?: string; layoutId?: string; data?: Record<string, unknown>; active?: string }>("layout:changed", (event) => {
    const { kind, threadId, layoutId, data, active } = event.payload;
    if (kind === "data" && threadId && layoutId) {
      // 只推数据：直接打补丁，不重取整份 HTML。
      const index = layoutState.byThread[threadId]?.layouts.findIndex((l) => l.id === layoutId) ?? -1;
      if (index < 0) return void fetchState(threadId);
      setLayoutState("byThread", threadId, "layouts", index, "data", reconcile(data ?? {}));
      if (active) setLayoutState("byThread", threadId, "active", active);
    } else if (kind === "presets") {
      void fetchState("");
    } else if (threadId) {
      void fetchState(threadId);
    }
  });
}

/**
 * 沙箱页面运行时：novaLayout.onData / novaLayout.data + data-bind 零代码绑定。
 * 页面沙箱无同源权限、无网络（CSP），只通过 postMessage 收数据。
 */
const RUNTIME = `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src data:; connect-src 'none'; form-action 'none';"><script>(function(){
var fns=[],cur={};
function get(o,p){return String(p).split('.').reduce(function(a,k){return a==null?a:a[k]},o)}
function bind(){
 document.querySelectorAll('[data-bind]').forEach(function(e){var v=get(cur,e.getAttribute('data-bind'));e.textContent=v==null?'':typeof v==='object'?JSON.stringify(v):String(v)});
 document.querySelectorAll('[data-bind-html]').forEach(function(e){var v=get(cur,e.getAttribute('data-bind-html'));e.innerHTML=v==null?'':String(v)});
}
window.novaLayout={get data(){return cur},onData:function(f){fns.push(f);try{f(cur,[])}catch(e){console.error(e)}}};
window.addEventListener('message',function(m){var d=m.data;if(!d||d.type!=='nova-layout-data')return;cur=d.data||{};bind();fns.forEach(function(f){try{f(cur,d.keys||[])}catch(e){console.error(e)}})});
document.addEventListener('DOMContentLoaded',function(){bind();parent.postMessage({type:'nova-layout-ready'},'*')});
})();</script>`;

export function layoutSrcdoc(html: string): string {
  const head = /<head[^>]*>/i.exec(html);
  if (head) return html.slice(0, head.index + head[0].length) + RUNTIME + html.slice(head.index + head[0].length);
  const doc = /<html[^>]*>/i.exec(html);
  if (doc) return html.slice(0, doc.index + doc[0].length) + RUNTIME + html.slice(doc.index + doc[0].length);
  return RUNTIME + html;
}
