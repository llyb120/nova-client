import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { api } from "../ipc";
import { refreshProjects, roamingPeers, state } from "../store";
import type { Peer, ProjectEntry } from "../types";
import { isScratch } from "../utils";
import { IconBroadcast, IconChevron, IconFolder, IconPlus, IconX } from "./icons";

const PROJ_POP_WIDTH = 380;
/** 搜索框 + 列表上限 + 浏览按钮，与 CSS 实际高度对齐 */
const PROJ_POP_HEIGHT = 330;

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
  /** 仅允许选择自己的本地项目：隐藏临时会话、条目移除与「使用现有文件夹」入口
   *  （漫游分组由是否传 onPickRoaming 决定）。 */
  ownOnly?: boolean;
  /** Portal + fixed，避免被设置弹窗滚动区裁剪，并按可用空间翻转 */
  portal?: boolean;
}) {
  const [opened, setOpened] = createSignal(false);
  const [query, setQuery] = createSignal("");
  const [placeDown, setPlaceDown] = createSignal(false);
  const [coords, setCoords] = createSignal<{
    left: number;
    top?: number;
    bottom?: number;
    width: number;
  }>({ left: 0, width: PROJ_POP_WIDTH });
  let rootRef: HTMLDivElement | undefined;
  let popRef: HTMLDivElement | undefined;
  let searchRef: HTMLInputElement | undefined;
  let focusFrame: number | undefined;

  const usePortal = () => !!props.portal;

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

  const computePlacement = () => {
    if (!rootRef) return;
    const r = rootRef.getBoundingClientRect();
    const width = Math.min(PROJ_POP_WIDTH, window.innerWidth - 16);
    const height = Math.min(PROJ_POP_HEIGHT, window.innerHeight - 16);
    const spaceBelow = window.innerHeight - r.bottom;
    const spaceAbove = r.top;
    // portal / 显式向下：下方够用则向下，否则翻到上方，避免盖住弹窗底部操作区
    const down = usePortal() || props.popDown
      ? spaceBelow >= height + 8 || (spaceAbove < height + 8 && spaceBelow > spaceAbove)
      : spaceAbove < height + 8 && spaceBelow > spaceAbove;
    setPlaceDown(down);
    if (!usePortal()) return;
    const left = Math.max(8, Math.min(r.left, window.innerWidth - width - 8));
    if (down) {
      const top = Math.max(8, Math.min(r.bottom + 8, window.innerHeight - height - 8));
      setCoords({ left, top, width });
    } else {
      const bottom = Math.max(
        8,
        Math.min(window.innerHeight - r.top + 8, window.innerHeight - height - 8),
      );
      setCoords({ left, bottom, width });
    }
  };

  const toggle = () => {
    const willOpen = !opened();
    if (willOpen) computePlacement();
    setOpened(willOpen);
    cancelPendingFocus();
    if (willOpen) {
      focusFrame = requestAnimationFrame(() => {
        focusFrame = undefined;
        searchRef?.focus({ preventScroll: true });
      });
    }
  };

  createEffect(() => {
    if (!usePortal() || !opened()) return;
    const onReflow = () => computePlacement();
    window.addEventListener("scroll", onReflow, true);
    window.addEventListener("resize", onReflow);
    onCleanup(() => {
      window.removeEventListener("scroll", onReflow, true);
      window.removeEventListener("resize", onReflow);
    });
  });

  const onDocClick = (e: MouseEvent) => {
    const target = e.target as Node;
    if (
      rootRef &&
      !rootRef.contains(target) &&
      (!popRef || !popRef.contains(target))
    ) {
      setOpened(false);
      cancelPendingFocus();
    }
  };
  document.addEventListener("mousedown", onDocClick);
  onCleanup(() => {
    document.removeEventListener("mousedown", onDocClick);
    cancelPendingFocus();
  });

  const renderPop = () => (
    <div
      ref={popRef}
      class={`proj-pop ${!usePortal() && (props.popDown || placeDown()) ? "down" : ""} ${
        usePortal() ? "portal" : ""
      }`}
      style={
        usePortal()
          ? {
              position: "fixed",
              left: `${coords().left}px`,
              top: coords().top !== undefined ? `${coords().top}px` : "auto",
              bottom: coords().bottom !== undefined ? `${coords().bottom}px` : "auto",
              width: `${coords().width}px`,
            }
          : undefined
      }
    >
      <input
        ref={searchRef}
        class="proj-search"
        placeholder="搜索项目"
        value={query()}
        onInput={(e) => setQuery(e.currentTarget.value)}
      />
      <div class="proj-list">
        <Show when={!props.ownOnly}>
          <div
            class={`proj-item scratch ${props.value && isScratch(props.value) ? "active" : ""}`}
            onClick={() => void useScratch()}
            title="不关联项目，在系统临时目录新建一个空目录作为工作区"
          >
            <IconPlus size={13} />
            <span class="proj-name">临时会话（不使用项目）</span>
          </div>
        </Show>
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
              <Show when={!props.ownOnly}>
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
              </Show>
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
      <Show when={!props.ownOnly}>
        <button class="proj-browse" onClick={() => void browse()}>
          <IconPlus size={13} />
          使用现有文件夹…
        </button>
      </Show>
    </div>
  );

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
      <Show when={opened()}>
        {usePortal() ? <Portal mount={document.body}>{renderPop()}</Portal> : renderPop()}
      </Show>
    </div>
  );
}
