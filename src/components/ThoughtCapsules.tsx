import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createEffect, createMemo, createSignal, createUniqueId, For, onCleanup, onMount, Show } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import "./ThoughtCapsules.css";

export type Capsule = {
  id: string;
  title: string;
  text: string;
  files: Array<{ name: string; path: string; mimeType: string; size: number }>;
  summary: string;
  status: "ready" | "pending" | "unavailable";
  createdAt: number;
  summaryError?: string;
};

export type ThoughtCapsulesProps = { active: boolean; onCaptured?: () => void; captureError?: string };

/** Shared with the shell's DOM/native drop handlers. Pass real native paths, not File.name.
 * Privacy: automatic summaries run the lightweight-scene model (any backend) with the source file attached; captured text
 * is never replaced by a summary. Capsule.text may only sample a file; the full file
 * copy is saved by the backend, not the webview.
 * This only captures: the caller owns tab switching; nothing is sent to a conversation.
 */
export async function captureCapsule(text: string, paths: string[] = []): Promise<Capsule[]> {
  if (typeof text !== "string" || !Array.isArray(paths) || paths.some((p) => typeof p !== "string" || !p.trim() || p.includes("\0"))) {
    throw new Error("请提供文本或有效的本地文件路径。");
  }
  if (!text.trim() && !paths.length) throw new Error("先写下一点想法，或拖入文件。");
  // Preserve whitespace in the original; deduplicate only the file paths.
  return invoke<Capsule[]>("capsule_capture", { text, paths: [...new Set(paths)] });
}

const paths = {
  spark: "m12 3 2.6 6.4L21 12l-6.4 2.6L12 21l-2.6-6.4L3 12l6.4-2.6L12 3ZM20 3v4m-2-2h4",
  search: "M21 21l-5-5M18 10a8 8 0 1 1-16 0 8 8 0 0 1 16 0Z",
  plus: "M12 5v14M5 12h14",
  close: "m6 6 12 12M6 18 18 6",
  arrow: "M4 12h16m-6-6 6 6-6 6",
  file: "M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8l-6-6Zm0 0v6h6M8 13h8m-8 4h5",
  expand: "M8 3H3v5m13-5h5v5M3 16v5h5m13-5v5h-5",
  trash: "M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7m4-7v7",
  refresh: "M20 7a9 9 0 1 0 1 9M20 2v5h-5",
  shield: "m12 3 8 3v6c0 5-8 9-8 9s-8-4-8-9V6l8-3Zm-4 9 3 3 5-6",
};
const icon = (name: keyof typeof paths) => <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d={paths[name]} /></svg>;
const excerpt = (text: string, length = 220) => text.length > length ? `${text.slice(0, length).replace(/[\uD800-\uDBFF]$/, "")}…` : text;
export const titleOf = (capsule: Capsule) => capsule.title.trim() || excerpt(capsule.text.trim().split("\n", 1)[0], 50) || capsule.files[0]?.name || "未命名星屑";
const hasSummary = (capsule: Capsule) => capsule.status === "ready" && !!capsule.summary.trim() && capsule.summary.trim() !== capsule.text.trim();
const matches = (capsule: Capsule, query: string) => [capsule.title, capsule.text, capsule.summary, ...capsule.files.map((f) => `${f.name}\n${f.path}`)].some((value) => value.toLocaleLowerCase().includes(query));
const select = (previous: Set<string>, id: string) => {
  // Every click toggles: selecting several capsules needs no modifier key.
  const next = new Set(previous);
  if (next.has(id)) next.delete(id); else next.add(id);
  return next;
};
const fileSize = (size: number) => size < 1024 ? `${size} B` : size < 1024 ** 2 ? `${(size / 1024).toFixed(1)} KB` : `${(size / 1024 ** 2).toFixed(1)} MB`;
const dateLabel = (time: number) => new Date(time).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
const isImage = (capsule: Capsule) => !!capsule.files[0]?.mimeType.startsWith("image/");
// Stored copies have no extension (the asset protocol cannot serve them): fetch inline, keep a few.
const thumbs = new Map<string, Promise<string | null>>();
const thumbnail = (id: string) => {
  let hit = thumbs.get(id);
  if (!hit) {
    hit = invoke<string | null>("capsule_preview", { id }).catch(() => null);
    thumbs.set(id, hit);
    if (thumbs.size > 24) thumbs.delete(thumbs.keys().next().value!);
  }
  return hit;
};
function Thumb(props: { id: string; large?: boolean }) {
  const [src, setSrc] = createSignal<string | null>(null);
  createEffect(() => { const id = props.id; setSrc(null); void thumbnail(id).then((url) => { if (id === props.id) setSrc(url); }); });
  return <Show when={src()}><img class="tc-thumb" classList={{ "tc-thumb-large": props.large }} src={src()!} alt="" draggable={false} /></Show>;
}
type View = { kind: "detail"; id: string } | { kind: "remove"; ids: string[]; hidden: number };

