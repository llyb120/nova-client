import { getVersion } from "@tauri-apps/api/app";
import { confirm, message } from "@tauri-apps/plugin-dialog";
import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { api } from "../ipc";
import { latestFireStage } from "../threadDisplay";
import type { ThreadMeta, Worktree } from "../types";
import {
  checkAndStageUpdate,
  closeThread,
  deleteProjectThreads,
  deleteThread,
  openNewSession,
  openThread,
  setTrainingProject,
  setView,
  state,
} from "../store";
import { agentLabel, agentShort, isScratch, scratchParent } from "../utils";
import {
  IconBell,
  IconBroadcast,
  IconCheck,
  IconChevron,
  IconClue,
  IconBrowser,
  IconDownload,
  IconFolder,
  IconGear,
  IconLogo,
  IconMerge,
  IconPlus,
  IconTerminal,
  IconTrash,
  IconTrophy,
  IconX,
} from "./icons";
import { TypewriterText } from "./TypewriterText";

const COLLAPSED_THREAD_LIMIT = 5;

function basename(p: string) {
  return p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || p;
}

function groupName(cwd: string): string {
  return isScratch(cwd) ? "临时会话" : basename(cwd);
}

function fmtTime(ts: number): string {
  const d = new Date(ts);
  const now = new Date();
  const sameDay = d.toDateString() === now.toDateString();
  if (sameDay) {
    return d.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
  }
  return d.toLocaleDateString("zh-CN", { month: "2-digit", day: "2-digit" });
}

