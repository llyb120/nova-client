import { buildTimelineGraph, EMPTY_TIMELINE, visibleTimeline, type TimelineNode } from "../timelineGraph";
import type { HistoryOutline } from "../historyTypes";
import { HistoryDetails, HistoryImagePreview } from "./HistoryDetails";
import { confirm, message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, lazy, onCleanup, onMount, Show, Suspense, untrack } from "solid-js";
import { Portal } from "solid-js/web";
import { api } from "../ipc";
import { workspaceLayout, setWorkspaceLayout } from "../workspaceLayout";
import { buildTimeNotesPrompt } from "../builtinPrompts";
import {
  compactThread,
  chatScrollToBottomSignal,
  createThread,
  deleteThread,
  markThreadSwitchPointerDown,
  openThread,
  loadHistoryPage,
  historyResetRevision,
  pickThreadModel,
  refreshThreads,
  sendPrompt,
  setState,
  setTimeMachineEditTarget,
  state,
  timeMachineChangedSignal,
} from "../store";
import { mountSessionShortcuts } from "../sessionShortcuts";
import { resolveUserScrollStick } from "../scrollStick";
import type { AgentKind, Item, ThreadMeta, TimeMachineCheckpoint, TimeMachinePrompt, TimeMachineTimeline } from "../types";
import { agentLabel } from "../utils";
import { CanvasTranscript, type CanvasTranscriptHandle } from "./CanvasTranscript";
import { Composer } from "./Composer";
import { IconBroadcast, IconCompress, IconDownload, IconFile, IconPencil, IconShare, IconStar, IconStopwatch } from "./icons";
import { PermissionCard } from "./PermissionCard";
import { PlanActionCard } from "./PlanActionCard";
import { ShareModal } from "./ShareModal";
import { TimeNotesModal } from "./TimeNotesModal";
import { TypewriterText } from "./TypewriterText";
import { fmtTokens, groupItems } from "./TurnGroup";

const WorkspacePanel = lazy(() => import("./WorkspacePanel"));

