import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { api } from "../ipc";
import { compactThread, chatScrollToBottomSignal, setState, state } from "../store";
import { firstWakeDoPairForThread } from "../threadDisplay";
import type { Item, Thread } from "../types";
import { agentLabel } from "../utils";
import { Composer } from "./Composer";
import { IconBroadcast, IconCompress, IconDownload, IconShare } from "./icons";
import { PermissionCard } from "./PermissionCard";
import { PlanActionCard } from "./PlanActionCard";
import { PlanCard } from "./PlanCard";
import { ShareModal } from "./ShareModal";
import { TypewriterText } from "./TypewriterText";
import { fmtTokens, type Group, groupItems, TurnGroup } from "./TurnGroup";

/**
 * transcript 虚拟化包裹层：长会话若把每一轮（含 Markdown 结论、工具卡片、diff）都常驻
 * DOM，节点数随会话线性增长，WebView2 渲染进程内存单调上涨直至崩溃。这里给每个轮次套一层
 * 轻量 wrapper（始终存在，成本仅一个 div），用 IntersectionObserver 判断是否临近视口：
 * 远离视口时卸载内部重内容、用等高占位撑住（滚动位置不跳），滚回来再挂载。
 * 正在流式输出的当前轮（active）与列表末组永不卸载，避免高度剧变 / 发送后钉底失效。
 */
function VirtualGroup(props: {
  group: Group;
  active: boolean;
  /** 列表最后一组：始终挂载，保证新提示词有真实高度可供吸底 */
  keepMounted?: boolean;
  scrollEl: () => HTMLElement | undefined;
  /** 滚动/布局变化时递增，兜底校正 WebView2 偶发漏掉的 IntersectionObserver 回调 */
  viewportTick: number;
}) {
  let ref: HTMLDivElement | undefined;
  const [visible, setVisible] = createSignal(true);
  const [height, setHeight] = createSignal(0);
  const mounted = () => visible() || props.active || !!props.keepMounted;

  const rememberHeight = () => {
    if (!ref || !mounted()) return;
    const h = ref.getBoundingClientRect().height;
    if (h > 0) setHeight((prev) => (Math.abs(prev - h) > 0.5 ? h : prev));
  };

  /**
   * 不直接信任 IntersectionObserver 传来的 entry：快速程序化滚动时，WebView2 可能在
   * 回调执行前已经滚到了新位置，旧 entry 会把当前视口里的轮次误卸载成一整块空白。
   * 每次都用当前几何位置复核，并由父级滚动 tick 再兜一层。
   */
  const syncMounted = () => {
    if (!ref || props.active || props.keepMounted) {
      setVisible(true);
      return;
    }
    const root = props.scrollEl();
    if (!root) {
      // 找不到滚动根时宁可保留 DOM，不能把内容变成无法恢复的空占位。
      setVisible(true);
      return;
    }
    const rect = ref.getBoundingClientRect();
    const rootRect = root.getBoundingClientRect();
    const buffer = 1200;
    const nearViewport =
      rect.bottom >= rootRect.top - buffer && rect.top <= rootRect.bottom + buffer;
    if (nearViewport) {
      setVisible(true);
    } else {
      rememberHeight();
      setVisible(false);
    }
  };

  onMount(() => {
    if (!ref) return;
    const root = props.scrollEl();
    if (!root) return;
    const io = new IntersectionObserver(
      () => syncMounted(),
      // 视口上下各留 1200px 缓冲，减少快速滚动时的空白闪烁
      { root, rootMargin: "1200px 0px" },
    );
    io.observe(ref);
    const ro = new ResizeObserver(() => rememberHeight());
    ro.observe(ref);
    syncMounted();
    onCleanup(() => {
      io.disconnect();
      ro.disconnect();
    });
  });

  // keepMounted / active、滚动位置或整体布局变化后，立即用当前几何位置校正挂载状态。
  createEffect(() => {
    void props.viewportTick;
    syncMounted();
  });

  return (
    <div
      ref={ref}
      class="vgroup"
      style={!mounted() && height() > 0 ? { height: `${height()}px` } : undefined}
    >
      <Show when={mounted()}>
        <TurnGroup group={props.group} active={props.active} />
      </Show>
    </div>
  );
}

interface TranscriptSegmentProps {
  stage: "Wake" | "Do";
  agentKind: Thread["agentKind"];
  model?: string | null;
}

