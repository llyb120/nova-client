import { createEffect, onCleanup, onSettled, untrack } from "solid-js";
import type { Marked } from "marked";
import { chunkMarkdown, markdownTokenText, type MarkdownChunk } from "../markdownChunks";
import "./LongMarkdown.css";

/** Dispatch on [data-markdown-virtual] before walking its text nodes. Mounting is
 * synchronous; element is the first matching chunk, or undefined for no match.
 * An empty query releases the search pin. Source stays in JS, not DOM attributes. */
export interface MarkdownRevealDetail { query: string; element?: HTMLElement }
/** Parent may preventDefault and apply delta through its programmatic scroll path. */
export interface MarkdownAnchorDetail { delta: number }

export function LongMarkdown(props: {
  text: string;
  live?: boolean;
  markFiles?: boolean;
  marked: Marked;
  render: (chunk: MarkdownChunk, markFiles: boolean) => string;
}) {
  type Block = { element: HTMLElement; top: number; bottom: number };
  type Entry = { element: HTMLDivElement; chunk: MarkdownChunk; near: boolean; mounted: boolean; dirty: boolean; height: number; offset: number; blocks: Block[]; plain?: string };
  let host!: HTMLDivElement;
  let root: HTMLElement | null = null;
  let model = chunkMarkdown(props.marked, untrack(() => props.text));
  const entries: Entry[] = [];
  const byElement = new Map<Element, Entry>();
  let observer: IntersectionObserver | undefined;
  let sizes: ResizeObserver | undefined;
  let rootSize: ResizeObserver | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let ready = false;
  let searchPins = new Set<Entry>();
  let markFiles = !!props.markFiles;
  let anchor: { element: HTMLElement; offset: number } | undefined;
  let bottom = false;
  let anchorScrollTop = 0;
  let scrollFrame = 0;
  let margin = 0;

  function indexHeights() {
    // ponytail: O(chunks) only on geometry/source changes, never plain scroll;
    // a prefix-sum tree is the upgrade if resize/streaming profiles warrant it.
    let offset = 0;
    for (const entry of entries) { entry.offset = offset; offset += entry.height; }
  }

  function measure(entry: Entry, height?: number) {
    const rect = entry.element.getBoundingClientRect();
    entry.height = height ?? rect.height;
    entry.blocks = Array.from(entry.element.children, element => {
      const block = element.getBoundingClientRect();
      return { element: element as HTMLElement, top: block.top - rect.top, bottom: block.bottom - rect.top };
    });
  }

  function rememberAnchor() {
    cancelAnimationFrame(scrollFrame); scrollFrame = 0;
    if (!root) return;
    anchorScrollTop = root.scrollTop;
    // Only the host origin is live: a parent can move this reply without resizing
    // its chunks. Block geometry stays pre-reflow until RO restores the anchor.
    const top = root.getBoundingClientRect().top - host.getBoundingClientRect().top;
    const height = root.clientHeight, max = root.scrollHeight - height;
    bottom = max > 0 && max - anchorScrollTop <= 2;
    let lo = 0, hi = entries.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (entries[mid].offset + entries[mid].height <= top) lo = mid + 1;
      else hi = mid;
    }
    const entry = entries[lo];
    anchor = undefined;
    if (!entry || entry.offset >= top + height) return;
    // Direct children are vertically ordered by md-seg's flex column layout.
    lo = 0; hi = entry.blocks.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (entry.offset + entry.blocks[mid].bottom <= top) lo = mid + 1;
      else hi = mid;
    }
    const block = entry.blocks[lo];
    anchor = { element: block?.element ?? entry.element, offset: entry.offset + (block?.top ?? 0) - top };
  }

  function scrolled() {
    if (!scrollFrame) scrollFrame = requestAnimationFrame(rememberAnchor);
  }

  function restoreAnchor() {
    if (!root) return;
    // A parent jump/search may precede the browser's scroll event. Never undo it.
    if (Math.abs(root.scrollTop - anchorScrollTop) > 1) { rememberAnchor(); return; }
    const delta = bottom ? root.scrollHeight - root.clientHeight - root.scrollTop
      : anchor?.element.isConnected ? anchor.element.getBoundingClientRect().top - root.getBoundingClientRect().top - anchor.offset : 0;
    if (Math.abs(delta) > .5) {
      const event = new CustomEvent<MarkdownAnchorDetail>("markdown-anchor", { bubbles: true, cancelable: true, detail: { delta } });
      if (host.dispatchEvent(event)) root.scrollTop += delta;
    }
    rememberAnchor();
  }

  function refresh(candidates: Iterable<Entry> = entries) {
    if (root && Math.abs(root.scrollTop - anchorScrollTop) > 1) rememberAnchor();
    const selection = window.getSelection(), active = document.activeElement;
    const measured: Entry[] = [];
    for (const entry of candidates) if (paint(entry, selection, active)) measured.push(entry);
    // Batch writes before reads; ordinary scroll/IO never scans native children.
    if (measured.length) { measured.forEach(entry => measure(entry)); indexHeights(); }
    restoreAnchor();
  }

  function selected(entry: Entry, selection: Selection | null) {
    if (!selection || selection.isCollapsed) return false;
    for (let i = 0; i < selection.rangeCount; i++) {
      if (selection.getRangeAt(i).intersectsNode(entry.element)) return true;
    }
    return false;
  }

  function paint(entry: Entry, selection: Selection | null, active: Element | null) {
    const pin = selected(entry, selection), focused = entry.element.contains(active);
    const wanted = entry.near || pin || searchPins.has(entry) || focused;
    if (wanted) {
      if (!entry.mounted || (entry.dirty && !pin && !focused)) {
        entry.element.innerHTML = props.render(entry.chunk, markFiles);
        entry.element.style.height = "";
        entry.mounted = true;
        entry.dirty = false;
        sizes?.observe(entry.element, { box: "border-box" });
        return true;
      }
    } else if (entry.mounted) {
      sizes?.unobserve(entry.element);
      entry.element.style.height = `${entry.height}px`;
      entry.element.replaceChildren();
      entry.mounted = false;
      entry.blocks = [];
    }
    return false;
  }

  function sync() {
    const next = chunkMarkdown(props.marked, props.text, model);
    if (!next.source.startsWith(model.source)) { searchPins.clear(); anchor = undefined; bottom = false; }
    const changedFiles = markFiles !== !!props.markFiles;
    markFiles = !!props.markFiles;
    model = next;
    const width = host.clientWidth || 700;
    for (let i = 0; i < model.chunks.length; i++) {
      const chunk = model.chunks[i];
      let entry = entries[i];
      if (!entry) {
        const element = document.createElement("div");
        element.className = "md-seg";
        element.dataset.markdownChunk = String(i);
        // Only an estimate until first visible measurement. No hidden HTML parse/layout.
        const lines = chunk.source.split("\n").reduce((n, line) => n + Math.max(1, Math.ceil(line.length * 8 / width)), 0);
        const height = Math.max(24, lines * 20);
        element.style.height = `${height}px`;
        entry = { element, chunk, near: false, mounted: false, dirty: true, height, offset: 0, blocks: [] };
        entries.push(entry);
        byElement.set(element, entry);
        host.append(element);
        observer?.observe(element);
      } else if (entry.chunk !== chunk || changedFiles) {
        entry.dirty = true;
        entry.plain = undefined;
        entry.chunk = chunk;
      }
    }
    for (const entry of entries.splice(model.chunks.length)) {
      observer?.unobserve(entry.element);
      sizes?.unobserve(entry.element);
      byElement.delete(entry.element);
      searchPins.delete(entry);
      entry.element.remove();
    }
    indexHeights();
    refresh();
  }

  // The shared Markdown stream animation can tick every frame. Only lex its
  // unfinished tail at most every 80ms; completion/edit flushes synchronously.
  createEffect(() => [props.text, props.live, props.markFiles] as const, ([text, live]) => {
    if (!ready) return;
    if (!live || !text.replace(/\r\n?/g, "\n").startsWith(model.source)) {
      clearTimeout(timer); timer = undefined;
      sync();
    } else if (!timer) timer = setTimeout(() => { timer = undefined; sync(); }, 80);
  });

  function selectionChanged() {
    // ponytail: explicit cross-screen selections pin their native DOM (unbounded
    // only by user selection); source-offset copy is the upgrade for Select All.
    refresh();
  }
  function focusChanged() { queueMicrotask(selectionChanged); }

  function reveal(event: Event) {
    const detail = (event as CustomEvent<MarkdownRevealDetail>).detail;
    if (!detail || typeof detail.query !== "string") return;
    detail.element = undefined;
    searchPins.clear();
    if (detail.query) {
      // Decode entities without ever inserting untrusted HTML into the document.
      const decoder = document.createElement("textarea");
      const texts = entries.map(entry => {
        if (entry.plain === undefined) {
          decoder.innerHTML = markdownTokenText(entry.chunk.tokens).replace(/</g, "&lt;");
          entry.plain = decoder.value;
        }
        return entry.plain;
      });
      const pattern = new RegExp(detail.query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "iu");
      const hit = pattern.exec(texts.join(""));
      if (hit) {
        let offset = 0, first = -1, last = -1;
        for (let i = 0; i < texts.length; i++) {
          if (first < 0 && offset + texts[i].length > hit.index) first = i;
          offset += texts[i].length;
          if (offset >= hit.index + hit[0].length) { last = i; break; }
        }
        if (first >= 0) {
          for (let i = Math.max(0, first - 1); i <= Math.min(entries.length - 1, last + 1); i++) searchPins.add(entries[i]);
          detail.element = entries[first].element;
        }
      }
    }
    refresh();
  }

  function observe() {
    const height = root?.clientHeight || window.innerHeight;
    // IO already tracks width/position changes; only its pixel margin needs a rebuild.
    if (observer && margin === height) return;
    margin = height;
    observer?.disconnect();
    observer = new IntersectionObserver(changes => {
      const changed: Entry[] = [];
      for (const change of changes) {
        const entry = byElement.get(change.target);
        if (!entry || entry.near === change.isIntersecting) continue;
        entry.near = change.isIntersecting;
        changed.push(entry);
      }
      if (changed.length) refresh(changed);
    }, { root, rootMargin: `${height * 2}px 0px` });
    entries.forEach(entry => observer!.observe(entry.element));
  }

  onSettled(() => {
    root = host.closest<HTMLElement>(".html-transcript");
    sizes = new ResizeObserver(changes => {
      // Scroll may precede RO (including a late image load). Capture its NEW
      // reading block against the OLD cached geometry before replacing the cache.
      if (root && Math.abs(root.scrollTop - anchorScrollTop) > 1) rememberAnchor();
      let changed = false;
      for (const change of changes) {
        const entry = byElement.get(change.target);
        if (!entry?.mounted) continue;
        measure(entry, change.borderBoxSize[0]?.blockSize);
        changed = true;
      }
      if (changed) { indexHeights(); restoreAnchor(); }
    });
    observe();
    sync();
    // First paint does not depend on IO delivery. Seed the local viewport using
    // placeholders; the transcript's initial bottom pass then triggers IO there.
    const top = (root?.getBoundingClientRect().top ?? 0) - host.getBoundingClientRect().top;
    const height = root?.clientHeight || window.innerHeight;
    for (const entry of entries) {
      entry.near = entry.offset + entry.height >= top - height * 2 && entry.offset <= top + height * 3;
    }
    refresh();
    ready = true;
    root?.addEventListener("scroll", scrolled, { passive: true });
    rootSize = new ResizeObserver(observe);
    rootSize.observe(root ?? document.documentElement);
    document.addEventListener("selectionchange", selectionChanged);
    host.addEventListener("focusout", focusChanged);
    host.addEventListener("markdown-reveal", reveal);
  });
  onCleanup(() => {
    clearTimeout(timer);
    cancelAnimationFrame(scrollFrame);
    observer?.disconnect(); sizes?.disconnect(); rootSize?.disconnect();
    document.removeEventListener("selectionchange", selectionChanged);
    root?.removeEventListener("scroll", scrolled);
    host?.removeEventListener("focusout", focusChanged);
    host?.removeEventListener("markdown-reveal", reveal);
  });
  return <div ref={host} class="md-virtual" data-markdown-virtual />;
}
