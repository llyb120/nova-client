import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { api } from "../ipc";
import { compactThread, chatScrollToBottomSignal, openThread, setState, state } from "../store";
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

interface VirtualObserverPool {
  observer: IntersectionObserver;
  callbacks: Map<Element, () => void>;
}

const virtualObserverPools = new WeakMap<HTMLElement, VirtualObserverPool>();

const virtualBuffer = (root: HTMLElement) => Math.max(1200, root.clientHeight * 2);

/** 同一个滚动根只创建一个 IO；无论会话有多少轮，观察器数量都保持为 1。 */
function observeVirtualGroup(root: HTMLElement, element: Element, callback: () => void) {
  let pool = virtualObserverPools.get(root);
  if (!pool) {
    const callbacks = new Map<Element, () => void>();
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) callbacks.get(entry.target)?.();
      },
      { root, rootMargin: `${virtualBuffer(root)}px 0px` },
    );
    pool = { observer, callbacks };
    virtualObserverPools.set(root, pool);
  }
  pool.callbacks.set(element, callback);
  pool.observer.observe(element);
  return () => {
    pool!.observer.unobserve(element);
    pool!.callbacks.delete(element);
    if (pool!.callbacks.size === 0) {
      pool!.observer.disconnect();
      virtualObserverPools.delete(root);
    }
  };
}

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
  /** 已挂载内容在视口上方变高/变矮时补偿 scrollTop，保持正在阅读的内容不跳 */
  compensateHeight: (delta: number) => void;
}) {
  let ref: VirtualGroupElement | undefined;
  const [visible, setVisible] = createSignal(true);
  const [height, setHeight] = createSignal(0);
  const mounted = () => visible() || props.active || !!props.keepMounted;

  const rememberHeight = () => {
    if (!ref || !mounted()) return;
    const h = ref.getBoundingClientRect().height;
    const prev = height();
    if (h <= 0 || Math.abs(prev - h) <= 0.5) return;

    // 浏览器滚动锚定被禁用后，视口上方内容的真实尺寸变化必须由虚拟列表自己补偿。
    // 首次测量时内容本来就在正常流里，不能重复补；只修正已有占位高度的差值。
    const root = props.scrollEl();
    const aboveViewport =
      !!root && ref.getBoundingClientRect().bottom <= root.getBoundingClientRect().top;
    setHeight(h);
    if (prev > 0 && aboveViewport) props.compensateHeight(h - prev);
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
    const buffer = virtualBuffer(root);
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
    const stopObserving = observeVirtualGroup(root, ref, syncMounted);
    const ro = new ResizeObserver(() => rememberHeight());
    ro.observe(ref);
    // scroll 事件可以通过命中测试直接唤醒当前视口内的占位，避免等待异步 IO 回调。
    ref.mountVirtualGroup = () => setVisible(true);
    syncMounted();
    onCleanup(() => {
      stopObserving();
      ro.disconnect();
      if (ref) delete ref.mountVirtualGroup;
    });
  });

  // keepMounted / active 变为 true 时立即挂回；普通滚动交给 IO 和视口命中唤醒处理。
  createEffect(() => {
    if (props.active || props.keepMounted) setVisible(true);
  });

  return (
    <div
      ref={ref}
      class="vgroup"
      // 仅卸载时使用缓存高度。挂载后必须恢复自然高度，否则内容折叠时旧 min-height
      // 会反过来撑住观察目标，ResizeObserver 无法测到变矮后的真实尺寸。
      style={height() > 0 && !mounted() ? { height: `${height()}px` } : undefined}
    >
      <Show when={mounted()}>
        <TurnGroup group={props.group} active={props.active} />
      </Show>
    </div>
  );
}

interface VirtualGroupElement extends HTMLDivElement {
  mountVirtualGroup?: () => void;
}

interface TranscriptSegmentProps {
  stage: "Wake" | "Do";
  threadId: string;
  agentKind: Thread["agentKind"];
  model?: string | null;
}

