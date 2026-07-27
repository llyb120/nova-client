// TranscriptCanvas.tsx — 会话消息列表的 canvas 实现。
// 一个 <canvas> 承担全部消息渲染（文本/气泡/工具卡/diff/权限卡/选区/滚动），
// DOM 只保留：编辑覆盖层（用户消息编辑、权限卡自定义输入）、tooltip、右键菜单。
// 滚动/吸底状态机移植自 ChatView.tsx（scrollRef 时代），几何来源换成 canvas 布局表。

import { message } from "@tauri-apps/plugin-dialog";
import {
  createEffect,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
} from "solid-js";
import { api } from "../../ipc.js";
import {
  chatScrollToBottomSignal,
  editUserMessage,
  isExpanded,
  respondPermission,
  state,
  toggleExpanded,
} from "../../store.js";
import type { Item, PermissionRequest, RevertChange, UserItem } from "../../types.js";
import { agentLabel } from "../../utils.js";
import { CanvasRenderer } from "../canvasdom.js";
import { hitTest } from "../interaction.js";
import { Node } from "../node.js";
import { onThemeChange } from "../theme.js";
import {
  buildCheckpointBanner,
  buildGroup,
  buildHint,
  buildItem,
  buildPermissionCard,
  type BuilderCtx,
} from "./builders.js";
import type { Group } from "./groups.js";
import { createFileContextMenu } from "../../components/FileContextMenu.js";
import { createImageAttachments, ImageAttachmentStrip } from "../../components/ImageAttachmentStrip.js";

export interface TranscriptCanvasApi {
  scrollToGroup: (index: number) => void;
  returnToNow: () => void;
  groupCount: () => number;
}

// —— 流式平滑出字参数（与 Markdown.tsx 一致） ——
const TICK_MS = 33;
const MIN_STEP = 2;
const CATCH_UP = 8;
const JUMP_AT = 3000;

