/**
 * canvas 版会话主视图：整个 transcript 只有一个 <canvas> 节点。
 * 布局在 layout.ts；本组件负责滚动、命中、选择、悬停、浮层编辑与流式出字。
 */
import { convertFileSrc } from "@tauri-apps/api/core";
import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { createFileContextMenu } from "../components/FileContextMenu";
import { IMAGE_FILE_RE } from "../components/Markdown";
import type { Group } from "../components/TurnGroup";
import { api } from "../ipc";
import { editUserMessage, isExpanded, respondPermission, toggleExpanded } from "../store";
import type { PermissionRequest, PromptImage, RevertChange, UserItem } from "../types";
import {
  type Action,
  getTheme,
  measure,
  roundRectPath,
} from "./base";
import {
  type Doc,
  type GroupLayout,
  type LayoutEnv,
  type PermState,
  type Region,
  type ScrollBox,
  type TLine,
  type TRun,
  type View,
  assembleDoc,
  groupCacheSig,
  layoutGroup,
  layoutHint,
  layoutPermission,
} from "./layout";

export interface TranscriptCanvasApi {
  scrollToGroup(index: number): void;
  scrollToBottom(): void;
  enableBottomFollow(): void;
}

interface ImgEntry {
  img: HTMLImageElement;
  w: number;
  h: number;
  loaded: boolean;
}

function attachmentSrc(img: PromptImage): string {
  if (img.data) return `data:${img.mimeType};base64,${img.data}`;
  const path = img.uri ? decodeURI(img.uri.replace(/^file:\/+/, "")) : "";
  return convertFileSrc(path);
}