function TranscriptSegment(props: TranscriptSegmentProps) {
  return (
    <div class="transcript-segment" data-thread-id={props.threadId}>
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
  let scrollQueued = false;
  let lastScrollTop = 0;
  let lastVirtualMountTop = Number.NaN;
  let pointerActive = false;

  const permissions = createMemo(() =>
    state.permissions.filter((p) => p.threadId === state.currentId),
  );

  const groups = createMemo<ReturnType<typeof groupItems>>(
    (prev) => groupItems(state.items as Item[], prev),
    [],
  );
  const isRunning = () => !!(state.currentId && state.running[state.currentId]);
  const lastGroupIndex = () => groups().length - 1;

  /**
   * IO 回调是异步的，拖动滚动条跨很长距离时可能晚一帧。先用命中测试找到一个锚点，
   * 再沿 DOM 上下连续唤醒整个视口和两屏缓冲区；不会像坐标采样那样漏掉短分组。
   */
  const mountVisibleVirtualGroups = (force = false) => {
    if (!scrollRef || !innerRef) return;
    const viewportHeight = scrollRef.clientHeight;
    if (
      !force &&
      Number.isFinite(lastVirtualMountTop) &&
      Math.abs(scrollRef.scrollTop - lastVirtualMountTop) < viewportHeight / 3
    ) {
      return;
    }
    const rootRect = scrollRef.getBoundingClientRect();
    const x = Math.min(
      rootRect.right - 1,
      Math.max(rootRect.left + 1, rootRect.left + rootRect.width / 2),
    );
    const sampleY = [
      rootRect.top + 1,
      rootRect.top + rootRect.height / 2,
      rootRect.bottom - 1,
    ];
    let anchor: VirtualGroupElement | undefined;
    for (const y of sampleY) {
      const hit = document.elementFromPoint(x, y)?.closest<VirtualGroupElement>(".vgroup");
      if (hit && innerRef.contains(hit)) {
        anchor = hit;
        break;
      }
    }
    if (!anchor) return;
    lastVirtualMountTop = scrollRef.scrollTop;

    const top = rootRect.top - virtualBuffer(scrollRef);
    const bottom = rootRect.bottom + virtualBuffer(scrollRef);
    const mount = (element: Element) => {
      if (element instanceof HTMLDivElement && element.classList.contains("vgroup")) {
        (element as VirtualGroupElement).mountVirtualGroup?.();
      }
    };
    mount(anchor);
    for (
      let element = anchor.previousElementSibling;
      element;
      element = element.previousElementSibling
    ) {
      if (element.getBoundingClientRect().bottom < top) break;
      mount(element);
    }
    for (
      let element = anchor.nextElementSibling;
      element;
      element = element.nextElementSibling
    ) {
      if (element.getBoundingClientRect().top > bottom) break;
      mount(element);
    }
  };

  const maxScrollTop = () =>
    scrollRef ? Math.max(0, scrollRef.scrollHeight - scrollRef.clientHeight) : 0;

  const isAtBottom = () => !scrollRef || maxScrollTop() - scrollRef.scrollTop <= 1;

  const cancelBottomFollow = () => setStickToBottom(false);

  const isToolDetailScroll = (target: EventTarget | null) =>
    target instanceof Element && !!target.closest(".tool-output, .tool-raw");

  const handleWheel = (event: WheelEvent) => {
    // 工具详情有独立滚动区，内部滚动不应改变外层会话的吸底状态。
    if (isToolDetailScroll(event.target)) return;
    if (!scrollRef || scrollRef.scrollHeight <= scrollRef.clientHeight + 1) return;
    if (event.deltaY > 0 && isAtBottom()) {
      if (!stickToBottom()) enableBottomFollow();
      return;
    }
    if (event.deltaY !== 0) cancelBottomFollow();
  };

  const handlePointerDown = (event: PointerEvent) => {
    // 仅跟踪外层滚动区的指针交互；拖动工具详情滚动条不能暂停吸底。
    if (isToolDetailScroll(event.target)) return;
    pointerActive = true;
  };

  const handleTranscriptScroll = () => {
    mountVisibleVirtualGroups();
    const currentTop = scrollRef?.scrollTop ?? 0;
    const atBottom = isAtBottom();
    if (stickToBottom()) {
      // 流式布局和虚拟分组高度变化也会触发 scroll；只有指针拖动时才把位移视为用户操作。
      if (pointerActive && !atBottom && currentTop !== lastScrollTop) cancelBottomFollow();
    } else if (atBottom && currentTop > lastScrollTop) {
      setStickToBottom(true);
    }
    lastScrollTop = currentTop;
  };

  const pinBottom = () => {
    if (!scrollRef || !stickToBottom() || pointerActive) return;
    scrollRef.scrollTop = maxScrollTop();
    lastScrollTop = scrollRef.scrollTop;
    mountVisibleVirtualGroups(true);
  };

  const compensateVirtualHeight = (delta: number) => {
    if (!scrollRef || Math.abs(delta) <= 0.5) return;
    scrollRef.scrollTop += delta;
    lastScrollTop = scrollRef.scrollTop;
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
    state.items.reduce(
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
      scheduleBottomPin();
    });
    ro.observe(innerRef);
    ro.observe(scrollRef);

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
      if (isToolDetailScroll(target)) return;
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
  const isFireThread = () => /^\[Fire\]/.test(currentMeta()?.title ?? "");
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
       <div
        class="transcript"
        ref={scrollRef}
        onScroll={handleTranscriptScroll}
        onWheel={handleWheel}
        onPointerDown={handlePointerDown}
      >
        <div class="transcript-inner" ref={innerRef}>
          <Show when={state.items.length === 0 && !state.loadingThread}>
            <div class="transcript-hint">
              在下方输入任务，{agentLabel(state.agentKind)} 将在{" "}
              <code>{cwdDisplay()}</code> 中工作。
            </div>
          </Show>
          <Show keyed when={state.currentId}>

            <For each={groups()}>
              {(g, i) => (
                <VirtualGroup
                  group={g}
                  // 运行中所有尚未闭合的轮次都保持活跃：补充提示词会新开一组，
                  // 若只标最后一组，前面仍在跑的工具/输出会像已停止。
                  active={isRunning() && !g.turn}
                  keepMounted={i() === lastGroupIndex()}
                  scrollEl={() => scrollRef}
                  compensateHeight={compensateVirtualHeight}
                />
              )}
            </For>
          </Show>
          <For each={permissions()}>{(req) => <PermissionCard req={req} />}</For>
        </div>
      </div>
      <Show when={stageThreads().length > 1}>
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

      <footer class="chat-foot">
        <Show when={state.plan && state.plan.length > 0}>
          <div classList={{ "fire-plan-inline": isFireThread() && stageThreads().length > 1 }}>
            <PlanCard plan={state.plan!} />
          </div>
        </Show>
        <PlanActionCard />
        <Composer />
      </footer>
    </main>
  );
}
