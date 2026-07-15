import { getVersion } from "@tauri-apps/api/app";
import { confirm, message } from "@tauri-apps/plugin-dialog";
import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { api } from "../ipc";
import { firstWakeDoChild } from "../threadDisplay";
import type { ThreadMeta, Worktree } from "../types";
import {
  checkAndStageUpdate,
  closeThread,
  deleteThread,
  deleteThreads,
  openThread,
  pendingDecisionCount,
  refreshQuota,
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
  IconDownload,
  IconFolder,
  IconGear,
  IconLogo,
  IconMerge,
  IconPlus,
  IconTerminal,
  IconTrash,
  IconUsers,
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

function fmtReset(unix: number | null): string {
  if (!unix) return "";
  const d = new Date(unix * 1000);
  return d.toLocaleString("zh-CN", { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });
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

function isMindThread(t: ThreadMeta): boolean {
  return !!t.mindThread || /\]\s*Mind(?:\s|$|·)/.test(t.title);
}

export function Sidebar(props: {
  onOpenSettings: () => void;
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
  // 数字员工：配置入口，无日常操作角标
  // 御书房：候旨 + 进行中任务，作为日常操作入口角标
  const workbenchBadge = createMemo(() => {
    const decisions = pendingDecisionCount();
    const active = state.employeeTasks.filter(
      (t) => t.status === "queued" || t.status === "working",
    ).length;
    return decisions + active;
  });
  // 主区域切换：证据链只是右侧页面；左侧仍沿用普通会话卷宗。
  const switchView = (view: "home" | "clues" | "employees" | "workbench") => {
    setView(view);
    closeThread();
  };
  const openHome = () => switchView("home");
  const openClues = () => switchView("clues");
  const openEmployees = () => switchView("employees");
  const openWorkbench = () => switchView("workbench");

  // 数字员工（配置）/ 御书房（日常）视图下，左侧切换为「员工会话」这一卷。
  const isEmployeeView = () => state.view === "employees" || state.view === "workbench";

  const openHistoryThread = async (id: string) => {
    if (state.view === "clues") setView("home");
    await openThread(id);
  };

  // 按目录分组（普通会话与员工会话共用同一套分组/结构，只是各看各的、不混在一起）。
  // worktree 会话的 cwd 是 uuid 工作目录，不适合展示/分组：归到源仓库组，用分支 badge 区分。
  // （guest 漫游会话 worktree.path 为空，真实目录在对端，仍按对方目录分组。）
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
      // 子会话无论是否在 worktree/新 cwd 中执行，都归到预检父会话所在分组，
      // 这样左侧能稳定显示为一棵“预检 → 开发”树。
      const key = parent ? rawKey(parent) : rawKey(t);
      const list = map.get(key) ?? [];
      if (list.length === 0) map.set(key, list);
      list.push(t);
    }
    return [...map.entries()];
  };

  // 当前这一卷：员工视图看员工产生的会话，否则看用户自己的会话。结构完全一致。
  const currentGroups = createMemo(() =>
    isEmployeeView()
      ? groupByCwd(state.threads.filter((t) => t.employeeId && !isMindThread(t)))
      : groupByCwd(state.threads.filter((t) => !t.employeeId)),
  );

  type ThreadTreeRow = {
    thread: ThreadMeta;
    child: boolean;
    childCount: number;
    mergedChild?: ThreadMeta;
  };
  const threadTreeRows = (threads: ThreadMeta[]): ThreadTreeRow[] => {
    const byId = new Map(threads.map((t) => [t.id, t]));
    const children = new Map<string, ThreadMeta[]>();
    for (const t of threads) {
      const parent = t.parentThreadId;
      if (!parent || !byId.has(parent)) continue;
      const list = children.get(parent) ?? [];
      list.push(t);
      children.set(parent, list);
    }
    const rows: ThreadTreeRow[] = [];
    const add = (t: ThreadMeta, child: boolean) => {
      const kids = children.get(t.id) ?? [];
      const mergedChild = firstWakeDoChild(threads, t);
      const visibleKids = mergedChild ? kids.filter((kid) => kid.id !== mergedChild.id) : kids;
      rows.push({ thread: t, child, childCount: visibleKids.length, mergedChild });
      for (const kid of visibleKids) add(kid, true);
    };
    const roots = threads.filter((t) => !t.parentThreadId || !byId.has(t.parentThreadId));
    roots.sort((a, b) => {
      const aUpdatedAt = Math.max(a.updatedAt, firstWakeDoChild(threads, a)?.updatedAt ?? 0);
      const bUpdatedAt = Math.max(b.updatedAt, firstWakeDoChild(threads, b)?.updatedAt ?? 0);
      return bUpdatedAt - aUpdatedAt;
    });
    for (const root of roots) add(root, false);
    return rows;
  };

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
      `删除会话「${title}」？聊天记录将一并删除。${childCount > 0 ? `\n\n该预检会话下的 ${childCount} 个子会话也会一起删除。` : ""}`,
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
    const deletable = threads.filter((t) => !state.running[t.id]);
    const ids = deletable.map((t) => t.id);
    if (ids.length === 0 || deletingGroup()) return;
    const runningCount = threads.length - deletable.length;
    const name = groupName(cwd);
    const ok = await confirm(
      `删除「${name}」里的 ${ids.length} 个会话？聊天记录将一并删除。${runningCount > 0 ? "运行中的会话会保留。" : ""}`,
      {
        title: "批量删除会话",
        kind: "warning",
      },
    );
    if (!ok) return;
    setDeletingGroup(cwd);
    try {
      await deleteThreads(ids);
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
  ) => {
    const activeThread = mergedChild ?? t;
    const running = () =>
      !!state.running[t.id] || !!(mergedChild && state.running[mergedChild.id]);
    const active = () => state.currentId === t.id || state.currentId === mergedChild?.id;
    const title = () => mergedChild?.title ?? t.title;
    const updatedAt = () => Math.max(t.updatedAt, mergedChild?.updatedAt ?? 0);
    return (
      <div
        class={`thread-item ${active() ? "active" : ""}`}
        classList={{ child, parent: childCount > 0 }}
        onClick={() => void openHistoryThread(activeThread.id)}
        onContextMenu={(e) => {
          // 目前只有 worktree 会话有右键动作（合并到分支）；漫游 guest 的 worktree 在对端，不提供
          if (!activeThread.worktree?.path) return;
          e.preventDefault();
          setTmenu({
            x: Math.min(e.clientX, window.innerWidth - 200),
            y: Math.min(e.clientY, window.innerHeight - 90),
            id: activeThread.id,
            wt: activeThread.worktree,
            running: running(),
          });
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
        <TypewriterText
          class="thread-title"
          text={title()}
          title={title()}
          animate={state.titleTyping[t.id] || !!(mergedChild && state.titleTyping[mergedChild.id])}
        />
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
              classList={{ active: state.view === "home" || state.view === "clues" }}
              onClick={openHome}
              title="普通模式：查看你自己的会话"
            >
              普通模式
            </button>
            <button
              class="mode-seg-btn"
              classList={{ open: state.view === "clues" }}
              onClick={openClues}
              title="打开证据链页面；左侧仍保持普通会话"
            >
              <IconClue size={14} />
              证据链
            </button>
          </div>
          <div class="mode-seg secondary">
            <button
              class="mode-seg-btn"
              classList={{ active: state.view === "workbench" }}
              onClick={openWorkbench}
              title="御书房：下旨交办、查看进行中事件、批阅奏折与汇报"
            >
              <IconBell size={14} />
              御书房
              <Show when={workbenchBadge() > 0}>
                <span class="mode-seg-badge alert">{workbenchBadge()}</span>
              </Show>
            </button>
            <button
              class="mode-seg-btn"
              classList={{ active: state.view === "employees" }}
              onClick={openEmployees}
              title="数字员工配置：岗位、心跳、模型与知识库"
            >
              <IconUsers size={14} />
              数字员工
            </button>
          </div>
        </div>
      </div>

      <div class="thread-list">
        <Show
          when={currentGroups().length > 0}
          fallback={
            <div class="thread-empty">
              <Show
                when={isEmployeeView()}
                fallback="还没有会话。在右侧输入任务开始。"
              >
                数字员工还没有留下会话。
                <br />
                在御书房下旨后，员工巡查与工作的足迹会在此处留痕。
              </Show>
            </div>
          }
        >
          <For each={currentGroups()}>
            {([cwd, threads]) => {
              const guestThread = threads.find((t) => t.roamingRole === "guest");
              const isRemote = !!guestThread;
              const peerName = guestThread?.roamingPeerName ?? "";
              const rows = createMemo(() => threadTreeRows(threads));
              const expanded = () => expandedGroups().has(cwd);
              const collapsible = () =>
                !isEmployeeView() && rows().length > COLLAPSED_THREAD_LIMIT;
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
                        deletingGroup() === cwd || threads.every((t) => state.running[t.id])
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
      </div>

      <div class="sidebar-foot">
        <Show when={state.quota}>
          <button
            class="quota-row"
            onClick={() => void refreshQuota()}
            title={`${state.quota!.plan ?? ""} 套餐\n日额度重置：${fmtReset(state.quota!.dailyResetAt)}\n周额度重置：${fmtReset(state.quota!.weeklyResetAt)}${state.quota!.flexCredits != null ? `\n按量积分：${state.quota!.flexCredits}` : ""}\n点击刷新`}
          >
            <span class="quota-label">额度</span>
            <span class="quota-meter">
              <span class={`quota-pct ${state.quota!.dailyPercent < 20 ? "low" : ""}`}>
                日 {Math.round(state.quota!.dailyPercent)}%
              </span>
              <span class={`quota-pct ${state.quota!.weeklyPercent < 20 ? "low" : ""}`}>
                周 {Math.round(state.quota!.weeklyPercent)}%
              </span>
            </span>
          </button>
        </Show>
        <button class="settings-btn" onClick={props.onOpenSettings}>
          <IconGear size={15} />
          设置
        </button>
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
