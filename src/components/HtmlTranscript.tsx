import { For, Show, createEffect, createSignal, onCleanup, onMount, untrack } from "solid-js";
import { optimisticHandoffs, toggleExpanded, traceThreadSwitchLayoutDone } from "../store";
import { promptDisplayOf } from "../promptDisplay";
import { processSegments, splitTurnBody } from "../processDisplay";
import { resolveUserScrollStick } from "../scrollStick";
import { TranscriptHeights } from "../transcriptHeights";
import type { PermissionRequest } from "../types";
import type { Group } from "./TurnGroup";
import { HtmlTurn } from "./HtmlTurn";
import type { MarkdownRevealDetail } from "./LongMarkdown";
import "./HtmlTranscript.css";

export interface HtmlTranscriptHandle {
  scrollToBottom(): void;
  scrollToGroup(index: number): void;
  revealMatch(itemId: number, query: string): void;
  scrollBy(delta: number): void;
  isAtBottom(): boolean;
  scrollTop(): number;
  maxScrollTop(): number;
  activeGroup(): number;
  hasFocusedInput(): boolean;
}

interface HtmlTranscriptProps {
  threadId: string | null;
  groups: Group[];
  permissions: PermissionRequest[];
  running: boolean;
  readOnly?: boolean;
  loading: boolean;
  emptyHint: string;
  preview: boolean;
  onReturnToCurrent: () => void;
  onScroll?: (top: number, max: number, user: boolean) => void;
  loadItems?: (ids: number[]) => Promise<void>;
  onBrowseDetail?: () => void;
  ref?: (handle: HtmlTranscriptHandle) => void;
}

const TOP = 24;
const BOTTOM = 16;

