import { convertFileSrc } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { batch, createEffect, createMemo, createSignal, ErrorBoundary, For, lazy, Match, onCleanup, onMount, Show, Suspense, Switch, untrack } from "solid-js";
import DOMPurify from "dompurify";
import { marked } from "marked";
import { EditorView } from "@codemirror/view";
import WorkspaceCode from "./WorkspaceCode";
import WorkspaceGit from "./WorkspaceGit";
import WorkspaceBrowser from "./WorkspaceBrowser";
import { api } from "../ipc";
import { state } from "../store";
import { isFileDropBlocked, workspaceFileDropTarget } from "../utils";
import { workspaceLayout, setWorkspaceLayout, type WorkspaceMode } from "../workspaceLayout";
import { collectWorkspaceArtifacts } from "../workspaceArtifacts";
import { absolutePath, createFileContextMenu } from "./FileContextMenu";
import { IconChevron, IconFile, IconFolder, IconRefresh, IconX, IconCopy, IconBrowser, IconGear, IconTerminal } from "./icons";
import "./WorkspacePanel.css";

const WorkspaceSheet = lazy(() => import("./WorkspaceSheet"));
import type { SheetEditor } from "./WorkspaceSheet";

type Preview = Awaited<ReturnType<typeof api.previewWorkspaceFile>>;
type FileTab = { file: Preview; original: string; draft: string; editing: boolean; source: boolean; saved: boolean; error: string; scroll: number; start: number; end: number };
type PanelSnapshot = { tabs: FileTab[]; activePath?: string; browse: boolean; expanded: string[]; filter: string; artifactFilter: string };
// Keep unsaved buffers when ChatView unmounts on a thread switch; never evict user edits.
const drafts = new Map<string, { file: Preview; text: string }>();
// ChatView remounts the panel on every thread switch; remember each thread's
// tabs/picker state so the side bar is restored exactly as it was left.
const panels = new Map<string, PanelSnapshot>();
const normalized = (text: string) => text.replace(/\r\n/g, "\n");
function FileIcon(props: { path: string; directory?: boolean }) {
  const ext = () => props.path.split(".").pop()?.toLowerCase() ?? "";
  const kind = () => props.directory ? "folder" : /^(png|jpe?g|gif|webp|svg|bmp|ico|avif)$/.test(ext()) ? "image"
    : /^(md|markdown|txt|pdf|docx?)$/.test(ext()) ? "document"
    : /^(json|ya?ml|toml|ini|xml|env)$/.test(ext()) ? "config"
    : /^(html?|css|scss)$/.test(ext()) ? "web"
    : /^(tsx?|jsx?|rs|py|go|java|c|cpp|h|sh|ps1|sql|vue|svelte)$/.test(ext()) ? "code"
    : /^(xlsx?|csv|tsv)$/.test(ext()) ? "table" : "file";
  return <span class={`workspace-type-icon ${kind()}`} aria-hidden="true"><Switch fallback={<IconFile size={16} />}>
    <Match when={kind() === "folder"}><IconFolder size={16} /></Match>
    <Match when={kind() === "config"}><IconGear size={16} /></Match>
    <Match when={kind() === "web"}><IconBrowser size={16} /></Match>
    <Match when={kind() === "code"}><IconTerminal size={16} /></Match>
    <Match when={kind() === "image"}><svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"><rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="8" cy="8" r="2"/><path d="m3 18 5-5 4 3 4-6 5 8"/></svg></Match>
    <Match when={kind() === "table"}><svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"><rect x="3" y="3" width="18" height="18" rx="2"/><path d="M3 9h18M3 15h18M9 3v18"/></svg></Match>
  </Switch></span>;
}
export default function WorkspacePanel(props: { threadId: string; request: { path: string; line?: number } | null; onClose: () => void }) {
  const threadId = props.threadId;
  const remembered = panels.get(threadId);
  // 视图选择是全局偏好：在一个会话里切到 Git，别的会话也默认停在 Git。
  const mode = () => workspaceLayout.mode;
  const setMode = (mode: WorkspaceMode) => setWorkspaceLayout({ mode });
  const [artifacts, setArtifacts] = createSignal<string[]>([]);
  const [artifactFilter, setArtifactFilter] = createSignal(remembered?.artifactFilter ?? "");
  const [tabs, setTabs] = createSignal<FileTab[]>(remembered?.tabs ?? []);
  const [expanded, setExpanded] = createSignal(new Set<string>(remembered?.expanded));
  const [treeRevision, setTreeRevision] = createSignal(0);
  type Listing = Awaited<ReturnType<typeof api.listWorkspaceDirectory>>;
  const directories = new Map<string, Promise<Listing>>();
  const aliases = new Map<string, string>();
  const [preview, setPreview] = createSignal<Preview>();
  const [error, setError] = createSignal("");
  const [loading, setLoading] = createSignal(false);
  const [source, setSource] = createSignal(false);
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [original, setOriginal] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const [browse, setBrowse] = createSignal(false);
  const [saved, setSaved] = createSignal(false);
  const [closing, setClosing] = createSignal<string>();
  const dirty = () => (preview()?.text != null || preview()?.kind === "spreadsheet") && draft() !== normalized(original());
  const draftKey = (path: string) => `${props.threadId}\0${path}`;
  const changeDraft = (text: string) => {
    setDraft(text); setSaved(false);
    const file = preview();
    if (!file) return;
    if (dirty()) drafts.set(draftKey(file.path), { file: file.kind === "spreadsheet" ? { ...file, sheet: original() } : { ...file, text: original() }, text });
    else drafts.delete(draftKey(file.path));
  };
  const [filter, setFilter] = createSignal(remembered?.filter ?? "");
  const [search, setSearch] = createSignal<Listing>();
  const [searching, setSearching] = createSignal(false);
  const [searchError, setSearchError] = createSignal('');
  createEffect(() => {
    const query = filter().trim();
    if (!browse() || !query) { setSearch(undefined); setSearching(false); return; }
    let disposed = false;
    setSearch(undefined); setSearching(true); setSearchError('');
    const timer = setTimeout(() => {
      void api.searchWorkspaceFiles(threadId, query).then(result => { if (!disposed) setSearch(result); },
        error => { if (!disposed) setSearchError(String(error)); }).finally(() => { if (!disposed) setSearching(false); });
    }, 250);
    onCleanup(() => { disposed = true; clearTimeout(timer); });
  });
  const [width, setWidth] = createSignal(480);
  const [dragging, setDragging] = createSignal(false);
  const menu = createFileContextMenu();
  let request = 0;
  let frame = 0;
  let panel!: HTMLElement;
  let codeView: EditorView | undefined;
  let sheetEditor: SheetEditor | undefined;
  const commitSheet = async () => {
    try { await sheetEditor?.flush(); return true; }
    catch (e) { setError(String(e)); return false; }
  };
  let picker!: HTMLDivElement;
  onMount(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    try {
      void getCurrentWebview().onDragDropEvent(event => {
        const payload = event.payload;
        const hit = payload.type !== 'leave' && !isFileDropBlocked() && !saving() && workspaceFileDropTarget(payload.position) === panel;
        setDragging(hit && payload.type !== 'drop');
        if (hit && payload.type === 'drop') {
          setMode('files');
          void (async () => {
            for (const path of [...new Set(payload.paths)]) {
              if (disposed) break;
              await open(path);
              if (error()) break;
            }
          })();
        }
      }).then(remove => { if (disposed) remove(); else unlisten = remove; }).catch(() => setDragging(false));
    } catch { setDragging(false); }
    onCleanup(() => { disposed = true; unlisten?.(); });
  });
  onMount(() => {
    const restoreWidth = () => {
      const available = panel.parentElement!.clientWidth;
      setWidth(Math.min(available * .7, Math.max(300, workspaceLayout.widthRatio === null ? 480 : available * workspaceLayout.widthRatio)));
    };
    createEffect(restoreWidth);
    const observer = new ResizeObserver(restoreWidth);
    observer.observe(panel.parentElement!);
    onCleanup(() => observer.disconnect());
  });
  onCleanup(() => {
    request++; cancelAnimationFrame(frame);
    snapshot();
    panels.delete(threadId);
    panels.set(threadId, { tabs: tabs(), activePath: preview()?.path, browse: browse(), expanded: [...expanded()], filter: filter(), artifactFilter: artifactFilter() });
    // ponytail: 最多记住 16 个会话的侧栏状态，更早的丢弃；未保存草稿仍由 drafts 兜底。
    while (panels.size > 16) panels.delete(panels.keys().next().value!);
  });
  const dismissPicker = (event: PointerEvent) => {
    if (!(event.target as HTMLElement).closest('.workspace-picker, .workspace-picker-toggle')) setBrowse(false);
    const actions = panel.querySelector<HTMLDetailsElement>('.workspace-actions[open]');
    if (actions && !actions.contains(event.target as Node)) actions.open = false;
  };
  onMount(() => document.addEventListener('pointerdown', dismissPicker));
  onCleanup(() => document.removeEventListener('pointerdown', dismissPicker));
  const report = (action: Promise<unknown>) => { void action.catch(e => setError(String(e))); };
  const refreshArtifacts = () => {
    const seen = new Set<string>();
    setArtifacts([
    ...[...drafts].filter(([key]) => key.startsWith(`${props.threadId}\0`)).map(([, value]) => value.file.path),
    ...untrack(() => collectWorkspaceArtifacts(state.items)),
    ].filter(path => {
      let key = absolutePath(path).replace(/\\/g, '/').replace(/^\/\/\?\//, '');
      if (/^[a-z]:/i.test(key) || key.startsWith('//')) key = key.toLowerCase();
      if (seen.has(key)) return false;
      seen.add(key); return true;
    }));
  };
  // Restore artifacts when the history snapshot arrives, without subscribing to text deltas.
  createEffect(() => { state.items; state.items.length; state.loadingThread; if (!state.running[props.threadId]) refreshArtifacts(); });
  refreshArtifacts();
  const snapshot = () => {
    const file = preview();
    if (!file) return;
    const scroll = codeView?.scrollDOM ?? panel.querySelector<HTMLElement>('.workspace-preview');
    setTabs(all => all.map(tab => tab.file.path === file.path ? {
      file, original: original(), draft: draft(), editing: editing(), source: source(), saved: saved(), error: error(),
      scroll: scroll?.scrollTop ?? 0, start: codeView?.state.selection.main.anchor ?? 0, end: codeView?.state.selection.main.head ?? 0,
    } : tab));
  };
  const activate = (tab: FileTab) => {
    batch(() => {
      setPreview(tab.file); setOriginal(tab.original); setDraft(tab.draft); setEditing(tab.editing);
      setSource(tab.source); setSaved(tab.saved); setError(tab.error); setLoading(false); setBrowse(false);
    });
    queueMicrotask(() => {
      if (preview()?.path !== tab.file.path) return;
      const scroll = codeView?.scrollDOM ?? panel.querySelector<HTMLElement>('.workspace-preview');
      codeView?.dispatch({ selection: { anchor: tab.start, head: tab.end } });
      if (scroll) scroll.scrollTop = tab.scroll;
    });
  };
  const selectTab = async (path: string) => {
    if (saving()) return;
    if (!(await commitSheet())) return;
    if (mode() === "git") setMode("files");
    ++request; snapshot();
    const tab = tabs().find(tab => tab.file.path === path);
    if (tab) activate(tab);
  };
  const finishClose = (tab: FileTab) => {
    const path = tab.file.path;
    drafts.delete(draftKey(path));
    const index = tabs().indexOf(tab);
    const remaining = tabs().filter(tab => tab.file.path !== path);
    setTabs(remaining);
    if (preview()?.path === path) {
      ++request;
      const next = remaining[Math.min(index, remaining.length - 1)];
      if (next) activate(next);
      else { setPreview(undefined); setLoading(false); setError(''); setBrowse(false); }
    }
  };
  // 直接按标签页数据写盘，无需先激活——对未激活的脏标签同样适用（含表格）。
  const saveTab = async (tab: FileTab) => {
    setSaving(true); setError("");
    try {
      let file = tab.file;
      let diskOriginal = tab.draft;
      if (file.kind === "spreadsheet") {
        const { exportSpreadsheet } = await import("../workspaceSpreadsheet");
        const data = await exportSpreadsheet(JSON.parse(tab.draft));
        await api.saveWorkspaceFile(props.threadId, file.path, file.data!, data);
        file = { ...file, data, sheet: tab.draft, size: Math.floor(data.length * 3 / 4) };
        if (preview()?.path === file.path) setPreview(file);
      } else {
        diskOriginal = tab.original.includes("\r\n") ? tab.draft.replace(/\n/g, "\r\n") : tab.draft;
        await api.saveWorkspaceFile(props.threadId, file.path, tab.original, diskOriginal);
      }
      drafts.delete(draftKey(file.path));
      setTabs(all => all.map(item => item === tab ? { ...item, file, original: diskOriginal, saved: true } : item));
      if (preview()?.path === file.path) { setOriginal(diskOriginal); setSaved(true); }
      setArtifacts(all => [...new Set([file.path, ...all])]);
      return true;
    } catch (e) { setError(String(e)); return false; }
    finally { setSaving(false); }
  };
  const closeTab = async (path: string) => {
    if (saving()) return;
    if (!(await commitSheet())) return;
    snapshot();
    const tab = tabs().find(tab => tab.file.path === path);
    if (!tab) return;
    if (tab.draft !== normalized(tab.original)) { setClosing(path); return; }
    finishClose(tab);
  };
  const resolveClose = async (saveFirst: boolean) => {
    const path = closing();
    setClosing(undefined);
    const tab = path && tabs().find(tab => tab.file.path === path);
    if (!tab) return;
    if (saveFirst && !(await saveTab(tab))) return;
    finishClose(tab);
  };
  const revealLine = (line?: number) => {
    if (!Number.isSafeInteger(line) || line! < 1 || !preview()?.text) return;
    setSource(true);
    queueMicrotask(() => {
      if (!codeView) return;
      const target = codeView.state.doc.line(Math.min(line!, codeView.state.doc.lines));
      codeView.dispatch({ selection: { anchor: target.from, head: target.to }, effects: EditorView.scrollIntoView(target.from, { y: 'start' }) });
    });
  };
  const open = async (path: string, reload = false, line?: number) => {
    if (saving()) return;
    if (!(await commitSheet())) return;
    snapshot();
    const existing = tabs().find(tab => tab.file.path === (aliases.get(path) ?? path));
    if (existing && !reload) { await selectTab(existing.file.path); revealLine(line); return; }
    // ponytail: 最多 24 个打开的文本缓冲，只有当前面板挂载 DOM；更多文件先关闭标签。
    if (!existing && tabs().length >= 24) { setError('最多打开 24 个文件，请先关闭不需要的标签'); return; }
    const token = ++request;
    setLoading(true); setError("");
    try {
      const result = await api.previewWorkspaceFile(props.threadId, path);
      if (result.kind === "spreadsheet" && result.data) {
        const { importSpreadsheet } = await import("../workspaceSpreadsheet");
        result.sheet = JSON.stringify(await importSpreadsheet(result.data, label(result.path)));
      }
      if (token === request) {
        snapshot();
        aliases.set(path, result.path);
        const duplicate = tabs().find(tab => tab.file.path === result.path);
        if (duplicate && !reload) { activate(duplicate); revealLine(line); return; }
        const buffer = drafts.get(draftKey(result.path));
        const tab: FileTab = { file: buffer?.file ?? result, original: buffer?.file.sheet ?? buffer?.file.text ?? result.sheet ?? result.text ?? '',
          draft: buffer?.text ?? normalized(result.sheet ?? result.text ?? ''), editing: result.text != null, source: false, saved: false, error: '', scroll: 0, start: 0, end: 0 };
        setTabs(all => duplicate ? all.map(item => item === duplicate ? tab : item) : [...all, tab]);
        activate(tab);
        revealLine(line);
      }
    } catch (e) { if (token === request) setError(String(e)); }
    finally { if (token === request) setLoading(false); }
  };
  const save = async () => {
    const file = preview();
    if (!file || saving()) return;
    if (file.kind === "spreadsheet") { await saveSheet(file); return; }
    if (file.text === null || !dirty()) return;
    ++request; setLoading(false);
    const text = original().includes("\r\n") ? draft().replace(/\n/g, "\r\n") : draft();
    setSaving(true); setError("");
    try {
      await api.saveWorkspaceFile(props.threadId, file.path, original(), text);
      drafts.delete(draftKey(file.path));
      setOriginal(text);
      setSaved(true);
      setArtifacts(all => [...new Set([file.path, ...all])]);
    } catch (e) { setError(String(e)); }
    finally { setSaving(false); }
  };
  const saveSheet = async (file: Preview) => {
    if (!sheetEditor) return;
    setSaving(true); setError("");
    try {
      await sheetEditor.flush();
      if (!dirty()) return;
      const text = draft();
      const { exportSpreadsheet } = await import("../workspaceSpreadsheet");
      const data = await exportSpreadsheet(JSON.parse(text));
      await api.saveWorkspaceFile(props.threadId, file.path, file.data!, data);
      drafts.delete(draftKey(file.path));
      setOriginal(text); setSaved(true);
      setPreview({ ...file, data, sheet: text, size: Math.floor(data.length * 3 / 4) });
      snapshot();
    } catch (e) { setError(String(e)); }
    finally { setSaving(false); }
  };
  const reload = async () => {
    if (saving()) return;
    if (!(await commitSheet())) return;
    if (dirty() && !window.confirm("放弃此文件的未保存修改并重新读取？")) return;
    if (preview()) { drafts.delete(draftKey(preview()!.path)); void open(preview()!.path, true); }
    refreshArtifacts();
  };
  function Directory(props: { path: string; depth: number }) {
    const [listing, setListing] = createSignal<Listing>();
    const [failure, setFailure] = createSignal('');
    const [retry, setRetry] = createSignal(0);
    createEffect(() => {
      treeRevision(); retry();
      let disposed = false;
      onCleanup(() => { disposed = true; });
      setListing(undefined); setFailure('');
      let pending = directories.get(props.path);
      if (!pending) {
        pending = api.listWorkspaceDirectory(threadId, props.path);
        directories.set(props.path, pending);
        // ponytail: Cache metadata for 64 directories; older collapsed branches reload on demand.
        if (directories.size > 64) directories.delete(directories.keys().next().value!);
      }
      void pending.then(result => { if (!disposed) setListing(result); }, error => {
        if (directories.get(props.path) === pending) directories.delete(props.path);
        if (!disposed) setFailure(String(error));
      });
    });
    return <div role="group">
      <Show when={listing()} fallback={<p role="status">{failure() || '正在读取目录…'}<Show when={failure()}><button onClick={() => setRetry(v => v + 1)}>重试</button></Show></p>}>{result => <>
        <For each={result().entries.filter(entry => entry.directory || entry.name.toLowerCase().includes(filter().toLowerCase()))}>{entry => <>
          <button class="workspace-file" role="treeitem" aria-level={props.depth + 1} aria-expanded={entry.directory ? expanded().has(entry.path) : undefined}
            style={{ 'padding-left': `${8 + props.depth * 16}px` }} title={entry.path} onContextMenu={e => menu.open(e, entry.path)}
            onKeyDown={e => {
              if (entry.directory && (e.key === 'ArrowRight' || e.key === 'ArrowLeft')) {
                e.preventDefault(); setExpanded(old => { const next = new Set(old); if (e.key === 'ArrowRight') next.add(entry.path); else next.delete(entry.path); return next; });
              }
            }}
            onClick={() => entry.directory ? setExpanded(old => { const next = new Set(old); if (next.has(entry.path)) next.delete(entry.path); else next.add(entry.path); return next; }) : void open(entry.path)}>
            <span class="workspace-tree-chevron"><Show when={entry.directory}><IconChevron size={14} open={expanded().has(entry.path)} /></Show></span><FileIcon path={entry.path} directory={entry.directory} /><span>{entry.name}</span>
          </button>
          <Show when={entry.directory && expanded().has(entry.path)}><Directory path={entry.path} depth={props.depth + 1} /></Show>
        </>}</For>
        <Show when={!result().entries.length}><p>空目录</p></Show>
        <Show when={result().truncated}><p>显示前 500 项 <button onClick={() => report(api.openInExplorer(props.path || state.cwd))}>查看全部</button></p></Show>
      </>}</Show>
    </div>;
  }
  createEffect(() => { const target = props.request; if (target) untrack(() => { setMode("files"); void open(target.path, false, target.line); }); });
  const markdown = createMemo(() => {
    const file = preview();
    if (file?.kind !== "markdown" || source() || editing()) return "";
    const html = DOMPurify.sanitize(marked.parse(draft(), { async: false }) as string);
    const template = document.createElement("template");
    template.innerHTML = html;
    // Local images are relative to the document, not the app URL.
    for (const img of template.content.querySelectorAll("img")) {
      const src = img.getAttribute("src") ?? "";
      if (!/^(?:https?:|data:)/i.test(src)) img.src = convertFileSrc(relative(src));
      img.loading = "lazy";
    }
    return template.innerHTML;
  });
  function relative(path: string) {
    if (/^(?:[a-z]:[\\/]|[\\/])/i.test(path)) return path;
    return `${preview()!.path.replace(/[\\/][^\\/]*$/, "")}/${path}`;
  }
  const label = (path: string) => path.split(/[\\/]/).pop() || path;
  const resize = (value: number) => {
    const available = panel.parentElement!.clientWidth;
    if (!available) return;
    const next = Math.min(available * .7, Math.max(300, value));
    setWidth(next);
    setWorkspaceLayout({ widthRatio: next / available });
  };
  if (remembered) {
    const tab = remembered.tabs.find(tab => tab.file.path === remembered.activePath);
    if (tab) activate(tab);
    if (remembered.browse) setBrowse(true);
  }
  return <aside ref={panel} class="workspace-panel" classList={{ 'is-dragging': dragging() }} style={{ width: `${width()}px` }} aria-label="产物与项目文件"
    onKeyDown={e => { if (mode() !== "browser" && (e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") { e.preventDefault(); e.stopPropagation(); if (mode() !== "git") void save(); } }}>
    <div class="workspace-resize" role="separator" aria-label="调整文件面板宽度" aria-orientation="vertical" tabindex="0"
      aria-valuenow={width()} onKeyDown={e => { if (e.key === "ArrowLeft" || e.key === "ArrowRight") { e.preventDefault(); resize(width() + (e.key === "ArrowLeft" ? 24 : -24)); } }}
      onPointerDown={e => { e.currentTarget.setPointerCapture(e.pointerId); }}
      onPointerMove={e => {
        if (!e.currentTarget.hasPointerCapture(e.pointerId)) return;
        const value = panel.getBoundingClientRect().right - e.clientX;
        cancelAnimationFrame(frame); frame = requestAnimationFrame(() => resize(value));
      }} />
    <nav class="workspace-toolbar workspace-modes" aria-label="文件来源">
      <button class="workspace-picker-toggle" aria-pressed={mode() === "files"} onClick={() => { setMode("files"); setBrowse(true); }}>文件</button>
      <button aria-pressed={mode() === "artifacts"} onClick={() => { setMode("artifacts"); setBrowse(false); refreshArtifacts(); }}>产物 · {artifacts().length}</button>
      <button aria-pressed={mode() === "git"} onClick={() => { setMode("git"); setBrowse(false); }}>Git 变动</button>
      <button aria-pressed={mode() === "browser"} onClick={() => { setMode("browser"); setBrowse(false); }}>浏览器</button>
      <button class="workspace-panel-close" aria-label="关闭文件面板" title="关闭（未保存草稿保留至应用退出）" disabled={saving()} onClick={async () => { if (await commitSheet()) props.onClose(); }}><IconX size={14} /></button>
    </nav>
    <Show when={mode() === "browser"}><WorkspaceBrowser threadId={threadId} /></Show>
    <header class="workspace-toolbar workspace-tabbar" style={{ display: mode() === "browser" ? "none" : undefined }}>
      <span class="workspace-open-label">已打开</span>
      <div class="workspace-tabs" role="tablist" aria-label="已打开文件" onWheel={e => {
        const el = e.currentTarget;
        if (el.scrollWidth <= el.clientWidth) return;
        const delta = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
        if (!delta) return;
        e.preventDefault();
        el.scrollLeft += e.deltaMode === 1 ? delta * 16 : delta;
      }} onKeyDown={e => {
        if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(e.key)) return;
        const all = tabs(); if (!all.length) return;
        e.preventDefault();
        const index = all.findIndex(tab => tab.file.path === preview()?.path);
        const next = e.key === 'Home' ? 0 : e.key === 'End' ? all.length - 1 : (index + (e.key === 'ArrowRight' ? 1 : -1) + all.length) % all.length;
        selectTab(all[next].file.path);
        queueMicrotask(() => panel.querySelector<HTMLButtonElement>('[role="tab"][aria-selected="true"]')?.focus());
      }}>
        <For each={tabs()}>{tab => <div class="workspace-tab" classList={{ active: preview()?.path === tab.file.path }}>
          <button role="tab" aria-selected={preview()?.path === tab.file.path} tabindex={preview()?.path === tab.file.path ? 0 : -1}
            title={tab.file.path} onClick={() => selectTab(tab.file.path)}>
            <FileIcon path={tab.file.path} /><span>{label(tab.file.path)}</span>
            <span class="workspace-dirty">{(preview()?.path === tab.file.path ? dirty() : tab.draft !== normalized(tab.original)) ? '●' : ''}</span>
          </button>
          <button class="workspace-tab-close" aria-label={`关闭 ${label(tab.file.path)}`} disabled={saving()} onClick={() => closeTab(tab.file.path)}><IconX size={12} /></button>
        </div>}</For>
      </div>
      <button class="workspace-picker-toggle" aria-label="打开文件" aria-expanded={browse()} title="从目录树打开文件" onClick={() => { setMode("files"); setBrowse(v => !v); refreshArtifacts(); }}>＋</button>
    </header>
    <Show when={mode() === "git"}><WorkspaceGit threadId={threadId} onOpen={(path, line) => { setMode("files"); void open(path, false, line); }} /></Show>
    <div class="workspace-file-view" style={{ display: mode() === "git" || mode() === "browser" ? "none" : undefined }}>
    <div class="workspace-toolbar workspace-location">
      <button class="workspace-picker-toggle workspace-breadcrumb" aria-label="选择项目文件" aria-expanded={browse()} onClick={() => { setBrowse(v => !v); refreshArtifacts(); }}>
        <IconFolder size={16} /><span class="workspace-breadcrumb-text">{label(state.cwd)} › {preview() ? label(preview()!.path) : '选择文件'}</span><IconChevron size={14} open={browse()} />
      </button>
      <Show when={preview()?.text != null}>
        <button aria-pressed={editing()} onClick={() => setEditing(v => !v)}>{editing() ? '预览' : '编辑'}</button>
        <button disabled={!dirty() || saving()} onClick={() => void save()} title="保存 (Ctrl/Cmd+S)">{saving() ? '保存中' : saved() ? '已保存' : '保存'}</button>
      </Show>
      <Show when={preview()?.kind === "spreadsheet"}>
        <button disabled={saving()} onClick={() => void save()} title="保存表格 (Ctrl/Cmd+S)">{saving() ? '保存中' : saved() ? '已保存' : '保存表格'}</button>
      </Show>
      <Show when={preview()}>{file => <details class="workspace-actions" onKeyDown={e => {
        if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); e.currentTarget.open = false; e.currentTarget.querySelector('summary')?.focus(); }
      }}>
        <summary aria-label="更多文件操作" title="更多文件操作">···</summary>
        <div onClick={e => {
          if ((e.target as HTMLElement).closest('button')) (e.currentTarget.parentElement as HTMLDetailsElement).open = false;
        }}>
          <Show when={!editing() && (file().kind === "markdown" || file().kind === "html")}><button onClick={() => setSource(v => !v)}>{source() ? "查看预览" : "查看源码"}</button></Show>
          <button onClick={() => report(api.openInEditor(props.threadId, file().path))}><IconTerminal size={14} />外部编辑器</button>
          <button onClick={() => report(api.openFileDefault(props.threadId, file().path))}><IconBrowser size={14} />系统打开</button>
          <button onClick={() => report(api.openInExplorer(file().path))}><IconFolder size={14} />定位文件</button>
          <button onClick={() => report(navigator.clipboard.writeText(file().path))}><IconCopy size={14} />复制路径</button>
        </div>
      </details>}</Show>
      <button aria-label="刷新文件" title="重新读取当前文件" disabled={saving()} onClick={reload}><IconRefresh size={14} /></button>
    </div>
    <Show when={browse()}>
      <div ref={picker} class="workspace-picker" aria-label="选择文件" onKeyDown={e => {
        if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); setBrowse(false); panel.querySelector<HTMLButtonElement>('[aria-label="选择项目文件"]')?.focus(); }
        if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(e.key) && (e.target as HTMLElement).getAttribute('role') === 'treeitem') {
          e.preventDefault(); const rows = [...picker.querySelectorAll<HTMLButtonElement>('[role="treeitem"]')];
          const index = rows.indexOf(e.target as HTMLButtonElement);
          const next = e.key === 'Home' ? 0 : e.key === 'End' ? rows.length - 1 : Math.max(0, Math.min(rows.length - 1, index + (e.key === 'ArrowDown' ? 1 : -1)));
          rows[next]?.focus();
        }
      }}>
        <div class="workspace-toolbar"><input aria-label="按文件名搜索项目" placeholder="搜索项目文件名…" value={filter()} onInput={e => setFilter(e.currentTarget.value)} />
          <button aria-label="刷新目录树" title="刷新已展开目录" onClick={() => { directories.clear(); setTreeRevision(v => v + 1); refreshArtifacts(); }}><IconRefresh size={14} /></button>
        </div>
        <Show when={filter().trim()} fallback={<div class="workspace-tree" role="tree" aria-label="项目文件树"><Directory path="" depth={0} /></div>}>
          <div class="workspace-search-results" aria-label="文件搜索结果">
            <Show when={searching()}><p role="status">正在搜索项目…</p></Show>
            <Show when={searchError()}><p role="alert">{searchError()}</p></Show>
            <For each={search()?.entries}>{entry => <button class="workspace-file" title={entry.path} onClick={() => void open(entry.path)} onContextMenu={e => menu.open(e, entry.path)}><FileIcon path={entry.path} /><span>{entry.name}</span><small>{entry.path}</small></button>}</For>
            <Show when={search() && !search()!.entries.length}><p>没有匹配的文件</p></Show>
            <Show when={search()?.truncated}><p>已显示部分结果，请使用更精确的文件名</p></Show>
          </div>
        </Show>
      </div>
    </Show>
    <Show when={mode() === "artifacts"}><section class="workspace-artifact-strip" aria-label="会话产物">
      <input aria-label="筛选会话产物" placeholder={`筛选本会话产物（${artifacts().length}）…`} value={artifactFilter()} onInput={e => setArtifactFilter(e.currentTarget.value)} />
      <div><For each={artifacts().filter(path => path.toLowerCase().includes(artifactFilter().toLowerCase()))}>{path => <button title={path} onClick={() => void open(path)} onContextMenu={e => menu.open(e, path)}><FileIcon path={path} /><span>{label(path)}</span><small aria-hidden="true">{path}</small></button>}</For>
        <Show when={!artifacts().length}><span class="workspace-artifact-empty">本会话尚无产物</span></Show>
      </div>
    </section></Show>
    <Show when={error()}><p class="workspace-error" role="alert">{error()}</p></Show>
    <Show when={loading()}><p role="status">正在读取文件…</p></Show>
    <Show when={preview()} keyed>{file => <>
      <Show when={editing() && file.text !== null} fallback={<div class="workspace-preview">
        <Switch fallback={<p>此文件类型或大小不适合内嵌预览，请使用“系统打开”或“编辑”。文本上限 256 KB，图片上限 16 MB。</p>}>
          <Match when={file.kind === "image"}><img class="workspace-image" src={convertFileSrc(file.path)} alt={label(file.path)} onError={() => setError("图片加载失败，请使用系统打开")} onContextMenu={e => menu.open(e, file.path)} /></Match>
          <Match when={file.kind === "markdown" && !source()}><div class="markdown" innerHTML={markdown()} onClick={e => {
            const link = (e.target as HTMLElement).closest("a"); if (!link) return;
            e.preventDefault(); const href = link.getAttribute("href") ?? "";
            if (/^https?:/i.test(href)) report(api.openUrl(href));
            else if (href && !/^(?:#|[a-z][a-z\d+.-]*:)/i.test(href)) void open(relative(href));
          }} /></Match>
          <Match when={file.kind === "html" && !source()}><iframe title={label(file.path)} sandbox="allow-scripts" referrerpolicy="no-referrer" srcdoc={`<meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src data:; connect-src 'none'; form-action 'none';">${draft()}`} /></Match>
          <Match when={file.kind === "spreadsheet"}>
            <ErrorBoundary fallback={error => <p role="alert">表格加载失败：{String(error)}，请刷新重试或使用系统打开。</p>}>
              <Suspense fallback={<p role="status">正在加载表格编辑器…</p>}><WorkspaceSheet text={draft()} readOnly={saving()}
                onReady={text => { if (!dirty()) { setOriginal(text); setDraft(text); } }}
                onChange={text => { if (preview() === file) changeDraft(text); }} onEditor={editor => { sheetEditor = editor; }} /></Suspense>
            </ErrorBoundary>
          </Match>
          <Match when={file.text !== null}><WorkspaceCode path={file.path} text={draft()} readOnly onChange={changeDraft} onView={view => { codeView = view; }} /></Match>
        </Switch>
      </div>}>
        <WorkspaceCode path={file.path} text={draft()} readOnly={saving()} onChange={changeDraft} onView={view => { codeView = view; }} />
      </Show>
      <footer class="workspace-status"><span>{dirty() ? "未保存 · 草稿保留至应用退出" : saved() ? "已保存到文件" : ""}</span><span>{Math.ceil((file.text !== null ? new TextEncoder().encode(original()).length : file.size) / 1024)} KB{file.text !== null ? " · UTF-8" : ""}</span></footer>
    </>}</Show>
    <Show when={!preview() && !loading()}><div class="workspace-empty">选择文件以查看预览</div></Show>
    </div>
    <menu.Menu />
    <Show when={closing()}>{path => (
      <div class="modal-backdrop" onClick={() => setClosing(undefined)}>
        <div class="modal workspace-close-modal" role="alertdialog" aria-modal="true" aria-label="关闭未保存的文件" onClick={e => e.stopPropagation()}>
          <div class="modal-head"><span>未保存的修改</span>
            <button class="icon-btn" aria-label="取消" onClick={() => setClosing(undefined)}><IconX size={16} /></button>
          </div>
          <div class="modal-body"><p>{label(path())} 有未保存的修改，关闭前要保存吗？</p></div>
          <div class="modal-foot">
            <button class="btn secondary" onClick={() => setClosing(undefined)}>取消</button>
            <button class="btn secondary" onClick={() => void resolveClose(false)}>不保存</button>
            <button class="btn primary" onClick={() => void resolveClose(true)}>保存</button>
          </div>
        </div>
      </div>
    )}</Show>
  </aside>;
}
