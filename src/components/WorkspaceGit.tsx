import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { api } from "../ipc";
import { state } from "../store";
import { workspaceDiffRows, type DiffRow } from "../workspaceDiff";
import { IconRefresh } from "./icons";

const IMAGE_PATH = /\.(png|jpe?g|gif|webp|bmp|ico|avif|svg)$/i;

export default function WorkspaceGit(props: { threadId: string; onOpen: (path: string, line?: number) => void }) {
  type Status = Awaited<ReturnType<typeof api.workspaceGitStatus>>;
  const [status, setStatus] = createSignal<Status>();
  const [selection, setSelection] = createSignal<{ path: string; staged: boolean }>();
  const [patch, setPatch] = createSignal("");
  const [images, setImages] = createSignal<{ before: string | null; after: string | null }>();
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [loadingDiff, setLoadingDiff] = createSignal(false);
  const [revision, setRevision] = createSignal(0);
  const [filter, setFilter] = createSignal("");
  const [expandAll, setExpandAll] = createSignal(false);
  let request = 0;
  const refresh = async () => {
    const token = ++request;
    setBusy(true); setError("");
    try {
      const result = await api.workspaceGitStatus(props.threadId);
      if (token !== request) return;
      setStatus(result); setRevision(v => v + 1);
      const chosen = selection();
      if (chosen && !result.files.some(file => file.path === chosen.path && (chosen.staged ? file.index !== " " && file.index !== "?" : file.worktree !== " "))) setSelection(undefined);
    } catch (e) { if (token === request) { setStatus(undefined); setSelection(undefined); setError(String(e)); } }
    finally { if (token === request) setBusy(false); }
  };
  createEffect(() => { state.running[props.threadId]; void refresh(); });
  onMount(() => { window.addEventListener("focus", refresh); });
  onCleanup(() => { ++request; window.removeEventListener("focus", refresh); });
  createEffect(() => {
    const chosen = selection(); revision();
    setPatch(""); setImages(undefined); setLoadingDiff(false);
    if (!chosen) return;
    let disposed = false;
    onCleanup(() => { disposed = true; });
    setLoadingDiff(true); setError("");
    const failed = (error: unknown) => { if (!disposed) setError(String(error)); };
    const done = () => { if (!disposed) setLoadingDiff(false); };
    if (IMAGE_PATH.test(chosen.path)) {
      void api.workspaceGitImage(props.threadId, chosen.path, chosen.staged)
        .then(pair => { if (!disposed) setImages(pair); }, failed).finally(done);
      return;
    }
    void api.workspaceGitDiff(props.threadId, chosen.path, chosen.staged).then(text => {
      if (!disposed) setPatch(text);
    }, failed).finally(done);
  });
  const groups = createMemo(() => workspaceDiffRows(patch()));
  const stats = createMemo(() => {
    const rows = groups().flat();
    return { add: rows.filter(row => row.kind === "add").length, del: rows.filter(row => row.kind === "del").length };
  });
  const Row = (p: { row: DiffRow }) => <div class={`workspace-diff-line ${p.row.kind}`}>
    <span class="workspace-diff-number">{p.row.old}</span><span class="workspace-diff-number">{p.row.next}</span>
    <span class="workspace-diff-sign">{p.row.kind === "add" ? "+" : p.row.kind === "del" ? "−" : " "}</span><span>{p.row.text || " "}</span>
  </div>;
  const Group = (p: { rows: DiffRow[] }) => {
    const [expanded, setExpanded] = createSignal(false);
    createEffect(() => setExpanded(expandAll()));
    const foldable = () => p.rows[0].kind === "context" && p.rows.length > 8;
    return <Show when={foldable()} fallback={<For each={p.rows}>{row => <Row row={row} />}</For>}>
      <For each={p.rows.slice(0, 3)}>{row => <Row row={row} />}</For>
      <button class="workspace-diff-fold" aria-expanded={expanded()} onClick={() => setExpanded(v => !v)}>{expanded() ? "折叠" : "展开"} {p.rows.length - 6} 行未变动内容</button>
      <Show when={expanded()}><For each={p.rows.slice(3, -3)}>{row => <Row row={row} />}</For></Show>
      <For each={p.rows.slice(-3)}>{row => <Row row={row} />}</For>
    </Show>;
  };
  return <section class="workspace-git" aria-label="Git 变动">
    <div class="workspace-toolbar"><input aria-label="筛选 Git 文件" placeholder="筛选变动文件…" value={filter()} onInput={e => setFilter(e.currentTarget.value)} />
      <button aria-label="刷新 Git 变动" disabled={busy()} onClick={() => void refresh()}><IconRefresh size={14} /></button></div>
    <Show when={busy()}><p role="status">正在读取 Git 变动…</p></Show>
    <Show when={error()}><p class="workspace-error" role="alert">{error()}</p></Show>
    <div class="workspace-git-files">
      <For each={[false, true]}>{staged => {
        const files = () => status()?.files.filter(file => (staged ? file.index !== " " && file.index !== "?" : file.worktree !== " ") && file.path.toLowerCase().includes(filter().toLowerCase())) ?? [];
        return <details open><summary>{staged ? "已暂存" : "未暂存（含未跟踪）"} · {files().length}</summary>
          <For each={files()}>{file => <button class="workspace-file" aria-pressed={selection()?.path === file.path && selection()?.staged === staged} title={file.oldPath ? `${file.oldPath} → ${file.path}` : file.path} onClick={() => setSelection({ path: file.path, staged })}>
            <strong>{staged ? file.index : file.worktree}</strong><span>{file.path}</span>
          </button>}</For>
        </details>;
      }}</For>
    </div>
    <Show when={status() && !status()!.files.length}><p>工作区干净，没有 Git 变动</p></Show>
    <Show when={selection()}>{chosen => <>
      <div class="workspace-toolbar workspace-diff-toolbar"><span title={chosen().path}>{chosen().path} · {chosen().staged ? "已暂存" : "未暂存"}</span>
        <Show when={!images()}><span class="workspace-diff-count">+{stats().add} −{stats().del}</span>
          <button aria-pressed={expandAll()} onClick={() => setExpandAll(v => !v)}>{expandAll() ? "折叠未变动" : "展开全部"}</button></Show>
        <button onClick={() => props.onOpen(`${status()!.repo}/${chosen().path}`)}>打开文件</button>
      </div>
      <Show when={loadingDiff()} fallback={<Show when={images()} fallback={<div class="workspace-diff" aria-label="文件差异"><For each={groups()}>{rows => <Group rows={rows} />}</For><Show when={!patch()}><p>没有文本差异</p></Show></div>}>{pair =>
        <div class="workspace-diff workspace-image-diff" aria-label="图片差异">
          <figure><figcaption>修改前</figcaption><Show when={pair().before} fallback={<p>没有旧版本</p>}>{src => <img src={src()} alt="修改前的图片" />}</Show></figure>
          <figure><figcaption>修改后</figcaption><Show when={pair().after} fallback={<p>已删除</p>}>{src => <img src={src()} alt="修改后的图片" />}</Show></figure>
        </div>}</Show>}><p role="status">正在读取差异…</p></Show>
    </>}</Show>
    <Show when={!selection() && status()?.files.length}><div class="workspace-empty">选择文件查看差异</div></Show>
  </section>;
}