export function ChatView() {
  const workspaceOpen = () => workspaceLayout.open;
  const setWorkspaceOpen = (open: boolean) => setWorkspaceLayout({ open });
  const [workspaceRequest, setWorkspaceRequest] = createSignal<{ path: string; line?: number } | null>(null);
  createEffect(() => { state.currentId; setWorkspaceRequest(null); });
  const previewFile = (event: Event) => {
    const detail = (event as CustomEvent<string | { path: string; line?: number }>).detail;
    const target = typeof detail === 'string' ? { path: detail } : detail;
    if (!target || typeof target.path !== "string" || !state.currentId) return;
    setWorkspaceRequest(target);
    setWorkspaceLayout({ open: true, mode: "files" });
  };
  onMount(() => window.addEventListener("nova:preview-file", previewFile));
  onCleanup(() => window.removeEventListener("nova:preview-file", previewFile));
  let transcriptRef: CanvasTranscriptHandle | undefined;
  const [stickToBottom, setStickToBottom] = createSignal(true);

  mountSessionShortcuts({
    allowedActions: ["selectModel"],
    onSelectProject: () => {},
    onSelectModel: (agentKind, model, quotaPeer) => {
      // 额度会话的共享模型条目（快捷键 target 带队友）不能拿来换后端；只当前后端
      // 的模型走 set_thread_model，由后端按已持有的额度租约把关。
      if (quotaPeer) return;
      void pickThreadModel(agentKind, model);
    },
  });
  let scrollQueued = false;
  let lastScrollTop = 0;
  let historyScrollDirection: "before" | "after" | null = null;

  const permissions = createMemo(() =>
    state.permissions.filter((p) => p.threadId === state.currentId),
  );

  const [previewItems, setPreviewItems] = createSignal<Item[] | null>(null);
  const [previewCheckpointId, setPreviewCheckpointId] = createSignal<string | null>(null);
  let previewRequest = 0;
  let previewTimer: ReturnType<typeof setTimeout> | undefined;
  const displayedItems = () => previewItems() ?? (state.items as Item[]);
  const groups = createMemo<ReturnType<typeof groupItems>>(
    (prev) => groupItems(displayedItems(), prev),
    [],
  );
  const isRunning = () => !!(state.currentId && state.running[state.currentId]);
  const timeStops = createMemo(() => {
    let turn = previewItems() ? 0 : state.history?.turnOffset ?? 0;
    return groups().flatMap((group, index) => {
      if (!group.user) return [];
      turn++;
      const text = group.user.text.slice(0, 160).replace(/\s+/g, " ").trim();
      return [{ index, turn, label: text || `第 ${turn} 轮` }];
    });
  });
  const [activeTimeIndex, setActiveTimeIndex] = createSignal(-1);
  const latestTimeIndex = () => timeStops().at(-1)?.index ?? -1;

  const syncTimeCursor = () => {
    const stops = timeStops();
    if (stops.length === 0) { setActiveTimeIndex(-1); return; }
    if (!transcriptRef) return;
    const groupIndex = transcriptRef.activeGroup();
    let best = 0;
    for (const stop of stops) {
      if (stop.index > groupIndex) break;
      best = stop.index;
    }
    setActiveTimeIndex(best);
  };

  const travelTo = (index: number) => {
    historyScrollDirection = null;
    cancelBottomFollow();
    transcriptRef?.scrollToGroup(index);
    syncTimeCursor();
  };

  const returnToNow = () => {
    historyScrollDirection = null;
    setState("historyFollowing", true);
    if (!previewItems() && state.history?.afterCursor) {
      void loadHistoryPage("latest").then(ok => { if (ok) { enableBottomFollow(); setActiveTimeIndex(latestTimeIndex()); } });
    } else { enableBottomFollow(); setActiveTimeIndex(latestTimeIndex()); }
  };

  const isAtBottom = () => transcriptRef?.isAtBottom() ?? true;
  const cancelBottomFollow = () => setStickToBottom(false);
  const pinBottom = () => {
    if (!stickToBottom()) return;
    transcriptRef?.scrollToBottom();
    lastScrollTop = transcriptRef?.scrollTop() ?? 0;
  };

  const scheduleBottomPin = () => {
    if (scrollQueued) return;
    scrollQueued = true;
    queueMicrotask(() => {
      scrollQueued = false;
      pinBottom();
    });
  };

  const enableBottomFollow = () => {
    setStickToBottom(true);
    scheduleBottomPin();
  };

  // 会话累计 token 用量：直接对当前展示的 turn 项求和。turn 项经 upsert 按 id
  // 覆盖落位，求和天然不会重复累计；世界线预览（previewItems）或恢复切换后
  // items 被整体替换，总量随之指向所预览/所在的那条分支。
  const tokenStats = createMemo(() => {
    const stats = !previewItems() ? state.history?.stats : null;
    if (stats) return { total: stats.totalTokens, read: Math.max(0, stats.inputTokens - stats.cacheReadTokens - stats.cacheWriteTokens),
      output: stats.outputTokens, cacheRead: stats.cacheReadTokens, cacheWrite: stats.cacheWriteTokens };
    let total = 0;
    let read = 0;
    let output = 0;
    let cacheRead = 0;
    let cacheWrite = 0;
    for (const it of displayedItems()) {
      if (it.type !== "turn") continue;
      total += it.totalTokens ?? 0;
      // inputTokens 是总输入量（含缓存命中/写入），拆出互斥的「读取」避免重复计。
      const cr = it.cacheReadTokens ?? 0;
      const cw = it.cacheWriteTokens ?? 0;
      read += Math.max(0, (it.inputTokens ?? 0) - cr - cw);
      output += it.outputTokens ?? 0;
      cacheRead += cr;
      cacheWrite += cw;
    }
    return { total, read, output, cacheRead, cacheWrite };
  });
  // 本轮进行中的实时用量（Lyra 流式上报；预览世界线时不混入当前轮）。
  const liveUsage = () => (previewItems() ? null : state.liveUsage);
  const totalTokens = () => tokenStats().total + (liveUsage()?.totalTokens ?? 0);
  const totalTokensTitle = () => {
    const s = tokenStats();
    const parts = [`读取 ${fmtTokens(s.read)}`, `写入 ${fmtTokens(s.output)}`];
    if (s.cacheRead > 0) parts.push(`缓存读取 ${fmtTokens(s.cacheRead)}`);
    if (s.cacheWrite > 0) parts.push(`缓存写入 ${fmtTokens(s.cacheWrite)}`);
    const live = liveUsage();
    if (live?.totalTokens) parts.push(`本轮进行中 ${fmtTokens(live.totalTokens)}`);
    const scope = previewItems() ? "当前预览的世界线节点" : "本会话";
    return `${scope}累计 token 用量\n${parts.join(" / ")} tokens`;
  };

  // 数字滚动效果：总量变化时从旧值平滑跳到新值（类似金额跳动），
  // 方向不限（世界线切换可能变少），动画期间加高亮。
  const [shownTokens, setShownTokens] = createSignal(0);
  const [tokensRolling, setTokensRolling] = createSignal(false);
  let tokenRollFrame = 0;
  let tokenRollDoneTimer: number | undefined;
  const stopTokenRoll = () => {
    if (tokenRollFrame) cancelAnimationFrame(tokenRollFrame);
    tokenRollFrame = 0;
    if (tokenRollDoneTimer !== undefined) window.clearTimeout(tokenRollDoneTimer);
    tokenRollDoneTimer = undefined;
    setTokensRolling(false);
  };
  createEffect(() => {
    // 用量来自多个异步来源（实时估算、provider 校正、Turn 落库接替）。
    // 任一来源切换都不应让计数器进入负区间；同时防御异常的非有限值。
    const rawTarget = totalTokens();
    const target = Number.isFinite(rawTarget) ? Math.max(0, Math.round(rawTarget)) : 0;
    const current = untrack(shownTokens);
    const from = Number.isFinite(current) ? Math.max(0, current) : 0;
    if (from === target) return;
    stopTokenRoll();
    setTokensRolling(true);
    // 变化越大滚动越久，设上限避免世界线大跨度切换时数字跑太久。
    const duration = Math.min(1200, 350 + Math.abs(target - from) / 20);
    const start = performance.now();
    const lower = Math.max(0, Math.min(from, target));
    const upper = Math.max(from, target);
    const step = (now: number) => {
      const progress = Math.min(1, Math.max(0, (now - start) / duration));
      // easeOutCubic：前快后慢；钳制到起止区间，避免异步目标切换时出现负数或过冲。
      const eased = 1 - Math.pow(1 - progress, 3);
      const next = Math.round(from + (target - from) * eased);
      setShownTokens(Math.max(lower, Math.min(upper, next)));
      if (progress < 1) {
        tokenRollFrame = requestAnimationFrame(step);
      } else {
        setShownTokens(target);
        tokenRollFrame = 0;
        // 数值到位后高亮稍留一拍再消退。
        tokenRollDoneTimer = window.setTimeout(() => {
          tokenRollDoneTimer = undefined;
          setTokensRolling(false);
        }, 250);
      }
    };
    tokenRollFrame = requestAnimationFrame(step);
  });
  onCleanup(stopTokenRoll);

  // 流式内容变化后请求一次绘制前钉底；自由浏览时 pinBottom 会直接退出。
  createEffect(() => {
    const len = state.items.length;
    const last = state.items[len - 1];
    if (last && "text" in last) void (last as { text: string }).text.length;
    void permissions().length;
    scheduleBottomPin();
  });

  onMount(() => {
    const scrollUpKeys = new Set(["ArrowUp", "PageUp", "Home"]);
    const scrollDownKeys = new Set(["ArrowDown", "PageDown", "End"]);
    const handleScrollKey = (event: KeyboardEvent) => {
      const scrollsUp = scrollUpKeys.has(event.key) || (event.key === " " && event.shiftKey);
      const scrollsDown = scrollDownKeys.has(event.key) || (event.key === " " && !event.shiftKey);
      if (event.altKey || event.ctrlKey || event.metaKey) return;
      if (!scrollsUp && !scrollsDown) return;
      if (transcriptRef?.hasFocusedInput()) return;
      event.preventDefault();
      transcriptRef?.scrollBy(scrollsDown ? 100 : -100);
      if (scrollsDown && isAtBottom() && !stickToBottom()) enableBottomFollow();
      else if (!isAtBottom()) cancelBottomFollow();
    };
    window.addEventListener("keydown", handleScrollKey, true);
    onCleanup(() => window.removeEventListener("keydown", handleScrollKey, true));
  });

  // 切换会话时从底部开始；后续尺寸变化由 ResizeObserver 持续对齐。
  createEffect((prevId: string | null | undefined) => {
    const id = state.currentId;
    if (id !== prevId) {
      historyScrollDirection = null;
      enableBottomFollow();
      setActiveTimeIndex(latestTimeIndex());
    }
    return id;
  }, undefined);

  // 会话加载和新增轮次后，让“现在”刻度跟随最新用户轮次；回看过去时不抢走光标。
  createEffect(() => {
    const latest = latestTimeIndex();
    if (stickToBottom()) setActiveTimeIndex(latest);
  });

  // 主动发送新提示词时重新进入吸底，无动画直接显示最新内容。
  createEffect(() => {
    const tick = chatScrollToBottomSignal();
    if (tick === 0) return;
    // Only a new send signal authorizes jumping to the latest page. Do not
    // subscribe this effect to pagination metadata after the first send.
    untrack(() => {
      if (state.history?.afterCursor) returnToNow();
      else enableBottomFollow();
    });
  });

  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [showShare, setShowShare] = createSignal(false);
  const [timeline, setTimeline] = createSignal<TimeMachineTimeline | null>(null);
  const [timeMachineExpanded, setTimeMachineExpanded] = createSignal(false);
  const [timeMachineHintTurn, setTimeMachineHintTurn] = createSignal(0);
  const [restoringCheckpoint, setRestoringCheckpoint] = createSignal<string | null>(null);
  let observedTimeMachineThread: string | null = null;
  let observedTimeMachineChange = timeMachineChangedSignal();

  createEffect(() => {
    const threadId = state.currentId;
    const change = timeMachineChangedSignal();
    const changedCurrentTimeline =
      threadId !== null && threadId === observedTimeMachineThread && change !== observedTimeMachineChange;
    observedTimeMachineThread = threadId;
    observedTimeMachineChange = change;
    if (changedCurrentTimeline && !timeMachineExpanded()) {
      // 收起时世界线不可见，用卡扣里的表针转动提示历史编辑已生成新分支。
      setTimeMachineHintTurn((turn) => turn + 1);
    }
  });


  const [outline, setOutline] = createSignal<HistoryOutline | null>(null);
  const [timelineError, setTimelineError] = createSignal("");
  let timelineRequest = 0, outlineRequest = 0, timelineLoadedKey = "", outlineLoadedKey = "";
  createEffect(() => {
    state.currentId;
    setTimeline(null); setOutline(null); setTimelineError("");
    setPreviewItems(null); setPreviewCheckpointId(null); setTimeMachineEditTarget(null);
    timelineLoadedKey = outlineLoadedKey = "";
    timelineRequest++; outlineRequest++;
  });
  createEffect(() => {
    if (!timeMachineExpanded() || workspaceOpen()) return;
    const threadId = state.currentId;
    if (!threadId) return;
    const key = `${threadId}:${timeMachineChangedSignal()}`;
    if (key === timelineLoadedKey) return;
    const request = ++timelineRequest;
    void api.getTimeMachineTimeline(threadId).then(value => {
      if (state.currentId !== threadId || request !== timelineRequest) return;
      timelineLoadedKey = key; setTimeline(value);
    }).catch(error => { if (request === timelineRequest) setTimelineError(String(error)); });
  });
  createEffect(() => {
    if (!timeMachineExpanded() || workspaceOpen()) return;
    const threadId = state.currentId;
    if (!threadId) return;
    const key = `${threadId}:${state.history?.generation ?? "initial"}:${state.history?.stats.users ?? 0}:${timeMachineChangedSignal()}`;
    if (key === outlineLoadedKey) return;
    const request = ++outlineRequest;
    void api.getThreadOutline(threadId).then(value => {
      if (state.currentId !== threadId || request !== outlineRequest) return;
      outlineLoadedKey = key; setOutline(value);
    }).catch(error => { if (request === outlineRequest) setTimelineError(String(error)); });
  });

  const currentMeta = createMemo(() =>
    state.threads.find((t) => t.id === state.currentId),
  );
  const isFireThread = () => /^\[Fire\]/.test(currentMeta()?.title ?? "");
  const showTimeMachine = () => !isFireThread();
  // 索引只依赖会话树；同链切换时根 id 不变，不重扫历史、不重建阶段列表。
  const stageIndex = createMemo(() => {
    const byId = new Map(state.threads.map((thread) => [thread.id, thread]));
    const children = new Map<string, ThreadMeta[]>();
    for (const thread of state.threads) {
      if (!thread.parentThreadId) continue;
      const siblings = children.get(thread.parentThreadId) ?? [];
      siblings.push(thread);
      children.set(thread.parentThreadId, siblings);
    }
    return { byId, children };
  });
  const stageRootId = createMemo(() => {
    const { byId } = stageIndex();
    let root = byId.get(state.currentId ?? "");
    if (!root) return null;
    const seen = new Set<string>([root.id]);
    while (root.parentThreadId) {
      const parent = byId.get(root.parentThreadId);
      if (!parent || seen.has(parent.id)) break;
      root = parent;
      seen.add(root.id);
    }
    return root.id;
  });
  const stageThreads = createMemo(() => {
    const { byId, children } = stageIndex();
    const root = byId.get(stageRootId() ?? "");
    if (!root) return [];
    const chain = [root];
    const seen = new Set<string>([root.id]);
    for (let i = 0; i < chain.length; i++) {
      for (const child of children.get(chain[i].id) ?? []) {
        if (seen.has(child.id)) continue;
        seen.add(child.id);
        chain.push(child);
      }
    }
    return chain.sort((a, b) => a.createdAt - b.createdAt);
  });
  /** 是否工作流/Fire/员工事件链的会话标题（决定导航栏是否从第一个节点起就显示）。 */
  const isStageTitle = (title: string) =>
    /^\[WF\]/.test(title) ||
    /^\[Hard\]/.test(title) ||
    /^\[Fire\]/.test(title) ||
    /\]\s*(Wake|Do|Dream|巡查)/.test(title);
  const showStageRail = () => {
    const threads = stageThreads();
    // 链上有多个会话，或链本身就是工作流/Fire/员工事件链（从第一个节点起就显示）。
    return threads.length > 1 || threads.some((thread) => isStageTitle(thread.title));
  };
  const stageName = (thread: (typeof state.threads)[number]) => {
    // 工作流节点：[WF] 节点名 · 第N次（· 待补充等状态后缀），显示节点名。
    const hardStage = thread.title.match(/^\[Hard\]\s*(.+?)\s*$/);
    if (hardStage) return hardStage[1].trim() || "Hard";
    const wfStage = thread.title.match(/^\[WF\]\s*(.+?)(?:\s+·\s+.*)?$/);
    if (wfStage) return wfStage[1].trim() || "节点";
    if (/\]\s*Wake/.test(thread.title)) return "Wake";
    if (/\]\s*Do/.test(thread.title)) return "Do";
    if (/\]\s*Dream/.test(thread.title)) return "Dream";
    if (/\]\s*巡查/.test(thread.title)) return "巡查";
    const fireJudge = thread.title.match(/^\[Fire\]\s*判断\s+(\d+)/);
    if (fireJudge) return `判断 ${fireJudge[1]}`;
    const fireStage = thread.title.match(/^\[Fire\]\s*阶段\s+(\d+)/);
    if (fireStage) return `阶段 ${fireStage[1]}`;
    if (/^\[Fire\]/.test(thread.title)) return "目标";
    // 普通 /stage 会话显示自己的会话名，不再显示泛化的「事件」。
    if (thread.stageSourceThreadId) return thread.title.replace(/^\[Stage\]\s*/, "").trim() || "Stage";
    // 工作流链的起点会话（用户输入目标的会话）显示为「目标」。
    if (!thread.parentThreadId && stageThreads().some((t) => isStageTitle(t.title))) return "目标";
    return thread.title || "会话";
  };
  // 链上其它 stage 的未读轮次数：当前打开的 stage 在 openThread 时已清零，不会再显示角标。
  const stageUnread = (thread: (typeof state.threads)[number]) =>
    thread.id === state.currentId ? 0 : (state.unreadTurns[thread.id] ?? 0);
  const jumpToStage = async (threadId: string) => {
    // 每个 stage 都是独立会话；切换 stage 只切换会话，不再拼接 transcript。
    await openThread(threadId);
  };
  const [stageContextMenu, setStageContextMenu] = createSignal<{
    x: number;
    y: number;
    thread: ThreadMeta;
  } | null>(null);
  const deleteStageEvent = async (thread: ThreadMeta) => {
    setStageContextMenu(null);
    const descendants = new Set([thread.id]);
    let changed = true;
    while (changed) {
      changed = false;
      for (const candidate of stageThreads()) {
        if (candidate.parentThreadId && descendants.has(candidate.parentThreadId) && !descendants.has(candidate.id)) {
          descendants.add(candidate.id);
          changed = true;
        }
      }
    }
    const followingCount = descendants.size - 1;
    const ok = await confirm(
      `删除事件「${stageName(thread)}」？聊天记录将一并删除。${followingCount > 0 ? `\n\n其后的 ${followingCount} 个事件也会一起删除。` : ""}`,
      { title: "删除事件", kind: "warning" },
    );
    if (!ok) return;
    try {
      await deleteThread(thread.id);
    } catch (error) {
      await message(String(error), { kind: "error" });
    }
  };
  const [starUpdating, setStarUpdating] = createSignal(false);
  const roamingRole = () => currentMeta()?.roamingRole ?? null;
  const canStar = () => {
    const meta = currentMeta();
    return !!meta && !meta.roamingRole;
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

  type GraphNode = TimelineNode;
  type ContextDeleteMode = "to-start" | "up" | "self" | "down" | "to-end";
  const [contextMenu, setContextMenu] = createSignal<{ x: number; y: number; node: GraphNode } | null>(null);
  const timelineGraph = createMemo(() => {
    if (!timeMachineExpanded() || workspaceOpen()) return EMPTY_TIMELINE;
    return buildTimelineGraph(timeline()?.checkpoints ?? [], outline()?.prompts ?? []);
  });
  const [timelineScroll, setTimelineScroll] = createSignal({ top: 0, height: 800 });
  const timelineVisible = createMemo(() => visibleTimeline(timelineGraph(), timelineScroll().top, timelineScroll().height));
  const timeMachineWidth = () => timeMachineExpanded() ? Math.max(64, 38 + Math.min(5, timelineGraph().laneCount) * 26) : 64;
  const switchPreview = (items: Item[] | null, checkpointId: string | null) => {
    if (previewTimer) clearTimeout(previewTimer);
    previewTimer = setTimeout(() => {
      setPreviewItems(items);
      setPreviewCheckpointId(checkpointId);
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
      if (node.currentPromptIndex !== null) scrollToCurrentPrompt(node.currentPromptIndex);
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
    const scroll = async () => {
      const threadId = state.currentId, prompt = outline()?.prompts[promptIndex];
      if (!threadId || !prompt) return;
      if (!state.items.some(item => item.id === prompt.id)) {
        if (!await loadHistoryPage("latest", prompt.id) || state.currentId !== threadId) return;
      }
      const index = groups().findIndex(group => group.user?.id === prompt.id);
      if (index >= 0) travelTo(index);
    };
    // 选择当前时间线时必须立即取消尚未完成的旁支预览。此前只有 previewItems 已经
    // 落地后才清理 target；若用户在 90ms 预览延迟内点回主线，旧请求仍会完成，
    // 下一次发送便会误从旁支 restore，表现为世界线有时分裂、有时不分裂。
    previewRequest++;
    if (previewTimer) clearTimeout(previewTimer);
    setTimeMachineEditTarget(null);
    if (!previewItems()) {
      setPreviewCheckpointId(null);
      scroll();
      return;
    }

    // 从旁支预览切回主线时，先恢复当前会话，再在 Canvas 新布局中定位提示词。
    previewTimer = setTimeout(() => {
      setPreviewItems(null);
      setPreviewCheckpointId(null);
      requestAnimationFrame(() => {
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
  const [showTimeNotes, setShowTimeNotes] = createSignal(false);
  const startTimeNotes = async (skillName: string, agentKind: AgentKind, model: string) => {
    const threadId = state.currentId;
    if (!threadId || restoringCheckpoint()) return;
    // 还没有任何时间点时先给当前状态补一个，让整段会话轨迹成为可分析的材料。
    // timeline 为 null 可能只是还在加载，先重拉一次再决定是否需要补建。
    let timelineValue = timeline();
    if (!timelineValue) {
      timelineValue = await api.getTimeMachineTimeline(threadId);
      setTimeline(timelineValue);
    }
    if (!timelineValue || timelineValue.checkpoints.length === 0) {
      timelineValue = await api.createTimeMachineCheckpoint(threadId);
      setTimeline(timelineValue);
    }
    const prepared = await api.getTimeMachineTrainingDigest(threadId);
    const meta = state.threads.find((t) => t.id === threadId);
    const cwd = meta?.cwd ?? state.cwd;
    setShowTimeNotes(false);
    await createThread(cwd, agentKind, model, state.mode, "", false);
    await sendPrompt(buildTimeNotesPrompt(skillName, prepared.digestPath, prepared.skillsDir));
  };
  const markOutcome = async (node: GraphNode, outcome: string | null) => {
    const checkpoint = node.checkpoint ?? node.previewCheckpoint;
    const threadId = state.currentId;
    if (!checkpoint || !threadId) return;
    setContextMenu(null);
    try {
      const updated = await api.setTimeMachineCheckpointOutcome(threadId, checkpoint.id, outcome);
      setTimeline(updated);
    } catch (error) {
      await message(String(error), { kind: "error" });
    }
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

  const [detailItem, setDetailItem] = createSignal<Item | null>(null);
  const [imageSource, setImageSource] = createSignal("");
  createEffect(() => { state.currentId; setDetailItem(null); setImageSource(""); });
  const imageRequest = (event: Event) => setImageSource(String((event as CustomEvent).detail || ""));
  onMount(() => window.addEventListener("nova:history-image", imageRequest));
  onCleanup(() => window.removeEventListener("nova:history-image", imageRequest));
  const navigateHistory = async (direction: "before" | "after") => {
    if (previewItems() || state.historyLoading) return;
    const threadId = state.currentId;
    const start = state.history?.start, end = state.history?.end;
    cancelBottomFollow();
    const loaded = await loadHistoryPage(direction, undefined, () => transcriptRef?.captureAnchor() ?? null);
    if (threadId === state.currentId && (!loaded ||
        (state.history?.start === start && state.history?.end === end))) historyScrollDirection = null;
    // Canvas captures/restores the live message anchor when the new window
    // arrives. Never restore an anchor captured before the asynchronous read.
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
        <span
          class="chat-tokens"
          classList={{
            "chat-tokens-empty": totalTokens() === 0,
            "chat-tokens-rolling": tokensRolling(),
          }}
          title={totalTokensTitle()}
        >
          {fmtTokens(shownTokens())} tokens
        </span>
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
        <Show when={showTimeMachine() && !workspaceOpen()}>
          <button
            type="button"
            class="chat-time-machine-btn"
            classList={{ active: timeMachineExpanded() }}
            title={timeMachineExpanded() ? "收起世界线" : "展开世界线"}
            aria-label={timeMachineExpanded() ? "收起世界线" : "展开世界线"}
            aria-expanded={timeMachineExpanded()}
            onClick={() => setTimeMachineExpanded(!timeMachineExpanded())}
          >
            <span class="repo-time-toggle-clock" aria-hidden="true">
              <IconStopwatch size={15} />
              <Show keyed when={!timeMachineExpanded() && timeMachineHintTurn()}>
                <span class="repo-time-toggle-hand" />
              </Show>
            </span>
          </button>
        </Show>
        <Show when={!!state.currentId && roamingRole() !== "guest"}>
          <button
            type="button"
            class="chat-files-btn"
            classList={{ active: workspaceOpen() }}
            title="文件与产物"
            aria-label="文件与产物"
            onClick={() => setWorkspaceOpen(!workspaceOpen())}
          >
            <IconFile size={15} />
          </button>
        </Show>
      </header>
      <Show when={showShare() && state.currentId}>
        <ShareModal threadId={state.currentId!} onClose={() => setShowShare(false)} />
      </Show>
      <Show when={showTimeNotes() && state.currentId}>
        <TimeNotesModal
          defaultName={state.title.trim()}
          onConfirm={startTimeNotes}
          onClose={() => setShowTimeNotes(false)}
        />
      </Show>

      <div class="chat-shell">
        <div class="chat-primary">
      <div class="chat-body history-body">
          <Show when={state.historyError}><div class="history-load-error" role="alert">{state.historyError} <button onClick={() => state.currentId && void openThread(state.currentId)}>重新加载</button></div></Show>
          <CanvasTranscript
            ref={(handle) => { transcriptRef = handle; scheduleBottomPin(); }}
            threadId={state.currentId}
            revision={`${previewCheckpointId() ?? state.history?.generation ?? ""}:${historyResetRevision()}`}
            groups={groups()}
            permissions={permissions()}
            running={isRunning() && !previewItems()}
            loading={state.loadingThread}
            preview={!!previewCheckpointId()}
            onReturnToCurrent={returnToCurrentTimeline}
            onScroll={(top, max, user) => {
              if (user) {
                if (top < lastScrollTop) historyScrollDirection = "before";
                else if (top > lastScrollTop) historyScrollDirection = "after";
                // 与 canvas 内部判定共用方向语义：上滚即解除吸底，下滚贴底才恢复。
                const following = resolveUserScrollStick(lastScrollTop, top, max);
                setStickToBottom(following);
                setState("historyFollowing", following && !state.history?.afterCursor);
              }
              // user=false（rebuild 钉底/钳位）也要同步基准位置，
              // 否则下一次上滚的方向判定拿旧基准会误判为下滚。
              lastScrollTop = top;
              syncTimeCursor();
              // Fetch several screens ahead, and re-check after a committed
              // layout so a held wheel/key does not need a second gesture.
              const lead = Math.max(600, window.innerHeight * 3);
              if (!previewItems() && !state.historyLoading && !state.historyError) {
                if (historyScrollDirection === "before" && top < lead && state.history?.beforeCursor)
                  void navigateHistory("before");
                else if (historyScrollDirection === "after" && max - top < lead && state.history?.afterCursor)
                  void navigateHistory("after");
              }
            }}
            onInspectItem={setDetailItem}
            onOpenImage={setImageSource}
            onBrowseDetail={cancelBottomFollow}
            emptyHint={`在下方输入任务，${agentLabel(state.agentKind)} 将在 ${cwdDisplay()} 中工作。`}
          />
      </div>

      <footer class="chat-foot">
        <Show when={previewCheckpointId()}>
          <button class="checkpoint-preview-banner" onClick={returnToCurrentTimeline}>回到当前时间线</button>
        </Show>
        <Show when={permissions().length}>
          <div style={{ "max-height": "35vh", overflow: "auto" }}>
            <For each={permissions()}>{req => <PermissionCard req={req} />}</For>
          </div>
        </Show>
        {/* 暂时隐藏「计划」面板；内部 plan 状态与事件仍照常更新 */}
        <PlanActionCard />
        <Composer />
      </footer>
        </div>

      <Show when={showTimeMachine() && !workspaceOpen()}>
        <aside
          class="repo-time-machine"
          classList={{ collapsed: !timeMachineExpanded(), expanded: timeMachineExpanded() }}
          aria-label="会话与工作目录分支时间线"
        >
          <div class="repo-time-machine-label" aria-hidden={!timeMachineExpanded()}>
            <button
              type="button"
              class="repo-time-notes"
              aria-label="时光笔记"
              title="时光笔记：把这条世界线的经验沉淀为一个 skill"
              disabled={!!restoringCheckpoint() || !timeMachineExpanded()}
              onClick={() => setShowTimeNotes(true)}
            >
              <IconPencil size={16} />
            </button>
            <button
              type="button"
              class="repo-time-magic-clock"
              title="收起世界线"
              aria-label="收起世界线"
              tabindex={timeMachineExpanded() ? 0 : -1}
              onClick={() => setTimeMachineExpanded(false)}
            >
              <span class="repo-time-clock-face" aria-hidden="true">
                <IconStopwatch size={19} />
                <span class="repo-time-clock-hand" />
              </span>
            </button>
            <Show when={restoringCheckpoint()}><span class="repo-time-toggle-label" role="status">跳转中…</span></Show>
          </div>
          <div class="repo-time-machine-track" onScroll={event => setTimelineScroll({ top: event.currentTarget.scrollTop, height: event.currentTarget.clientHeight })}>
            <Show when={timeMachineExpanded()}>
            <Show when={timelineError()}><p role="alert">{timelineError()}</p></Show>
            <div
              class="repo-time-graph"
              style={{ width: `${timelineGraph().width}px`, height: `${timelineGraph().height}px` }}
            >
              <svg class="repo-time-edges" width={timelineGraph().width} height={timelineGraph().height} aria-hidden="true">
                <For each={timelineVisible().edges}>
                  {(edge) => (
                    <path
                      classList={{ current: edge.current }}
                      d={`M ${edge.from.x} ${edge.from.y} C ${edge.from.x} ${edge.from.y + 16}, ${edge.to.x} ${edge.to.y - 16}, ${edge.to.x} ${edge.to.y}`}
                    />
                  )}
                </For>
              </svg>
              <For each={timelineVisible().nodes}>
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
                      "outcome-success": (node.checkpoint ?? node.previewCheckpoint)?.outcome === "success",
                      "outcome-failure": (node.checkpoint ?? node.previewCheckpoint)?.outcome === "failure",
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
              <button
                type="button"
                class="repo-time-now"
                classList={{ active: stickToBottom() && !previewItems() }}
                title="回到当前时间线的最新消息"
                onClick={returnToCurrentTimeline}
              >
                <span class="repo-time-now-pulse" />
                现在
              </button>
            </div>
            </Show>
          </div>
        </aside>
      </Show>
      <Show when={detailItem()} keyed>{item => <HistoryDetails threadId={state.currentId!} item={item} generation={state.history?.generation} onClose={() => setDetailItem(null)} />}</Show>
      <Show when={imageSource()} keyed>{source => <HistoryImagePreview source={source} onClose={() => setImageSource("")} />}</Show>
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
                <Show when={menu.node.checkpoint ?? menu.node.previewCheckpoint}>
                  <button onClick={() => void markOutcome(menu.node, "success")}>标记成功</button>
                  <button onClick={() => void markOutcome(menu.node, "failure")}>标记失败</button>
                  <Show when={(menu.node.checkpoint ?? menu.node.previewCheckpoint)?.outcome}>
                    <button onClick={() => void markOutcome(menu.node, null)}>清除结局标记</button>
                  </Show>
                  <div class="repo-time-context-hint">结局标记会作为时光笔记的提示</div>
                </Show>
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
          <div class="stage-rail-count">{stageThreads().length} {stageThreads().some((t) => isStageTitle(t.title)) ? "个节点" : "个事件"}</div>
          <For each={stageThreads()}>
            {(thread, index) => (
              <button
                type="button"
                class="stage-rail-item"
                classList={{ active: thread.id === state.currentId, unread: stageUnread(thread) > 0 }}
                title={thread.title}
                onPointerDown={() => markThreadSwitchPointerDown()}
                onClick={() => {
                  setStageContextMenu(null);
                  void jumpToStage(thread.id);
                }}
                onContextMenu={(event) => {
                  event.preventDefault();
                  event.stopPropagation();
                  setStageContextMenu({ x: event.clientX, y: event.clientY, thread });
                }}
              >
                <span>{stageName(thread)}</span>
                <small>{index() + 1}</small>
                <Show when={stageUnread(thread) > 0}>
                  <span class="thread-unread-badge">{stageUnread(thread) > 9 ? "9+" : stageUnread(thread)}</span>
                </Show>
              </button>
            )}
          </For>
        </aside>
      </Show>
      {/* 文件/产物侧栏排在 Stage 导航右边：聊天 → 世界线 → Stage → 侧边栏。 */}
      <Show when={workspaceOpen() && roamingRole() !== "guest"}>
        <Show keyed when={state.currentId}>
          {id => <Suspense fallback={<aside role="status">正在加载文件面板…</aside>}><WorkspacePanel threadId={id} request={workspaceLayout.mode === "terminal" ? null : workspaceRequest()} onClose={() => setWorkspaceOpen(false)} /></Suspense>}
        </Show>
      </Show>
      <Portal>
        <Show when={stageContextMenu()} keyed>
          {(menu) => (
            <div class="repo-time-context-backdrop" onMouseDown={() => setStageContextMenu(null)}>
              <div
                class="repo-time-context-menu"
                style={{
                  left: `${Math.max(8, Math.min(menu.x, window.innerWidth - 210))}px`,
                  top: `${Math.max(8, Math.min(menu.y, window.innerHeight - 80))}px`,
                }}
                onMouseDown={(event) => event.stopPropagation()}
              >
                <button onClick={() => void deleteStageEvent(menu.thread)}>删除事件</button>
              </div>
            </div>
          )}
        </Show>
      </Portal>
      </div>
    </main>
  );
}