/** Native DOM/selection/scrolling, with only the viewport and one screen either side mounted. */
export function HtmlTranscript(props: HtmlTranscriptProps) {
  let host!: HTMLDivElement;
  let space!: HTMLDivElement;
  const heights = new TranscriptHeights();
  const rows = new Map<number, HTMLDivElement>();
  const [revision, setRevision] = createSignal(0);
  const [session, setSession] = createSignal({});
  const [imagesLoading, setImagesLoading] = createSignal(false);
  const [visible, setVisible] = createSignal<number[]>([], {
    equals: (a, b) => a.length === b.length && a.every((value, i) => value === b[i]),
  });
  let frame = 0;
  let firstLayoutQueued = false;
  let imageGeneration = 0;
  let imageTimeout: ReturnType<typeof setTimeout> | undefined;
  let disposed = false;
  let observer: ResizeObserver | undefined;
  let width = 0;
  let keepBottom = true;
  let lastTop = 0;
  let expectedTop: number | undefined;
  let threadId = props.threadId;
  let previous: Group[] = [];
  let jump: number | null = null;
  let focused: number | null = null;
  let selected: [number, number] | null = null;
  let detailAnchor: { element: HTMLElement; offset: number } | null = null;
  let readingAnchor: { index: number; inset: number } | null = null;
  let match: { itemId: number; query: string; pending: boolean } | null = null;
  let markedElement: HTMLElement | null = null;
  let searchPinned = false;
  let highlight: { delete(name: string): void; set(name: string, value: unknown): void } | undefined;
  const maxScroll = () => Math.max(0, host.scrollHeight - host.clientHeight);
  const offset = (index: number) => TOP + heights.offset(index);
  const at = (top: number) => heights.at(Math.max(0, top - TOP));

  function setTop(top: number) {
    host.scrollTop = Math.max(0, Math.min(maxScroll(), top));
    expectedTop = lastTop = host.scrollTop;
  }

  function updateWindow(top: number) {
    if (!heights.length) { setVisible([]); return; }
    let start = at(Math.max(0, top - host.clientHeight));
    let end = at(top + host.clientHeight * 2) + 1;
    // ponytail: explicit cross-screen selection retains selected turns for native copy;
    // enormous selections can grow DOM. Upgrade to source-offset selection if needed.
    if (selected) { start = Math.min(start, selected[0]); end = Math.max(end, selected[1] + 1); }
    const indices = Array.from({ length: Math.max(0, end - start) }, (_, i) => start + i);
    if (focused !== null && focused < heights.length && !indices.includes(focused)) indices.push(focused);
    for (const [index, row] of rows) {
      if (index < heights.length && row.querySelector("[data-editing]") && !indices.includes(index)) indices.push(index);
    }
    indices.sort((a, b) => a - b);
    setVisible(indices);
  }

  function markMatch() {
    if (!match) return;
    const element = host.querySelector<HTMLElement>(`[data-item-id="${match.itemId}"]`);
    if (!element || element.closest("[data-history-loading]") || element.querySelector(".ht-user-text[aria-busy]")
      || (element === markedElement && !match.pending)) return;
    for (const virtual of element.querySelectorAll<HTMLElement>("[data-markdown-virtual]")) {
      const detail: MarkdownRevealDetail = { query: match.query };
      virtual.dispatchEvent(new CustomEvent("markdown-reveal", { detail }));
      searchPinned = true;
    }
    const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT, {
      acceptNode: node => node.parentElement?.closest("button, textarea") ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT,
    });
    const ranges: Range[] = [];
    const nodes: { node: Node; start: number; end: number }[] = [];
    let node: Node | null, text = "";
    while ((node = walker.nextNode())) {
      const start = text.length;
      text += node.textContent ?? "";
      nodes.push({ node, start, end: text.length });
    }
    const pattern = new RegExp(match.query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "giu");
    let nodeIndex = 0;
    for (const hit of text.matchAll(pattern)) {
      const pos = hit.index, length = hit[0].length;
      while (nodeIndex < nodes.length && nodes[nodeIndex].end <= pos) nodeIndex++;
      const start = nodes[nodeIndex];
      let endIndex = nodeIndex;
      while (endIndex < nodes.length && nodes[endIndex].end < pos + length) endIndex++;
      const end = nodes[endIndex];
      if (!start || !end) break;
      const range = document.createRange();
      range.setStart(start.node, pos - start.start);
      range.setEnd(end.node, pos + length - end.start);
      ranges.push(range);
    }
    const css = CSS as typeof CSS & { highlights?: typeof highlight };
    const ctor = (window as unknown as { Highlight?: new (...ranges: Range[]) => unknown }).Highlight;
    highlight = css.highlights;
    highlight?.delete("transcript-search");
    if (ctor && ranges.length) highlight?.set("transcript-search", new ctor(...ranges));
    host.querySelectorAll(".transcript-search-hit").forEach(el => el.classList.remove("transcript-search-hit"));
    element.classList.add("transcript-search-hit");
    markedElement = element;
    if (match.pending && !element.querySelector("[data-history-loading]")) {
      const rect = ranges[0]?.getBoundingClientRect() ?? element.getBoundingClientRect();
      setTop(host.scrollTop + rect.top - host.getBoundingClientRect().top - 32);
      match.pending = false;
    }
  }

  function layout() {
    frame = 0;
    if (disposed || !host) return;
    const anchor = readingAnchor?.index ?? at(host.scrollTop + 32);
    const anchorOffset = readingAnchor?.inset ?? host.scrollTop - offset(anchor);
    let changed = false;
    // Read in a batch, then write: no per-row read/write layout thrashing.
    const measured = [...rows].map(([index, row]) => [index, row.getBoundingClientRect().height] as const);
    for (const [index, height] of measured) if (height > 0) changed = heights.set(index, height) || changed;
    if (changed) setRevision(value => value + 1);
    if (jump !== null) {
      setTop(offset(jump) - 20);
      if (rows.has(jump)) jump = null;
    } else if (detailAnchor?.element.isConnected) {
      setTop(host.scrollTop + detailAnchor.element.getBoundingClientRect().top - host.getBoundingClientRect().top - detailAnchor.offset);
      detailAnchor = null;
    } else if (keepBottom) setTop(maxScroll());
    else if (changed || readingAnchor) setTop(offset(anchor) + Math.min(anchorOffset, heights.get(anchor) - 1));
    readingAnchor = null;
    markMatch();
    updateWindow(host.scrollTop);
    props.onScroll?.(host.scrollTop, maxScroll(), false);
    if (props.groups.length) traceThreadSwitchLayoutDone(props.threadId, props.groups.length);
  }

  function schedule() {
    if (!frame && !disposed) frame = requestAnimationFrame(layout);
  }

  function initialLayout() {
    if (firstLayoutQueued) return;
    firstLayoutQueued = true;
    const generation = ++imageGeneration;
    clearTimeout(imageTimeout);
    setImagesLoading(false);
    queueMicrotask(() => {
      firstLayoutQueued = false;
      if (disposed || generation !== imageGeneration) return;
      cancelAnimationFrame(frame);
      layout();
      const images = [...space.querySelectorAll<HTMLImageElement>("img")].filter(image => !image.complete);
      if (!images.length) return;
      // Cold images must contribute their real sizes before the first content frame.
      // Only the mounted window is decoded; a failed/slow image cannot block a session indefinitely.
      setImagesLoading(true);
      let finished = false;
      const finish = () => {
        if (finished || disposed || generation !== imageGeneration) return;
        finished = true;
        clearTimeout(imageTimeout);
        cancelAnimationFrame(frame);
        layout();
        setImagesLoading(false);
      };
      imageTimeout = setTimeout(finish, 10_000);
      void Promise.all(images.map(image => { image.loading = "eager"; return image.decode().catch(() => {}); })).then(finish);
    });
  }

  function releaseSearch() {
    if (match) match.pending = false;
    if (!searchPinned) return;
    searchPinned = false;
    for (const virtual of host.querySelectorAll("[data-markdown-virtual]")) {
      virtual.dispatchEvent(new CustomEvent("markdown-reveal", { detail: { query: "" } }));
    }
  }

  function scrollToGroup(index: number) {
    if (!Number.isInteger(index) || !props.groups[index]) return;
    releaseSearch();
    keepBottom = false;
    detailAnchor = readingAnchor = null;
    jump = index;
    setTop(offset(index) - 20);
    updateWindow(host.scrollTop);
    schedule();
  }

  function revealMatch(itemId: number, query: string) {
    const index = props.groups.findIndex(g => g.user?.id === itemId || g.body.some(it => it.id === itemId));
    const group = props.groups[index];
    if (!group || !query) return;
    const q = query.toLowerCase();
    if (group.user?.id === itemId) {
      const typed = promptDisplayOf(group.user.text);
      if (typed !== undefined && !typed.toLowerCase().includes(q)) toggleExpanded(`user-sys-${itemId}`, true);
      if (group.user.text.length > 4000) toggleExpanded(`user-text-${itemId}`, true);
    } else {
      const { process } = splitTurnBody(group.body, !!group.turn);
      if (group.turn && process.some(it => it.id === itemId)) toggleExpanded(`turn-${group.turn.id}`, true);
      for (const segment of processSegments(process)) {
        if (segment.type === "process" && segment.items.some(it => it.id === itemId)) {
          toggleExpanded(`process-${segment.id}-done`, true);
          toggleExpanded(`process-${segment.id}-live`, true);
        }
      }
      const item = group.body.find(it => it.id === itemId);
      if (item?.type === "tool" || item?.type === "thought") toggleExpanded(`${item.type}-${itemId}`, true);
    }
    markedElement = null;
    scrollToGroup(index);
    match = { itemId, query, pending: true };
  }

  function onScroll() {
    const top = host.scrollTop;
    const user = expectedTop === undefined || Math.abs(top - expectedTop) > 1;
    expectedTop = undefined;
    if (user) {
      keepBottom = resolveUserScrollStick(lastTop, top, maxScroll());
      jump = null;
      detailAnchor = readingAnchor = null;
      releaseSearch();
    }
    lastTop = top;
    updateWindow(top);
    props.onScroll?.(top, maxScroll(), user);
    schedule();
  }

  function browseDetail(event: MouseEvent) {
    const button = (event.target as Element).closest<HTMLElement>("button[aria-expanded]");
    if (!button) return;
    keepBottom = false;
    detailAnchor = { element: button, offset: button.getBoundingClientRect().top - host.getBoundingClientRect().top };
    props.onBrowseDetail?.();
    schedule();
  }

  const rowIndex = (node: Node | null) => {
    const element = node instanceof Element ? node : node?.parentElement;
    const value = element?.closest<HTMLElement>("[data-group-index]")?.dataset.groupIndex;
    return value === undefined ? null : Number(value);
  };

  createEffect(() => {
    const groups = props.groups;
    const id = props.threadId;
    const handoff = !!id && !!threadId && optimisticHandoffs.get(id) === threadId;
    const firstContent = (id !== threadId && !handoff) || (!previous.length && groups.length > 0);
    if (id !== threadId && !handoff) {
      heights.reset(groups.length);
      keepBottom = true;
      jump = focused = null;
      selected = detailAnchor = readingAnchor = match = null;
      highlight?.delete("transcript-search");
      previous = [];
      setSession({});
      setVisible([]);
    } else {
      if (host && !keepBottom && !readingAnchor && heights.length) {
        const index = at(host.scrollTop + 32);
        readingAnchor = { index, inset: host.scrollTop - offset(index) };
      }
      heights.resize(groups.length);
      for (let i = 0; i < groups.length; i++) {
        const old = previous[i], next = groups[i];
        // Keep a growing turn's measured height; replacing history/preview invalidates only changed rows.
        if (old && old !== next && (old.user?.id !== next.user?.id || old.turn || next.turn)) heights.set(i, 240);
      }
    }
    previous = groups;
    threadId = id;
    setRevision(value => value + 1);
    untrack(() => {
      if (host) {
        if (keepBottom) setTop(maxScroll());
        updateWindow(host.scrollTop);
        if (match?.pending) revealMatch(match.itemId, match.query);
      }
      if (firstContent) initialLayout();
      else schedule();
    });
  });

  onMount(() => {
    observer = new ResizeObserver(entries => {
      host.style.setProperty("--transcript-gutter", `${host.offsetWidth - host.clientWidth}px`);
      if (entries.some(entry => entry.target === space) && width !== space.clientWidth) {
        const anchor = at(host.scrollTop + 32);
        const inset = host.scrollTop - offset(anchor);
        width = space.clientWidth;
        heights.reset(props.groups.length);
        // Preserve a tall visible turn before calculating the new window; an estimated
        // 240px height could otherwise move its in-turn anchor into unrelated history.
        for (const [index, row] of rows) heights.set(index, row.getBoundingClientRect().height);
        setRevision(value => value + 1);
        if (!keepBottom) setTop(offset(anchor) + Math.min(inset, heights.get(anchor) - 1));
      }
      schedule();
    });
    host.style.setProperty("--transcript-gutter", `${host.offsetWidth - host.clientWidth}px`);
    width = space.clientWidth;
    observer.observe(host);
    observer.observe(space);
    for (const row of rows.values()) observer.observe(row);
    const selectionChanged = () => {
      const selection = window.getSelection();
      const a = rowIndex(selection?.anchorNode ?? null), b = rowIndex(selection?.focusNode ?? null);
      selected = selection && !selection.isCollapsed && a !== null && b !== null ? [Math.min(a, b), Math.max(a, b)] : null;
      schedule();
    };
    const focusChanged = () => {
      focused = host.contains(document.activeElement) ? rowIndex(document.activeElement) : null;
      schedule();
    };
    const markdownAnchor = (event: Event) => {
      const delta = (event as CustomEvent<{ delta: number }>).detail?.delta;
      event.preventDefault();
      if (Number.isFinite(delta) && !keepBottom && jump === null && !match?.pending) {
        setTop(host.scrollTop + delta);
        readingAnchor = detailAnchor = null;
      }
      schedule();
    };
    document.addEventListener("selectionchange", selectionChanged);
    document.addEventListener("focusin", focusChanged);
    host.addEventListener("click", browseDetail, true);
    host.addEventListener("markdown-anchor", markdownAnchor);
    props.ref?.({
      scrollToBottom() { keepBottom = true; jump = null; detailAnchor = readingAnchor = null; releaseSearch(); setTop(maxScroll()); updateWindow(host.scrollTop); schedule(); },
      scrollToGroup,
      revealMatch,
      scrollBy(delta) {
        expectedTop = undefined;
        host.scrollTop += delta;
        onScroll();
        expectedTop = host.scrollTop;
      },
      isAtBottom: () => maxScroll() - host.scrollTop <= 2,
      scrollTop: () => host.scrollTop,
      maxScrollTop: maxScroll,
      activeGroup: () => heights.length ? at(host.scrollTop + 32) : -1,
      hasFocusedInput() {
        const el = document.activeElement;
        return el instanceof HTMLElement && (el.isContentEditable || /^(INPUT|TEXTAREA|SELECT|BUTTON|A)$/.test(el.tagName)
          || (el !== host && host.contains(el) && el.tabIndex >= 0));
      },
    });
    updateWindow(host.scrollTop);
    initialLayout();
    onCleanup(() => {
      document.removeEventListener("selectionchange", selectionChanged);
      document.removeEventListener("focusin", focusChanged);
      host.removeEventListener("click", browseDetail, true);
      host.removeEventListener("markdown-anchor", markdownAnchor);
    });
  });

  onCleanup(() => {
    disposed = true;
    imageGeneration++;
    clearTimeout(imageTimeout);
    cancelAnimationFrame(frame);
    observer?.disconnect();
    highlight?.delete("transcript-search");
  });

  function Row(rowProps: { index: number }) {
    let element!: HTMLDivElement;
    onMount(() => { rows.set(rowProps.index, element); observer?.observe(element); schedule(); });
    onCleanup(() => { rows.delete(rowProps.index); observer?.unobserve(element); });
    return <div ref={element} class="html-transcript-row" data-group-index={rowProps.index}
      style={{ transform: `translateY(${(revision(), offset(rowProps.index))}px)` }}>
      <Show when={props.groups[rowProps.index]}>
        <HtmlTurn group={props.groups[rowProps.index]} running={props.running} readOnly={props.readOnly}
          preview={props.preview} threadId={props.threadId} loadItems={props.loadItems}
          onReturnToCurrent={props.onReturnToCurrent}
          onBrowseDetail={() => { keepBottom = false; props.onBrowseDetail?.(); schedule(); }} />
      </Show>
    </div>;
  }

  return <div ref={host} class="html-transcript" tabindex="0" role="region" aria-label="会话记录" onScroll={onScroll}
    onWheel={event => { if (event.deltaY < 0) { keepBottom = false; props.onBrowseDetail?.(); } }}>
    <Show when={imagesLoading()}><div class="html-transcript-loading" role="status">正在加载会话…</div></Show>
    <div ref={space} class="html-transcript-space" aria-busy={imagesLoading()}
      style={{ height: `${(revision(), heights.total + TOP + BOTTOM)}px`, visibility: imagesLoading() ? "hidden" : undefined }}>
      <Show when={props.groups.length} fallback={<div class="html-transcript-empty" role="status">{props.loading ? "正在加载会话…" : props.emptyHint}</div>}>
        <Show when={session()} keyed><For each={visible()}>{index => <Row index={index} />}</For></Show>
      </Show>
    </div>
  </div>;
}
