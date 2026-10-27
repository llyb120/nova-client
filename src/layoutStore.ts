import { createSignal, untrack } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { setWorkspaceLayout, workspaceLayout } from "./workspaceLayout";

export type LayoutItem = {
  id: string;
  name: string;
  html: string;
  schema: string;
  data: Record<string, unknown>;
  designing: boolean;
};
export type LayoutPreset = { id: string; name: string; html: string; schema: string; prompt: string; mock?: Record<string, unknown>; designing: boolean };
type ThreadLayouts = { active: string; layouts: LayoutItem[] };

const cmd = <T = unknown>(op: string, args: Record<string, unknown> = {}) => invoke<T>("layout_cmd", { op, args });

export const layoutApi = {
  switch: (threadId: string, layoutId: string) => cmd("switch", { threadId, layoutId }),
  remove: (threadId: string, layoutId: string) => cmd("delete", { threadId, layoutId }),
  rename: (threadId: string, layoutId: string, name: string) => cmd("rename", { threadId, layoutId, name }),
  usePreset: (threadId: string, presetId: string) => cmd("use_preset", { threadId, presetId }),
  clearData: (threadId: string, layoutId: string) => cmd("clear_data", { threadId, layoutId }),
  /** 上报正在看哪个会话的看板（null = 没在看）：后端只为在看的会话生成数据。 */
  viewing: (threadId: string | null) => cmd("viewing", { threadId: threadId ?? "" }),
  presetPrompt: (presetId: string, prompt: string) => cmd("preset_prompt", { presetId, prompt }),
  presetDesign: (text: string, opts: { presetId?: string; name?: string } = {}) => cmd("preset_design", { text, ...opts }),
  presetRemove: (presetId: string) => cmd("preset_delete", { presetId }),
  presetRename: (presetId: string, name: string) => cmd("preset_rename", { presetId, name }),
};

export const [layoutState, setLayoutState] = createStore<{
  byThread: Record<string, ThreadLayouts>;
  presets: LayoutPreset[];
}>({ byThread: {}, presets: [] });

const fetchSeq: Record<string, number> = {};
async function fetchState(threadId: string) {
  const seq = (fetchSeq[threadId] = (fetchSeq[threadId] ?? 0) + 1);
  try {
    const got = await cmd<ThreadLayouts & { presets: LayoutPreset[] }>("get", { threadId });
    // 较早发出的读取若晚于数据事件返回，会把看板数据回退成旧值，只认最新一次。
    if (seq !== fetchSeq[threadId]) return;
    if (threadId) setLayoutState("byThread", threadId, reconcile({ active: got.active, layouts: got.layouts }));
    setLayoutState("presets", reconcile(got.presets));
  } catch {
    // 布局是增强能力，读取失败不打扰会话。
  }
}

/** 载入某会话的布局（含预设）；会话切换时调用。 */
export const loadLayouts = fetchState;

// 侧栏「看板」页里是否在配置（设计/管理预设），而不是显示本会话的看板。
export const [presetsOpen, setPresetsOpen] = createSignal(false);

// 新会话页预选的预设：会话创建后由 applyPendingPreset 套用（一次性）。
export const [pendingPreset, setPendingPreset] = createSignal<string | null>(null);
export async function applyPendingPreset(threadId: string) {
  const id = pendingPreset();
  if (!id) return;
  setPendingPreset(null);
  try {
    await layoutApi.usePreset(threadId, id);
  } catch {
    // 预设已被删除等：不打扰会话创建。
  }
}

let started = false;
// 后端正在为哪些会话生成看板数据。
const [refreshingThreads, setRefreshingThreads] = createSignal<Record<string, boolean>>({});
export const layoutRefreshing = (threadId: string) => !!refreshingThreads()[threadId];
export function initLayoutEvents() {
  if (started) return;
  started = true;
  void listen<{ kind: "meta" | "data" | "presets" | "refreshing"; threadId?: string; layoutId?: string; data?: Record<string, unknown>; active?: string; on?: boolean }>("layout:changed", (event) => {
    const { kind, threadId, layoutId, data, active, on } = event.payload;
    if (kind === "refreshing" && threadId) {
      setRefreshingThreads((m) => ({ ...m, [threadId]: !!on }));
    } else if (kind === "data" && threadId && layoutId) {
      // 只推数据：直接打补丁，不重取整份 HTML。
      const index = layoutState.byThread[threadId]?.layouts.findIndex((l) => l.id === layoutId) ?? -1;
      if (index < 0) return void fetchState(threadId);
      fetchSeq[threadId] = (fetchSeq[threadId] ?? 0) + 1; // 作废还在路上的旧读取
      setLayoutState("byThread", threadId, "layouts", index, "data", reconcile(data ?? {}));
      if (active) setLayoutState("byThread", threadId, "active", active);
    } else if (kind === "presets") {
      void fetchState("");
    } else if (threadId) {
      void fetchState(threadId);
    }
  });
}

