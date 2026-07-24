import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { api } from "../ipc";
import { refreshProjects, roamingPeers, state } from "../store";
import type { Peer, ProjectEntry } from "../types";
import { isScratch } from "../utils";
import { IconBroadcast, IconChevron, IconFolder, IconPlus, IconX } from "./icons";

function basename(p: string) {
  return p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || p;
}

/** 项目条目展示名：worktree 用「仓库名」+ 分支徽标区分，避免显示 uuid 目录名 */
export function projectDisplayName(p: ProjectEntry): string {
  return p.worktree ? basename(p.worktree.repo) : basename(p.path);
}

/** codex 风格的项目选择下拉：最近项目 + 搜索 + 浏览文件夹 + 漫游到队友目录 */
export function ProjectPicker(props: {
  value: string;
  onChange: (cwd: string) => void;
  /** 当前漫游目标（选中队友目录时）；传入则启用漫游分组 */
  roam?: { peer: Peer; folder: string } | null;
  onPickRoaming?: (peer: Peer, folder: string) => void;
  /** 下拉框向下展开（默认向上，适配底部 composer） */
  popDown?: boolean;
}) {
  const [opened, setOpened] = createSignal(false);
  const [query, setQuery] = createSignal("");
  let rootRef: HTMLDivElement | undefined;
  let searchRef: HTMLInputElement | undefined;
  let focusFrame: number | undefined;

  // 项目列表由后端统一合并：最近项目 + 本地会话用过的目录（已排除临时目录 /
  // 别人的漫游目录 / 已删除的 worktree），并带 worktree 标注
  const filtered = createMemo(() => {
    const q = query().toLowerCase();
    if (!q) return state.projects;
    return state.projects.filter(
      (p) =>
        p.path.toLowerCase().includes(q) ||
        (p.worktree
          ? p.worktree.branch.toLowerCase().includes(q) ||
            p.worktree.repo.toLowerCase().includes(q)
          : false),
    );
  });

  const pick = (cwd: string) => {
    props.onChange(cwd);
    setOpened(false);
    setQuery("");
  };

  const pickRoaming = (peer: Peer, folder: string) => {
    props.onPickRoaming?.(peer, folder);
    setOpened(false);
    setQuery("");
  };

  const peers = createMemo(() => {
    if (!props.onPickRoaming) return [];
    const q = query().toLowerCase();
    if (!q) return roamingPeers();
    // 搜索：队友名命中则整体保留（连同手输路径入口）；否则只保留命中的目录
    return roamingPeers()
      .map((p) => ({
        ...p,
        folders: p.name.toLowerCase().includes(q)
          ? p.folders
          : p.folders.filter((f) => (f.name || f.path).toLowerCase().includes(q)),
      }))
      .filter((p) => p.name.toLowerCase().includes(q) || p.folders.length > 0);
  });

  const pillText = () => {
    if (props.roam) return `${props.roam.peer.name} / ${basename(props.roam.folder)}`;
    if (!props.value) return "选择项目";
    if (isScratch(props.value)) return "临时会话";
    // 选中的是 worktree 目录：显示「仓库名 ⎇ 分支」而非 uuid 目录名
    const entry = state.projects.find((p) => p.path === props.value);
    if (entry?.worktree) return `${basename(entry.worktree.repo)} ⎇ ${entry.worktree.branch}`;
    return basename(props.value);
  };

  const browse = async () => {
    const dir = await openDialog({ directory: true, title: "选择项目目录" });
    if (typeof dir === "string" && dir) pick(dir);
  };

  // 不使用项目：在系统临时目录新建一个空目录作为工作区
  const useScratch = async () => {
    const dir = await api.scratchDir();
    pick(dir);
  };

  const cancelPendingFocus = () => {
    if (focusFrame === undefined) return;
    cancelAnimationFrame(focusFrame);
    focusFrame = undefined;
  };

  const toggle = () => {
    const willOpen = !opened();
    setOpened(willOpen);
    cancelPendingFocus();
    if (willOpen) {
      focusFrame = requestAnimationFrame(() => {
        focusFrame = undefined;
        searchRef?.focus({ preventScroll: true });
      });
    }
  };

  const onDocClick = (e: MouseEvent) => {
    if (rootRef && !rootRef.contains(e.target as Node)) {
      setOpened(false);
      cancelPendingFocus();
    }
  };
  document.addEventListener("mousedown", onDocClick);
  onCleanup(() => {
    document.removeEventListener("mousedown", onDocClick);
    cancelPendingFocus();
  });

  return (
    <div class="proj-picker" ref={rootRef}>
      <button
        class={`pill ${props.roam ? "roam" : ""}`}
        onClick={toggle}
        title={props.roam ? `漫游到 ${props.roam.peer.name} 的 ${props.roam.folder}` : props.value || "选择项目"}
      >
        {props.roam ? <IconBroadcast size={13} /> : <IconFolder size={13} />}
        <span class="pill-text">{pillText()}</span>
        <IconChevron size={12} open={opened()} />
      </button>
      <div class={`proj-pop ${props.popDown ? "down" : ""}`} hidden={!opened()}>
        <input
          ref={searchRef}
          class="proj-search"
          placeholder="搜索项目"
          value={query()}
          onInput={(e) => setQuery(e.currentTarget.value)}
        />
        <div class="proj-list">
            <div
              class={`proj-item scratch ${props.value && isScratch(props.value) ? "active" : ""}`}
              onClick={() => void useScratch()}
              title="不关联项目，在系统临时目录新建一个空目录作为工作区"
            >
              <IconPlus size={13} />
              <span class="proj-name">临时会话（不使用项目）</span>
            </div>
            <For each={filtered()}>
              {(p) => (
                <div
                  class={`proj-item ${p.path === props.value ? "active" : ""}`}
                  onClick={() => pick(p.path)}
                  title={
                    p.worktree
                      ? `worktree 会话目录\n源仓库：${p.worktree.repo}\n分支：${p.worktree.branch}\n${p.path}`
                      : p.path
                  }
                >
                  <IconFolder size={13} />
                  <span class="proj-name">{projectDisplayName(p)}</span>
                  <Show when={p.worktree}>
                    <span class="proj-wt" title={`worktree 分支：${p.worktree!.branch}`}>
                      ⎇ {p.worktree!.branch}
                    </span>
                  </Show>
                  <span class="proj-path">{p.path}</span>
                  <button
                    class="proj-remove"
                    title="从列表移除（不删除文件）"
                    onClick={(e) => {
                      e.stopPropagation();
                      void api.removeProject(p.path).then(refreshProjects);
                    }}
                  >
                    <IconX size={12} />
                  </button>
                </div>
              )}
            </For>
            <Show when={filtered().length === 0 && peers().length === 0}>
              <div class="proj-empty">没有匹配的项目</div>
            </Show>
            <Show when={props.onPickRoaming && peers().length > 0}>
              <div class="proj-section">
                <IconBroadcast size={11} />
                漫游到队友（在对方机器上执行，需对方确认）
              </div>
              <For each={peers()}>
                {(p) => (
                  <div class="roam-peer">
                    <div class="roam-peer-head">
                      <IconBroadcast size={12} />
                      <span class="roam-peer-name">{p.name}</span>
                    </div>
                    <For each={p.folders}>
                      {(f) => (
                        <div
                          class={`proj-item roam ${props.roam?.peer.token === p.token && props.roam?.folder === f.path ? "active" : ""}`}
                          onClick={() => pickRoaming(p, f.path)}
                          title={`${p.name}：${f.path}`}
                        >
                          <IconBroadcast size={13} />
                          <span class="proj-name">{f.name || basename(f.path)}</span>
                          <span class="proj-path">{f.path}</span>
                        </div>
                      )}
                    </For>
                    <Show when={p.folders.length === 0}>
                      <div class="roam-peer-empty">对方暂未共享可漫游的项目</div>
                    </Show>
                  </div>
                )}
              </For>
            </Show>
          </div>
        <button class="proj-browse" onClick={() => void browse()}>
          <IconPlus size={13} />
          使用现有文件夹…
        </button>
      </div>
    </div>
  );
}
