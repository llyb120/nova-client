import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show, untrack } from "solid-js";
import { Portal } from "solid-js/web";
import { api } from "../ipc";
import { buildTimeNotesPrompt } from "../builtinPrompts";
import {
  compactThread,
  chatScrollToBottomSignal,
  createThread,
  openThread,
  pickThreadModel,
  refreshThreads,
  sendPrompt,
  setState,
  setTimeMachineEditTarget,
  state,
  timeMachineChangedSignal,
} from "../store";
import { mountSessionShortcuts } from "../sessionShortcuts";
import type { AgentKind, Item, Thread, TimeMachineCheckpoint, TimeMachinePrompt, TimeMachineTimeline } from "../types";
import { agentLabel } from "../utils";
import { TranscriptCanvas, type TranscriptCanvasApi } from "../canvasTranscript/TranscriptCanvas";
import { CanvasTranscript, type CanvasTranscriptHandle } from "./CanvasTranscript";
import { Composer } from "./Composer";
import { IconBroadcast, IconCompress, IconDownload, IconShare, IconStar, IconStopwatch } from "./icons";
import { PermissionCard } from "./PermissionCard";
import { PlanActionCard } from "./PlanActionCard";
import { ShareModal } from "./ShareModal";
import { TimeNotesModal } from "./TimeNotesModal";
import { TypewriterText } from "./TypewriterText";
import { fmtTokens, type Group, groupItems, TurnGroup } from "./TurnGroup";

interface VirtualObserverPool {
  intersectionObserver: IntersectionObserver;
  resizeObserver: ResizeObserver;
  intersectionCallbacks: Map<Element, () => void>;
  resizeCallbacks: Map<Element, () => void>;
}

const virtualObserverPools = new WeakMap<HTMLElement, VirtualObserverPool>();

const virtualBuffer = (root: HTMLElement) => Math.max(1200, root.clientHeight * 2);