/** 宿主主题 → 沙箱页面：模式 + 一组 --nova-* 变量，布局只用这些变量就能跟随亮/暗外观。 */
const THEME_VARS: Record<string, string> = {
  "--nova-bg": "--bg", "--nova-panel": "--bg-panel", "--nova-hover": "--bg-hover", "--nova-text": "--text",
  "--nova-text-dim": "--text-dim", "--nova-text-faint": "--text-faint", "--nova-border": "--border-light",
  "--nova-accent": "--accent", "--nova-red": "--red", "--nova-green": "--green", "--nova-sans": "--sans", "--nova-mono": "--mono",
};
export function themePayload() {
  const root = document.documentElement;
  const css = getComputedStyle(root);
  const vars: Record<string, string> = {};
  for (const [name, source] of Object.entries(THEME_VARS)) vars[name] = css.getPropertyValue(source).trim();
  return { mode: root.dataset.theme === "ink-light" ? "light" : "dark", vars };
}
/** 外观切换时回调（立即不触发）；返回取消函数。 */
export function watchTheme(cb: () => void) {
  const mo = new MutationObserver(cb);
  mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
  return () => mo.disconnect();
}

// 布局显示在右侧侧栏的「布局」标签页里。
/** 布局标签页当前是否可见。 */
export const layoutTabOpen = () => workspaceLayout.open && workspaceLayout.mode === "layout";
/** 打开侧栏并切到布局标签页。 */
export const showLayouts = () => setWorkspaceLayout({ open: true, mode: "layout" });
/** 打开侧栏看板页的预设配置。 */
export const openPresets = () => { setPresetsOpen(true); showLayouts(); };

/**
 * 沙箱页面运行时：novaLayout.onData / novaLayout.data + data-bind 零代码绑定 + 主题变量。
 * 页面沙箱无同源权限、无网络（CSP），只通过 postMessage 收数据。
 */
const runtime = (theme: string, data: string) => `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src data:; connect-src 'none'; form-action 'none';"><style>html{background:var(--nova-bg);color:var(--nova-text);font-family:var(--nova-sans)}</style>${data ? `<script>window.__novaData=${data};</script>` : ""}<script>(function(){
var fns=[],cur={},booted=0;
// 初始数据烘焙在页面里（window.__novaData，可能在本脚本之后才定义）：首次用到时再读。
function boot(){if(!booted&&window.__novaData){booted=1;cur=window.__novaData}}
function theme(t){var r=document.documentElement;r.setAttribute('data-theme',t.mode);r.style.colorScheme=t.mode;for(var k in t.vars)r.style.setProperty(k,t.vars[k])}
theme(${theme});
function get(o,p){return String(p).split('.').reduce(function(a,k){return a==null?a:a[k]},o)}
function bind(){
 document.querySelectorAll('[data-bind]').forEach(function(e){var v=get(cur,e.getAttribute('data-bind'));e.textContent=v==null?'':typeof v==='object'?JSON.stringify(v):String(v)});
 document.querySelectorAll('[data-bind-html]').forEach(function(e){var v=get(cur,e.getAttribute('data-bind-html'));e.innerHTML=v==null?'':String(v)});
}
window.novaLayout={get data(){boot();return cur},onData:function(f){boot();fns.push(f);try{f(cur,[])}catch(e){console.error(e)}}};
window.addEventListener('message',function(m){var d=m.data;if(!d)return;if(d.type==='nova-layout-theme'){theme(d.theme);return}if(d.type!=='nova-layout-data')return;booted=1;cur=d.data||{};bind();fns.forEach(function(f){try{f(cur,d.keys||[])}catch(e){console.error(e)}})});
document.addEventListener('DOMContentLoaded',function(){boot();bind();parent.postMessage({type:'nova-layout-ready'},'*')});
})();</script>`;

/** 页面文档：注入运行时；data 给出时一并烘焙为初始数据（预设预览的 mock）。看板的 html 已由后端烘焙好数据。 */
export function layoutSrcdoc(html: string, data?: Record<string, unknown>): string {
  // 初始主题随文档带入（避免首帧闪烁）；untrack 让 srcdoc 不随主题变化重载，后续由 postMessage 更新。
  const rt = runtime(untrack(() => JSON.stringify(themePayload())).replace(/</g, "\\u003c"), data ? JSON.stringify(data).replace(/</g, "\\u003c") : "");
  const head = /<head(\s[^>]*)?>/i.exec(html);
  if (head) return html.slice(0, head.index + head[0].length) + rt + html.slice(head.index + head[0].length);
  const doc = /<html(\s[^>]*)?>/i.exec(html);
  if (doc) return html.slice(0, doc.index + doc[0].length) + rt + html.slice(doc.index + doc[0].length);
  return rt + html;
}
