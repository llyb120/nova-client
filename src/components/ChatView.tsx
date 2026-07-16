import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { api } from "../ipc";
import { compactThread, chatScrollToBottomSignal, setState, state } from "../store";
import { firstWakeDoPairForThread } from "../threadDisplay";
import type { Item, Thread } from "../types";
import { agentLabel } from "../utils";
import { Composer } from "./Composer";
import { IconBroadcast, IconCompress, IconDownload, IconShare, IconStar } from "./icons";
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
  const [stickToBottom, setStickToBottom] = createSignal(true);
  const [viewportTick, setViewportTick] = createSignal(0);
  let scrollQueued = false;
  let viewportRaf = 0;
  let lastScrollTop = 0;
  let pointerActive = false;

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

  const maxScrollTop = () =>
    scrollRef ? Math.max(0, scrollRef.scrollHeight - scrollRef.clientHeight) : 0;

  const isAtBottom = () => !scrollRef || maxScrollTop() - scrollRef.scrollTop <= 1;

  const cancelBottomFollow = () => setStickToBottom(false);

  const isToolDetailScroll = (target: EventTarget | null) =>
    target instanceof Element && !!target.closest(".tool-output, .tool-raw");

  const handleWheel = (event: WheelEvent) => {
    if (isToolDetailScroll(event.target)) {
      cancelBottomFollow();
      return;
    }
    if (!scrollRef || scrollRef.scrollHeight <= scrollRef.clientHeight + 1) return;
    if (event.deltaY > 0 && isAtBottom()) {
      if (!stickToBottom()) enableBottomFollow();
      return;
    }
    if (event.deltaY !== 0) cancelBottomFollow();
  };

  const handlePointerDown = (event: PointerEvent) => {
    pointerActive = true;
    if (isToolDetailScroll(event.target)) cancelBottomFollow();
  };

  const handleTranscriptScroll = () => {
    refreshVirtualGroups();
    const currentTop = scrollRef?.scrollTop ?? 0;
    const atBottom = isAtBottom();
    if (stickToBottom()) {
      if (!atBottom && currentTop !== lastScrollTop) cancelBottomFollow();
    } else if (atBottom && currentTop > lastScrollTop) {
      setStickToBottom(true);
    }
    lastScrollTop = currentTop;
  };

  const pinBottom = () => {
    if (!scrollRef || !stickToBottom() || pointerActive) return;
    scrollRef.scrollTop = maxScrollTop();
    lastScrollTop = scrollRef.scrollTop;
    refreshVirtualGroups();
  };

  // 合并同一轮内容变化，并在下一次绘制前直接钉底；不做滚动动画或多帧追赶。
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

  const finishPointerInteraction = () => {
    handleTranscriptScroll();
    pointerActive = false;
    if (stickToBottom()) scheduleBottomPin();
  };

  // 会话累计 token 用量
  const totalTokens = createMemo(() =>
    [...(mergedWake()?.items ?? []), ...state.items].reduce(
      (sum, it) => (it.type === "turn" && it.totalTokens ? sum + it.totalTokens : sum),
      0,
    ),
  );

  // 流式内容变化后请求一次绘制前钉底；自由浏览时 pinBottom 会直接退出。
  createEffect(() => {
    const len = state.items.length;
    const last = state.items[len - 1];
    if (last && "text" in last) void (last as { text: string }).text.length;
    void permissions().length;
    scheduleBottomPin();
  });

  onMount(() => {
    if (!innerRef || !scrollRef) return;
    const ro = new ResizeObserver(() => {
      refreshVirtualGroups();
      scheduleBottomPin();
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
      if (isToolDetailScroll(target)) {
        cancelBottomFollow();
        return;
      }
      if (scrollsDown) {
        if (isAtBottom()) {
          if (!stickToBottom()) enableBottomFollow();
          return;
        }
      }
      cancelBottomFollow();
    };
    window.addEventListener("keydown", handleScrollKey, true);
    window.addEventListener("pointerup", finishPointerInteraction, true);
    window.addEventListener("pointercancel", finishPointerInteraction, true);
    onCleanup(() => {
      ro.disconnect();
      window.removeEventListener("keydown", handleScrollKey, true);
      window.removeEventListener("pointerup", finishPointerInteraction, true);
      window.removeEventListener("pointercancel", finishPointerInteraction, true);
      if (viewportRaf) cancelAnimationFrame(viewportRaf);
    });
  });

  // 切换会话时从底部开始；后续尺寸变化由 ResizeObserver 持续对齐。
  createEffect((prevId: string | null | undefined) => {
    const id = state.currentId;
    if (id !== prevId) enableBottomFollow();
    return id;
  }, undefined);

  // 主动发送新提示词时重新进入吸底，无动画直接显示最新内容。
  createEffect(() => {
    const tick = chatScrollToBottomSignal();
    if (tick === 0) return;
    enableBottomFollow();
  });

  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [showShare, setShowShare] = createSignal(false);

  const currentMeta = createMemo(() =>
    state.threads.find((t) => t.id === state.currentId),
  );
  const [starUpdating, setStarUpdating] = createSignal(false);
  const roamingRole = () => currentMeta()?.roamingRole ?? null;
  const canStar = () => {
    const meta = currentMeta();
    return !!meta && !meta.employeeId && !meta.mindThread && !meta.roamingRole && !meta.quotaPeerName;
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
