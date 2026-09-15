import { createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { basicSetup } from "codemirror";
import { Compartment, EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { LanguageDescription, syntaxHighlighting, HighlightStyle } from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import { tags } from "@lezer/highlight";
import { workspaceLayout } from "../workspaceLayout";

const colors = HighlightStyle.define([
  { tag: tags.keyword, color: "var(--code-keyword)" },
  { tag: [tags.string, tags.regexp], color: "var(--code-string)" },
  { tag: [tags.number, tags.bool, tags.null], color: "var(--code-number)" },
  { tag: tags.comment, color: "var(--text-muted)", fontStyle: "italic" },
  { tag: [tags.typeName, tags.className, tags.tagName], color: "var(--code-type)" },
  { tag: [tags.function(tags.variableName), tags.attributeName], color: "var(--code-function)" },
]);

export default function WorkspaceCode(props: {
  path: string; text: string; readOnly: boolean;
  onChange: (text: string) => void; onView: (view: EditorView | undefined) => void;
}) {
  let host!: HTMLDivElement;
  let map!: HTMLDivElement;
  let canvas!: HTMLCanvasElement;
  let overlay!: HTMLDivElement;
  let view: EditorView | undefined;
  let disposed = false;
  let frame = 0;
  let repaint = true;
  const [overflows, setOverflows] = createSignal(false);
  const scheduleMap = (draw = false) => {
    repaint ||= draw;
    if (frame) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      if (!view || disposed) return;
      setOverflows(view.scrollDOM.scrollHeight > view.scrollDOM.clientHeight + 1);
      const height = map.clientHeight;
      const width = map.clientWidth;
      if (!height || !width) return;
      const doc = view.state.doc;
      if (repaint) {
        repaint = false;
        const ratio = window.devicePixelRatio || 1;
        canvas.width = Math.round(width * ratio);
        canvas.height = Math.round(height * ratio);
        const ctx = canvas.getContext('2d');
        if (ctx) {
          ctx.scale(ratio, ratio);
          ctx.fillStyle = getComputedStyle(map).color;
          ctx.globalAlpha = .75;
          // ponytail: 每 4px 采样一行，保留行间留白；需逐行细节时可升级为局部放大预览。
          const rows = Math.min(doc.lines, Math.max(1, Math.floor(height / 4)));
          const rowHeight = height / rows;
          const strokeHeight = Math.min(1.5, rowHeight * .45);
          for (let row = 0; row < rows; row++) {
            const text = doc.line(Math.min(doc.lines, Math.floor((row + .5) * doc.lines / rows) + 1)).text.replace(/\t/g, '    ');
            for (const match of text.slice(0, 120).matchAll(/\S+/g)) {
              ctx.fillRect(4 + match.index * (width - 8) / 120, row * rowHeight + (rowHeight - strokeHeight) / 2, match[0].length * (width - 8) / 120, strokeHeight);
            }
          }
        }
      }
      const bounds = view.scrollDOM.getBoundingClientRect();
      const mapY = (y: number) => {
        const block = view!.lineBlockAtHeight(Math.max(0, y));
        const line = doc.lineAt(block.from).number - 1;
        return Math.max(0, Math.min(height, (line + Math.max(0, Math.min(1, (y - block.top) / block.height))) * height / doc.lines));
      };
      const top = mapY(bounds.top - view.documentTop);
      const bottom = mapY(bounds.bottom - view.documentTop);
      overlay.style.top = `${top}px`;
      overlay.style.height = `${Math.max(2, bottom - top)}px`;
      map.setAttribute('aria-valuemax', String(doc.lines));
      map.setAttribute('aria-valuenow', String(Math.min(doc.lines, Math.floor((top + bottom) / 2 / height * doc.lines) + 1)));
    });
  };
  const jump = (line: number) => {
    if (!view) return;
    const target = view.state.doc.line(Math.max(1, Math.min(view.state.doc.lines, Math.floor(line))));
    view.dispatch({ selection: { anchor: target.from }, effects: EditorView.scrollIntoView(target.from, { y: 'center' }) });
  };
  const jumpAtPointer = (event: PointerEvent) => {
    const bounds = map.getBoundingClientRect();
    if (view && bounds.height) jump((event.clientY - bounds.top) / bounds.height * view.state.doc.lines + 1);
  };
  const access = new Compartment();
  const language = new Compartment();
  const wrapping = new Compartment();
  onMount(() => {
    view = new EditorView({
      parent: host,
      state: EditorState.create({ doc: props.text, extensions: [
        basicSetup, wrapping.of(workspaceLayout.softWrap ? EditorView.lineWrapping : []), syntaxHighlighting(colors), language.of([]),
        access.of(EditorState.readOnly.of(props.readOnly)),
        EditorView.contentAttributes.of({ "aria-label": "文件内容编辑", spellcheck: "false" }),
        EditorView.updateListener.of(update => {
          if (update.docChanged) props.onChange(update.state.doc.toString());
          scheduleMap(update.docChanged || update.geometryChanged);
        }),
      ] }),
    });
    props.onView(view);
    const scroll = () => scheduleMap();
    view.scrollDOM.addEventListener('scroll', scroll, { passive: true });
    const resize = new ResizeObserver(() => scheduleMap(true));
    resize.observe(map);
    const theme = new MutationObserver(() => scheduleMap(true));
    theme.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
    scheduleMap(true);
    onCleanup(() => { resize.disconnect(); theme.disconnect(); view?.scrollDOM.removeEventListener('scroll', scroll); });
    const support = LanguageDescription.matchFilename(languages, props.path.split(/[\\/]/).pop() ?? props.path);
    if (support) void support.load().then(extension => {
      if (!disposed) view?.dispatch({ effects: language.reconfigure(extension) });
    }).catch(error => console.warn("文件语法加载失败", error));
  });
  createEffect(() => {
    const readOnly = props.readOnly;
    view?.dispatch({ effects: access.reconfigure(EditorState.readOnly.of(readOnly)) });
  });
  createEffect(() => {
    const wrap = workspaceLayout.softWrap;
    view?.dispatch({ effects: wrapping.reconfigure(wrap ? EditorView.lineWrapping : []) });
  });
  onCleanup(() => { disposed = true; cancelAnimationFrame(frame); props.onView(undefined); view?.destroy(); });
  return <div class="workspace-code" classList={{ 'workspace-code-nowrap': !workspaceLayout.softWrap }}>
    <div ref={host} class="workspace-code-editor" />
    <div ref={map} class="workspace-minimap" style={{ display: workspaceLayout.minimap && overflows() ? undefined : 'none' }} role="slider" aria-label="代码缩略图" aria-orientation="vertical" aria-valuemin="1" aria-valuemax="1" aria-valuenow="1" tabindex="0"
      onPointerDown={event => { event.preventDefault(); map.focus(); map.setPointerCapture(event.pointerId); jumpAtPointer(event); }}
      onPointerMove={event => { if (map.hasPointerCapture(event.pointerId)) jumpAtPointer(event); }}
      onKeyDown={event => {
        const line = Number(map.getAttribute('aria-valuenow'));
        const target = event.key === 'Home' ? 1 : event.key === 'End' ? view?.state.doc.lines : event.key === 'ArrowUp' ? line - 1 : event.key === 'ArrowDown' ? line + 1 : undefined;
        if (target !== undefined) { event.preventDefault(); jump(target); }
      }}>
      <canvas ref={canvas} aria-hidden="true" /><div ref={overlay} class="workspace-minimap-viewport" />
    </div>
  </div>;
}