function TranscriptSegment(props: TranscriptSegmentProps) {
  return (
    <div class="transcript-segment">
      <span class={`agent-badge ${props.agentKind}`}>{props.stage}</span>
      <span class="transcript-segment-agent">{agentLabel(props.agentKind)}</span>
      <span class="transcript-segment-model" title={props.model || "默认模型"}>
        {props.model || "默认模型"}
      </span>
    </div>
  );
}

export function ChatView() {
  let scrollRef: HTMLDivElement | undefined;
  let innerRef: HTMLDivElement | undefined;
  let endRef: HTMLDivElement | undefined;
  const [stickToBottom, setStickToBottom] = createSignal(false);
  const [viewportTick, setViewportTick] = createSignal(0);
  let scrollQueued = false;
  let settleRaf = 0;
  let viewportRaf = 0;
  let awaitingSendUserItem = false;
  let itemsLenAtSend = 0;
  let manualScroll = false;
  let manualScrollMovedAway = false;
  let lastPinnedScrollTop: number | null = null;
  let userActivelyScrolledDown = false;
  let userReachedBottom = false;
  let manualScrollResumeTimer = 0;
  let lastKnownScrollTop = 0;
  let pointerScrolling = false;
  let pointerScrollEndTimer = 0;

  const permissions = createMemo(() =>
    state.permissions.filter((p) => p.threadId === state.currentId),
  );

  const groups = createMemo<ReturnType<typeof groupItems>>(
    (prev) => groupItems(state.items as Item[], prev),
    [],
  );
  const mergedPair = createMemo(() => firstWakeDoPairForThread(state.threads, state.currentId));
  const [mergedWake, setMergedWake] = createSignal<Thread | null>(null);
  let wakeLoad = 0;
  createEffect(() => {
    const pair = mergedPair();
    const wakeId = pair?.doThread?.id === state.currentId ? pair.wake.id : null;
    if (!wakeId) {
      wakeLoad++;
      setMergedWake(null);
      return;
    }
    if (mergedWake()?.id === wakeId) return;
    const load = ++wakeLoad;
    void api
      .getThread(wakeId)
      .then((thread) => {
        if (load === wakeLoad) setMergedWake(thread);
      })
      .catch(() => {
        if (load === wakeLoad) setMergedWake(null);
      });
  });
  onCleanup(() => wakeLoad++);
  const wakeGroups = createMemo<ReturnType<typeof groupItems>>(
    (prev) => groupItems((mergedWake()?.items ?? []) as Item[], prev),
    [],
  );
  const isRunning = () => !!(state.currentId && state.running[state.currentId]);
  const lastGroupIndex = () => groups().length - 1;

  const refreshVirtualGroups = () => {
    if (viewportRaf) return;
    viewportRaf = requestAnimationFrame(() => {
      viewportRaf = 0;
      setViewportTick((n) => n + 1);
    });
  };

  const isAtBottom = () => {
    if (!scrollRef) return true;
    return scrollRef.scrollHeight - scrollRef.scrollTop - scrollRef.clientHeight <= 1;
  };

  const cancelManualScrollResume = () => {
    if (manualScrollResumeTimer) window.clearTimeout(manualScrollResumeTimer);
    manualScrollResumeTimer = 0;
    userActivelyScrolledDown = false;
    userReachedBottom = false;
  };

  const cancelPointerScrollFinish = () => {
    if (pointerScrollEndTimer) window.clearTimeout(pointerScrollEndTimer);
    pointerScrollEndTimer = 0;
  };

  const beginManualScroll = () => {
    manualScroll = true;
    cancelManualScrollResume();
    awaitingSendUserItem = false;
    // 保留程序化钉底位置，直到 scroll 事件确认用户真的离开；否则排队中的旧 scroll
    // 事件会被误认为用户已滚回底部，并立刻恢复吸底。
    if (settleRaf) {
      cancelAnimationFrame(settleRaf);
      settleRaf = 0;
    }
  };

  const cancelBottomFollow = () => {
    beginManualScroll();
    setStickToBottom(false);
  };

  const isToolDetailScroll = (target: EventTarget | null) =>
    target instanceof Element && !!target.closest(".tool-output, .tool-raw");

  // 工具详情本身可滚动；用户在其中阅读时不应被外层会话自动吸底打断。
  const suspendBottomFollowForToolDetail = () => {
    beginManualScroll();
    manualScrollMovedAway = true;
    setStickToBottom(false);
  };

  const isFromProgrammaticPin = () =>
    !!scrollRef &&
    lastPinnedScrollTop !== null &&
    Math.abs(scrollRef.scrollTop - lastPinnedScrollTop) <= 1;

  const scheduleManualScrollResume = () => {
    if (!manualScroll || !userActivelyScrolledDown) return;
    if (manualScrollResumeTimer) window.clearTimeout(manualScrollResumeTimer);
    manualScrollResumeTimer = window.setTimeout(() => {
      manualScrollResumeTimer = 0;
      const shouldResume =
        manualScroll &&
        manualScrollMovedAway &&
        userActivelyScrolledDown &&
        userReachedBottom &&
        isAtBottom() &&
        !isFromProgrammaticPin();
      userActivelyScrolledDown = false;
      userReachedBottom = false;
      if (!shouldResume) return;
      manualScroll = false;
      manualScrollMovedAway = false;
      setStickToBottom(true);
    }, 120);
  };

  // 只在本次用户向下滚动真正到达底部、且滚动与虚拟分组布局稳定后恢复吸底。
  const syncManualScroll = () => {
    if (!manualScroll) return;
    if (!isAtBottom()) {
      manualScrollMovedAway = true;
      setStickToBottom(false);
    }
    scheduleManualScrollResume();
  };

  const completePointerScroll = () => {
    pointerScrollEndTimer = 0;
    pointerScrolling = false;
    if (!manualScroll || manualScrollMovedAway) return;
    manualScroll = false;
    cancelManualScrollResume();
    if (stickToBottom()) forceScrollToBottom();
  };

  const schedulePointerScrollFinish = () => {
    if (!pointerScrolling) return;
    cancelPointerScrollFinish();
    pointerScrollEndTimer = window.setTimeout(completePointerScroll, 120);
  };

  const finishManualScroll = () => schedulePointerScrollFinish();

  const handleWheel = (event: WheelEvent) => {
    if (isToolDetailScroll(event.target)) {
      suspendBottomFollowForToolDetail();
      return;
    }
    if (!scrollRef || scrollRef.scrollHeight <= scrollRef.clientHeight + 1) return;
    if (event.deltaY < 0) {
      cancelBottomFollow();
    } else if (event.deltaY > 0 && manualScroll) {
      userActivelyScrolledDown = true;
      scheduleManualScrollResume();
    }
  };

  const handlePointerDown = (event: PointerEvent) => {
    if (isToolDetailScroll(event.target)) suspendBottomFollowForToolDetail();
    else {
      cancelPointerScrollFinish();
      pointerScrolling = true;
      beginManualScroll();
    }
  };

  const handleTranscriptScroll = () => {
    refreshVirtualGroups();
    if (pointerScrollEndTimer) schedulePointerScrollFinish();
    const fromBottomPin = isFromProgrammaticPin();
    const currentTop = scrollRef?.scrollTop ?? 0;
    if (!fromBottomPin) {
      if (lastPinnedScrollTop !== null && manualScroll) lastPinnedScrollTop = null;
      if (!manualScroll && stickToBottom() && !isAtBottom()) cancelBottomFollow();
      if (manualScroll && currentTop < lastKnownScrollTop) {
        cancelManualScrollResume();
      } else if (
        manualScroll &&
        currentTop > lastKnownScrollTop &&
        (pointerScrolling || userActivelyScrolledDown)
      ) {
        userActivelyScrolledDown = true;
        if (isAtBottom()) userReachedBottom = true;
      }
    }
    lastKnownScrollTop = currentTop;
    syncManualScroll();
  };

  const pinBottom = (ensureSentinel = false) => {
    if (!scrollRef) return;
    scrollRef.scrollTop = Math.max(0, scrollRef.scrollHeight - scrollRef.clientHeight);
    if (ensureSentinel) endRef?.scrollIntoView({ block: "end", inline: "nearest" });
    lastPinnedScrollTop = scrollRef.scrollTop;
    lastKnownScrollTop = scrollRef.scrollTop;
    refreshVirtualGroups();
  };

  // DOM 更新后在下一次绘制前钉底，避免 rAF 留出一帧旧滚动位置造成闪动。
  const forceScrollToBottom = () => {
    if (!scrollRef) return;
    if (scrollQueued) return;
    scrollQueued = true;
    queueMicrotask(() => {
      scrollQueued = false;
      if (!stickToBottom() || manualScroll) return;
      pinBottom();
    });
  };

  /** 虚拟轮次挂回会改变 scrollHeight；连续钉底到布局稳定，避免停在旧占位高度中间。 */
  const settleToBottom = (maxMs = 1000) => {
    if (settleRaf) cancelAnimationFrame(settleRaf);
    manualScroll = false;
    manualScrollMovedAway = false;
    cancelManualScrollResume();
    cancelPointerScrollFinish();
    pointerScrolling = false;
    setStickToBottom(true);
    const deadline = performance.now() + maxMs;
    let lastHeight = -1;
    let stableFrames = 0;
    const step = () => {
      settleRaf = 0;
      // 用户已上滑脱离吸底：立刻停止 settle，绝不能无条件 pinBottom 把人拉回去。
      if (!scrollRef || !stickToBottom() || manualScroll) return;
      pinBottom(true);
      const height = scrollRef.scrollHeight;
      const distance = height - scrollRef.scrollTop - scrollRef.clientHeight;
      if (height === lastHeight && distance <= 1) stableFrames++;
      else stableFrames = 0;
      lastHeight = height;
      if (
        stickToBottom() &&
        !manualScroll &&
        performance.now() < deadline &&
        (awaitingSendUserItem || stableFrames < 3)
      ) {
        settleRaf = requestAnimationFrame(step);
      }
    };
    settleRaf = requestAnimationFrame(step);
  };

  const scrollToBottom = () => {
    if (!stickToBottom() || manualScroll) return;
    forceScrollToBottom();
  };

  /** 发送新提示词：强制跳到底，并等到 items 增长后再钉稳 */
  const jumpToBottomNow = () => {
    awaitingSendUserItem = true;
    itemsLenAtSend = state.items.length;
    settleToBottom(1200);
  };

  // 会话累计 token 用量
  const totalTokens = createMemo(() =>
    [...(mergedWake()?.items ?? []), ...state.items].reduce(
      (sum, it) => (it.type === "turn" && it.totalTokens ? sum + it.totalTokens : sum),
      0,
    ),
  );

  // 内容变化时自动吸底；发送后等 items 增长是关键钉底时机
  createEffect(() => {
    const len = state.items.length;
    const last = state.items[len - 1];
    if (last && "text" in last) void (last as { text: string }).text.length;
    void permissions().length;

    if (awaitingSendUserItem && len > itemsLenAtSend) {
      awaitingSendUserItem = false;
      settleToBottom(1000);
      return;
    }

    if (stickToBottom() && scrollRef) scrollToBottom();
  });

  onMount(() => {
    if (!innerRef || !scrollRef) return;
    const ro = new ResizeObserver(() => {
      refreshVirtualGroups();
      scheduleManualScrollResume();
      if (stickToBottom() && !manualScroll) scrollToBottom();
    });
    ro.observe(innerRef);

    const scrollUpKeys = new Set(["ArrowUp", "PageUp", "Home"]);
    const scrollDownKeys = new Set(["ArrowDown", "PageDown", "End"]);
    const handleScrollKey = (event: KeyboardEvent) => {
      const scrollsUp = scrollUpKeys.has(event.key) || (event.key === " " && event.shiftKey);
      const scrollsDown = scrollDownKeys.has(event.key) || (event.key === " " && !event.shiftKey);
      if (event.altKey || event.ctrlKey || event.metaKey || (!scrollsUp && !scrollsDown)) return;
      if (!scrollRef || scrollRef.scrollHeight <= scrollRef.clientHeight + 1) return;
      const target = event.target;
      if (target instanceof Node && target !== document.body && !scrollRef?.contains(target)) return;
      if (
        target instanceof HTMLElement &&
        (target.isContentEditable || target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT")
      ) return;
      if (scrollsDown) {
        if (manualScroll) {
          userActivelyScrolledDown = true;
          scheduleManualScrollResume();
        }
        return;
      }
      if (isToolDetailScroll(target)) suspendBottomFollowForToolDetail();
      else cancelBottomFollow();
    };
    window.addEventListener("keydown", handleScrollKey, true);
    window.addEventListener("pointerup", finishManualScroll, true);
    window.addEventListener("pointercancel", finishManualScroll, true);
    onCleanup(() => {
      ro.disconnect();
      window.removeEventListener("keydown", handleScrollKey, true);
      window.removeEventListener("pointerup", finishManualScroll, true);
      window.removeEventListener("pointercancel", finishManualScroll, true);
      if (settleRaf) cancelAnimationFrame(settleRaf);
      if (viewportRaf) cancelAnimationFrame(viewportRaf);
      cancelManualScrollResume();
      cancelPointerScrollFinish();
    });
  });

  // 仅在切换会话时重置吸底（不要在无关更新里清掉「等待发送落库」标记）
  createEffect((prevId: string | null | undefined) => {
    const id = state.currentId;
    if (id !== prevId) {
      awaitingSendUserItem = false;
      settleToBottom(800);
    }
    return id;
  }, undefined);

  // 会话中继续发送提示词：未在底部时也立刻跳到底（无过渡）
  createEffect(() => {
    const tick = chatScrollToBottomSignal();
    if (tick === 0) return;
    jumpToBottomNow();
  });

  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [showShare, setShowShare] = createSignal(false);

  const currentMeta = createMemo(() =>
    state.threads.find((t) => t.id === state.currentId),
  );
  const roamingRole = () => currentMeta()?.roamingRole ?? null;
  // worktree 会话的 cwd 是 uuid 工作目录，展示时用源仓库路径更直观
  const cwdDisplay = () => currentMeta()?.worktree?.repo || state.cwd;

  const startRename = () => {
    setDraft(state.title);
    setEditing(true);
  };

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
    <main class="chat">
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
        <Show when={state.currentId && state.running[state.currentId!]}>
          <span class="chat-running">
            <span class="spinner small" />
            运行中
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

      <div
        class="transcript"
        ref={scrollRef}
        onScroll={handleTranscriptScroll}
        onWheel={handleWheel}
        onPointerDown={handlePointerDown}
        onPointerUp={finishManualScroll}
        onPointerCancel={finishManualScroll}
      >
        <div class="transcript-inner" ref={innerRef}>
          <Show when={state.items.length === 0 && !mergedWake() && !state.loadingThread}>
            <div class="transcript-hint">
              在下方输入任务，{agentLabel(state.agentKind)} 将在{" "}
              <code>{cwdDisplay()}</code> 中工作。
            </div>
          </Show>
          <Show keyed when={state.currentId}>
            <Show when={mergedWake()}>
              {(wake) => (
                <>
                  <TranscriptSegment
                    stage="Wake"
                    agentKind={wake().agentKind}
                    model={wake().model}
                  />
                  <For each={wakeGroups()}>
                    {(g) => (
                      <VirtualGroup
                        group={g}
                        active={false}
                        scrollEl={() => scrollRef}
                        viewportTick={viewportTick()}
                      />
                    )}
                  </For>
                </>
              )}
            </Show>
            <Show when={mergedPair()}>
              {(pair) => (
                <TranscriptSegment
                  stage={pair().doThread?.id === state.currentId ? "Do" : "Wake"}
                  agentKind={state.agentKind}
                  model={state.model}
                />
              )}
            </Show>
            <For each={groups()}>
              {(g, i) => (
                <VirtualGroup
                  group={g}
                  // 运行中所有尚未闭合的轮次都保持活跃：补充提示词会新开一组，
                  // 若只标最后一组，前面仍在跑的工具/输出会像已停止。
                  active={isRunning() && !g.turn}
                  keepMounted={i() === lastGroupIndex()}
                  scrollEl={() => scrollRef}
                  viewportTick={viewportTick()}
                />
              )}
            </For>
          </Show>
          <For each={permissions()}>{(req) => <PermissionCard req={req} />}</For>
          {/* 吸底哨兵：发送新提示词时 scrollIntoView，不依赖虚拟列表占位高度 */}
          <div ref={endRef} class="transcript-end" aria-hidden="true" />
        </div>
      </div>

      <footer class="chat-foot">
        <Show when={state.plan && state.plan.length > 0}>
          <PlanCard plan={state.plan!} />
        </Show>
        <PlanActionCard />
        <Composer />
      </footer>
    </main>
  );
}