export function Sidebar(props: {
  onOpenSettings: () => void;
  onOpenAchievements: () => void;
  onOpenUpdate: () => void;
  onOpenInbox: () => void;
}) {
  const [version, setVersion] = createSignal("");
  let updateCheckTimer: number | undefined;
  let updateCheckClick = 0;

  onMount(() => void getVersion().then(setVersion));
  onCleanup(() => {
    updateCheckClick += 1;
    if (updateCheckTimer !== undefined) window.clearTimeout(updateCheckTimer);
  });

  const checkUpdateSilently = () => {
    const click = ++updateCheckClick;
    if (updateCheckTimer !== undefined) window.clearTimeout(updateCheckTimer);
    updateCheckTimer = window.setTimeout(() => {
      updateCheckTimer = undefined;
      if (click !== updateCheckClick) return;
      void checkAndStageUpdate().catch(() => {
        // 静默检查失败不打断当前操作。
      });
    }, 300);
  };

  const myToken = () => state.settings?.relayToken ?? "";
  // 本群组在线名单（含自己）：服务端已按群组过滤；自己置顶，渲染时标注「我」。
  const onlinePeers = createMemo(() => {
    const me = myToken();
    return [...state.peers.filter((p) => p.online)].sort(
      (a, b) => (a.token === me ? -1 : b.token === me ? 1 : 0),
    );
  });
  const onlineCount = createMemo(() => onlinePeers().length);
  // 主区域切换：证据链只是右侧页面；左侧仍沿用普通会话卷宗。
  const switchView = (view: "home" | "clues" | "workflows" | "training" | "browser") => {
    setView(view);
    closeThread();
  };
  const openHome = () => openNewSession();
  const openClues = () => switchView("clues");
  const openTraining = () => {
    const recent = state.threads.find((thread) => !thread.experienceThread);
    const cwd = recent?.worktree?.repo || recent?.cwd || state.projects[0]?.worktree?.repo || state.projects[0]?.path || "";
    if (!cwd) {
      void message("请先创建或选择一个项目，再打开大熊座。", { kind: "info" });
      return;
    }
    setTrainingProject(cwd);
    switchView("training");
  };
  const openWorkflows = () => switchView("workflows");
  const openBrowser = () => switchView("browser");

  const isTrainingView = () => state.view === "training";
  const isBrowserView = () => state.view === "browser";

  const openHistoryThread = async (id: string) => {
    const thread = state.threads.find((item) => item.id === id);
    // 大熊座、双子座会话各自保持对应 tab；普通会话回到普通模式。
    if (thread?.experienceThread) setTrainingProject(thread.worktree?.repo || thread.cwd);
    setView(thread?.experienceThread ? "training" : thread?.browserThread ? "browser" : "home");
    await openThread(id);
  };

  // 按目录分组。worktree 会话的 cwd 是 uuid 工作目录，不适合展示/分组：
  // 归到源仓库组，用分支 badge 区分。（guest 漫游会话仍按对方目录分组。）
  const groupByCwd = (threads: typeof state.threads) => {
    const map = new Map<string, typeof state.threads>();
    const byId = new Map(threads.map((t) => [t.id, t]));
    const rawKey = (t: ThreadMeta) =>
      t.worktree?.path
        ? t.worktree.repo
        : isScratch(t.cwd)
          ? scratchParent(t.cwd)
          : t.cwd;
    for (const t of threads) {
      const parent = t.parentThreadId ? byId.get(t.parentThreadId) : null;
      // 子会话无论是否在 worktree/新 cwd 中执行，都归到父会话所在分组。
      const key = parent ? rawKey(parent) : rawKey(t);
      const list = map.get(key) ?? [];
      if (list.length === 0) map.set(key, list);
      list.push(t);
    }
    return [...map.entries()];
  };

  const currentGroups = createMemo(() => {
    const threads = isTrainingView()
      ? state.threads.filter((t) => t.experienceThread)
      : isBrowserView()
        ? state.threads.filter((t) => t.browserThread)
        : state.threads.filter((t) => !t.experienceThread && !t.browserThread);
    return groupByCwd(threads);
  });

  type ThreadTreeRow = {
    thread: ThreadMeta;
    child: boolean;
    childCount: number;
    mergedChild?: ThreadMeta;
    chainUpdatedAt?: number;
  };
  const threadTreeRows = (threads: ThreadMeta[]): ThreadTreeRow[] => {
    const byId = new Map(threads.map((t) => [t.id, t]));
    const children = new Map<string, ThreadMeta[]>();
    for (const thread of threads) {
      if (!thread.parentThreadId || !byId.has(thread.parentThreadId)) continue;
      const list = children.get(thread.parentThreadId) ?? [];
      list.push(thread);
      children.set(thread.parentThreadId, list);
    }
    const descendants = (rootId: string) => {
      const result: ThreadMeta[] = [];
      const pending = [...(children.get(rootId) ?? [])];
      while (pending.length > 0) {
        const thread = pending.shift()!;
        result.push(thread);
        pending.push(...(children.get(thread.id) ?? []));
      }
      return result;
    };
    // 左侧每条任务链只显示根会话；各阶段由会话右侧的 Stage 导航切换。
    const rows = threads
      .filter((thread) => !thread.parentThreadId || !byId.has(thread.parentThreadId))
      .map((thread) => ({
        thread,
        child: false,
        childCount: 0,
        chainUpdatedAt: Math.max(
          thread.updatedAt,
          ...descendants(thread.id).map((item) => item.updatedAt),
        ),
      }));
    rows.sort((a, b) => b.chainUpdatedAt - a.chainUpdatedAt);
    return rows;
  };
  const showHistoryByTime = () => state.settings?.historyDisplayMode === "time";
  const timeRows = createMemo(() => {
    const effectiveUpdatedAt = (row: ThreadTreeRow) =>
      row.chainUpdatedAt ?? Math.max(row.thread.updatedAt, row.mergedChild?.updatedAt ?? 0);
    return currentGroups()
      .flatMap(([cwd, threads]) =>
        threadTreeRows(threads).map((row) => ({ ...row, cwd })),
      )
      .sort((a, b) => effectiveUpdatedAt(b) - effectiveUpdatedAt(a));
  });

  const descendantCount = (id: string) => {
    let count = 0;
    const seen = new Set<string>([id]);
    let changed = true;
    while (changed) {
      changed = false;
      for (const t of state.threads) {
        if (t.parentThreadId && seen.has(t.parentThreadId) && !seen.has(t.id)) {
          seen.add(t.id);
          count += 1;
          changed = true;
        }
      }
    }
    return count;
  };

  const remove = async (id: string, title: string) => {
    const childCount = descendantCount(id);
    const ok = await confirm(
      `删除会话「${title}」？聊天记录将一并删除。${childCount > 0 ? `\n\n该任务下的 ${childCount} 个阶段会话也会一起删除。` : ""}`,
      {
      title: "删除会话",
      kind: "warning",
      },
    );
    if (ok) await deleteThread(id);
  };

  const [deletingGroup, setDeletingGroup] = createSignal<string | null>(null);
  const [expandedGroups, setExpandedGroups] = createSignal<Set<string>>(new Set());
  const toggleGroup = (cwd: string) => {
    setExpandedGroups((current) => {
      const next = new Set(current);
      if (next.has(cwd)) next.delete(cwd);
      else next.add(cwd);
      return next;
    });
  };
  const removeGroup = async (cwd: string, threads: typeof state.threads) => {
    const deletable = threads.filter((t) => !state.running[t.id] && !t.starred);
    const ids = deletable.map((t) => t.id);
    if (ids.length === 0 || deletingGroup()) return;
    const starredCount = threads.filter((t) => t.starred).length;
    const runningCount = threads.filter((t) => !t.starred && state.running[t.id]).length;
    const name = groupName(cwd);
    const ok = await confirm(
      `删除「${name}」里的非星标会话？聊天记录将一并删除。${starredCount > 0 ? `${starredCount} 个星标会话会保留。` : ""}${runningCount > 0 ? "运行中的会话会保留。" : ""}`,
      {
        title: "批量删除会话",
        kind: "warning",
      },
    );
    if (!ok) return;
    setDeletingGroup(cwd);
    try {
      await deleteProjectThreads(ids);
    } finally {
      setDeletingGroup(null);
    }
  };

  // 文件夹右键菜单（打开终端/资源管理器）
  const [menu, setMenu] = createSignal<{
    x: number;
    y: number;
    path: string;
    remote: boolean;
  } | null>(null);
  // worktree 会话右键菜单（合并到分支等）
  const [tmenu, setTmenu] = createSignal<{
    x: number;
    y: number;
    id: string;
    wt: Worktree;
    running: boolean;
  } | null>(null);
  const closeMenu = () => {
    setMenu(null);
    setTmenu(null);
  };
  const onDocDown = (e: MouseEvent) => {
    if (!(e.target as HTMLElement).closest(".ctx-menu")) closeMenu();
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") closeMenu();
  };
  document.addEventListener("mousedown", onDocDown);
  document.addEventListener("keydown", onKey);
  onCleanup(() => {
    document.removeEventListener("mousedown", onDocDown);
    document.removeEventListener("keydown", onKey);
  });

  // ===== worktree 会话：合并到指定分支（冲突交给该会话的 AI 自动解决）=====
  const [mergeFor, setMergeFor] = createSignal<{ id: string; wt: Worktree } | null>(null);
  const [mergeBranches, setMergeBranches] = createSignal<string[]>([]);
  const [mergeTarget, setMergeTarget] = createSignal("");
  const [merging, setMerging] = createSignal(false);

  const openMergeModal = async (id: string, wt: Worktree) => {
    closeMenu();
    try {
      const bl = await api.listBranches(wt.repo);
      const list = bl.branches.filter((b) => b !== wt.branch);
      if (list.length === 0) {
        void message("仓库里没有其它分支可作为合并目标。", { kind: "info" });
        return;
      }
      setMergeBranches(list);
      // 默认目标：主仓库当前检出的分支（最常见的「合回主线」场景）
      setMergeTarget(list.includes(bl.current) ? bl.current : list[0]);
      setMergeFor({ id, wt });
    } catch (e) {
      void message(String(e), { kind: "error" });
    }
  };

  const doMerge = async () => {
    const m = mergeFor();
    const target = mergeTarget();
    if (!m || !target || merging()) return;
    setMerging(true);
    try {
      const r = await api.mergeWorktreeThread(m.id, target);
      setMergeFor(null);
      if (r === "merged") {
        void message(`已将分支 ${m.wt.branch} 合并到 ${target}。`, { kind: "info" });
      } else {
        // 有冲突：现场已交给该会话的 AI，打开会话让用户旁观处理进展
        await openThread(m.id);
        void message(
          `合并到 ${target} 出现冲突，已交给该会话的 AI 自动解决并完成合并，请在会话中关注进展。`,
          { kind: "warning" },
        );
      }
    } catch (e) {
      void message(String(e), { kind: "error" });
    } finally {
      setMerging(false);
    }
  };

  // 单条会话行：用户会话与员工会话共用同一渲染。
  const ThreadRow = (
    t: (typeof state.threads)[number],
    child = false,
    childCount = 0,
    mergedChild?: ThreadMeta,
    historyProject?: string,
  ) => {
    const activeThread = mergedChild ?? t;
    const chainThreads = () => {
      const ids = new Set<string>([t.id]);
      let changed = true;
      while (changed) {
        changed = false;
        for (const thread of state.threads) {
          if (thread.parentThreadId && ids.has(thread.parentThreadId) && !ids.has(thread.id)) {
            ids.add(thread.id);
            changed = true;
          }
        }
      }
      return state.threads.filter((thread) => ids.has(thread.id));
    };
    const running = () => chainThreads().some((thread) => !!state.running[thread.id]);
    const active = () => !!state.currentId && chainThreads().some((thread) => thread.id === state.currentId);
    const title = () => mergedChild?.title ?? t.title;
    const updatedAt = () => Math.max(...chainThreads().map((thread) => thread.updatedAt));
    return (
      <div
        class="thread-item"
        classList={{
          active: active(),
          "history-time": !!historyProject,
          child,
          parent: childCount > 0,
          starred: !!(t.starred || mergedChild?.starred),
        }}
        onClick={() =>
          void openHistoryThread(
            latestFireStage(state.threads, activeThread)?.id ?? activeThread.id,
          )
        }
        onContextMenu={(e) => {
          e.preventDefault();
          if (activeThread.worktree?.path) {
            setTmenu({
              x: Math.min(e.clientX, window.innerWidth - 200),
              y: Math.min(e.clientY, window.innerHeight - 120),
              id: activeThread.id,
              wt: activeThread.worktree,
              running: running(),
            });
          } else {
            setMenu({
              x: Math.min(e.clientX, window.innerWidth - 180),
              y: Math.min(e.clientY, window.innerHeight - 90),
              path: activeThread.cwd,
              remote: activeThread.roamingRole === "guest",
            });
          }
        }}
      >
        <Show when={child}>
          <span class="thread-tree-mark" title="属于上方预检会话">
            └
          </span>
        </Show>
        <span class={`thread-agent ${t.agentKind}`} title={`Wake · ${agentLabel(t.agentKind)}`}>
          {agentShort(t.agentKind)}
        </span>
        <Show when={mergedChild}>
          <span class="thread-pair-arrow">→</span>
          <span
            class={`thread-agent ${mergedChild!.agentKind}`}
            title={`Do · ${agentLabel(mergedChild!.agentKind)}`}
          >
            {agentShort(mergedChild!.agentKind)}
          </span>
        </Show>
        <span class="thread-run-slot">
          <Show when={running()}>
            <span class="spinner small" />
          </Show>
        </span>
        <span class="thread-content">
          <TypewriterText
            class="thread-title"
            text={title()}
            title={title()}
            animate={state.titleTyping[t.id] || !!(mergedChild && state.titleTyping[mergedChild.id])}
          />
          <Show when={historyProject}>
            <span
              class="thread-history-meta"
              title={`项目：${historyProject}\n模型：${activeThread.model || "默认模型"}`}
            >
              {historyProject} · {activeThread.model || "默认模型"}
            </span>
          </Show>
        </span>
        <Show when={activeThread.worktree}>
          <span
            class="thread-worktree"
            title={`在 worktree 中执行 · 分支：${activeThread.worktree!.branch}`}
          >
            ⎇ {activeThread.worktree!.branch}
          </span>
        </Show>
        <Show when={childCount > 0}>
          <span class="thread-tree-badge" title={`该预检会话下有 ${childCount} 个开发子会话`}>
            预检 · {childCount}
          </span>
        </Show>
        <Show when={t.ephemeral}>
          <span class="thread-ephemeral" title="临时会话：程序关闭时自动删除">
            临时
          </span>
        </Show>
        <span class="thread-time">{fmtTime(updatedAt())}</span>
        <button
          class="thread-delete"
          title="删除会话"
          onClick={(e) => {
            e.stopPropagation();
            void remove(t.id, title());
          }}
        >
          <IconTrash size={13} />
        </button>
      </div>
    );
  };

  return (
    <aside class="sidebar">
      <div class="sidebar-head">
        <div class="brand">
          <IconLogo size={20} class="brand-icon" />
          <span class="brand-name">Nova</span>
          <Show when={version()}>
            <button
              type="button"
              class="brand-version"
              title="点击静默检查更新"
              onClick={checkUpdateSilently}
            >
              v{version()}
            </button>
          </Show>
          <span class="brand-spacer" />
          <Show when={state.relay.enabled}>
            <div class="relay-badge-wrap">
              <span class={`head-badge relay ${state.relay.connected ? "on" : "off"}`}>
                <IconBroadcast size={14} />
                <Show when={state.relay.connected && onlineCount() > 0}>
                  <span class="relay-count">{onlineCount()}</span>
                </Show>
              </span>
              <div class="relay-pop">
                <Show
                  when={state.relay.connected}
                  fallback={<div class="relay-pop-empty">团队中转站未连接</div>}
                >
                  <div class="relay-pop-head">
                    本群组在线 · {onlinePeers().length} 人
                  </div>
                  <Show
                    when={onlinePeers().length > 0}
                    fallback={<div class="relay-pop-empty">暂无在线成员</div>}
                  >
                    <For each={onlinePeers()}>
                      {(p) => (
                        <div class="relay-pop-peer" title={p.name}>
                          <span class="relay-pop-dot" />
                          <span class="relay-pop-name">{p.name}</span>
                          <Show when={p.token === myToken()}>
                            <span class="relay-pop-me">我</span>
                          </Show>
                        </div>
                      )}
                    </For>
                  </Show>
                </Show>
              </div>
            </div>
          </Show>
          <Show when={state.inbox.length > 0}>
            <button
              class="head-badge alert"
              title={`收到 ${state.inbox.length} 个 Flow`}
              onClick={props.onOpenInbox}
            >
              <IconBell size={14} />
              <span class="badge-count">{state.inbox.length}</span>
            </button>
          </Show>
          <Show when={state.update?.staged || state.updateStaging}>
            <button
              class={`head-badge update ${state.updateStaging ? "busy" : "ready"}`}
              title={state.updateStaging ? "正在下载新版本…" : "新版本已就绪，点击更新"}
              onClick={props.onOpenUpdate}
            >
              <Show when={state.updateStaging} fallback={<IconDownload size={14} />}>
                <span class="spinner small" />
              </Show>
            </button>
          </Show>
        </div>
        <button class="new-thread-btn" onClick={openHome}>
          <IconPlus size={15} />
          新对话
        </button>
        <div class="mode-stack" role="tablist" aria-label="主区域">
          <div class="mode-seg">
            <button
              class="mode-seg-btn"
              classList={{ active: state.view === "home" }}
              onClick={openHome}
              title="普通模式：查看你自己的会话"
            >
              普通模式
            </button>
            <button
              class="mode-seg-btn"
              classList={{ active: state.view === "workflows" }}
              onClick={openWorkflows}
              title={
                state.workflowInbox.length > 0
                  ? `有 ${state.workflowInbox.length} 个队友分享的工作流待接收`
                  : "配置可编排的工作流：阶段接力、转移条件与提示词模板"
              }
            >
              <IconMerge size={14} />
              工作流
              <Show when={state.workflowInbox.length > 0}>
                <span class="badge-count">{state.workflowInbox.length}</span>
              </Show>
            </button>
            <button
              class="mode-seg-btn"
              classList={{ active: state.view === "clues" }}
              onClick={openClues}
              title="打开证据链页面"
            >
              <IconClue size={14} />
              证据链
              <Show when={state.unreadClueMentions.length > 0}>
                <span class="mode-seg-badge alert">{state.unreadClueMentions.length}</span>
              </Show>
            </button>
          </div>
          <div class="mode-seg secondary">
            <button
              class="mode-seg-btn"
              classList={{ active: state.view === "training" }}
              onClick={openTraining}
              title="大熊座：查看隔离的训练记录、北斗七星专家经验，并进行点赞点踩"
            >
              大熊座
            </button>
            <button
              class="mode-seg-btn"
              classList={{ active: state.view === "browser" }}
              onClick={openBrowser}
              title="内嵌浏览器：录制操作、框选标记并编排为 Playwright 计划"
            >
              <IconBrowser size={14} />
              双子座
            </button>
          </div>

        </div>
      </div>

      <div class="thread-list">
        <Show
          when={currentGroups().length > 0}
          fallback={
            <div class="thread-empty">
              {isTrainingView()
                ? "还没有训练会话。点击右侧“立即训练”开始。"
                : isBrowserView()
                  ? "还没有双子座执行会话。运行一个片段后会显示在这里。"
                  : "还没有会话。在右侧输入任务开始。"}
            </div>
          }
        >
          <Show when={showHistoryByTime()}>
            <For each={timeRows()}>
              {(row) => ThreadRow(row.thread, false, 0, row.mergedChild, groupName(row.cwd))}
            </For>
          </Show>
          <Show when={!showHistoryByTime()}>
            <For each={currentGroups()}>
              {([cwd, threads]) => {
                const guestThread = threads.find((t) => t.roamingRole === "guest");
                const isRemote = !!guestThread;
                const peerName = guestThread?.roamingPeerName ?? "";
                const rows = createMemo(() => threadTreeRows(threads));
                const expanded = () => expandedGroups().has(cwd);
                const collapsible = () => rows().length > COLLAPSED_THREAD_LIMIT;
                const visibleRows = () =>
                  collapsible() && !expanded()
                    ? rows().slice(0, COLLAPSED_THREAD_LIMIT)
                    : rows();
                return (
                  <div class="thread-group">
                    <div
                      class="group-label"
                      title={
                        isRemote
                          ? `${peerName} 的目录（漫游，只读）\n${cwd}`
                          : `${cwd}\n右键：打开终端 / 资源管理器`
                      }
                      onContextMenu={(e) => {
                        e.preventDefault();
                        setMenu({
                          x: Math.min(e.clientX, window.innerWidth - 180),
                          y: Math.min(e.clientY, window.innerHeight - 90),
                          path: cwd,
                          remote: isRemote,
                        });
                      }}
                    >
                      {isRemote ? <IconBroadcast size={12} /> : <IconFolder size={12} />}
                      <span class="group-name">{groupName(cwd)}</span>
                      <Show when={isRemote}>
                        <span class="group-roam" title={`在 ${peerName} 的机器上执行`}>
                          @{peerName}
                        </span>
                      </Show>
                      <button
                        class="group-delete"
                        title="删除该文件夹里的会话"
                        disabled={
                          deletingGroup() === cwd ||
                          threads.every((t) => state.running[t.id] || t.starred)
                        }
                        onClick={(e) => {
                          e.stopPropagation();
                          void removeGroup(cwd, threads);
                        }}
                      >
                        <IconTrash size={12} />
                      </button>
                    </div>
                    <For each={visibleRows()}>
                      {(row) => ThreadRow(row.thread, row.child, row.childCount, row.mergedChild)}
                    </For>
                    <Show when={collapsible()}>
                      <button
                        type="button"
                        class="thread-group-toggle"
                        classList={{ expanded: expanded() }}
                        aria-expanded={expanded()}
                        aria-label={
                          expanded()
                            ? "收起会话"
                            : `展开其余 ${rows().length - COLLAPSED_THREAD_LIMIT} 个会话`
                        }
                        title={
                          expanded()
                            ? `收起到最近 ${COLLAPSED_THREAD_LIMIT} 个会话`
                            : `展开其余 ${rows().length - COLLAPSED_THREAD_LIMIT} 个会话`
                        }
                        onClick={() => toggleGroup(cwd)}
                      >
                        <IconChevron size={14} open />
                      </button>
                    </Show>
                  </div>
                );
              }}
            </For>
          </Show>
        </Show>
      </div>

      <div class="sidebar-foot">
        <div class="sidebar-foot-actions">
          <button class="settings-btn" onClick={props.onOpenSettings}>
            <IconGear size={15} />
            设置
          </button>
          <button class="settings-btn" onClick={props.onOpenAchievements}>
            <IconTrophy size={15} />
            成就
            <Show when={state.unseenAchievementIds.length > 0}>
              <span class="sidebar-badge">{state.unseenAchievementIds.length}</span>
            </Show>
          </button>
        </div>
      </div>

      <Show when={tmenu()}>
        <div class="ctx-menu" style={{ left: `${tmenu()!.x}px`, top: `${tmenu()!.y}px` }}>
          <Show
            when={!tmenu()!.running}
            fallback={<div class="ctx-note">会话正在运行，停止后才能合并</div>}
          >
            <button
              class="ctx-item"
              onClick={() => void openMergeModal(tmenu()!.id, tmenu()!.wt)}
            >
              <IconMerge size={13} />
              合并到分支…
            </button>
          </Show>
          <button
            class="ctx-item"
            onClick={() => {
              void api.openInTerminal(tmenu()!.wt.path);
              closeMenu();
            }}
          >
            <IconTerminal size={13} />
            在终端中打开 worktree
          </button>
          <button
            class="ctx-item"
            onClick={() => {
              void api.openInExplorer(tmenu()!.wt.path);
              closeMenu();
            }}
          >
            <IconFolder size={13} />
            在资源管理器中打开 worktree
          </button>
        </div>
      </Show>

      <Show when={mergeFor()}>
        <div class="modal-backdrop" onClick={() => !merging() && setMergeFor(null)}>
          <div class="modal merge-modal" onClick={(e) => e.stopPropagation()}>
            <div class="modal-head">
              <span>合并分支</span>
              <button class="icon-btn" disabled={merging()} onClick={() => setMergeFor(null)}>
                <IconX size={16} />
              </button>
            </div>
            <div class="modal-body">
              <div class="merge-desc">
                把 worktree 分支 <b>⎇ {mergeFor()!.wt.branch}</b> 合并到：
              </div>
              <div class="merge-branches">
                <For each={mergeBranches()}>
                  {(b) => (
                    <button
                      class="merge-branch"
                      classList={{ on: mergeTarget() === b }}
                      disabled={merging()}
                      onClick={() => setMergeTarget(b)}
                    >
                      <span class="merge-branch-name">⎇ {b}</span>
                      <Show when={mergeTarget() === b}>
                        <IconCheck size={13} />
                      </Show>
                    </button>
                  )}
                </For>
              </div>
              <div class="merge-hint">
                合并前请确认 worktree 里的改动都已提交。若合并产生冲突，会自动交给该会话的 AI
                解决并完成合并提交。
              </div>
            </div>
            <div class="modal-foot">
              <button class="btn secondary" disabled={merging()} onClick={() => setMergeFor(null)}>
                取消
              </button>
              <button class="btn primary" disabled={merging() || !mergeTarget()} onClick={() => void doMerge()}>
                <Show when={merging()} fallback={<IconMerge size={14} />}>
                  <span class="spinner small" />
                </Show>
                合并
              </button>
            </div>
          </div>
        </div>
      </Show>

      <Show when={menu()}>
        <div class="ctx-menu" style={{ left: `${menu()!.x}px`, top: `${menu()!.y}px` }}>
          <Show
            when={!menu()!.remote}
            fallback={<div class="ctx-note">队友的目录，无法在本机操作</div>}
          >
            <button
              class="ctx-item"
              onClick={() => {
                void api.openInTerminal(menu()!.path);
                closeMenu();
              }}
            >
              <IconTerminal size={13} />
              在终端中打开
            </button>
            <button
              class="ctx-item"
              onClick={() => {
                void api.openInExplorer(menu()!.path);
                closeMenu();
              }}
            >
              <IconFolder size={13} />
              在资源管理器中打开
            </button>
          </Show>
        </div>
      </Show>
    </aside>
  );
}