export function TranscriptCanvas(props: {
  items: Item[];
  groups: Group[];
  permissions: PermissionRequest[];
  previewFading: boolean;
  showPreviewBanner: boolean;
  onReturnToNow: () => void;
  onActiveGroupChange: (index: number) => void;
  onStickToBottomChange?: (stick: boolean) => void;
  registerApi: (api: TranscriptCanvasApi) => void;
}) {
  let wrapRef: HTMLDivElement | undefined;
  let canvasRef: HTMLCanvasElement | undefined;
  let overlayRef: HTMLDivElement | undefined;
  let renderer: CanvasRenderer | undefined;

  const [stickToBottom, setStickToBottomRaw] = createSignal(true);
  /** 吸底状态变更同时上报（世界线「现在」按钮的 active 态） */
  const setStickToBottom = (v: boolean) => {
    if (stickToBottom() === v) return;
    setStickToBottomRaw(v);
    props.onStickToBottomChange?.(v);
  };
  let pointerActive = false;
  let lastScrollTop = 0;

  // 编辑覆盖层
  const [editing, setEditing] = createSignal<{ item: UserItem; draft: string } | null>(null);
  const [editHeight, setEditHeight] = createSignal(120);
  const attach = createImageAttachments();

  // 复制按钮「已复制」态
  const [copiedCodeKey, setCopiedCodeKey] = createSignal<string | null>(null);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;

  // tooltip
  const [tooltip, setTooltip] = createSignal<{ text: string; x: number; y: number } | null>(null);

  // 权限卡 question 状态（DOM 版是组件内 signal，canvas 重建会丢 → 按 requestKey 存）
  const permStates = new Map<string, { answers: string[][]; custom: string[] }>();
  const [permVersion, setPermVersion] = createSignal(0);
  const undoBusySet = new Set<string>();
  const [undoVersion, setUndoVersion] = createSignal(0);

  // 覆盖层几何
  const [editRect, setEditRect] = createSignal<{ x: number; y: number; w: number } | null>(null);
  const [permInputRects, setPermInputRects] = createSignal<
    Array<{ key: string; x: number; y: number; w: number; h: number; requestKey: string; index: number }>
  >([]);

  const fileMenu = createFileContextMenu();

  // —— 节点树与缓存 ——
  const rootNode = new Node("div", { overflow: "hidden", width: "100%", height: 100 });
  const centerNode = new Node("div", { width: "100%", display: "flex", justifyContent: "center" });
  rootNode.appendChild(centerNode);
  let contentNode: Node | null = null;
  let groupNodes: Node[] = [];
  let userRowNodes = new Map<number, Node>();
  const itemCache = new Map<string, { node: Node | null; sig: string }>();
  const typewriter = new Map<number, { shown: string; timer?: number }>();

  const columnWidth = () => Math.min(Math.max(720, window.innerWidth * 0.78), 980);
  const columnPadX = () => Math.min(Math.max(14, window.innerWidth * 0.03), 28);

  const isRunning = () => !!(state.currentId && state.running[state.currentId]);

  function sigOf(text: string): string {
    return `${text.length}:${text.charCodeAt(0) || 0}:${text.charCodeAt(text.length - 1) || 0}`;
  }

  /** 流式平滑出字：返回该 assistant 消息当前应显示的文本前缀（与 Markdown.tsx 同节拍） */
  function typewriterText(itemId: number, full: string): string {
    let entry = typewriter.get(itemId);
    if (!entry) {
      entry = { shown: full };
      typewriter.set(itemId, entry);
      return full;
    }
    if (!full.startsWith(entry.shown)) {
      entry.shown = full;
      if (entry.timer !== undefined) {
        window.clearTimeout(entry.timer);
        entry.timer = undefined;
      }
      return full;
    }
    const backlog = full.length - entry.shown.length;
    if (backlog <= 0) return entry.shown;
    if (backlog > JUMP_AT) {
      entry.shown = full;
      return full;
    }
    if (entry.timer === undefined) {
      const tick = () => {
        entry!.timer = undefined;
        const cur = entry!.shown;
        const target = fullTextOf(itemId);
        if (!target.startsWith(cur)) {
          entry!.shown = target;
        } else {
          const left = target.length - cur.length;
          if (left > 0) {
            const step = Math.max(MIN_STEP, Math.ceil(left / CATCH_UP));
            let end = cur.length + step;
            const c = target.charCodeAt(end - 1);
            if (c >= 0xd800 && c <= 0xdbff && end < target.length) end += 1;
            entry!.shown = target.slice(0, end);
            if (end < target.length) entry!.timer = window.setTimeout(tick, TICK_MS);
          }
        }
        rebuildSoon();
      };
      entry.timer = window.setTimeout(tick, 0);
    }
    return entry.shown;
  }

  function fullTextOf(itemId: number): string {
    const it = (props.items as Item[]).find((i) => i.id === itemId);
    return it && "text" in it ? (it as { text: string }).text : "";
  }

  // —— builder ctx ——
  const builderCtx: BuilderCtx = {
    md: {
      onOpenFile: (path, line) => {
        const isImage = /\.(?:avif|bmp|gif|ico|jpe?g|png|svg|webp)$/i.test(path);
        const id = state.currentId;
        if (!id) return;
        const action = isImage ? api.openFileDefault(id, path) : api.openInEditor(id, path, line);
        void action.catch((err) => void message(String(err), { kind: "error" }));
      },
      onOpenUrl: (url) => {
        void api.openUrl(url).catch((err) => console.error("open url failed", err));
      },
      onFileMenu: (e, path) => fileMenu.open(e.native, path),
      onCopyCode: (key, text) => {
        void navigator.clipboard?.writeText(text);
        setCopiedCodeKey(key);
        if (copiedTimer) clearTimeout(copiedTimer);
        copiedTimer = setTimeout(() => setCopiedCodeKey(null), 1200);
      },
    },
    copiedCodeKey: null,
    editingUserId: null,
    editingHeight: 120,
    running: false,
    isExpanded: (key, def) => isExpanded(key, def),
    toggleExpanded: (key, value) => toggleExpanded(key, value),
    onUserEdit: (item) => startEdit(item),
    onUndoEdits: (changes, undoneKey) => void doUndoEdits(changes, undoneKey),
    onRespondPermission: (requestKey, optionId) => void respondPermission(requestKey, optionId),
    onOpenInEditor: (path, line) => {
      const id = state.currentId;
      if (!id || !path) return;
      void api.openInEditor(id, path, line).catch((e) => void message(String(e), { kind: "error" }));
    },
    undoBusy: (key) => undoBusySet.has(key),
    permAnswers: (requestKey) => permStateOf(requestKey),
    onPermSelect: (requestKey, index, label, multiple) => {
      const st = permStateOf(requestKey);
      st.answers = st.answers.map((answer, ai) => {
        if (ai !== index) return answer;
        if (!multiple) return [label];
        return answer.includes(label) ? answer.filter((v) => v !== label) : [...answer, label];
      });
      if (!multiple) {
        st.custom = st.custom.map((v, ai) => (ai === index ? "" : v));
      }
      setPermVersion((v) => v + 1);
    },
  };

  function permStateOf(requestKey: string): { answers: string[][]; custom: string[] } {
    let st = permStates.get(requestKey);
    if (!st) {
      const req = props.permissions.find((p) => p.requestKey === requestKey);
      st = {
        answers: req?.questions?.map(() => []) ?? [],
        custom: req?.questions?.map(() => "") ?? [],
      };
      permStates.set(requestKey, st);
    }
    return st;
  }

  async function doUndoEdits(changes: RevertChange[], undoneKey: string) {
    const id = state.currentId;
    if (!id || undoBusySet.has(undoneKey) || isExpanded(undoneKey)) return;
    undoBusySet.add(undoneKey);
    setUndoVersion((v) => v + 1);
    try {
      const res = await api.revertFileChanges(id, changes);
      if (res.conflicts.length === 0 && res.errors.length === 0) {
        toggleExpanded(undoneKey, true);
      }
    } catch (e) {
      void message(String(e), { kind: "error" });
    } finally {
      undoBusySet.delete(undoneKey);
      setUndoVersion((v) => v + 1);
    }
  }

  // —— 编辑覆盖层 ——
  function startEdit(item: UserItem) {
    attach.set(item.images ?? []);
    setEditing({ item, draft: item.text });
    setEditHeight(120);
  }

  function saveEdit() {
    const e = editing();
    if (!e) return;
    const text = e.draft.trim();
    const images = attach.images();
    if (!text && images.length === 0) return;
    setEditing(null);
    void editUserMessage(e.item.id, text, images);
  }

  // —— 树重建 ——
  let rebuildQueued = false;
  function rebuildSoon() {
    if (rebuildQueued) return;
    rebuildQueued = true;
    queueMicrotask(() => {
      rebuildQueued = false;
      rebuild();
    });
  }

  function cached(key: string, sig: string, build: () => Node | null): Node | null {
    const hit = itemCache.get(key);
    if (hit && hit.sig === sig) return hit.node;
    const node = build();
    itemCache.set(key, { node, sig });
    return node;
  }

  /** 按 sig 缓存的单条 item 构建（markdown/工具卡/diff 只在内容变化时重建） */
  function buildItemCached(
    item: Item,
    active: boolean,
    ctx: BuilderCtx,
    env: { cwd: string; agentKind: string; threadId: string },
  ): Node | null {
    const e = editing();
    const copied = copiedCodeKey() ?? "";
    switch (item.type) {
      case "user":
        return cached(
          `u${item.id}`,
          `${e?.item.id === item.id}|${editHeight()}|${ctx.running}|${item.text.length}|${item.images?.length ?? 0}`,
          () => buildItem(item, active, ctx, env),
        );
      case "assistant": {
        const shown = typewriterText(item.id, item.text);
        return cached(`a${item.id}`, `${sigOf(shown)}|${active}|${copied}`, () =>
          buildItem({ ...item, text: shown }, active, ctx, env),
        );
      }
      case "thought":
        return cached(
          `h${item.id}`,
          `${ctx.isExpanded(`thought-${item.id}`, active)}|${active}|${sigOf(item.text)}|${copied}`,
          () => buildItem(item, active, ctx, env),
        );
      case "tool": {
        const open = ctx.isExpanded(
          `tool-${item.id}`,
          active || item.status === "pending" || item.status === "in_progress",
        );
        const raw = ctx.isExpanded(`tool-raw-${item.id}`);
        let clen = 0;
        for (const c of item.content) clen += JSON.stringify(c).length;
        return cached(
          `t${item.id}`,
          `${item.status}|${open}|${raw}|${copied}|${item.content.length}:${clen}|${item.locations.length}|${item.rawInput !== undefined}|${item.rawOutput !== undefined}`,
          () => buildItem(item, active, ctx, env),
        );
      }
      case "system":
        return cached(`s${item.id}`, `${sigOf(item.text)}|${item.level}`, () =>
          buildItem(item, active, ctx, env),
        );
      default:
        return null;
    }
  }

  function rebuild() {
    const r = renderer;
    if (!r) return;
    builderCtx.copiedCodeKey = copiedCodeKey();
    builderCtx.editingUserId = editing()?.item.id ?? null;
    builderCtx.editingHeight = editHeight();
    builderCtx.running = isRunning();
    const env = {
      cwd: state.cwd,
      agentKind: state.agentKind,
      threadId: state.currentId ?? "",
    };
    const colW = Math.min(columnWidth(), wrapRef?.clientWidth ?? columnWidth());
    const padX = columnPadX();
    const content = new Node("div", {
      width: colW,
      display: "flex",
      flexDirection: "column",
      padding: [24, padX, 16, padX],
    });
    contentNode = content;
    userRowNodes = new Map();
    const children: Node[] = [];

    if (props.showPreviewBanner) {
      children.push(buildCheckpointBanner(() => props.onReturnToNow()));
    }
    if (props.items.length === 0 && !state.loadingThread) {
      children.push(buildHint(agentLabel(state.agentKind), cwdDisplay()));
    }

    const running = isRunning();
    groupNodes = [];
    props.groups.forEach((g, i) => {
      const active = running && !g.turn;
      const node = buildGroup(g, active, builderCtx, env, buildItemCached);
      groupNodes[i] = node;
      children.push(node);
      if (g.user) {
        const row = node.children[0];
        if (row?.data?.kind === "user") userRowNodes.set(g.user.id, row);
      }
    });

    for (const req of props.permissions) {
      children.push(buildPermissionCard(req, builderCtx, agentLabel(req.agentKind ?? state.agentKind)));
    }

    content.replaceChildren(children);
    if (centerNode.children[0] !== content) centerNode.replaceChildren([content]);
    r.markDirty();
    scheduleBottomPin();
  }

  // —— 吸底状态机（移植 ChatView） ——
  const cancelBottomFollow = () => setStickToBottom(false);

  let pinQueued = false;
  /** 布局后钉底：contentHeight 只有在渲染器布局完成帧才有效（见 onAfterLayout） */
  const pinBottom = () => {
    pinQueued = true;
    renderer?.markDirty();
  };

  const afterLayout = () => {
    const r = renderer;
    if (!r) return;
    if (pinQueued) {
      pinQueued = false;
      if (stickToBottom() && !pointerActive) {
        r.setScrollTop(r.maxScrollTop());
        lastScrollTop = r.scrollTop;
      }
    }
    syncTimeCursor();
    repositionOverlays();
  };

  const scheduleBottomPin = () => {
    pinBottom();
  };

  const enableBottomFollow = () => {
    setStickToBottom(true);
    scheduleBottomPin();
  };

  const processScroll = () => {
    const r = renderer;
    if (!r) return;
    const currentTop = r.scrollTop;
    const atBottom = r.isAtBottom();
    if (stickToBottom()) {
      if (pointerActive && !atBottom && currentTop !== lastScrollTop) cancelBottomFollow();
    } else if (atBottom && currentTop > lastScrollTop) {
      setStickToBottom(true);
    }
    lastScrollTop = currentTop;
    syncTimeCursor();
    repositionOverlays();
  };

  const finishPointerInteraction = () => {
    processScroll();
    pointerActive = false;
    if (stickToBottom()) scheduleBottomPin();
  };

  /** 当前视口顶部 +32 命中的分组下标（世界线时间轴光标） */
  const syncTimeCursor = () => {
    const r = renderer;
    if (!r || groupNodes.length === 0) {
      props.onActiveGroupChange(-1);
      return;
    }
    const top = r.scrollTop + 32;
    let low = 0;
    let high = groupNodes.length;
    while (low < high) {
      const mid = (low + high) >> 1;
      if (groupNodes[mid]._y <= top) low = mid + 1;
      else high = mid;
    }
    props.onActiveGroupChange(Math.max(0, low - 1));
  };

  // —— 覆盖层定位 ——
  function repositionOverlays() {
    const r = renderer;
    if (!r) return;
    const e = editing();
    if (e) {
      const row = userRowNodes.get(e.item.id);
      if (row) {
        const rect = r.nodeScreenRect(row);
        setEditRect({ x: rect.x, y: rect.y, w: rect.w });
      } else {
        setEditRect(null);
      }
    } else if (editRect()) {
      setEditRect(null);
    }
    const rects: Array<{ key: string; x: number; y: number; w: number; h: number; requestKey: string; index: number }> = [];
    if (contentNode) {
      for (const n of contentNode.walk()) {
        const key = n.data?.overlayKey as string | undefined;
        if (!key) continue;
        const m = /^perm-custom-(.+)-(\d+)$/.exec(key);
        if (!m) continue;
        const rect = r.nodeScreenRect(n);
        rects.push({ key, x: rect.x, y: rect.y, w: rect.w, h: rect.h, requestKey: m[1], index: Number(m[2]) });
      }
    }
    setPermInputRects(rects);
  }

  function cwdDisplay() {
    const meta = state.threads.find((t) => t.id === state.currentId);
    return meta?.worktree?.repo || state.cwd;
  }

  // wheel/键盘滚动意图（吸底跟随的取消/恢复，与 ChatView 的 handleWheel/handleScrollKey 一致）
  const wheelIntent = (e: WheelEvent) => {
    const r = renderer;
    if (!r || r.maxScrollTop() <= 1) return;
    if (e.deltaY > 0 && r.isAtBottom()) {
      if (!stickToBottom()) enableBottomFollow();
      return;
    }
    if (e.deltaY !== 0) cancelBottomFollow();
  };

  const keyIntent = (e: KeyboardEvent) => {
    const r = renderer;
    if (!r || r.maxScrollTop() <= 0) return;
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    const target = e.target;
    if (
      target instanceof HTMLElement &&
      (target.isContentEditable || target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT")
    ) {
      return;
    }
    const downs = new Set(["ArrowDown", "PageDown", "End"]);
    const ups = new Set(["ArrowUp", "PageUp", "Home"]);
    const down = downs.has(e.key) || (e.key === " " && !e.shiftKey);
    const up = ups.has(e.key) || (e.key === " " && e.shiftKey);
    if (!down && !up) return;
    if (down && r.isAtBottom()) {
      if (!stickToBottom()) enableBottomFollow();
      return;
    }
    cancelBottomFollow();
  };

  const onPointerDown = (e: PointerEvent) => {
    pointerActive = e.target === canvasRef;
  };

  // —— 挂载 ——
  onMount(() => {
    const r = new CanvasRenderer(canvasRef!, {
      onScrollChange: () => processScroll(),
      onAfterLayout: () => afterLayout(),
      onHoverTitle: (node, x, y) => {
        if (!node || !node.style.title) {
          setTooltip(null);
          return;
        }
        setTooltip({ text: node.style.title, x, y });
      },
    });
    renderer = r;
    r.setRoot(rootNode);

    const ro = new ResizeObserver(() => {
      if (!wrapRef) return;
      r.resize(wrapRef.clientWidth, wrapRef.clientHeight);
      rootNode.setStyle({ width: wrapRef.clientWidth, height: wrapRef.clientHeight });
      rebuildSoon();
      scheduleBottomPin();
    });
    ro.observe(wrapRef!);
    r.resize(wrapRef!.clientWidth, wrapRef!.clientHeight);
    rootNode.setStyle({ width: wrapRef!.clientWidth, height: wrapRef!.clientHeight });

    canvasRef!.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("pointerup", finishPointerInteraction, true);
    window.addEventListener("pointercancel", finishPointerInteraction, true);
    wrapRef!.addEventListener("wheel", wheelIntent, { capture: true, passive: true });
    window.addEventListener("keydown", keyIntent, true);

    // 主题切换：节点颜色是构建期烘焙的，整树重建
    const offTheme = onThemeChange(() => {
      itemCache.clear();
      rebuildSoon();
    });

    // 编辑覆盖层高度跟随（textarea 自动行高）
    const overlayRO = new ResizeObserver(() => {
      if (overlayRef) {
        const h = overlayRef.offsetHeight;
        if (h > 0 && Math.abs(h - editHeight()) > 1) setEditHeight(h);
      }
    });
    createEffect(() => {
      if (overlayRef) overlayRO.observe(overlayRef);
    });

    props.registerApi({
      scrollToGroup: (index) => {
        const node = groupNodes[index];
        if (!node) return;
        cancelBottomFollow();
        r.scrollToNode(node, 20);
        syncTimeCursor();
      },
      returnToNow: () => {
        enableBottomFollow();
      },
      groupCount: () => groupNodes.length,
    });

    // 测试/走查用的节点定位 API（canvas 无 DOM 选择器，供截图脚本等驱动交互）
    const w = window as unknown as {
      __canvasFind?: (
        q: { clickId?: string; text?: string; itemId?: number },
        opts?: { scrollIntoView?: boolean },
      ) => Array<{
        x: number;
        y: number;
        w: number;
        h: number;
      }>;
      __canvasDump?: (q: { tag?: string; scrollKey?: string }) => Array<Record<string, unknown>>;
      __canvasState?: () => Record<string, unknown>;
      __canvasRepaint?: () => void;
      __canvasHit?: (x: number, y: number) => Array<Record<string, unknown>>;
    };
    w.__canvasState = () => r.debugState();
    w.__canvasRepaint = () => r.markAllDirty();
    w.__canvasHit = (x, y) => {
      const out: Array<Record<string, unknown>> = [];
      let n: Node | null = null;
      // 与 renderer 相同的命中逻辑（页面坐标 → canvas 坐标）
      const cr = canvasRef?.getBoundingClientRect();
      if (!cr) return out;
      const cx = x - cr.left;
      const cy = y - cr.top;
      // 复用 hitTest：从 root 找
      n = hitTest(rootNode, cx, cy);
      while (n) {
        out.push({
          tag: n.tag,
          id: n.id,
          x: n._x,
          y: n._y,
          w: n._width,
          h: n._height,
          click: !!n.onClick,
          ctx: !!n.onContextMenu,
          text: n.textContent.slice(0, 24),
          display: n.style.display,
        });
        n = n.parent;
      }
      return out;
    };
    w.__canvasDump = (q) => {
      const out: Array<Record<string, unknown>> = [];
      if (!contentNode) return out;
      for (const n of contentNode.walk()) {
        if (q.tag !== undefined && n.tag !== q.tag) continue;
        if (q.scrollKey !== undefined && n.data?.scrollKey !== q.scrollKey) continue;
        out.push({
          id: n.id,
          tag: n.tag,
          x: n._x,
          y: n._y,
          w: n._width,
          h: n._height,
          contentW: n._contentWidth,
          contentH: n._contentHeight,
          scrollY: n._scrollY,
          scrollX: n._scrollX,
          maxScrollY: n._maxScrollY,
          maxH: n.style.maxHeight,
          overflow: n.style.overflow,
          lines: n._textLines?.length,
          text: n.textContent.slice(0, 30),
          hover: n._hover,
          hoverWithin: n._hoverWithin,
          reveal: n.revealOnHover,
          clickId: n.data?.clickId,
          copyKey: n.data?.copyKey,
          mdBlock: n.data?.mdBlock,
          kind: n.data?.kind,
          imgLoaded: n.tag === "img" ? n._imgLoaded : undefined,
          src: n.tag === "img" ? (n.style.src ?? "").slice(0, 40) : undefined,
          frags: n._textLines?.map((l) => ({ t: l.text, w: +l.w.toFixed(2), x: +l.x.toFixed(1) })),
        });
      }
      return out;
    };
    w.__canvasFind = (q, opts) => {
      const matches: Node[] = [];
      if (contentNode) {
        for (const n of contentNode.walk()) {
          if (q.clickId !== undefined && n.data?.clickId !== q.clickId) continue;
          if (q.itemId !== undefined && n.data?.itemId !== q.itemId) continue;
          if (q.text !== undefined) {
            const selfOrChildText =
              n.textContent.includes(q.text) ||
              !!n.find((c) => c.textContent.includes(q.text!));
            if (!selfOrChildText) continue;
            if (!n.onClick && !n.textContent) continue;
          }
          if (n._width <= 0 || n._height <= 0) continue;
          matches.push(n);
        }
      }
      // scrollIntoView：把首个匹配滚进视口（模拟 Playwright 的 scrollIntoViewIfNeeded）
      if (opts?.scrollIntoView && matches.length) {
        const n = matches[0];
        const rect = r.nodeScreenRect(n);
        const vh = wrapRef?.clientHeight ?? 800;
        if (rect.y < 0) {
          r.setScrollTop(r.scrollTop + rect.y - 60);
        } else if (rect.y + rect.h > vh) {
          r.setScrollTop(r.scrollTop + (rect.y + rect.h - vh) + 60);
        }
      }
      const cr = canvasRef?.getBoundingClientRect();
      if (!cr) return [];
      return matches.map((n) => {
        const rect = r.nodeScreenRect(n);
        return { x: rect.x + cr.left, y: rect.y + cr.top, w: rect.w, h: rect.h };
      });
    };

    rebuild();

    onCleanup(() => {
      delete w.__canvasFind;
      delete w.__canvasDump;
      ro.disconnect();
      overlayRO.disconnect();
      offTheme();
      canvasRef?.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("pointerup", finishPointerInteraction, true);
      window.removeEventListener("pointercancel", finishPointerInteraction, true);
      wrapRef?.removeEventListener("wheel", wheelIntent, { capture: true } as EventListenerOptions);
      window.removeEventListener("keydown", keyIntent, true);
      r.destroy();
      if (copiedTimer) clearTimeout(copiedTimer);
      for (const [, entry] of typewriter) {
        if (entry.timer !== undefined) window.clearTimeout(entry.timer);
      }
    });
  });

  // —— 响应式触发 ——
  // 内容变化 → 重建 + 钉底（与 ChatView 的流式 effect 相同依赖）
  createEffect(() => {
    const len = props.items.length;
    const last = props.items[len - 1];
    if (last && "text" in last) void (last as { text: string }).text.length;
    void props.permissions.length;
    rebuildSoon();
    scheduleBottomPin();
  });

  // 展开态/复制态/权限选择/编辑态/运行态变化 → 重建
  createEffect(() => {
    void JSON.stringify(state.expanded); // 逐 key 订阅（新建/切换 key 都触发）
    void copiedCodeKey();
    void permVersion();
    void undoVersion();
    void editing();
    void editHeight();
    void isRunning();
    rebuildSoon();
  });

  // 切换会话 → 清缓存、从底部开始
  createEffect((prevId: string | null | undefined) => {
    const id = state.currentId;
    if (id !== prevId) {
      itemCache.clear();
      typewriter.clear();
      permStates.clear();
      renderer?.setScrollTop(0, false);
      enableBottomFollow();
    }
    return id;
  }, undefined);

  // 发送新提示词 → 重新吸底
  createEffect(() => {
    const tick = chatScrollToBottomSignal();
    if (tick === 0) return;
    enableBottomFollow();
  });

  return (
    <div
      ref={wrapRef}
      class="transcript-canvas-wrap"
      classList={{
        "checkpoint-preview": props.showPreviewBanner,
        "checkpoint-preview-fading": props.previewFading,
      }}
    >
      <canvas ref={canvasRef} class="transcript-canvas" tabindex="0" />
      {/* 用户消息编辑覆盖层 */}
      <Show when={editing() && editRect()}>
        {(rect) => (
          <div
            ref={overlayRef}
            class="transcript-overlay"
            style={{
              left: `${rect().x}px`,
              top: `${rect().y}px`,
              width: `${rect().w}px`,
            }}
          >
            <div class="user-edit">
              <ImageAttachmentStrip images={attach.images()} onRemove={attach.remove} />
              <textarea
                class="user-edit-input"
                value={editing()!.draft}
                rows={Math.min(10, Math.max(2, editing()!.draft.split("\n").length))}
                onPaste={attach.onPaste}
                onInput={(e) => setEditing({ item: editing()!.item, draft: e.currentTarget.value })}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
                    e.preventDefault();
                    saveEdit();
                  }
                  if (e.key === "Escape") setEditing(null);
                }}
                ref={(el) =>
                  queueMicrotask(() => {
                    el.focus();
                    el.setSelectionRange(el.value.length, el.value.length);
                  })
                }
              />
              <div class="user-edit-actions">
                <span class="user-edit-hint">发送后将从此处重新开始会话</span>
                <button class="btn secondary small" onClick={() => setEditing(null)}>
                  取消
                </button>
                <button class="btn primary small" onClick={saveEdit}>
                  发送
                </button>
              </div>
            </div>
          </div>
        )}
      </Show>
      {/* 权限卡自定义输入覆盖层 */}
      <For each={permInputRects()}>
        {(r) => (
          <input
            class="question-custom transcript-overlay-input"
            style={{
              left: `${r.x}px`,
              top: `${r.y}px`,
              width: `${r.w}px`,
              height: `${r.h}px`,
            }}
            value={permStateOf(r.requestKey).custom[r.index] ?? ""}
            placeholder="输入其他答案"
            onInput={(e) => {
              const st = permStateOf(r.requestKey);
              const value = e.currentTarget.value;
              st.custom = st.custom.map((v, ai) => (ai === r.index ? value : v));
              // 非 multiple 且输入了内容 → 清空已选（与 DOM 版 setCustomAnswer 一致）
              const req = props.permissions.find((p) => p.requestKey === r.requestKey);
              const multiple = Boolean(req?.questions?.[r.index]?.multiple);
              if (!multiple && value.trim()) {
                st.answers = st.answers.map((answer, ai) => (ai === r.index ? [] : answer));
              }
              setPermVersion((v) => v + 1);
            }}
          />
        )}
      </For>
      {/* tooltip */}
      <Show when={tooltip()}>
        {(tt) => (
          <div
            class="canvas-tooltip"
            style={{
              left: `${Math.min(tt().x + 10, (wrapRef?.clientWidth ?? 400) - 240)}px`,
              top: `${Math.max(4, tt().y - 30)}px`,
            }}
          >
            {tt().text}
          </div>
        )}
      </Show>
      <fileMenu.Menu />
    </div>
  );
}
