import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { TranscriptCanvas, type TranscriptCanvasApi } from "../canvasTranscript/TranscriptCanvas";
import { api } from "../ipc";
import {
  compactThread,
  chatScrollToBottomSignal,
  openThread,
  refreshThreads,
  setState,
  setTimeMachineEditTarget,
  state,
  timeMachineChangedSignal,
} from "../store";
import type { Item, TimeMachineCheckpoint, TimeMachinePrompt, TimeMachineTimeline } from "../types";
import { agentLabel } from "../utils";
import { Composer } from "./Composer";
import { IconBroadcast, IconCompress, IconDownload, IconShare, IconStar, IconStopwatch } from "./icons";
import { PlanActionCard } from "./PlanActionCard";
import { PlanCard } from "./PlanCard";
import { ShareModal } from "./ShareModal";
import { TypewriterText } from "./TypewriterText";
import { fmtTokens, groupItems } from "./TurnGroup";

export function ChatView() {
  const [stickToBottom, setStickToBottom] = createSignal(true);
  let canvasApi: TranscriptCanvasApi | null = null;

  const permissions = createMemo(() =>
    state.permissions.filter((p) => p.threadId === state.currentId),
  );

  const [previewItems, setPreviewItems] = createSignal<Item[] | null>(null);
  const [previewCheckpointId, setPreviewCheckpointId] = createSignal<string | null>(null);
  const [previewFading, setPreviewFading] = createSignal(false);
  let previewRequest = 0;
  let previewTimer: ReturnType<typeof setTimeout> | undefined;
  const displayedItems = () => previewItems() ?? (state.items as Item[]);
  const groups = createMemo<ReturnType<typeof groupItems>>(
    (prev) => groupItems(displayedItems(), prev),
    [],
  );
  const isRunning = () => !!(state.currentId && state.running[state.currentId]);
  const timeStops = createMemo(() => {
    let turn = 0;
    return groups().flatMap((group, index) => {
      if (!group.user) return [];
      turn++;
      const text = group.user.text.replace(/\s+/g, " ").trim();
      return [{ index, turn, label: text || `第 ${turn} 轮` }];
    });
  });
  const travelTo = (index: number) => {
    setStickToBottom(false);
    canvasApi?.scrollToGroup(index);
  };

  const returnToNow = () => {
    canvasApi?.enableBottomFollow();
  };

  // 会话累计 token 用量
  const totalTokens = createMemo(() =>
    state.items.reduce(
      (sum, it) => (it.type === "turn" && it.totalTokens ? sum + it.totalTokens : sum),
      0,
    ),
  );

  // 切换会话时从底部开始；后续尺寸 / 内容变化由 canvas 自身持续对齐。
  createEffect((prevId: string | null | undefined) => {
    const id = state.currentId;
    if (id !== prevId) setStickToBottom(true);
    return id;
  }, undefined);

  // 主动发送新提示词时重新进入吸底，无动画直接显示最新内容。
  createEffect(() => {
    const tick = chatScrollToBottomSignal();
    if (tick === 0) return;
    canvasApi?.enableBottomFollow();
  });
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [showShare, setShowShare] = createSignal(false);
  const [timeline, setTimeline] = createSignal<TimeMachineTimeline | null>(null);
  const [restoringCheckpoint, setRestoringCheckpoint] = createSignal<string | null>(null);

  createEffect(() => {
    const threadId = state.currentId;
    void timeMachineChangedSignal();
    setTimeline(null);
    setPreviewItems(null);
    setPreviewCheckpointId(null);
    setTimeMachineEditTarget(null);
    if (!threadId) return;
    void api.getTimeMachineTimeline(threadId).then((value) => {
      if (state.currentId === threadId) setTimeline(value);
    }).catch(() => {});
  });

  const currentMeta = createMemo(() =>
    state.threads.find((t) => t.id === state.currentId),
  );
  const isFireThread = () => /^\[Fire\]/.test(currentMeta()?.title ?? "");
  const showTimeMachine = () => timeStops().length > 0 && !isFireThread();
  const stageThreads = createMemo(() => {
    const current = currentMeta();
    if (!current) return [];
    const byId = new Map(state.threads.map((thread) => [thread.id, thread]));
    const rootOf = (start: typeof current) => {
      let root = start;
      const seen = new Set<string>();
      while (root.parentThreadId && !seen.has(root.id)) {
        seen.add(root.id);
        const parent = byId.get(root.parentThreadId);
        if (!parent) break;
        root = parent;
      }
      return root;
    };
    const root = rootOf(current);
    const chain = state.threads.filter((thread) => rootOf(thread).id === root.id);
    return chain.sort((a, b) => a.createdAt - b.createdAt);
  });
  const showStageRail = () => stageThreads().length > 1;
  const stageName = (thread: (typeof state.threads)[number]) => {
    if (/\]\s*Wake/.test(thread.title)) return "Wake";
    if (/\]\s*Do/.test(thread.title)) return "Do";
    if (/\]\s*Dream/.test(thread.title)) return "Dream";
    if (/\]\s*巡查/.test(thread.title)) return "巡查";
    const fireJudge = thread.title.match(/^\[Fire\]\s*判断\s+(\d+)/);
    if (fireJudge) return `判断 ${fireJudge[1]}`;
    const fireStage = thread.title.match(/^\[Fire\]\s*阶段\s+(\d+)/);
    if (fireStage) return `阶段 ${fireStage[1]}`;
    if (/^\[Fire\]/.test(thread.title)) return "目标";
    return "事件";
  };
  const jumpToStage = async (threadId: string) => {
    // 每个 stage 都是独立会话；切换 stage 只切换会话，不再拼接 transcript。
    await openThread(threadId);
  };
  const [starUpdating, setStarUpdating] = createSignal(false);
  const roamingRole = () => currentMeta()?.roamingRole ?? null;
  const canStar = () => {
    const meta = currentMeta();
    return !!meta && !meta.employeeId && !meta.mindThread && !meta.roamingRole;
  };
  const toggleStar = async () => {
    const meta = currentMeta();
    if (!meta || starUpdating()) return;
    const starred = !meta.starred;
    setStarUpdating(true);
    setState("threads", (thread) => thread.id === meta.id, "starred", starred);
    try {
      await api.setThreadStarred(meta.id, starred);
    } catch (error) {
      setState("threads", (thread) => thread.id === meta.id, "starred", !starred);
      void message(String(error), { kind: "error" });
    } finally {
      setStarUpdating(false);
    }
  };
  // worktree 会话的 cwd 是 uuid 工作目录，展示时用源仓库路径更直观
  const cwdDisplay = () => currentMeta()?.worktree?.repo || state.cwd;

  const startRename = () => {
    setDraft(state.title);
    setEditing(true);
  };

  type GraphNode = {
    id: string;
    checkpoint: TimeMachineCheckpoint | null;
    previewCheckpoint: TimeMachineCheckpoint | null;
    promptCount: number;
    currentPromptIndex: number | null;
    branchPrompts: TimeMachinePrompt[];
    title: string;
    x: number;
    y: number;
    current: boolean;
    onCurrentPath: boolean;
  };
  type PromptTreeNode = Omit<GraphNode, "x" | "y"> & { children: PromptTreeNode[] };
  type ContextDeleteMode = "to-start" | "up" | "self" | "down" | "to-end";
  const [contextMenu, setContextMenu] = createSignal<{ x: number; y: number; node: GraphNode } | null>(null);
  const timelineGraph = createMemo(() => {
    const checkpoints = timeline()?.checkpoints ?? [];
    const root: PromptTreeNode = {
      id: "__time_root__",
      checkpoint: null,
      previewCheckpoint: null,
      promptCount: 0,
      currentPromptIndex: null,
      branchPrompts: [],
      title: "会话开始",
      current: false,
      onCurrentPath: true,
      children: [],
    };
    const insertPath = (
      prompts: Array<{ id: number; text: string }>,
      checkpoint: TimeMachineCheckpoint | null,
      current: boolean,
    ): PromptTreeNode => {
      let parent = root;
      if (prompts.length === 0 && checkpoint) {
        root.checkpoint = checkpoint;
        root.previewCheckpoint = checkpoint;
      }
      prompts.forEach((prompt, index) => {
        const key = `${prompt.id}:${prompt.text}`;
        let node = parent.children.find((child) => child.id === `${parent.id}/${key}`);
        if (!node) {
          node = {
            id: `${parent.id}/${key}`,
            checkpoint: null,
            previewCheckpoint: checkpoint,
            promptCount: index + 1,
            currentPromptIndex: null,
            branchPrompts: prompts,
            title: prompt.text.trim() || `第 ${index + 1} 条提示词`,
            current: false,
            onCurrentPath: false,
            children: [],
          };
          parent.children.push(node);
        }
        if (!node.previewCheckpoint && checkpoint) node.previewCheckpoint = checkpoint;
        if (checkpoint && !current) node.branchPrompts = prompts;
        if (current) {
          node.onCurrentPath = true;
          node.currentPromptIndex = index;
          node.branchPrompts = prompts;
        }
        parent = node;
      });
      if (checkpoint && prompts.length > 0) parent.checkpoint = checkpoint;
      return parent;
    };
    for (const checkpoint of checkpoints) insertPath(checkpoint.prompts, checkpoint, false);
    const currentPrompts = state.items.flatMap((item) =>
      item.type === "user" ? [{ id: item.id, text: item.text }] : [],
    );
    const currentEnd = insertPath(currentPrompts, null, true);
    currentEnd.current = true;

    // 当前时间线固定占最左一列；每条旁支只在分叉时申请新列，之后沿该列向下。
    const nodes: GraphNode[] = [];
    const edgeIds: Array<{ from: string; to: string }> = [];
    let nextLane = 1;
    let maxPromptCount = currentPrompts.length;
    const place = (node: PromptTreeNode, lane: number) => {
      const nodeLane = node.onCurrentPath ? 0 : lane;
      maxPromptCount = Math.max(maxPromptCount, node.promptCount);
      nodes.push({ ...node, x: 18 + nodeLane * 26, y: 20 + (node.promptCount - 1) * 32 });

      const children = [...node.children].sort(
        (left, right) => Number(right.onCurrentPath) - Number(left.onCurrentPath),
      );
      let continuedBranch = false;
      for (const child of children) {
        edgeIds.push({ from: node.id, to: child.id });
        if (child.onCurrentPath) {
          place(child, 0);
        } else if (!node.onCurrentPath && !continuedBranch) {
          continuedBranch = true;
          place(child, nodeLane);
        } else {
          place(child, nextLane++);
        }
      }
    };
    const roots = [...root.children].sort(
      (left, right) => Number(right.onCurrentPath) - Number(left.onCurrentPath),
    );
    for (const node of roots) place(node, node.onCurrentPath ? 0 : nextLane++);

    const positions = new Map(nodes.map((node) => [node.id, node]));
    const nowY = 20 + Math.max(1, maxPromptCount) * 32;
    const laneCount = Math.max(1, nextLane);
    return {
      nodes,
      edges: edgeIds.flatMap((edge) => {
        const from = positions.get(edge.from);
        const to = positions.get(edge.to);
        return from && to ? [{ from, to, current: from.onCurrentPath && to.onCurrentPath }] : [];
      }),
      laneCount,
      // 左右各留约 8px（节点中心 18、半宽 10），避免图内容偏左、右侧多出一截空白
      width: 36 + (laneCount - 1) * 26,
      height: nowY + 28,
    };
  });
  const timeMachineWidth = () => Math.max(64, 38 + Math.min(5, timelineGraph().laneCount) * 26);
  const switchPreview = (items: Item[] | null, checkpointId: string | null) => {
    if (previewTimer) clearTimeout(previewTimer);
    setPreviewFading(true);
    previewTimer = setTimeout(() => {
      setPreviewItems(items);
      setPreviewCheckpointId(checkpointId);
      requestAnimationFrame(() => setPreviewFading(false));
    }, 90);
  };
  const itemsThroughPrompt = (items: Item[], promptCount: number) => {
    if (promptCount <= 0) return [];
    let seen = 0;
    for (let index = 0; index < items.length; index++) {
      if (items[index].type !== "user") continue;
      seen++;
      if (seen > promptCount) return items.slice(0, index);
    }
    return items;
  };
  const previewGraphNode = async (node: GraphNode) => {
    const threadId = state.currentId;
    if (!threadId || restoringCheckpoint()) return;
    if (node.onCurrentPath) {
      switchPreview(itemsThroughPrompt(state.items as Item[], node.promptCount), node.id);
      return;
    }
    const checkpoint = node.previewCheckpoint;
    if (!checkpoint) return;
    const request = ++previewRequest;
    try {
      const preview = await api.getTimeMachineCheckpointPreview(threadId, checkpoint.id);
      if (request === previewRequest && state.currentId === threadId) {
        switchPreview(itemsThroughPrompt(preview.items as Item[], node.promptCount), node.id);
      }
    } catch {
      // hover 预览失败不打断用户；右键时间跳跃时仍会显示真实恢复错误。
    }
  };
  const scrollToCurrentPrompt = (promptIndex: number) => {
    const scroll = () => {
      const stop = timeStops()[promptIndex];
      if (stop) travelTo(stop.index);
    };
    // 已经位于当前时间线时只滚动，不触发会话内容重绘。
    if (!previewItems()) {
      scroll();
      return;
    }

    // 从旁支预览切回主线时，先恢复当前会话，再在新 DOM 中定位提示词。
    previewRequest++;
    if (previewTimer) clearTimeout(previewTimer);
    setTimeMachineEditTarget(null);
    setPreviewFading(true);
    previewTimer = setTimeout(() => {
      setPreviewItems(null);
      setPreviewCheckpointId(null);
      requestAnimationFrame(() => {
        setPreviewFading(false);
        requestAnimationFrame(scroll);
      });
    }, 90);
  };
  const returnToCurrentTimeline = () => {
    previewRequest++;
    if (previewTimer) clearTimeout(previewTimer);
    setTimeMachineEditTarget(null);
    setPreviewItems(null);
    setPreviewCheckpointId(null);
    setPreviewFading(false);
    returnToNow();
  };
  const contextPrompts = (node: GraphNode, mode: ContextDeleteMode, count = 0) => {
    const prompts = node.branchPrompts;
    const index = Math.max(0, Math.min(prompts.length - 1, node.promptCount - 1));
    if (mode === "to-start") return prompts.slice(0, index);
    if (mode === "up") return prompts.slice(Math.max(0, index - count), index);
    if (mode === "self") return prompts.slice(index, index + 1);
    if (mode === "down") return prompts.slice(index + 1, index + 1 + count);
    return prompts.slice(index + 1);
  };
  const deleteContext = async (node: GraphNode, mode: ContextDeleteMode) => {
    let count = 0;
    if (mode === "up" || mode === "down") {
      const raw = window.prompt(mode === "up" ? "向上删除多少个节点？" : "向下删除多少个节点？", "1");
      if (raw === null) return;
      count = Number.parseInt(raw, 10);
      if (!Number.isFinite(count) || count <= 0) {
        await message("请输入大于 0 的整数", { kind: "error" });
        return;
      }
    }
    const prompts = contextPrompts(node, mode, count);
    setContextMenu(null);
    if (prompts.length === 0) return;
    if (!window.confirm(`确定删除 ${prompts.length} 个上下文节点？该操作会立即重组世界线，并使旧摘要失效。`)) {
      return;
    }
    let threadId = state.currentId;
    if (!threadId || restoringCheckpoint()) return;
    setRestoringCheckpoint(node.id);
    try {
      if (!node.onCurrentPath && node.previewCheckpoint) {
        const restored = await api.restoreTimeMachineCheckpoint(threadId, node.previewCheckpoint.id);
        await refreshThreads();
        await openThread(restored.threadId);
        threadId = restored.threadId;
      }
      const result = await api.deleteTimeMachineContext(threadId, prompts);
      setTimeline(result.timeline);
      setTimeMachineEditTarget(null);
      setPreviewItems(null);
      setPreviewCheckpointId(null);
      await refreshThreads();
      await openThread(result.threadId);
    } catch (error) {
      await message(String(error), { kind: "error" });
    } finally {
      setRestoringCheckpoint(null);
    }
  };
  onCleanup(() => {
    previewRequest++;
    if (previewTimer) clearTimeout(previewTimer);
  });
  // 漫游 guest：召回会话——host 自动把完整快照 Flow 回来，收件箱里选项目接收
  const [recalling, setRecalling] = createSignal(false);
  const recall = async () => {
    const id = state.currentId;
    if (!id || recalling()) return;
    setRecalling(true);
    try {
      await api.recallRoamingThread(id);
    } catch (e) {
      await message(String(e), { kind: "error" });
    } finally {
      setRecalling(false);
    }
  };

  const commitRename = async () => {
    setEditing(false);
    const id = state.currentId;
    const title = draft().trim();
    if (!id || !title || title === state.title) return;
    await api.renameThread(id, title);
    setState("title", title);
  };

  return (
    <main class="chat" style={`--time-width:${timeMachineWidth()}px`}>
      <header class="chat-head">
        <Show
          when={editing()}
          fallback={
            <div class="chat-title" onDblClick={startRename} title="双击重命名">
              <TypewriterText
                text={state.title}
                title={state.title}
                animate={!!state.currentId && state.titleTyping[state.currentId]}
              />
            </div>
          }
        >
          <input
            class="chat-title-input"
            value={draft()}
            onInput={(e) => setDraft(e.currentTarget.value)}
            onBlur={() => void commitRename()}
            onKeyDown={(e) => {
              if (e.key === "Enter") void commitRename();
              if (e.key === "Escape") setEditing(false);
            }}
            ref={(el) => queueMicrotask(() => el.focus())}
          />
        </Show>
        <Show when={canStar()}>
          <button
            type="button"
            class="chat-star"
            classList={{ starred: !!currentMeta()?.starred }}
            title={currentMeta()?.starred ? "取消星标" : "加星标并在项目内置顶"}
            aria-pressed={!!currentMeta()?.starred}
            onClick={() => void toggleStar()}
          >
            <IconStar size={15} filled={!!currentMeta()?.starred} />
          </button>
        </Show>
        <span class={`agent-badge ${state.agentKind}`}>
          {agentLabel(state.agentKind)}
        </span>
        <Show when={roamingRole()}>
          <span
            class={`roaming-badge ${roamingRole()}`}
            title={
              roamingRole() === "guest"
                ? `漫游中：在 ${currentMeta()?.roamingPeerName ?? "队友"} 的机器上执行`
                : `漫游中：替 ${currentMeta()?.roamingPeerName ?? "队友"} 在本机执行`
            }
          >
            <IconBroadcast size={11} />
            {roamingRole() === "guest"
              ? `漫游 @ ${currentMeta()?.roamingPeerName ?? "队友"}`
              : `代执行 · ${currentMeta()?.roamingPeerName ?? "队友"}`}
          </span>
        </Show>
        <Show when={currentMeta()?.quotaPeerName}>
          <span
            class="roaming-badge quota"
            title={`本机目录执行，临时使用 ${currentMeta()?.quotaPeerName} 的加密授权额度`}
          >
            <IconBroadcast size={11} />
            额度 · {currentMeta()?.quotaPeerName}
          </span>
        </Show>
        <div
          class="chat-cwd"
          title={
            currentMeta()?.worktree
              ? `源仓库：${currentMeta()!.worktree!.repo}\n分支：${currentMeta()!.worktree!.branch}${
                  state.cwd && state.cwd !== currentMeta()!.worktree!.repo
                    ? `\n工作目录：${state.cwd}`
                    : ""
                }`
              : state.cwd
          }
        >
          <Show when={currentMeta()?.worktree} fallback={state.cwd}>
            <span class="chat-cwd-repo">{currentMeta()!.worktree!.repo}</span>
            <span class="chat-cwd-wt">⎇ {currentMeta()!.worktree!.branch}</span>
          </Show>
        </div>
        <Show when={totalTokens() > 0}>
          <span class="chat-tokens" title="本会话累计 token 用量">
            {fmtTokens(totalTokens())} tokens
          </span>
        </Show>
        <Show
          when={
            state.agentKind === "codex" &&
            !!state.currentId &&
            state.items.length > 0 &&
            roamingRole() !== "guest"
          }
        >
          <button
            class="chat-compact-btn"
            title="压缩上下文：把当前长历史浓缩为摘要，后续仅基于摘要继续，加快响应"
            disabled={isRunning()}
            onClick={() => void compactThread()}
          >
            <IconCompress size={14} />
            压缩
          </button>
        </Show>
        <Show
          when={
            !!state.currentId &&
            roamingRole() !== "guest" &&
            (state.relay.connected ||
              state.items.some((item) => item.type === "assistant"))
          }
        >
          <button
            class="chat-share-btn"
            title="线索与 Flow 分享"
            onClick={() => setShowShare(true)}
          >
            <IconShare size={14} />
            Flow
          </button>
        </Show>
        <Show when={state.relay.connected && state.currentId && roamingRole() === "guest"}>
          <button
            class="chat-share-btn"
            title={`把这段漫游会话拿回本机：${currentMeta()?.roamingPeerName ?? "对方"} 会自动回传完整快照（等价于对方 Flow 给你），到收件箱选择本地项目即可接收`}
            disabled={recalling()}
            onClick={() => void recall()}
          >
            <IconDownload size={14} />
            {recalling() ? "召回中…" : "召回"}
          </button>
        </Show>
      </header>
      <Show when={showShare() && state.currentId}>
        <ShareModal threadId={state.currentId!} onClose={() => setShowShare(false)} />
      </Show>

      <div class="chat-body">
        <TranscriptCanvas
          groups={groups()}
          permissions={permissions()}
          running={isRunning()}
          showHint={displayedItems().length === 0 && !state.loadingThread}
          hintCwd={cwdDisplay()}
          threadId={state.currentId ?? ""}
          previewBanner={!!previewCheckpointId()}
          fading={previewFading()}
          stickToBottom={stickToBottom()}
          onStickChange={setStickToBottom}
          onReturnToTimeline={returnToCurrentTimeline}
          onApi={(a) => (canvasApi = a)}
        />
      <Show when={showTimeMachine()}>
        <aside
          class="repo-time-machine"
          aria-label="会话与工作目录分支时间线"
        >
          <div class="repo-time-machine-label">
            <IconStopwatch size={17} />
            <span>{restoringCheckpoint() ? "跳转中…" : "世界线"}</span>
          </div>
          <div class="repo-time-machine-track">
            <div
              class="repo-time-graph"
              style={{ width: `${timelineGraph().width}px`, height: `${timelineGraph().height}px` }}
            >
              <svg class="repo-time-edges" width={timelineGraph().width} height={timelineGraph().height} aria-hidden="true">
                <For each={timelineGraph().edges}>
                  {(edge) => (
                    <path
                      classList={{ current: edge.current }}
                      d={`M ${edge.from.x} ${edge.from.y} C ${edge.from.x} ${edge.from.y + 16}, ${edge.to.x} ${edge.to.y - 16}, ${edge.to.x} ${edge.to.y}`}
                    />
                  )}
                </For>
              </svg>
              <For each={timelineGraph().nodes}>
                {(node) => (
                  <button
                    type="button"
                    class="repo-time-node"
                    classList={{
                      active: node.current,
                      selected: node.id === timeline()?.currentCheckpointId,
                      previewing: node.id === previewCheckpointId(),
                      restoring: node.id === restoringCheckpoint(),
                      "current-path": node.onCurrentPath,
                      "off-current-path": !node.onCurrentPath,
                    }}
                    style={{ left: `${node.x}px`, top: `${node.y}px` }}
                    title={node.title}
                    disabled={!!restoringCheckpoint()}
                    onClick={() => {
                      setContextMenu(null);
                      if (node.currentPromptIndex !== null) {
                        scrollToCurrentPrompt(node.currentPromptIndex);
                      } else if (node.previewCheckpoint) {
                        setTimeMachineEditTarget({
                          threadId: state.currentId!,
                          checkpointId: node.previewCheckpoint.id,
                        });
                        void previewGraphNode(node);
                      }
                    }}
                    onContextMenu={(event) => {
                      event.preventDefault();
                      event.stopPropagation();
                      setContextMenu({ x: event.clientX, y: event.clientY, node });
                    }}
                  >
                    <span class="repo-time-dot">{node.promptCount}</span>
                  </button>
                )}
              </For>
            </div>
          </div>
          <button
            type="button"
            class="repo-time-now"
            classList={{ active: stickToBottom() && !previewItems() }}
            title="回到当前时间线的最新消息"
            onClick={() => previewItems() ? returnToCurrentTimeline() : returnToNow()}
          >
            <span class="repo-time-now-pulse" />
            现在
          </button>
        </aside>
      </Show>
      <Portal>
        <Show when={contextMenu()} keyed>
          {(menu) => (
            <div class="repo-time-context-backdrop" onMouseDown={() => setContextMenu(null)}>
              <div
                class="repo-time-context-menu"
                style={{
                  left: `${Math.max(8, Math.min(menu.x, window.innerWidth - 210))}px`,
                  top: `${Math.max(8, Math.min(menu.y, window.innerHeight - 250))}px`,
                }}
                onMouseDown={(event) => event.stopPropagation()}
              >
                <button disabled={contextPrompts(menu.node, "to-start").length === 0} onClick={() => void deleteContext(menu.node, "to-start")}>删除到开始</button>
                <button disabled={contextPrompts(menu.node, "up", 1).length === 0} onClick={() => void deleteContext(menu.node, "up")}>向上删除 n 个</button>
                <button onClick={() => void deleteContext(menu.node, "self")}>删除自身</button>
                <button disabled={contextPrompts(menu.node, "down", 1).length === 0} onClick={() => void deleteContext(menu.node, "down")}>向下删除 N 个</button>
                <button disabled={contextPrompts(menu.node, "to-end").length === 0} onClick={() => void deleteContext(menu.node, "to-end")}>删除到结尾</button>
                <div class="repo-time-context-hint">除“删除自身”外均不包含当前节点</div>
              </div>
            </div>
          )}
        </Show>
      </Portal>
      <Show when={showStageRail()}>
        <aside class="stage-rail" aria-label="会话阶段导航">
          <div class="stage-rail-count">{stageThreads().length} 个事件</div>
          <For each={stageThreads()}>
            {(thread, index) => (
              <button
                type="button"
                class="stage-rail-item"
                classList={{ active: thread.id === state.currentId }}
                title={thread.title}
                onClick={() => void jumpToStage(thread.id)}
              >
                <span>{stageName(thread)}</span>
                <small>{index() + 1}</small>
              </button>
            )}
          </For>
        </aside>
      </Show>
      </div>

      <footer
        class="chat-foot"
        classList={{
          "has-time-machine": showTimeMachine(),
          "has-stage-rail": showStageRail(),
        }}
      >
        <Show when={state.plan && state.plan.length > 0}>
          <div classList={{ "fire-plan-inline": isFireThread() && showStageRail() }}>
            <PlanCard plan={state.plan!} />
          </div>
        </Show>
        <PlanActionCard />
        <Composer />
      </footer>
    </main>
  );
}