/** 同一个滚动根只创建一组观察器；轮次再多也不新增 IO / ResizeObserver 实例。 */
function observeVirtualGroup(
  root: HTMLElement,
  element: Element,
  intersectionCallback: () => void,
  resizeCallback: () => void,
) {
  let pool = virtualObserverPools.get(root);
  if (!pool) {
    const intersectionCallbacks = new Map<Element, () => void>();
    const resizeCallbacks = new Map<Element, () => void>();
    const intersectionObserver = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) intersectionCallbacks.get(entry.target)?.();
      },
      { root, rootMargin: `${virtualBuffer(root)}px 0px` },
    );
    const resizeObserver = new ResizeObserver((entries) => {
      for (const entry of entries) resizeCallbacks.get(entry.target)?.();
    });
    pool = { intersectionObserver, resizeObserver, intersectionCallbacks, resizeCallbacks };
    virtualObserverPools.set(root, pool);
  }
  pool.intersectionCallbacks.set(element, intersectionCallback);
  pool.resizeCallbacks.set(element, resizeCallback);
  pool.intersectionObserver.observe(element);
  pool.resizeObserver.observe(element);
  return () => {
    pool!.intersectionObserver.unobserve(element);
    pool!.resizeObserver.unobserve(element);
    pool!.intersectionCallbacks.delete(element);
    pool!.resizeCallbacks.delete(element);
    if (pool!.intersectionCallbacks.size === 0) {
      pool!.intersectionObserver.disconnect();
      pool!.resizeObserver.disconnect();
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
  index: number;
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

  /** 挂回视口上方的占位时，立即补偿真实高度差，避免一次小滚动产生大幅跳跃。 */
  const mountContent = () => {
    if (!ref || visible()) return;
    const root = props.scrollEl();
    const before = ref.getBoundingClientRect();
    const aboveViewport = !!root && before.bottom <= root.getBoundingClientRect().top;
    setVisible(true);

    const h = ref.getBoundingClientRect().height;
    const prev = height();
    if (h > 0 && Math.abs(h - prev) > 0.5) {
      setHeight(h);
      if (prev > 0 && aboveViewport) props.compensateHeight(h - prev);
    }
  };

  /**
   * 不直接信任 IntersectionObserver 传来的 entry：快速程序化滚动时，WebView2 可能在
   * 回调执行前已经滚到了新位置，旧 entry 会把当前视口里的轮次误卸载成一整块空白。
   * 每次都用当前几何位置复核，并由父级滚动 tick 再兜一层。
   */
  const syncMounted = () => {
    if (!ref || props.active || props.keepMounted) {
      mountContent();
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
      mountContent();
    } else {
      rememberHeight();
      setVisible(false);
    }
  };

  onMount(() => {
    if (!ref) return;
    const root = props.scrollEl();
    if (!root) return;
    const stopObserving = observeVirtualGroup(root, ref, syncMounted, rememberHeight);
    // scroll 事件可以通过命中测试直接唤醒当前视口内的占位，并同步修正锚点。
    ref.mountVirtualGroup = mountContent;
    syncMounted();
    onCleanup(() => {
      stopObserving();
      if (ref) delete ref.mountVirtualGroup;
    });
  });

  // keepMounted / active 变为 true 时立即挂回；普通滚动交给 IO 和视口命中唤醒处理。
  createEffect(() => {
    if (props.active || props.keepMounted) mountContent();
  });

  return (
    <div
      ref={ref}
      class="vgroup"
      data-group-index={props.index}
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
  let transcriptRef: CanvasTranscriptHandle | undefined;
  let qwenApi: TranscriptCanvasApi | null = null;
  const renderMode = () => state.settings?.chatViewRender ?? "canvas";
  /** master 默认 canvas 渲染 */
  const useCanvas = () => renderMode() === "canvas";
  /** canvas(qwen)：feat/glm_canvas 分支的独立 canvas 渲染，与默认 canvas 互不影响 */
  const useQwenCanvas = () => renderMode() === "canvas_qwen";
  const useAnyCanvas = () => renderMode() !== "dom";
  const [stickToBottom, setStickToBottom] = createSignal(true);

  mountSessionShortcuts({
    allowedActions: ["selectModel"],
    onSelectProject: () => {},
    onSelectModel: (agentKind, model, quotaPeer) => {
      // 进行中的会话不支持额度租借切换（与 Composer 一致）。
      if (quotaPeer) return;
      void pickThreadModel(agentKind, model);
    },
  });
  let scrollQueued = false;
  let scrollFrame = 0;
  let lastScrollTop = 0;
  let lastVirtualMountTop = Number.NaN;
  let pointerActive = false;

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
  const lastGroupIndex = () => groups().length - 1;
  const timeStops = createMemo(() => {
    let turn = 0;
    return groups().flatMap((group, index) => {
      if (!group.user) return [];
      turn++;
      const text = group.user.text.replace(/\s+/g, " ").trim();
      return [{ index, turn, label: text || `第 ${turn} 轮` }];
    });
  });
  const [activeTimeIndex, setActiveTimeIndex] = createSignal(-1);
  const latestTimeIndex = () => timeStops().at(-1)?.index ?? -1;

  const syncTimeCursor = () => {
    const stops = timeStops();
    if (stops.length === 0) { setActiveTimeIndex(-1); return; }
    if (useQwenCanvas()) {
      // qwen canvas 内部自管滚动且不暴露当前分组位置；吸底时光标跟随最新轮次
      if (stickToBottom()) setActiveTimeIndex(latestTimeIndex());
      return;
    }
    if (useCanvas()) {
      if (!transcriptRef) return;
      const groupIndex = transcriptRef.activeGroup();
      let best = 0;
      for (let i = 0; i < stops.length; i++) {
        if (stops[i].index <= groupIndex) best = stops[i].index;
      }
      setActiveTimeIndex(best);
      return;
    }
    if (!scrollRef || !innerRef) return;
    const elements = innerRef.querySelectorAll<HTMLElement>(":scope > .vgroup");
    const top = scrollRef.getBoundingClientRect().top + 32;
    let low = 0;
    let high = stops.length;
    while (low < high) {
      const middle = (low + high) >> 1;
      const element = elements[stops[middle].index];
      if (element && element.getBoundingClientRect().top <= top) low = middle + 1;
      else high = middle;
    }
    setActiveTimeIndex(stops[Math.max(0, low - 1)].index);
  };

  const travelTo = (index: number) => {
    cancelBottomFollow();
    if (useQwenCanvas()) {
      qwenApi?.scrollToGroup(index);
      return;
    }
    if (useCanvas()) {
      transcriptRef?.scrollToGroup(index);
      syncTimeCursor();
      return;
    }
    const element = innerRef?.querySelector<VirtualGroupElement>(
      `.vgroup[data-group-index="${index}"]`,
    );
    if (!element) return;
    element.mountVirtualGroup?.();
    requestAnimationFrame(() => {
      element.scrollIntoView({ block: "start" });
      syncTimeCursor();
    });
  };

  const returnToNow = () => {
    enableBottomFollow();
    setActiveTimeIndex(latestTimeIndex());
  };

  /**
   * IO 回调是异步的，拖动滚动条跨很长距离时可能晚一帧。WebView2 的命中测试在合成器
   * 快速滚动期间还可能停留在旧位置，因此不能依赖 elementFromPoint 找锚点。这里直接按
   * wrapper 的当前几何位置二分出首个候选，再同步挂载视口和两屏缓冲区，避免工具详情占位
   * 在快速滑动时整屏留白。
   */
  const mountVisibleVirtualGroups = (force = false) => {
    if (useAnyCanvas() || !scrollRef || !innerRef) return;
    const viewportHeight = scrollRef.clientHeight;
    if (
      !force &&
      Number.isFinite(lastVirtualMountTop) &&
      Math.abs(scrollRef.scrollTop - lastVirtualMountTop) < viewportHeight / 3
    ) {
      return;
    }

    const elements = innerRef.querySelectorAll<VirtualGroupElement>(":scope > .vgroup");
    if (elements.length === 0) return;
    lastVirtualMountTop = scrollRef.scrollTop;

    const rootRect = scrollRef.getBoundingClientRect();
    const top = rootRect.top - virtualBuffer(scrollRef);
    const bottom = rootRect.bottom + virtualBuffer(scrollRef);

    let low = 0;
    let high = elements.length;
    while (low < high) {
      const middle = (low + high) >> 1;
      if (elements[middle].getBoundingClientRect().bottom < top) low = middle + 1;
      else high = middle;
    }

    for (let index = low; index < elements.length; index++) {
      const element = elements[index];
      if (element.getBoundingClientRect().top > bottom) break;
      element.mountVirtualGroup?.();
    }
  };

  const maxScrollTop = () =>
    useCanvas()
      ? (transcriptRef?.maxScrollTop() ?? 0)
      : useQwenCanvas()
        ? 0
        : scrollRef
          ? Math.max(0, scrollRef.scrollHeight - scrollRef.clientHeight)
          : 0;

  const isAtBottom = () =>
    useCanvas()
      ? (transcriptRef?.isAtBottom() ?? true)
      : useQwenCanvas()
        ? stickToBottom()
        : !scrollRef || maxScrollTop() - scrollRef.scrollTop <= 1;

  const cancelBottomFollow = () => setStickToBottom(false);

  const isToolDetailScroll = (target: EventTarget | null) =>
    target instanceof Element && !!target.closest(".tool-output, .tool-raw");

  const handleWheel = (event: WheelEvent) => {
    if (useAnyCanvas()) return;
    if (isToolDetailScroll(event.target)) return;
    if (!scrollRef || scrollRef.scrollHeight <= scrollRef.clientHeight + 1) return;
    if (event.deltaY > 0 && isAtBottom()) {
      if (!stickToBottom()) enableBottomFollow();
      return;
    }
    if (event.deltaY !== 0) cancelBottomFollow();
  };

  const handlePointerDown = (event: PointerEvent) => {
    if (useAnyCanvas()) return;
    if (isToolDetailScroll(event.target)) return;
    pointerActive = true;
  };

  const processTranscriptScroll = () => {
    if (useAnyCanvas()) {
      syncTimeCursor();
      return;
    }
    mountVisibleVirtualGroups();
    syncTimeCursor();
    const currentTop = scrollRef?.scrollTop ?? 0;
    const atBottom = isAtBottom();
    if (stickToBottom()) {
      if (pointerActive && !atBottom && currentTop !== lastScrollTop) cancelBottomFollow();
    } else if (atBottom && currentTop > lastScrollTop) {
      setStickToBottom(true);
    }
    lastScrollTop = currentTop;
  };

  const handleTranscriptScroll = () => {
    if (scrollFrame) return;
    scrollFrame = requestAnimationFrame(() => {
      scrollFrame = 0;
      processTranscriptScroll();
    });
  };

  const pinBottom = () => {
    if (!stickToBottom() || pointerActive) return;
    if (useQwenCanvas()) return; // qwen canvas 依据 stickToBottom prop 自行钉底
    if (useCanvas()) {
      transcriptRef?.scrollToBottom();
      lastScrollTop = transcriptRef?.scrollTop() ?? 0;
      return;
    }
    if (!scrollRef) return;
    scrollRef.scrollTop = maxScrollTop();
    lastScrollTop = scrollRef.scrollTop;
    mountVisibleVirtualGroups(true);
  };

  const compensateVirtualHeight = (delta: number) => {
    if (useAnyCanvas() || !scrollRef || Math.abs(delta) <= 0.5) return;
    scrollRef.scrollTop += delta;
    lastScrollTop = scrollRef.scrollTop;
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

  const finishPointerInteraction = () => {
    if (!useAnyCanvas() && scrollFrame) {
      cancelAnimationFrame(scrollFrame);
      scrollFrame = 0;
      processTranscriptScroll();
    }
    pointerActive = false;
    if (stickToBottom()) scheduleBottomPin();
  };

  // 会话累计 token 用量：直接对当前展示的 turn 项求和。turn 项经 upsert 按 id
  // 覆盖落位，求和天然不会重复累计；世界线预览（previewItems）或恢复切换后
  // items 被整体替换，总量随之指向所预览/所在的那条分支。
  const tokenStats = createMemo(() => {
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
  // 本轮进行中的实时用量（Vega 流式上报；预览世界线时不混入当前轮）。
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
    const target = totalTokens();
    const from = untrack(shownTokens);
    if (from === target) return;
    stopTokenRoll();
    setTokensRolling(true);
    // 变化越大滚动越久，设上限避免世界线大跨度切换时数字跑太久。
    const duration = Math.min(1200, 350 + Math.abs(target - from) / 20);
    const start = performance.now();
    const step = (now: number) => {
      const progress = Math.min(1, (now - start) / duration);
      // easeOutCubic：前快后慢，像计数器缓缓停到最终值。
      const eased = 1 - Math.pow(1 - progress, 3);
      setShownTokens(Math.round(from + (target - from) * eased));
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
      if (useQwenCanvas()) return; // qwen canvas 自带键盘滚动处理
      if (useCanvas()) {
        if (transcriptRef?.hasFocusedInput()) return;
        const delta = scrollsDown ? 100 : -100;
        transcriptRef?.scrollBy(delta);
        if (scrollsDown && isAtBottom() && !stickToBottom()) enableBottomFollow();
        else if (!isAtBottom()) cancelBottomFollow();
        return;
      }
      if (!scrollRef || scrollRef.scrollHeight <= scrollRef.clientHeight + 1) return;
      const target = event.target;
      if (target instanceof Node && target !== document.body && !scrollRef.contains(target)) return;
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
    let ro: ResizeObserver | undefined;
    if (innerRef && scrollRef) {
      ro = new ResizeObserver(() => { scheduleBottomPin(); });
      ro.observe(innerRef);
      ro.observe(scrollRef);
    }
    window.addEventListener("keydown", handleScrollKey, true);
    window.addEventListener("pointerup", finishPointerInteraction, true);
    window.addEventListener("pointercancel", finishPointerInteraction, true);
    onCleanup(() => {
      ro?.disconnect();
      if (scrollFrame) cancelAnimationFrame(scrollFrame);
      window.removeEventListener("keydown", handleScrollKey, true);
      window.removeEventListener("pointerup", finishPointerInteraction, true);
      window.removeEventListener("pointercancel", finishPointerInteraction, true);
    });
  });

  // 切换会话时从底部开始；后续尺寸变化由 ResizeObserver 持续对齐。
  createEffect((prevId: string | null | undefined) => {
    const id = state.currentId;
    if (id !== prevId) {
      enableBottomFollow();
      if (useQwenCanvas()) qwenApi?.enableBottomFollow();
      setActiveTimeIndex(latestTimeIndex());
    }
    return id;
  }, undefined);

  // 切换 DOM / Canvas 渲染后重新吸底，避免滚动状态串到另一套视图。
  createEffect((prevMode: string) => {
    const mode = renderMode();
    if (prevMode && prevMode !== mode) enableBottomFollow();
    return mode;
  }, "");

  // 会话加载和新增轮次后，让“现在”刻度跟随最新用户轮次；回看过去时不抢走光标。
  createEffect(() => {
    const latest = latestTimeIndex();
    if (stickToBottom()) setActiveTimeIndex(latest);
  });

  // 主动发送新提示词时重新进入吸底，无动画直接显示最新内容。
  createEffect(() => {
    const tick = chatScrollToBottomSignal();
    if (tick === 0) return;
    enableBottomFollow();
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
  /** 是否工作流/Fire/员工事件链的会话标题（决定导航栏是否从第一个节点起就显示）。 */
  const isStageTitle = (title: string) =>
    /^\[WF\]/.test(title) ||
    /^\[Fire\]/.test(title) ||
    /\]\s*(Wake|Do|Dream|巡查)/.test(title);
  const showStageRail = () => {
    const threads = stageThreads();
    // 链上有多个会话，或链本身就是工作流/Fire/员工事件链（从第一个节点起就显示）。
    return threads.length > 1 || threads.some((thread) => isStageTitle(thread.title));
  };
  const stageName = (thread: (typeof state.threads)[number]) => {
    // 工作流节点：[WF] 节点名 · 第N次（· 待补充等状态后缀），显示节点名。
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
    // 工作流链的起点会话（用户输入目标的会话）显示为「目标」。
    if (!thread.parentThreadId && stageThreads().some((t) => isStageTitle(t.title))) return "目标";
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

    // 当前时间线固定占最左一列；旁支在分叉时申请列，行区间不重叠的旁支复用同一列。
    const nodes: GraphNode[] = [];
    const edgeIds: Array<{ from: string; to: string }> = [];
    let maxPromptCount = currentPrompts.length;
    const sortedChildren = (node: PromptTreeNode) =>
      [...node.children].sort(
        (left, right) => Number(right.onCurrentPath) - Number(left.onCurrentPath),
      );
    // 旁支的“主脊”：沿首个子节点一路向下的链；该列被占用的行区间即 [起点行, 主脊末端行]。
    const spineEndRow = (node: PromptTreeNode): number => {
      let end = node.promptCount;
      let cursor = node;
      for (;;) {
        const next = sortedChildren(cursor).find((child) => !child.onCurrentPath);
        if (!next) return end;
        end = next.promptCount;
        cursor = next;
      }
    };
    // laneIntervals[lane] = 该列已占用的行区间；相邻区间至少空一行，避免上下分支首尾相接看似相连。
    const laneIntervals: Array<Array<[number, number]>> = [[]];
    const forkLane = (node: PromptTreeNode): number => {
      const start = node.promptCount;
      const end = spineEndRow(node);
      for (let lane = 1; lane < laneIntervals.length; lane++) {
        if (laneIntervals[lane].every(([s, e]) => start > e + 1 || end < s - 1)) {
          laneIntervals[lane].push([start, end]);
          return lane;
        }
      }
      laneIntervals.push([[start, end]]);
      return laneIntervals.length - 1;
    };
    const place = (node: PromptTreeNode, lane: number) => {
      const nodeLane = node.onCurrentPath ? 0 : lane;
      maxPromptCount = Math.max(maxPromptCount, node.promptCount);
      nodes.push({ ...node, x: 18 + nodeLane * 26, y: 20 + (node.promptCount - 1) * 32 });

      let continuedBranch = false;
      for (const child of sortedChildren(node)) {
        edgeIds.push({ from: node.id, to: child.id });
        if (child.onCurrentPath) {
          place(child, 0);
        } else if (!node.onCurrentPath && !continuedBranch) {
          continuedBranch = true;
          place(child, nodeLane);
        } else {
          place(child, forkLane(child));
        }
      }
    };
    for (const node of sortedChildren(root)) place(node, node.onCurrentPath ? 0 : forkLane(node));

    const positions = new Map(nodes.map((node) => [node.id, node]));
    const nowY = 20 + Math.max(1, maxPromptCount) * 32;
    const laneCount = Math.max(1, laneIntervals.length);
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
      <div class="chat-body">
        <Show
          when={renderMode() !== "dom"}
          fallback={
            <div
              class="transcript"
              classList={{ "checkpoint-preview": !!previewItems(), "checkpoint-preview-fading": previewFading() }}
              ref={scrollRef}
              onScroll={handleTranscriptScroll}
              onWheel={handleWheel}
              onPointerDown={handlePointerDown}
            >
              <div class="transcript-inner" ref={innerRef}>
                <Show when={previewCheckpointId()}>
                  <button
                    type="button"
                    class="checkpoint-preview-banner"
                    title="回到当前时间线和最新消息"
                    onClick={returnToCurrentTimeline}
                  >
                    回到当前时间线
                  </button>
                </Show>
                <Show when={displayedItems().length === 0 && !state.loadingThread}>
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
                        index={i()}
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
          }
        >
          <Show
            when={useQwenCanvas()}
            fallback={
              <CanvasTranscript
                ref={(handle) => { transcriptRef = handle; scheduleBottomPin(); }}
                threadId={state.currentId}
                groups={groups()}
                permissions={permissions()}
                running={isRunning()}
                loading={state.loadingThread}
                preview={!!previewCheckpointId()}
                onReturnToCurrent={returnToCurrentTimeline}
                onScroll={(top, max, user) => {
                  if (user) {
                    setStickToBottom(max - top <= 2);
                    lastScrollTop = top;
                  }
                  syncTimeCursor();
                }}
                emptyHint={`在下方输入任务，${agentLabel(state.agentKind)} 将在 ${cwdDisplay()} 中工作。`}
              />
            }
          >
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
              onApi={(a) => { qwenApi = a; scheduleBottomPin(); }}
            />
          </Show>
        </Show>
      </div>

      <footer class="chat-foot">
        {/* 暂时隐藏「计划」面板；内部 plan 状态与事件仍照常更新 */}
        <PlanActionCard />
        <Composer />
      </footer>
        </div>

      <Show when={showTimeMachine()}>
        <aside
          class="repo-time-machine"
          aria-label="会话与工作目录分支时间线"
        >
          <div class="repo-time-machine-label">
            <button
              type="button"
              class="repo-time-notes"
              title="把这条世界线的经验沉淀为一个 skill：新开一个训练会话自行阅读并逐级分析"
              disabled={!!restoringCheckpoint()}
              onClick={() => setShowTimeNotes(true)}
            >
              时光笔记
            </button>
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
                onClick={() => previewItems() ? returnToCurrentTimeline() : returnToNow()}
              >
                <span class="repo-time-now-pulse" />
                现在
              </button>
            </div>
          </div>
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
    </main>
  );
}
