import { createEffect, onCleanup, onMount, untrack } from "solid-js";
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
  type Entry = { element: HTMLDivElement; chunk: MarkdownChunk; near: boolean; mounted: boolean; dirty: boolean; height: number; plain?: string };
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

  function rememberAnchor() {
    if (!root) return;
    anchorScrollTop = root.scrollTop;
    const top = root.getBoundingClientRect().top;
    const max = root.scrollHeight - root.clientHeight;
    bottom = max > 0 && max - root.scrollTop <= 2;
    let lo = 0, hi = entries.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (entries[mid].element.getBoundingClientRect().bottom <= top) lo = mid + 1;
      else hi = mid;
    }
    const entry = entries[lo];
    anchor = undefined;
    if (!entry || entry.element.getBoundingClientRect().top >= top + root.clientHeight) return;
    // Anchor the first visible native block, not a whole multi-paragraph chunk.
    const element = Array.from(entry.element.children).find(child => child.getBoundingClientRect().bottom > top) as HTMLElement | undefined;
    const target = element ?? entry.element;
    anchor = { element: target, offset: target.getBoundingClientRect().top - top };
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

  function refresh() { entries.forEach(paint); restoreAnchor(); }

  function selected(entry: Entry) {
    const selection = window.getSelection();
    if (!selection || selection.isCollapsed) return false;
    for (let i = 0; i < selection.rangeCount; i++) {
      if (selection.getRangeAt(i).intersectsNode(entry.element)) return true;
    }
    return false;
  }

  function paint(entry: Entry) {
    const pin = selected(entry);
    const wanted = entry.near || pin || searchPins.has(entry) || entry.element.contains(document.activeElement);
    if (wanted) {
      if (!entry.mounted || (entry.dirty && !pin && !entry.element.contains(document.activeElement))) {
        entry.element.innerHTML = props.render(entry.chunk, markFiles);
        entry.element.style.height = "";
        entry.mounted = true;
        entry.dirty = false;
        sizes?.observe(entry.element);
      }
    } else if (entry.mounted) {
      sizes?.unobserve(entry.element);
      entry.element.style.height = `${entry.height}px`;
      entry.element.replaceChildren();
      entry.mounted = false;
    }
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
        entry = { element, chunk, near: false, mounted: false, dirty: true, height };
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
    refresh();
  }

  // The shared Markdown stream animation can tick every frame. Only lex its
  // unfinished tail at most every 80ms; completion/edit flushes synchronously.
  createEffect(() => {
    const text = props.text, live = props.live;
    props.markFiles;
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
    observer?.disconnect();
    observer = new IntersectionObserver(changes => {
      // Read all outgoing sizes before DOM writes (including image/font reflow).
      for (const change of changes) {
        const entry = byElement.get(change.target);
        if (!entry) continue;
        if (entry.mounted) entry.height = entry.element.getBoundingClientRect().height;
        entry.near = change.isIntersecting;
      }
      refresh();
    }, { root, rootMargin: `${root?.clientHeight || window.innerHeight}px 0px` });
    entries.forEach(entry => observer!.observe(entry.element));
  }

  onMount(() => {
    root = host.closest<HTMLElement>(".html-transcript");
    sizes = new ResizeObserver(changes => {
      for (const change of changes) {
        const entry = byElement.get(change.target);
        if (entry?.mounted) entry.height = change.borderBoxSize[0]?.blockSize ?? change.contentRect.height;
      }
      restoreAnchor();
    });
    observe();
    sync();
    // First paint does not depend on IO delivery. Seed the local viewport using
    // placeholders; the transcript's initial bottom pass then triggers IO there.
    const viewport = root?.getBoundingClientRect();
    const top = viewport?.top ?? 0, height = root?.clientHeight || window.innerHeight;
    for (const entry of entries) {
      const rect = entry.element.getBoundingClientRect();
      entry.near = rect.bottom >= top - height && rect.top <= top + height * 2;
    }
    refresh();
    ready = true;
    root?.addEventListener("scroll", rememberAnchor, { passive: true });
    rootSize = new ResizeObserver(observe);
    rootSize.observe(root ?? document.documentElement);
    document.addEventListener("selectionchange", selectionChanged);
    host.addEventListener("focusout", focusChanged);
    host.addEventListener("markdown-reveal", reveal);
  });
  onCleanup(() => {
    clearTimeout(timer);
    observer?.disconnect(); sizes?.disconnect(); rootSize?.disconnect();
    document.removeEventListener("selectionchange", selectionChanged);
    root?.removeEventListener("scroll", rememberAnchor);
    host?.removeEventListener("focusout", focusChanged);
    host?.removeEventListener("markdown-reveal", reveal);
  });
  return <div ref={host} class="md-virtual" data-markdown-virtual />;
}
