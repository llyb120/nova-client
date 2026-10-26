import { createEffect, onCleanup } from "solid-js";

const LONG_TEXT = 32_768;
const WINDOW = 4096;
const FRAME_BUDGET = 1; // Leave headroom within 8ms for the final non-preemptible native layout.
// Paragraph direction/embedding state cannot be restarted at an arbitrary soft wrap.
const BIDI = /[\u0590-\u08ff\u200e\u200f\u202a-\u202e\u2066-\u2069\ufb1d-\ufdff\ufe70-\ufeff\u{10800}-\u{10fff}\u{1e800}-\u{1eeff}]/u;

type Chunk = { end: number; height: number };

/** Native line breaking, not a second text engine. Each layout sees at most 4K UTF-16 units.
 * The caller advances this iterator under a frame deadline and can abandon it on resize/unmount.
 */
export function* measurePromptChunks(text: string, probe: HTMLElement): Generator<void, Chunk[]> {
  const chunks: Chunk[] = [];
  const range = document.createRange();
  let start = 0;
  while (start < text.length) {
    let end = Math.min(text.length, start + WINDOW);
    // Never present an incomplete surrogate to the shaper, including at the lookahead boundary.
    if (end < text.length && /[\uD800-\uDBFF]/.test(text[end - 1])) end--;
    const node = document.createTextNode(text.slice(start, end));
    probe.replaceChildren(node);
    yield;
    let cut = end - start;
    if (end < text.length) {
      // Keep 1K of lookahead: truncating the last word must not change our chosen line.
      const target = Math.min(WINDOW - 1024, node.length - 1);
      range.setStart(node, target); range.setEnd(node, target + 1);
      const line = range.getBoundingClientRect();
      yield;
      let lo = 0, hi = target;
      while (lo < hi) {
        const mid = (lo + hi) >>> 1;
        range.setStart(node, mid); range.setEnd(node, mid + 1);
        const rect = range.getBoundingClientRect();
        // Glyph fallback can have different ascenders on the SAME line; compare centers,
        // not tops. Half a glyph's height tolerates that without crossing a native line.
        if ((rect.top + rect.bottom) / 2 < line.top) lo = mid + 1;
        else hi = mid;
        yield;
      }
      cut = lo;
      // ponytail: a single >3K grapheme/unbreakable line has no bounded native cut.
      // Keep the paragraph native; a grapheme-aware shaping engine would be the upgrade.
      if (!cut) return [];
    }
    node.deleteData(cut, node.length - cut);
    yield;
    const height = probe.getBoundingClientRect().height;
    yield;
    start += cut;
    chunks.push({ end: start, height });
  }
  return chunks;
}

/** Drop-in replacement for the entire .ht-user-text span (including its chunk spans).
 * Short prompts stay synchronous. Long prompts commit atomically after bounded preparation;
 * aria-busy describes that interval, not a permanently truncated preview. No scroll writes:
 * HtmlTranscript's row ResizeObserver owns anchoring when the prepared height is committed.
 */
