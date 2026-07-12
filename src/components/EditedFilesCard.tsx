import { message } from "@tauri-apps/plugin-dialog";
import { diffLines } from "diff";
import { createMemo, createSignal, For, Show } from "solid-js";
import { api } from "../ipc";
import { isExpanded, state, toggleExpanded } from "../store";
import type { Item, RevertChange, ToolContent } from "../types";
import { createFileContextMenu } from "./FileContextMenu";
import { IconUndo } from "./icons";

interface FileEdit {
  path: string;
  oldText: string | null;
  newText: string;
  add: number;
  del: number;
}

/** 汇总一轮对话的所有文件编辑：同一文件多次编辑合并为「原始 → 最终」 */
function collectEdits(body: Item[]): FileEdit[] {
  const map = new Map<string, { first: string | null; last: string }>();
  for (const item of body) {
    if (item.type !== "tool") continue;
    for (const b of item.content) {
      if (b.type !== "diff") continue;
      const d = b as Extract<ToolContent, { type: "diff" }>;
      if (!d.path) continue;
      const prev = map.get(d.path);
      if (prev) prev.last = d.newText ?? "";
      else map.set(d.path, { first: d.oldText ?? null, last: d.newText ?? "" });
    }
  }
  return [...map.entries()]
    .map(([path, { first, last }]) => {
      let add = 0;
      let del = 0;
      for (const part of diffLines(first ?? "", last)) {
        if (part.added) add += part.count ?? 0;
        else if (part.removed) del += part.count ?? 0;
      }
      return { path, oldText: first, newText: last, add, del };
    })
    .filter((e) => e.add + e.del > 0);
}

/** 文件路径相对线程工作目录显示（不在其下则原样） */
export function relPath(p: string): string {
  const cwd = state.cwd;
  if (!cwd) return p;
  const norm = (s: string) => s.replace(/\//g, "\\").toLowerCase();
  const base = cwd.endsWith("\\") || cwd.endsWith("/") ? cwd : cwd + "\\";
  if (norm(p).startsWith(norm(base))) return p.slice(base.length);
  return p;
}

/** codex 风格「已编辑 N 个文件」卡片：路径点击用编辑器打开，支持撤销整轮改动 */
export function EditedFilesCard(props: { body: Item[]; undoneKey: string }) {
  const edits = createMemo(() => collectEdits(props.body));
  const fileMenu = createFileContextMenu();
  // 撤销状态放 store（流式更新会重挂组件，本地 signal 会丢）
  const undone = () => isExpanded(props.undoneKey);
  const [busy, setBusy] = createSignal(false);

  const totals = createMemo(() => {
    let add = 0;
    let del = 0;
    for (const e of edits()) {
      add += e.add;
      del += e.del;
    }
    return { add, del };
  });

  const openFile = (path: string) => {
    const id = state.currentId;
    if (!id) return;
    void api.openInEditor(id, path).catch((e) => void message(String(e), { kind: "error" }));
  };

  const revert = async () => {
    const id = state.currentId;
    if (!id || busy() || undone()) return;
    setBusy(true);
    try {
      const changes: RevertChange[] = edits().map((e) => ({
        path: e.path,
        oldText: e.oldText,
        newText: e.newText,
      }));
      const res = await api.revertFileChanges(id, changes);
      if (res.conflicts.length === 0 && res.errors.length === 0) {
        toggleExpanded(props.undoneKey, true);
      }
    } catch (e) {
      void message(String(e), { kind: "error" });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Show when={edits().length > 0}>
      <div class="files-card">
        <div class="files-head">
          <span class="files-title">已编辑 {edits().length} 个文件</span>
          <span class="tool-stats">
            <span class="stat-add">+{totals().add}</span>
            <span class="stat-del">-{totals().del}</span>
          </span>
          <span class="bar-spacer" />
          <button
            class="files-undo"
            disabled={busy() || undone()}
            onClick={() => void revert()}
            title={undone() ? "本轮改动已撤销" : "把这些文件恢复到本轮编辑前的内容（被后续修改过的文件会跳过）"}
          >
            <IconUndo size={12} />
            {undone() ? "已撤销" : busy() ? "撤销中…" : "撤销"}
          </button>
        </div>
        <For each={edits()}>
          {(e) => (
            <button
              class="files-row"
              onClick={() => openFile(e.path)}
              onContextMenu={(event) => fileMenu.open(event, e.path)}
              title={`在编辑器中打开 ${e.path}`}
            >
              <span class="files-path">{relPath(e.path)}</span>
              <span class="tool-stats">
                <span class="stat-add">+{e.add}</span>
                <span class="stat-del">-{e.del}</span>
              </span>
            </button>
          )}
        </For>
        <fileMenu.Menu />
      </div>
    </Show>
  );
}