/** Keep mounted across tab changes: active controls visibility, not lifetime.
 * The Butterfly shell owns ALL DOM/native drops and their feedback; do not intercept
 * them here, or the shell's drag overlay cannot observe the drop/leave and reset. */
export default function ThoughtCapsules(props: ThoughtCapsulesProps) {
  const [capsules, setCapsules] = createStore<Capsule[]>([]);
  const [selected, setSelected] = createSignal(new Set<string>());
  const [query, setQuery] = createSignal("");
  const [draft, setDraft] = createSignal("");
  const [busy, setBusy] = createSignal<"capture" | "open" | "remove" | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [loaded, setLoaded] = createSignal(false);
  const [initialized, setInitialized] = createSignal(false);
  const [error, setError] = createSignal("");
  const [loadError, setLoadError] = createSignal("");
  const [notice, setNotice] = createSignal("");
  const shownError = () => props.captureError || error() || loadError();
  const [view, setView] = createSignal<View | null>(null);
  const dialogTitle = createUniqueId();
  let dialog: HTMLElement | undefined;
  let returnFocus: HTMLElement | null = null;
  let disposed = false;
  let revision = 0;
  const unlisteners: Array<() => void> = [];

  const filtered = createMemo(() => {
    const term = query().trim().toLocaleLowerCase();
    return term ? capsules.filter((capsule) => matches(capsule, term)) : [...capsules];
  });
  const hiddenCount = createMemo(() => {
    const visible = new Set(filtered().map((capsule) => capsule.id));
    return [...selected()].filter((id) => !visible.has(id)).length;
  });
  const detail = createMemo(() => { const current = view(); return current?.kind === "detail" ? capsules.find((c) => c.id === current.id) : undefined; });
  const removal = createMemo(() => { const current = view(); return current?.kind === "remove" ? current : undefined; });
  const selectedIds = () => capsules.filter((capsule) => selected().has(capsule.id)).map((capsule) => capsule.id);
  const closeView = (restoreFocus = true) => {
    setView(null);
    if (restoreFocus) queueMicrotask(() => { if (!disposed && props.active && returnFocus?.isConnected) returnFocus.focus(); });
  };
  const showView = (next: View) => {
    if (busy() || !props.active) return;
    returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setView(next);
  };

  const refresh = async () => {
    const request = ++revision;
    setLoading(true);
    try {
      const next = await invoke<Capsule[]>("capsule_list");
      if (disposed || request !== revision) return;
      const ids = new Set(next.map((capsule) => capsule.id));
      const current = view();
      if (current && (current.kind === "detail" ? !ids.has(current.id) : current.ids.some((id) => !ids.has(id)))) closeView();
      setCapsules(reconcile([...next].sort((a, b) => b.createdAt - a.createdAt), { key: "id" }));
      // Only actual deletion clears a selection; a search never does.
      setSelected((previous) => new Set([...previous].filter((id) => ids.has(id))));
      setLoaded(true);
      setLoadError("");
    } catch (e) {
      if (!disposed && request === revision) setLoadError(`读取失败：${String(e)}`);
    } finally {
      if (!disposed && request === revision) setLoading(false);
    }
  };

  const capture = async (text: string) => {
    if (busy() || !props.active || !text.trim()) return;
    setBusy("capture"); setError(""); setNotice("");
    let captured = false;
    try {
      await captureCapsule(text);
      if (disposed) return;
      if (draft() === text) setDraft("");
      setNotice("已收好，原文完整保留。");
      captured = true;
      // capsule_capture may return only the new capsules: list is the source of truth.
      void refresh();
    } catch (e) {
      if (!disposed) setError(`收取失败，内容已保留在输入框：${String(e)}`);
    } finally {
      if (!disposed) setBusy(null);
    }
    if (captured && !disposed) props.onCaptured?.();
  };

  const openSelected = async () => {
    const ids = selectedIds();
    if (busy() || !props.active || !ids.length) return;
    setBusy("open"); setError(""); setNotice("");
    try {
      // Backend creates a new conversation draft. Never call a send command here.
      await invoke<void>("capsule_open", { ids });
      if (!disposed) setNotice("已带入新会话草稿，尚未发送。");
    } catch (e) {
      if (!disposed) setError(`带入失败：${String(e)}`);
    } finally {
      if (!disposed) setBusy(null);
    }
  };

  const removeConfirmed = async () => {
    const confirmation = removal();
    if (busy() || !props.active || !confirmation?.ids.length) return;
    setBusy("remove"); setError(""); setNotice("");
    try {
      await invoke<void>("capsule_remove", { ids: confirmation.ids });
      if (disposed) return;
      // Invalidate older list requests before updating locally; an unsuccessful reload
      // must not resurrect items whose deletion already succeeded.
      ++revision;
      const removed = new Set(confirmation.ids);
      closeView();
      setCapsules(reconcile(capsules.filter((capsule) => !removed.has(capsule.id))));
      setSelected((previous) => new Set([...previous].filter((id) => !removed.has(id))));
      setNotice(`已删除 ${confirmation.ids.length} 条星屑。`);
      void refresh();
    } catch (e) {
      if (!disposed) setError(`删除失败，仍保留原有选择：${String(e)}`);
    } finally {
      if (!disposed) setBusy(null);
    }
  };

  const choose = (id: string) => {
    if (!busy() && props.active) { setSelected((previous) => select(previous, id)); setNotice(""); }
  };
  const panelKeys = (event: KeyboardEvent) => {
    if (!view() || !props.active) return;
    if (event.key === "Escape") {
      event.preventDefault(); event.stopPropagation();
      if (!busy()) closeView();
    } else if (event.key === "Tab" && dialog) {
      const targets = [...dialog.querySelectorAll<HTMLElement>("button:not(:disabled), [tabindex='0']")];
      const first = targets[0], last = targets.at(-1);
      if (!first) { event.preventDefault(); return; }
      if (event.shiftKey && (document.activeElement === first || !dialog.contains(document.activeElement))) { event.preventDefault(); last?.focus(); }
      else if (!event.shiftKey && (document.activeElement === last || !dialog.contains(document.activeElement))) { event.preventDefault(); first.focus(); }
    }
  };

  onMount(() => {
    const subscriptions = [
      listen("capsules:changed", () => { if (!disposed) void refresh(); }),
    ];
    void Promise.all(subscriptions.map(async (subscription) => {
      try {
        const off = await subscription;
        if (disposed) off(); else unlisteners.push(off);
      } catch (e) {
        if (!disposed) setError(`实时更新未连接，可手动刷新：${String(e)}`);
      }
    })).then(() => { if (!disposed) setInitialized(true); });
  });
  createEffect(() => {
    if (props.active && initialized()) void refresh();
    if (!props.active) closeView(false);
  });
  createEffect(() => {
    const current = view();
    if (props.active && current) queueMicrotask(() => {
      if (!disposed && props.active && view() === current) dialog?.querySelector<HTMLElement>("[data-tc-initial]")?.focus();
    });
  });
  createEffect(() => {
    if (!notice()) return;
    const timer = window.setTimeout(() => setNotice(""), 4500);
    onCleanup(() => window.clearTimeout(timer));
  });
  onCleanup(() => { disposed = true; ++revision; unlisteners.forEach((off) => off()); });

  return (
    <section class="tc-panel" hidden={!props.active} aria-label="星屑" onKeyDown={panelKeys}>
      <div class="tc-main" inert={!!view()} aria-hidden={view() ? "true" : undefined}>
        <header class="tc-header">
          <span class="tc-emblem">{icon("spark")}</span>
          <div class="tc-heading"><h2>星屑 <span class="tc-total">{capsules.length}</span></h2><p>先留住灵感，稍后再展开。</p></div>
          <button type="button" class="tc-icon-button" classList={{ "tc-spinning": loading() }} aria-label="刷新星屑" title="刷新星屑" disabled={loading() || !!busy()} onClick={() => void refresh()}>{icon("refresh")}</button>
        </header>

        <div class="tc-composer" aria-busy={busy() === "capture"}>
          <textarea aria-label="新的闪念" placeholder="此刻想到什么？粘贴或拖入文字…" rows={2} value={draft()} disabled={!!busy()} onInput={(event) => setDraft(event.currentTarget.value)}
            onKeyDown={(event) => { if ((event.ctrlKey || event.metaKey) && event.key === "Enter" && !event.isComposing) { event.preventDefault(); event.stopPropagation(); if (!event.repeat) void capture(draft()); } }} />
          <button type="button" class="tc-capture" title="收好这段文字（Ctrl / ⌘ + Enter）" disabled={!!busy() || !draft().trim()} onClick={() => void capture(draft())}>{icon("plus")}<span>{busy() === "capture" ? "收取中" : "收好"}</span></button>
        </div>

        <div class="tc-search">
          {icon("search")}<input type="search" aria-label="搜索星屑原文、摘要和文件" placeholder="搜索原文、摘要、文件…" value={query()} onInput={(event) => setQuery(event.currentTarget.value)} />
          <Show when={query()}><button type="button" class="tc-icon-button" aria-label="清除搜索" onClick={() => setQuery("")}>{icon("close")}</button></Show>
          <span class="tc-result-count">{filtered().length} 条</span>
        </div>
        <Show when={!view() && shownError()}><div class="tc-message tc-error" role="alert"><span>{shownError()}</span><Show when={!props.captureError}><button type="button" class="tc-icon-button" aria-label="关闭错误提示" onClick={() => { setError(""); setLoadError(""); }}>{icon("close")}</button></Show></div></Show>

        <div class="tc-list-area" aria-busy={loading()}>
          <Show when={filtered().length} fallback={
            <div class="tc-empty" role="status">
              <span class="tc-empty-art" classList={{ "tc-pulse": loading() }}>{icon(query().trim() ? "search" : "spark")}</span>
              <strong>{loading() && !loaded() ? "正在找回你的灵感…" : !loaded() && loadError() ? "暂时没能读取星屑" : query().trim() ? "没有找到这段灵感" : "一闪而过，也值得留下"}</strong>
              <p>{!loaded() && loadError() ? "点击右上角刷新，已有内容不会丢失。" : query().trim() ? "换个关键词试试，已选星屑仍会保留。" : "写下一句，或拖入文字、链接与文件。"}</p>
              <Show when={query().trim()}><button type="button" class="tc-text-button" onClick={() => setQuery("")}>查看全部星屑</button></Show>
            </div>
          }>
            <ul class="tc-list" aria-label="星屑列表">
              <For each={filtered()}>{(capsule) => (
                <li class="tc-card" classList={{ "tc-selected": selected().has(capsule.id) }}>
                  <label class="tc-check"><input type="checkbox" class="tc-checkbox" aria-label={`多选：${titleOf(capsule)}`} checked={selected().has(capsule.id)} disabled={!!busy()} onChange={() => choose(capsule.id)} /></label>
                  <button type="button" class="tc-card-select" aria-pressed={selected().has(capsule.id)} aria-label={`选择星屑：${titleOf(capsule)}`} title="点击选择 / 取消，可多选" disabled={!!busy()}
                    onClick={() => choose(capsule.id)}
                    onKeyDown={(event) => { if (event.key === " " || event.key === "Enter") { event.preventDefault(); event.stopPropagation(); if (!event.repeat) choose(capsule.id); } }}>
                    <span class="tc-card-title">{titleOf(capsule)}</span>
                    <span class="tc-preview"><span class="tc-preview-label">{hasSummary(capsule) ? "摘要" : capsule.files.length ? "文件预览" : "原文"}</span>{excerpt(hasSummary(capsule) ? capsule.summary : capsule.text) || "这条星屑里装着文件，原件副本已收好。"}</span>
                    <Show when={isImage(capsule)}><Thumb id={capsule.id} /></Show>
                    <span class="tc-card-meta"><time title={new Date(capsule.createdAt).toLocaleString()}>{dateLabel(capsule.createdAt)}</time><Show when={capsule.files.length}><span class="tc-attachment">{icon("file")}{capsule.files.length} 个文件</span></Show><span class={`tc-state tc-state-${capsule.status}`} title={capsule.summaryError}>{capsule.status === "pending" ? "摘要中" : capsule.status === "unavailable" ? "原文已保存" : "已收好"}</span></span>
                  </button>
                  <button type="button" class="tc-icon-button tc-detail-button" aria-label={`查看${capsule.files.length ? "文件内容预览" : "完整原文"}：${titleOf(capsule)}`} title={capsule.files.length ? "查看文件内容预览与附件" : "查看完整原文"} disabled={!!busy()} onClick={() => showView({ kind: "detail", id: capsule.id })}>{icon("expand")}</button>
                </li>
              )}</For>
            </ul>
          </Show>
        </div>

        <footer class="tc-footer">
          <div class="tc-selection"><span aria-live="polite">已选 <strong>{selected().size}</strong> 条<Show when={hiddenCount()}><span class="tc-hidden-count">含 {hiddenCount()} 条搜索隐藏项</span></Show></span><span class="tc-multiselect-hint">点击可多选</span><button type="button" class="tc-text-button" disabled={!selected().size || !!busy()} onClick={() => { setSelected(new Set<string>()); setNotice(""); }}>清空选择</button></div>
          <div class="tc-actions"><button type="button" class="tc-delete" disabled={!selected().size || !!busy()} aria-label={`删除选中的 ${selected().size} 条星屑`} title="删除所选（需确认）" onClick={() => showView({ kind: "remove", ids: selectedIds(), hidden: hiddenCount() })}>{icon("trash")}</button><button type="button" class="tc-open" disabled={!selected().size || !!busy()} onKeyDown={(event) => { if (event.repeat && (event.key === "Enter" || event.key === " ")) event.preventDefault(); }} onClick={(event) => { if (event.detail < 2) void openSelected(); }}><span>{busy() === "open" ? "正在准备草稿…" : "带入新会话"}</span><small>仅草稿 · 不发送</small>{icon("arrow")}</button></div>
          <p class="tc-privacy" title="自动摘要交给轻量场景模型（任意后端），附带源文件由 agent 自行读取，请留意敏感信息。单文件最多 20 MiB，每次最多 16 个、合计 80 MiB；原文和文件副本独立保留。">{icon("shield")}自动摘要使用轻量场景模型 · 原文保留</p>
        </footer>
      </div>

      <Show when={view()}>
        <section class="tc-dialog" ref={dialog} role={removal() ? "alertdialog" : "dialog"} aria-modal="true" aria-labelledby={dialogTitle} aria-busy={!!busy()}>
          <header class="tc-dialog-header"><span class="tc-emblem">{icon(removal() ? "trash" : "file")}</span><h3 id={dialogTitle}>{removal() ? "删除这些星屑？" : detail()?.files.length ? "文件内容预览" : "星屑原文"}</h3><button type="button" class="tc-icon-button" data-tc-initial={detail() ? "" : undefined} aria-label="返回星屑列表" disabled={!!busy()} onClick={() => closeView()}>{icon("close")}</button></header>
          <Show when={shownError()}><div class="tc-message tc-error" role="alert">{shownError()}</div></Show>
          <Show when={detail()}>{(capsule) => <div class="tc-detail-content" tabIndex={0} aria-label="完整星屑内容">
            <div class="tc-detail-heading"><h4>{titleOf(capsule())}</h4><time>{new Date(capsule().createdAt).toLocaleString()}</time></div>
            <Show when={isImage(capsule())}><Thumb id={capsule().id} large /></Show>
            <h5>{capsule().files.length ? "文件内容预览" : "完整原文"} <span>{capsule().files.length ? "完整内容保存在附件中" : "始终保留"}</span></h5><div class="tc-original">{capsule().text || (capsule().files.length ? "暂无文件内容预览，完整内容保存在附件中。" : "此星屑没有文字。")}</div>
            <Show when={capsule().files.length}><h5>文件副本 <span>{capsule().files.length} 个</span></h5><ul class="tc-files"><For each={capsule().files}>{(file) => <li>{icon("file")}<div><strong>{file.name}</strong><small>{fileSize(file.size)} · {file.mimeType || "文件"}</small><code>{file.path}</code></div></li>}</For></ul><p class="tc-detail-note">实际副本由后端保存；这里展示记录中的文件路径。</p></Show>
            <h5>自动摘要 <span>{capsule().status === "pending" ? "生成中" : capsule().status === "unavailable" ? "暂不可用" : "仅供预览"}</span></h5><div class="tc-summary">{capsule().summary || (capsule().status === "pending" ? "正在整理这段灵感，原文已安全收好。" : "暂无摘要，不影响查看原文或带入草稿。")}</div><Show when={capsule().summaryError}><p class="tc-detail-note">摘要提示：{capsule().summaryError}</p></Show>
          </div>}</Show>
          <Show when={removal()}>{(confirmation) => <>
            <p class="tc-confirm-copy">将永久移除 <strong>{confirmation().ids.length}</strong> 条星屑记录。<Show when={confirmation().hidden}>其中 <strong>{confirmation().hidden}</strong> 条被当前搜索隐藏。</Show><br /><span>此操作无法撤销，请确认没有需要保留的灵感。</span></p>
            <ul class="tc-delete-list" tabIndex={0} aria-label="即将删除的星屑"><For each={confirmation().ids}>{(id) => <li>{icon("file")}<span>{titleOf(capsules.find((capsule) => capsule.id === id)!)}</span></li>}</For></ul>
            <div class="tc-confirm-actions"><button type="button" class="tc-secondary" data-tc-initial="" disabled={!!busy()} onClick={() => closeView()}>再想想</button><button type="button" class="tc-danger" disabled={!!busy()} onClick={() => void removeConfirmed()}>{busy() === "remove" ? "正在删除…" : `确认删除 ${confirmation().ids.length} 条`}</button></div>
          </>}</Show>
        </section>
      </Show>
      <Show when={notice() && !view()}><div class="tc-toast" role="status"><span>{notice()}</span><button type="button" class="tc-icon-button" aria-label="关闭操作提示" onClick={() => setNotice("")}>{icon("close")}</button></div></Show>
    </section>
  );
}