export function TranscriptCanvas(props: {
  groups: Group[];
  permissions: PermissionRequest[];
  running: boolean;
  showHint: boolean;
  hintCwd: string;
  threadId: string;
  previewBanner: boolean;
  fading: boolean;
  stickToBottom: boolean;
  onStickChange: (v: boolean) => void;
  onReturnToTimeline: () => void;
  onApi: (api: TranscriptCanvasApi) => void;
}) {
  let hostRef: HTMLDivElement | undefined;
  let canvasRef: HTMLCanvasElement | undefined;

  const [viewW, setViewW] = createSignal(0);
  const [viewH, setViewH] = createSignal(0);
  const [viewTop, setViewTop] = createSignal(0);
  const [relayoutTick, setRelayoutTick] = createSignal(0);
  const [mediaTick, setMediaTick] = createSignal(0);
  const [permTick, setPermTick] = createSignal(0);
  const [editState, setEditState] = createSignal<{
    item: UserItem;
    x: number;
    docY: number;
    w: number;
    draft: string;
  } | null>(null);
  const [permInput, setPermInput] = createSignal<{
    reqKey: string;
    index: number;
    multiple: boolean;
    x: number;
    docY: number;
    w: number;
    value: string;
  } | null>(null);

  /* ===== 非响应式交互状态 ===== */
  let doc: Doc | null = null;
  let scrollTop = 0;
  let hover: Region | null = null;
  let hoverAction: Action | null = null;
  let scrollbarHover = false;
  // 展开/折叠时锁住触发行在视口中的位置，避免吸底重排把滚动条和按钮一起推走。
  let toggleScrollAnchor: { key: string; viewportY: number } | null = null;
  let selA = -1;
  let selB = -1;
  let selAnchor = -1;
  let selecting = false;
  let pointerActive = false;
  let scrollDrag: { grab: number } | null = null;
  let downPos: { x: number; y: number } | null = null;
  let pendingClick: { region: Region; box?: ScrollBox } | null = null;
  let lastClick = { t: 0, count: 0 };
  let renderQueued = false;
  let prevThread = "";

  const copied = new Set<string>();
  const scrollPos = new Map<string, number>();
  const images = new Map<string, ImgEntry>();
  const reverting = new Set<string>();
  const permAnswers = new Map<string, string[][]>();
  const permCustom = new Map<string, string[]>();
  const shownMap = new Map<number, string>();
  const revealTargets = new Map<number, string>();
  let revealTimer: number | undefined;

  const fileMenu = createFileContextMenu();

  const bump = () => setRelayoutTick((t) => t + 1);

  /* ===== 图片缓存 ===== */
  const imageFor = (src: string): { w: number; h: number; img: HTMLImageElement } | null => {
    if (!src) return null;
    let entry = images.get(src);
    if (!entry) {
      const img = new Image();
      entry = { img, w: 0, h: 0, loaded: false };
      images.set(src, entry);
      img.onload = () => {
        entry!.w = img.naturalWidth || 240;
        entry!.h = img.naturalHeight || 160;
        entry!.loaded = true;
        setMediaTick((t) => t + 1);
      };
      img.onerror = () => {
        entry!.w = 240;
        entry!.h = 60;
        entry!.loaded = true;
        setMediaTick((t) => t + 1);
      };
      img.src = src;
    }
    return entry.loaded ? { w: entry.w, h: entry.h, img: entry.img } : null;
  };

  /* ===== 流式出字（与 Markdown.tsx 的 shown 指针算法一致） ===== */
  const revealText = (item: { id: number; text: string }): string => {
    const shown = shownMap.get(item.id);
    if (shown === undefined) return item.text;
    if (item.text.startsWith(shown)) return shown;
    return item.text;
  };

  /** 流式期间限制布局重建频率：每 100ms 最多一次，避免 marked.lexer 每帧跑 */
  let layoutDirty = false;
  let layoutThrottleTimer: number | undefined;
  const scheduleLayoutRebuild = () => {
    if (layoutThrottleTimer !== undefined) return;
    layoutThrottleTimer = window.setTimeout(() => {
      layoutThrottleTimer = undefined;
      if (layoutDirty) {
        layoutDirty = false;
        bump();
      }
    }, 100);
  };

  const revealStep = () => {
    revealTimer = undefined;
    let changed = false;
    let pending = false;
    for (const [id, target] of revealTargets) {
      const shown = shownMap.get(id) ?? target;
      if (!target.startsWith(shown)) {
        shownMap.set(id, target);
        continue;
      }
      const backlog = target.length - shown.length;
      if (backlog <= 0) continue;
      if (backlog > 3000) {
        shownMap.set(id, target);
        changed = true;
        continue;
      }
      const step = Math.max(2, Math.ceil(backlog / 8));
      let end = shown.length + step;
      const c = target.charCodeAt(end - 1);
      if (c >= 0xd800 && c <= 0xdbff && end < target.length) end += 1;
      shownMap.set(id, target.slice(0, end));
      changed = true;
      if (end < target.length) pending = true;
    }
    if (changed) {
      layoutDirty = true;
      scheduleLayoutRebuild();
    }
    if (pending) revealTimer = window.setTimeout(revealStep, 33);
  };

  /** 只在 groups 引用变化时同步 reveal 目标，不在每次 rebuild 里跑 */
  const syncReveals = () => {
    revealTargets.clear();
    for (const g of props.groups) {
      for (const it of g.body) {
        if (it.type !== "assistant" && it.type !== "thought") continue;
        revealTargets.set(it.id, it.text);
        if (!shownMap.has(it.id)) shownMap.set(it.id, it.text);
      }
    }
    let needs = false;
    for (const [id, target] of revealTargets) {
      const shown = shownMap.get(id)!;
      if (target.startsWith(shown) && target.length > shown.length) {
        needs = true;
        break;
      }
    }
    if (needs && revealTimer === undefined) revealTimer = window.setTimeout(revealStep, 33);
  };
  // groups 变化时同步 reveal 目标（独立于 rebuild）
  createEffect(() => {
    void props.groups;
    syncReveals();
  });

  /* ===== 权限卡片临时答案状态 ===== */
  const permState: PermState = {
    answers: (k) => permAnswers.get(k) ?? [],
    custom: (k) => permCustom.get(k) ?? [],
  };

  /* ===== 布局重建 ===== */
  const groupCache = new Map<
    Group,
    { sig: string; width: number; active: boolean; running: boolean; media: number; layout: GroupLayout }
  >();

  const rebuild = () => {
    const w = viewW();
    const h = viewH();
    const tick = relayoutTick();
    const media = mediaTick();
    void permTick();
    void tick;
    if (w <= 0 || h <= 0) return;

    if (props.threadId !== prevThread) {
      prevThread = props.threadId;
      shownMap.clear();
      selA = selB = selAnchor = -1;
    }

    const theme = getTheme();
    // CSS: max-width: clamp(720px, 78vw, 980px); 但受父容器宽度约束
    const clampW = Math.min(980, Math.max(720, window.innerWidth * 0.78));
    const innerW = Math.min(clampW, w);
    const padX = Math.min(28, Math.max(14, window.innerWidth * 0.03));
    const colW = innerW - padX * 2;
    const x0 = (w - innerW) / 2 + padX;

    const env: LayoutEnv = {
      theme,
      x0,
      width: colW,
      reveal: revealText,
      image: imageFor,
      scrollPos,
      threadId: props.threadId,
      running: props.running,
      editingItemId: editState()?.item.id ?? null,
      perm: permState,
      cwd: props.hintCwd,
    };

    const groups = props.groups;
    const running = props.running;
    const last = groups.length - 1;
    const groupSections: { top: number; layout: GroupLayout }[] = [];
    let y = 24;
    const seen = new Set<Group>();
    let anyLayoutChanged = !doc; // first build always assembles
    groups.forEach((g, i) => {
      seen.add(g);
      const active = running && !g.turn;
      const sig = groupCacheSig(g);
      let entry = groupCache.get(g);
      const isLast = i === last;
      if (isLast || !entry || entry.sig !== sig || entry.width !== colW || entry.active !== active || entry.running !== running || entry.media !== media) {
        const layout = layoutGroup(g, env, active);
        entry = { sig, width: colW, active, running, media, layout };
        groupCache.set(g, entry);
        anyLayoutChanged = true;
      }
      groupSections.push({ top: y, layout: entry.layout });
      y += entry.layout.height;
    });
    if (groupCache.size > groups.length + 64) {
      for (const key of [...groupCache.keys()]) if (!seen.has(key)) groupCache.delete(key);
    }

    const permSections: { top: number; layout: GroupLayout }[] = [];
    for (const req of props.permissions) {
      const layout = layoutPermission(req, env);
      permSections.push({ top: y, layout });
      y += layout.height;
      anyLayoutChanged = true; // permissions always re-layout (small count, cheap)
    }

    let hintSection: { top: number; layout: GroupLayout } | null = null;
    if (props.showHint && groups.length === 0) {
      const layout = layoutHint(env);
      hintSection = { top: 24, layout };
      y = Math.max(y, 24 + layout.height);
    }

    if (anyLayoutChanged) {
      doc = assembleDoc(groupSections, permSections, hintSection, y + 16, doc);
    } else if (doc) {
      doc.height = y + 16;
    }

    const max = Math.max(0, (doc?.height ?? 0) - h);
    const anchor = toggleScrollAnchor;
    toggleScrollAnchor = null;
    if (anchor && doc) {
      const entry = doc.regions.find(({ region }) =>
        region.action.kind === "toggle" && String(region.action.key) === anchor.key,
      );
      scrollTop = entry
        ? Math.max(0, Math.min(max, entry.top + entry.region.y - anchor.viewportY))
        : Math.min(scrollTop, max);
    } else if (props.stickToBottom && !pointerActive) scrollTop = max;
    else scrollTop = Math.min(scrollTop, max);
    setViewTop(scrollTop);
    requestRender();
  };

  let rebuildQueued = false;
  const scheduleRebuild = () => {
    // 必须在 effect 同步体内读取所有响应式信号，否则 Solid 不跟踪依赖
    void props.groups;
    void props.permissions;
    void props.running;
    void props.showHint;
    void props.hintCwd;
    void props.threadId;
    void props.previewBanner;
    void props.fading;
    void props.stickToBottom;
    void viewW();
    void viewH();
    void relayoutTick();
    void mediaTick();
    void permTick();
    void editState();
    // 跟踪展开/折叠状态：groupCacheSig 读取 state.expanded[k]，
    // 必须在 effect 同步体内调用才能被 Solid 跟踪
    for (const g of props.groups) groupCacheSig(g);

    if (rebuildQueued) return;
    rebuildQueued = true;
    requestAnimationFrame(() => {
      rebuildQueued = false;
      rebuild();
    });
  };
  createEffect(scheduleRebuild);

  /* ===== 绘制 ===== */
  const requestRender = () => {
    if (renderQueued) return;
    renderQueued = true;
    requestAnimationFrame(paint);
  };

  const scrollGeom = () => {
    if (!doc) return null;
    const h = viewH();
    const max = doc.height - h;
    if (max <= 0) return null;
    const thumbH = Math.max(48, (h / doc.height) * h);
    const thumbY = (scrollTop / max) * (h - thumbH);
    return { thumbY, thumbH, max, trackH: h };
  };

  const bannerRect = () => {
    const text = "回到当前时间线";
    const f = "10px " + '"Inter Variable","Noto Sans SC Variable","Segoe UI",sans-serif';
    const w = measure(text, f) + 20;
    return { x: (viewW() - w) / 2, y: 8, w, h: 25, text, f };
  };

  const paint = () => {
    renderQueued = false;
    const canvas = canvasRef;
    if (!canvas || !doc) return;
    const w = viewW();
    const h = viewH();
    const dpr = window.devicePixelRatio || 1;
    if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
    }
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    ctx.textBaseline = "alphabetic";
    const theme = getTheme();
    let a = Math.min(selA, selB);
    let b = Math.max(selA, selB);
    if (a === b) {
      a = -1;
      b = -1;
    }
    const baseView: View = {
      theme,
      top: scrollTop,
      bottom: scrollTop + h,
      hover,
      hoverAction,
      hoverGroup: hover?.groupId ?? null,
      selA: a,
      selB: b,
      now: performance.now(),
      copied,
    };
    for (const section of doc.sections) {
      if (section.blocks.length === 0) continue;
      const firstBlock = section.blocks[0];
      const lastBlock = section.blocks[section.blocks.length - 1];
      const secTop = section.top + firstBlock.y;
      const secBot = section.top + lastBlock.y + lastBlock.h;
      if (secBot < baseView.top || secTop > baseView.bottom) continue;
      ctx.save();
      ctx.translate(0, -scrollTop + section.top);
      const local: View = { ...baseView, top: baseView.top - section.top, bottom: baseView.bottom - section.top };
      for (const block of section.blocks) {
        if (block.y + block.h < local.top || block.y > local.bottom) continue;
        block.paint(ctx, local);
      }
      ctx.restore();
    }

    if (props.previewBanner) {
      const r = bannerRect();
      roundRectPath(ctx, r.x, r.y, r.w, r.h, r.h / 2);
      ctx.fillStyle = theme.bgPanel;
      ctx.globalAlpha = 0.92;
      ctx.fill();
      ctx.globalAlpha = 1;
      ctx.strokeStyle = theme.accent30;
      ctx.lineWidth = 1;
      ctx.stroke();
      ctx.font = r.f;
      ctx.fillStyle = theme.accent;
      ctx.fillText(r.text, r.x + 10, r.y + 16);
    }

    const geom = scrollGeom();
    if (geom) {
      roundRectPath(ctx, w - 9, geom.thumbY, 6, geom.thumbH, 3);
      ctx.fillStyle = scrollbarHover ? theme.scrollHover : theme.scroll;
      ctx.fill();
    }

    if (doc.hasSpinner) requestRender();
  };

  /* ===== 命中测试 ===== */
  const offsetInLine = (line: TLine, px: number): { off: number; run: TRun | null } => {
    if (line.runs.length === 0) return { off: -1, run: null };
    const first = line.runs[0];
    const lastRun = line.runs[line.runs.length - 1];
    if (px <= first.x) return { off: first.cs, run: first };
    if (px >= lastRun.x + lastRun.w) return { off: lastRun.ce, run: lastRun };
    for (const run of line.runs) {
      if (px < run.x + run.w || run === lastRun) {
        const text = run.text;
        let lo = 0;
        let hi = text.length;
        while (lo < hi) {
          const mid = (lo + hi) >> 1;
          const wMid = measure(text.slice(0, mid + 1), run.f);
          if (run.x + wMid < px) lo = mid + 1;
          else hi = mid;
        }
        // 取更近的字符边界
        if (lo > 0) {
          const wPrev = measure(text.slice(0, lo), run.f);
          const wCur = measure(text.slice(0, lo + 1 > text.length ? text.length : lo + 1), run.f);
          if (Math.abs(run.x + wPrev - px) < Math.abs(run.x + wCur - px)) lo -= 1;
        }
        return { off: run.cs + lo, run };
      }
    }
    return { off: lastRun.ce, run: lastRun };
  };

  /** 文档坐标命中文本；空隙处吸附到最近的行 */
  const textOffsetAtDoc = (px: number, yDoc: number): { off: number; run: TRun | null } | null => {
    if (!doc || doc.lines.length === 0) return null;
    const lines = doc.lines;
    let low = 0;
    let high = lines.length;
    while (low < high) {
      const mid = (low + high) >> 1;
      const e = lines[mid];
      if (e.top + e.line.y + e.line.h <= yDoc) low = mid + 1;
      else high = mid;
    }
    for (let i = Math.max(0, low - 1); i <= Math.min(lines.length - 1, low + 1); i++) {
      const e = lines[i];
      const ly = e.top + e.line.y;
      if (yDoc >= ly && yDoc <= ly + e.line.h) return offsetInLine(e.line, px);
    }
    // 空隙：吸附到上方行尾或下方行首
    if (low > 0 && low <= lines.length) {
      const prev = lines[low - 1];
      const next = lines[low];
      const prevY = prev.top + prev.line.y + prev.line.h;
      const nextY = next ? next.top + next.line.y : Number.POSITIVE_INFINITY;
      if (yDoc - prevY <= nextY - yDoc) {
        const lr = prev.line.runs[prev.line.runs.length - 1];
        return { off: lr ? lr.ce : 0, run: null };
      }
    }
    if (low < lines.length) {
      const nr = lines[low].line.runs[0];
      return { off: nr ? nr.cs : 0, run: null };
    }
    const lr = lines[lines.length - 1].line.runs.at(-1);
    return { off: lr ? lr.ce : 0, run: null };
  };

  interface Hit {
    kind: "region" | "text" | "box-region" | "box-text" | "box" | "scrollbar" | "banner" | "none";
    region?: Region;
    box?: ScrollBox;
    off?: number;
    run?: TRun | null;
    thumb?: boolean;
  }

  const hitTest = (px: number, py: number): Hit => {
    if (props.previewBanner) {
      const r = bannerRect();
      if (px >= r.x && px <= r.x + r.w && py >= r.y && py <= r.y + r.h) return { kind: "banner" };
    }
    const geom = scrollGeom();
    if (geom && px >= viewW() - 13) {
      return { kind: "scrollbar", thumb: py >= geom.thumbY && py <= geom.thumbY + geom.thumbH };
    }
    if (!doc) return { kind: "none" };
    // 滚动盒（视觉上在最上层）
    for (const { box, top } of doc.boxes) {
      const sy = top + box.y - scrollTop;
      if (px < box.x || px > box.x + box.w || py < sy || py > sy + box.h) continue;
      for (const r of box.fixedRegions) {
        const ry = top + r.y - scrollTop;
        if (px >= r.x && px <= r.x + r.w && py >= ry && py <= ry + r.h) return { kind: "box-region", region: r, box };
      }
      const cy = py - sy + box.scrollTop;
      for (const r of box.regions) {
        if (px >= r.x && px <= r.x + r.w && cy >= r.y && cy <= r.y + r.h) return { kind: "box-region", region: r, box };
      }
      for (const line of box.lines) {
        if (cy >= line.y && cy <= line.y + line.h) {
          const { off, run } = offsetInLine(line, px);
          if (off >= 0) return { kind: "box-text", off, run, box };
        }
      }
      return { kind: "box", box };
    }
    for (const { region, top } of doc.regions) {
      const ry = top + region.y - scrollTop;
      if (px >= region.x && px <= region.x + region.w && py >= ry && py <= ry + region.h) {
        return { kind: "region", region };
      }
    }
    const t = textOffsetAtDoc(px, py + scrollTop);
    if (t) return { kind: "text", off: t.off, run: t.run };
    return { kind: "none" };
  };

  /* ===== 选择 ===== */
  const findLineByOffset = (offset: number): TLine | null => {
    if (!doc) return null;
    const all = [...doc.copyOrder];
    for (const entry of all) {
      const runs = entry.line.runs;
      if (runs.length === 0) continue;
      if (offset >= runs[0].cs && offset <= runs[runs.length - 1].ce) return entry.line;
    }
    return null;
  };

  const selectWordAt = (offset: number) => {
    const line = findLineByOffset(offset);
    if (!line) return;
    for (const run of line.runs) {
      if (offset < run.cs || offset > run.ce) continue;
      const text = run.text;
      let i = offset - run.cs;
      const isWord = (ch: string) => /[\w\u2E80-\u9FFF\uAC00-\uD7AF\u3040-\u30FF]/.test(ch);
      const word = i < text.length && isWord(text[i]);
      let s = i;
      let e = i;
      if (word) {
        while (s > 0 && isWord(text[s - 1])) s--;
        while (e < text.length && isWord(text[e])) e++;
      } else {
        while (s > 0 && !isWord(text[s - 1])) s--;
        while (e < text.length && !isWord(text[e])) e++;
      }
      selA = run.cs + s;
      selB = run.cs + e;
      requestRender();
      return;
    }
  };

  const selectBlockAt = (offset: number) => {
    const line = findLineByOffset(offset);
    if (!line || !doc) return;
    let min = Number.POSITIVE_INFINITY;
    let max = Number.NEGATIVE_INFINITY;
    const scan = (l: TLine) => {
      if (l.blockId !== line.blockId || l.runs.length === 0) return;
      min = Math.min(min, l.runs[0].cs);
      max = Math.max(max, l.runs[l.runs.length - 1].ce);
    };
    for (const e of doc.copyOrder) scan(e.line);
    if (Number.isFinite(min)) {
      selA = min;
      selB = max;
      requestRender();
    }
  };

  const copySelection = (): string => {
    if (!doc) return "";
    const a = Math.min(selA, selB);
    const b = Math.max(selA, selB);
    if (a >= b) return "";
    let out = "";
    for (const entry of doc.copyOrder) {
      const runs = entry.line.runs;
      if (runs.length === 0) continue;
      const lcs = runs[0].cs;
      const lce = runs[runs.length - 1].ce;
      if (lce <= a || lcs >= b) continue;
      let lineText = "";
      for (const run of runs) {
        if (run.ce <= a || run.cs >= b) continue;
        const os = Math.max(a, run.cs) - run.cs;
        const oe = Math.min(b, run.ce) - run.cs;
        lineText += run.text.slice(os, oe);
      }
      out += lineText + "\n";
    }
    return out.replace(/\n$/, "");
  };

  /* ===== 滚动 ===== */
  const maxScrollTop = () => (doc ? Math.max(0, doc.height - viewH()) : 0);
  const isAtBottom = () => maxScrollTop() - scrollTop <= 1;

  const applyScrollTop = (v: number) => {
    scrollTop = Math.max(0, Math.min(v, maxScrollTop()));
    setViewTop(scrollTop);
    requestRender();
  };

  const pinBottom = () => {
    if (!doc || !props.stickToBottom || pointerActive) return;
    applyScrollTop(maxScrollTop());
  };

  /** 用户主动滚动：同步吸底状态（与 DOM 版 processTranscriptScroll 一致） */
  const userScrollTo = (v: number) => {
    const next = Math.max(0, Math.min(v, maxScrollTop()));
    const atBottom = maxScrollTop() - next <= 1;
    if (!atBottom && props.stickToBottom) props.onStickChange(false);
    else if (atBottom && !props.stickToBottom && next > scrollTop) props.onStickChange(true);
    applyScrollTop(next);
  };

  /* ===== 动作执行 ===== */
  const openFileMenu = (e: { clientX: number; clientY: number }, path: string) => {
    const synthetic = {
      clientX: e.clientX,
      clientY: e.clientY,
      preventDefault: () => {},
      stopPropagation: () => {},
    } as unknown as MouseEvent;
    fileMenu.open(synthetic, path);
  };

  const revertEdits = async (undoneKey: string, edits: { path: string; oldText: string | null; newText: string }[]) => {
    const id = props.threadId;
    if (!id || reverting.has(undoneKey) || isExpanded(undoneKey)) return;
    reverting.add(undoneKey);
    requestRender();
    try {
      const changes: RevertChange[] = edits.map((e) => ({ path: e.path, oldText: e.oldText, newText: e.newText }));
      const res = await api.revertFileChanges(id, changes);
      if (res.conflicts.length === 0 && res.errors.length === 0) toggleExpanded(undoneKey, true);
    } catch (e) {
      void message(String(e), { kind: "error" });
    } finally {
      reverting.delete(undoneKey);
      requestRender();
    }
  };

  const permSelect = (reqKey: string, index: number, label: string, multiple: boolean) => {
    const req = props.permissions.find((p) => p.requestKey === reqKey);
    if (!req?.questions) return;
    const cur = permAnswers.get(reqKey) ?? req.questions.map(() => []);
    permAnswers.set(
      reqKey,
      cur.map((ans, i) => {
        if (i !== index) return ans;
        if (!multiple) return [label];
        return ans.includes(label) ? ans.filter((v) => v !== label) : [...ans, label];
      }),
    );
    if (!multiple) {
      const cust = permCustom.get(reqKey) ?? req.questions.map(() => "");
      permCustom.set(reqKey, cust.map((v, i) => (i === index ? "" : v)));
    }
    setPermTick((t) => t + 1);
  };

  const commitPermInput = () => {
    const pi = permInput();
    if (!pi) return;
    const req = props.permissions.find((p) => p.requestKey === pi.reqKey);
    if (req?.questions) {
      const cust = permCustom.get(pi.reqKey) ?? req.questions.map(() => "");
      permCustom.set(pi.reqKey, cust.map((v, i) => (i === pi.index ? pi.value : v)));
      if (!pi.multiple && pi.value.trim()) {
        const cur = permAnswers.get(pi.reqKey) ?? req.questions.map(() => []);
        permAnswers.set(pi.reqKey, cur.map((ans, i) => (i === pi.index ? [] : ans)));
      }
    }
    setPermInput(null);
    setPermTick((t) => t + 1);
  };

  const openEditor = (itemId: number) => {
    const item = props.groups.flatMap((g) => (g.user ? [g.user] : [])).find((u) => u.id === itemId);
    if (!item || !doc) return;
    const entry = doc.regions.find(
      ({ region }) => region.action.kind === "edit-user" && region.action.id === itemId,
    );
    if (!entry) return;
    const action = entry.region.action as { ex?: number; ey?: number; ew?: number };
    setEditState({
      item,
      x: action.ex ?? entry.region.x,
      docY: entry.top + (action.ey ?? entry.region.y),
      w: action.ew ?? 400,
      draft: item.text,
    });
  };

  const saveEdit = () => {
    const es = editState();
    if (!es) return;
    const text = es.draft.trim();
    if (!text && (es.item.images?.length ?? 0) === 0) return;
    setEditState(null);
    void editUserMessage(es.item.id, text, es.item.images ?? []);
  };

  const executeAction = (action: Action, e: { clientX: number; clientY: number }) => {
    switch (action.kind) {
      case "toggle": {
        const key = String(action.key);
        const entry = doc?.regions.find(({ region }) => region.action === action);
        if (entry) {
          toggleScrollAnchor = { key, viewportY: entry.top + entry.region.y - scrollTop };
        }
        // 详情开合属于主动浏览；先退出吸底，下一次布局按上面的锚点稳定视口。
        if (props.stickToBottom) props.onStickChange(false);
        toggleExpanded(key, action.value as boolean | undefined);
        break;
      }
      case "copy": {
        void navigator.clipboard.writeText(String(action.text ?? ""));
        copied.add(String(action.id));
        requestRender();
        window.setTimeout(() => {
          copied.delete(String(action.id));
          requestRender();
        }, 1200);
        break;
      }
      case "url":
        void api.openUrl(String(action.href)).catch((err) => console.error("open url failed", err));
        break;
      case "file": {
        const id = props.threadId;
        const path = String(action.path ?? "");
        if (!id || !path) break;
        const line = action.line as number | undefined;
        const act = IMAGE_FILE_RE.test(path) ? api.openFileDefault(id, path) : api.openInEditor(id, path, line);
        void act.catch((err) => void message(String(err), { kind: "error" }));
        break;
      }
      case "edit-user":
        openEditor(Number(action.id));
        break;
      case "perm-select":
        permSelect(String(action.reqKey), Number(action.index), String(action.label), Boolean(action.multiple));
        break;
      case "perm-custom": {
        const reqKey = String(action.reqKey);
        const index = Number(action.index);
        const region = doc?.regions.find(({ region: r }) => r.action === action)?.region;
        const top = doc?.regions.find(({ region: r }) => r.action === action)?.top ?? 0;
        if (!region) break;
        const cust = permCustom.get(reqKey) ?? [];
        setPermInput({
          reqKey,
          index,
          multiple: Boolean(action.multiple),
          x: region.x,
          docY: top + region.y,
          w: region.w,
          value: cust[index] ?? "",
        });
        void e;
        break;
      }
      case "perm-submit":
        void respondPermission(String(action.reqKey), JSON.stringify(action.answers));
        break;
      case "perm-reject":
        void respondPermission(String(action.reqKey), "");
        break;
      case "perm-option":
        void respondPermission(String(action.reqKey), String(action.optionId));
        break;
      case "revert":
        void revertEdits(
          String(action.undoneKey),
          action.edits as { path: string; oldText: string | null; newText: string }[],
        );
        break;
      case "banner":
        props.onReturnToTimeline();
        break;
      default:
        break;
    }
  };

  /* ===== 指针事件 ===== */
  const localPos = (e: { clientX: number; clientY: number }) => {
    const rect = canvasRef!.getBoundingClientRect();
    return { x: e.clientX - rect.left, y: e.clientY - rect.top };
  };

  const onPointerDown = (e: PointerEvent) => {
    if (!canvasRef || (e.button !== 0 && e.button !== 2)) return;
    const { x, y } = localPos(e);
    downPos = { x, y };
    const hit = hitTest(x, y);

    if (e.button === 2) {
      const action = hit.region?.action;
      if (action && (action.kind === "file" || action.kind === "file-menu") && action.path) {
        e.preventDefault();
        openFileMenu(e, String(action.path));
      } else if (hit.kind === "text" || hit.kind === "box-text") {
        e.preventDefault();
      }
      return;
    }

    if (hit.kind === "scrollbar") {
      const geom = scrollGeom();
      if (geom) {
        if (hit.thumb) {
          scrollDrag = { grab: y - geom.thumbY };
        } else {
          userScrollTo(((y - geom.thumbH / 2) / (geom.trackH - geom.thumbH)) * geom.max);
        }
        canvasRef.setPointerCapture(e.pointerId);
        e.preventDefault();
      }
      return;
    }

    if (hit.kind === "banner") {
      pendingClick = null;
      props.onReturnToTimeline();
      return;
    }

    if (hit.kind === "region" || hit.kind === "box-region") {
      const region = hit.region!;
      pointerActive = region.action.kind !== "copy";
      pendingClick = { region, box: hit.box };
      canvasRef.setPointerCapture(e.pointerId);
      return;
    }

    if (hit.kind === "box") {
      // 工具详情内部：与 DOM 版 isToolDetailScroll 一致，不暂停吸底
      pointerActive = false;
      return;
    }

    // 文本 / 空白：开始选择
    pointerActive = true;
    const off = hit.off ?? textOffsetAtDoc(x, y + scrollTop)?.off ?? -1;
    const nowT = performance.now();
    const count = nowT - lastClick.t < 450 ? lastClick.count + 1 : 1;
    lastClick = { t: nowT, count };
    if (count === 2 && off >= 0) {
      selectWordAt(off);
      selecting = false;
    } else if (count >= 3 && off >= 0) {
      selectBlockAt(off);
      selecting = false;
      lastClick.count = 0;
    } else if (off >= 0) {
      selAnchor = off;
      selA = off;
      selB = off;
      selecting = true;
    }
    canvasRef.setPointerCapture(e.pointerId);
    e.preventDefault();
  };

  const updateHover = (x: number, y: number) => {
    const hit = hitTest(x, y);
    let region: Region | null = null;
    let action: Action | null = null;
    let cursor = "default";
    let title = "";
    let sbHover = false;
    if (hit.kind === "region" || hit.kind === "box-region") {
      region = hit.region!;
      cursor = region.cursor;
      title = region.title ?? "";
    } else if (hit.kind === "text" || hit.kind === "box-text") {
      const run = hit.run;
      if (run?.action && (run.action.kind === "url" || run.action.kind === "file")) {
        action = run.action;
        cursor = "pointer";
        title = String(run.action.title ?? "");
      } else {
        cursor = "text";
      }
    } else if (hit.kind === "scrollbar") {
      sbHover = !!hit.thumb;
      cursor = "default";
    } else if (hit.kind === "banner") {
      cursor = "pointer";
      title = "回到当前时间线和最新消息";
    }
    const changed = hover !== region || hoverAction !== action || scrollbarHover !== sbHover;
    hover = region;
    hoverAction = action;
    scrollbarHover = sbHover;
    if (canvasRef) {
      canvasRef.style.cursor = cursor;
      canvasRef.title = title;
    }
    if (changed) requestRender();
  };

  const onPointerMove = (e: PointerEvent) => {
    if (!canvasRef) return;
    const { x, y } = localPos(e);
    if (scrollDrag) {
      const geom = scrollGeom();
      if (geom) {
        const ty = Math.max(0, Math.min(geom.trackH - geom.thumbH, y - scrollDrag.grab));
        userScrollTo((ty / (geom.trackH - geom.thumbH)) * geom.max);
      }
      return;
    }
    if (selecting && selAnchor >= 0) {
      const hit = hitTest(x, y);
      let off: number | null = null;
      if (hit.kind === "text" || hit.kind === "box-text") off = hit.off ?? null;
      else if (y < 0) off = 0;
      else if (y > viewH()) off = doc?.charTotal ?? 0;
      else off = textOffsetAtDoc(x, y + scrollTop)?.off ?? null;
      if (off !== null) {
        selA = selAnchor;
        selB = off;
        requestRender();
      }
      return;
    }
    updateHover(x, y);
  };

  const finishPointer = () => {
    if (selecting) {
      selecting = false;
      if (selA === selB) {
        selA = selB = -1;
        requestRender();
      }
    }
    pointerActive = false;
    scrollDrag = null;
    downPos = null;
    if (props.stickToBottom) pinBottom();
  };

  const onPointerUp = (e: PointerEvent) => {
    if (!canvasRef) return;
    const { x, y } = localPos(e);
    const moved = downPos ? Math.hypot(x - downPos.x, y - downPos.y) > 4 : false;
    if (scrollDrag) {
      finishPointer();
      return;
    }
    const click = pendingClick;
    pendingClick = null;
    if (!moved && click && !selecting) {
      executeAction(click.region.action, e);
    }
    finishPointer();
  };

  /* ===== 滚轮 ===== */
  const onWheel = (e: WheelEvent) => {
    if (e.ctrlKey) return;
    e.preventDefault();
    if (!doc) return;
    let delta = e.deltaY;
    if (e.deltaMode === 1) delta *= 16;
    else if (e.deltaMode === 2) delta *= viewH();
    const rect = canvasRef!.getBoundingClientRect();
    const px = e.clientX - rect.left;
    const py = e.clientY - rect.top;
    // 工具详情独立滚动区：overscroll-behavior: contain，不链式传给外层
    for (const { box, top } of doc.boxes) {
      const sy = top + box.y - scrollTop;
      if (px < box.x || px > box.x + box.w || py < sy || py > sy + box.h) continue;
      const max = Math.max(0, box.contentH - box.h);
      if (max > 0) {
        box.scrollTop = Math.max(0, Math.min(box.scrollTop + delta, max));
        scrollPos.set(box.key, box.scrollTop);
        requestRender();
      }
      return;
    }
    if (doc.height <= viewH() + 1) return;
    if (delta > 0 && isAtBottom()) {
      if (!props.stickToBottom) props.onStickChange(true);
      return;
    }
    if (delta !== 0 && props.stickToBottom) props.onStickChange(false);
    applyScrollTop(scrollTop + delta);
  };

  /* ===== 键盘 ===== */
  const onKey = (e: KeyboardEvent) => {
    if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey) {
      if (e.key === "c" || e.key === "C") {
        const text = copySelection();
        if (text) {
          void navigator.clipboard.writeText(text);
          e.preventDefault();
        }
        return;
      }
      if (e.key === "a" || e.key === "A") {
        const target = e.target;
        if (
          target instanceof HTMLElement &&
          (target.isContentEditable || target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT")
        ) {
          return;
        }
        if (doc && doc.charTotal > 0) {
          selA = 0;
          selB = doc.charTotal;
          requestRender();
          e.preventDefault();
        }
      }
      return;
    }
    if (e.key === "Escape") {
      if (selA !== selB) {
        selA = selB = -1;
        requestRender();
      }
      return;
    }
    const scrollUpKeys = new Set(["ArrowUp", "PageUp", "Home"]);
    const scrollDownKeys = new Set(["ArrowDown", "PageDown", "End"]);
    const scrollsUp = scrollUpKeys.has(e.key) || (e.key === " " && e.shiftKey);
    const scrollsDown = scrollDownKeys.has(e.key) || (e.key === " " && !e.shiftKey);
    if (e.altKey || e.ctrlKey || e.metaKey || (!scrollsUp && !scrollsDown)) return;
    if (!doc || doc.height <= viewH() + 1) return;
    const target = e.target;
    if (target instanceof Node && target !== document.body && target !== canvasRef && !hostRef?.contains(target)) return;
    if (
      target instanceof HTMLElement &&
      (target.isContentEditable || target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT")
    ) {
      return;
    }
    e.preventDefault();
    if (scrollsDown && isAtBottom()) {
      if (!props.stickToBottom) props.onStickChange(true);
      pinBottom();
      return;
    }
    if (props.stickToBottom) props.onStickChange(false);
    const max = maxScrollTop();
    let next = scrollTop;
    switch (e.key) {
      case "ArrowUp":
        next -= 48;
        break;
      case "ArrowDown":
        next += 48;
        break;
      case "PageUp":
        next -= viewH() * 0.9;
        break;
      case "PageDown":
        next += viewH() * 0.9;
        break;
      case " ":
        next += e.shiftKey ? -viewH() * 0.9 : viewH() * 0.9;
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = max;
        break;
    }
    applyScrollTop(next);
  };

  /* ===== 挂载 ===== */
  const canvasApi: TranscriptCanvasApi = {
    scrollToGroup: (index) => {
      if (!doc) return;
      const top = doc.groupTops[index];
      if (top === undefined) return;
      applyScrollTop(Math.max(0, top - 20));
    },
    scrollToBottom: () => applyScrollTop(maxScrollTop()),
    enableBottomFollow: () => {
      props.onStickChange(true);
      pointerActive = false;
      pinBottom();
    },
  };

  onMount(() => {
    const host = hostRef;
    const canvas = canvasRef;
    if (!host || !canvas) return;
    const ro = new ResizeObserver(() => {
      setViewW(host.clientWidth);
      setViewH(host.clientHeight);
    });
    ro.observe(host);
    setViewW(host.clientWidth);
    setViewH(host.clientHeight);

    canvas.addEventListener("wheel", onWheel, { passive: false });
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("pointerup", finishPointer, true);
    window.addEventListener("pointercancel", finishPointer, true);
    const mo = new MutationObserver(() => bump());
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });

    props.onApi(canvasApi);

    onCleanup(() => {
      ro.disconnect();
      mo.disconnect();
      canvas.removeEventListener("wheel", onWheel);
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("pointerup", finishPointer, true);
      window.removeEventListener("pointercancel", finishPointer, true);
      if (revealTimer !== undefined) window.clearTimeout(revealTimer);
      if (layoutThrottleTimer !== undefined) window.clearTimeout(layoutThrottleTimer);
    });
  });

  return (
    <div
      class="transcript transcript-canvas-host"
      classList={{ "checkpoint-preview-fading": props.fading }}
      ref={hostRef}
    >
      <canvas
        ref={canvasRef}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onContextMenu={(e) => e.preventDefault()}
        onDragStart={(e) => e.preventDefault()}
      />
      <Show when={editState()} keyed>
        {(es) => (
          <div
            class="user-edit"
            style={{
              position: "absolute",
              left: `${es.x}px`,
              top: `${es.docY - viewTop()}px`,
              width: `${es.w}px`,
              "z-index": "6",
            }}
          >
            <textarea
              class="user-edit-input"
              rows={Math.min(10, Math.max(2, es.draft.split("\n").length))}
              value={es.draft}
              onInput={(e) => setEditState({ ...es, draft: e.currentTarget.value })}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
                  e.preventDefault();
                  saveEdit();
                }
                if (e.key === "Escape") setEditState(null);
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
              <button class="btn secondary small" onClick={() => setEditState(null)}>
                取消
              </button>
              <button class="btn primary small" onClick={saveEdit}>
                发送
              </button>
            </div>
          </div>
        )}
      </Show>
      <Show when={permInput()} keyed>
        {(pi) => (
          <input
            class="question-custom"
            style={{
              position: "absolute",
              left: `${pi.x}px`,
              top: `${pi.docY - viewTop()}px`,
              width: `${pi.w}px`,
              "z-index": "6",
            }}
            value={pi.value}
            placeholder="输入其他答案"
            onInput={(e) => setPermInput({ ...pi, value: e.currentTarget.value })}
            onBlur={commitPermInput}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitPermInput();
              if (e.key === "Escape") setPermInput(null);
            }}
            ref={(el) => queueMicrotask(() => el.focus())}
          />
        )}
      </Show>
      <fileMenu.Menu />
    </div>
  );
}