export function HtmlPromptText(props: { text: string }) {
  let host!: HTMLSpanElement;
  let frame = 0;
  let source = "";
  let rendered = "";
  let width = 0;
  let disposed = false;
  let probe: HTMLSpanElement | undefined;
  let observer: ResizeObserver | undefined;

  function cancel() {
    cancelAnimationFrame(frame);
    frame = 0;
    probe?.remove(); probe = undefined;
  }

  function commit(text: string, chunks: Chunk[]) {
    // Keep an in-progress native selection over a width/font rewrap (also backwards and
    // crossing the component boundary). Source changes intentionally do not retain it.
    const selection = getSelection();
    const offset = (node: Node | null, at: number) => {
      if (!node || !host.contains(node)) return null;
      const range = document.createRange();
      range.selectNodeContents(host); range.setEnd(node, at);
      return range.toString().length;
    };
    const anchor = rendered === text ? offset(selection?.anchorNode ?? null, selection?.anchorOffset ?? 0) : null;
    const focus = rendered === text ? offset(selection?.focusNode ?? null, selection?.focusOffset ?? 0) : null;
    const oldAnchor = selection?.anchorNode, oldFocus = selection?.focusNode;
    const oldA = selection?.anchorOffset ?? 0, oldF = selection?.focusOffset ?? 0;
    const fragment = document.createDocumentFragment();
    let start = 0;
    for (const chunk of chunks) {
      const span = document.createElement("span");
      // block inserts artificial newlines in Selection.toString()/native clipboard.
      // Full-width inline-blocks isolate layout without adding source characters.
      span.style.cssText = `display:inline-block;width:100%;vertical-align:top;content-visibility:${start ? "auto" : "visible"};contain-intrinsic-block-size:auto ${chunk.height}px`;
      span.textContent = text.slice(start, chunk.end);
      fragment.append(span);
      start = chunk.end;
    }
    if (!chunks.length) fragment.append(document.createTextNode(text));
    host.replaceChildren(fragment);
    rendered = text;
    host.removeAttribute("aria-busy");
    if (selection && oldAnchor && oldFocus && (anchor !== null || focus !== null)) {
      const point = (at: number | null, node: Node, index: number): [Node, number] => {
        if (at === null) return [node, index];
        const walker = document.createTreeWalker(host, NodeFilter.SHOW_TEXT);
        let child = walker.nextNode();
        while (child) {
          if (at <= child.textContent!.length) return [child, at];
          at -= child.textContent!.length;
          child = walker.nextNode();
        }
        return [host, host.childNodes.length];
      };
      selection.setBaseAndExtent(...point(anchor, oldAnchor, oldA), ...point(focus, oldFocus, oldF));
    }
  }

  function prepare() {
    cancel();
    if (disposed) return;
    const text = source;
    // ponytail: bidi embedding/paragraph context is deliberately native and unbounded.
    // Splitting those paragraphs requires a bidi-aware boundary engine, not a regex wrap.
    if (text.length <= LONG_TEXT || BIDI.test(text)) {
      commit(text, []);
      return;
    }
    host.setAttribute("aria-busy", "true");
    if (!rendered) {
      // A bounded, non-selectable sizing sample preserves shrink-to-fit bubble sizing.
      const sample = document.createElement("span");
      sample.style.cssText = "display:block;height:0;overflow:hidden;visibility:hidden;user-select:none";
      sample.setAttribute("aria-hidden", "true");
      sample.textContent = text.slice(0, WINDOW);
      host.replaceChildren(sample);
    }
    frame = requestAnimationFrame(() => {
      const style = getComputedStyle(host);
      width = host.getBoundingClientRect().width - parseFloat(style.paddingRight || "0") - parseFloat(style.paddingLeft || "0");
      if (width <= 0) return; // A later ResizeObserver delivery retries once shown.
      const measure = document.createElement("span");
      // Outside the transcript: never in its selection/search/height accounting.
      measure.style.cssText = "position:fixed;left:0;top:0;visibility:hidden;pointer-events:none;contain:layout style paint;display:block;white-space:pre-wrap;overflow-wrap:anywhere";
      for (const name of ["font", "font-kerning", "font-feature-settings", "font-variation-settings", "letter-spacing", "word-spacing", "line-height", "tab-size", "word-break", "line-break", "text-transform", "direction"]) {
        measure.style.setProperty(name, style.getPropertyValue(name));
      }
      // Blink rounds a resolved px line-height differently from the inherited unitless
      // value (1/64px per line). Preserve that ratio or skipped chunks accumulate drift.
      if (style.lineHeight !== "normal") measure.style.lineHeight = String(parseFloat(style.lineHeight) / parseFloat(style.fontSize));
      measure.style.width = `${width}px`;
      measure.setAttribute("aria-hidden", "true");
      document.body.append(measure);
      probe = measure;
      const steps = measurePromptChunks(text, measure);
      const advance = () => {
        const deadline = performance.now() + FRAME_BUDGET;
        do {
          const step = steps.next();
          if (step.done) {
            cancel();
            frame = requestAnimationFrame(() => commit(text, step.value));
            return;
          }
        } while (performance.now() < deadline);
        frame = requestAnimationFrame(advance);
      };
      frame = requestAnimationFrame(advance);
    });
  }

  createEffect(() => {
    source = props.text;
    prepare();
    if (!observer) {
      observer = new ResizeObserver(entries => {
        const next = entries[0].contentRect.width;
        if (!disposed && next > 0 && Math.abs(next - width) > 0.1) { width = next; prepare(); }
      });
      observer.observe(host);
    }
  });
  const fontsChanged = () => { if (!disposed) prepare(); };
  document.fonts.addEventListener("loadingdone", fontsChanged);
  onCleanup(() => {
    disposed = true; cancel(); observer?.disconnect();
    document.fonts.removeEventListener("loadingdone", fontsChanged);
  });
  return <span ref={host} class="ht-user-text" />;
}
