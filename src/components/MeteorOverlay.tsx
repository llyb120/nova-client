import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { batch, createMemo, createSignal, Index, onCleanup, onMount, Show } from "solid-js";
import { METEOR_MAX_ANNOTATIONS, METEOR_MAX_TEXT, meteorMonitor, meteorRegion, meteorRequest, type MeteorAnnotation, type MeteorPoint, type MeteorRegion } from "../meteorSelection";
import "./MeteorOverlay.css";

interface MeteorContext {
  captureId: string;
  width: number;
  height: number;
  monitors?: MeteorRegion[];
  pointer?: MeteorPoint;
  clipboard?: boolean;
  /** Alt+V smart paste: one request box, no annotations. */
  paste?: boolean;
}

export default function MeteorOverlay() {
  const [context, setContext] = createSignal<MeteorContext | null>(null);
  const [ready, setReady] = createSignal(false);
  const [annotations, setAnnotations] = createSignal<MeteorAnnotation[]>([]);
  const [activeIndex, setActiveIndex] = createSignal<number | null>(null);
  const [draftRegion, setDraftRegion] = createSignal<MeteorRegion | null>(null);
  const [dragging, setDragging] = createSignal(false);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [cancelling, setCancelling] = createSignal(false);
  const [submitted, setSubmitted] = createSignal(false);
  const [viewport, setViewport] = createSignal({ width: window.innerWidth, height: window.innerHeight });
  const [panelSize, setPanelSize] = createSignal({ width: 420, height: 280 });
  const [pasteText, setPasteText] = createSignal("");
  let surface!: HTMLDivElement;
  let input: HTMLTextAreaElement | undefined;
  let pasteInput: HTMLTextAreaElement | undefined;
  let dragStart: MeteorPoint | null = null;
  let pointerId: number | null = null;
  let previousActiveIndex: number | null = null;
  let panelObserver: ResizeObserver | undefined;
  let unlistenOpen: UnlistenFn | undefined;
  let disposed = false;
  const locked = () => busy() || cancelling() || submitted();
  const active = () => annotations()[activeIndex() ?? -1];
  const paste = () => !!context()?.paste;
  const canAdd = () => ready() && !locked() && !paste() && annotations().length < METEOR_MAX_ANNOTATIONS;
  const request = createMemo(() => meteorRequest(context()?.captureId ?? "", annotations()));
  const textLength = () => annotations().reduce((sum, item) => sum + [...item.text.trim()].length, 0);
  const focusInput = () => queueMicrotask(() => input?.focus({ preventScroll: true }));

  const cancel = async () => {
    if (busy() || cancelling()) return;
    const captureId = context()?.captureId;
    setCancelling(true);
    try {
      await invoke("meteor_cancel", { captureId: captureId ?? null });
      if (context()?.captureId === captureId) setReady(false);
    } catch (cause) {
      if (context()?.captureId === captureId) setError(`关闭失败：${String(cause)}`);
    } finally {
      if (context()?.captureId === captureId) setCancelling(false);
    }
  };

  const submit = async () => {
    if (locked() || dragging() || !ready()) return;
    const payload = request();
    if (!payload) {
      setError(textLength() > METEOR_MAX_TEXT ? "所有标注说明合计不能超过 16000 字。" : "请至少添加一条标注并填写操作要求；其他标注可仅作参考。");
      input?.focus();
      return;
    }
    setBusy(true);
    setError("");
    try {
      await invoke<string>("meteor_submit", payload);
      if (context()?.captureId === payload.captureId) setSubmitted(true);
    } catch (cause) {
      if (context()?.captureId === payload.captureId) {
        setError(`未能启动任务：${String(cause)}。全部标注已保留，请检查后重试。`);
        focusInput();
      }
    } finally {
      if (context()?.captureId === payload.captureId) setBusy(false);
    }
  };

  const submitPaste = async () => {
    const captureId = context()?.captureId;
    if (!captureId || locked() || !ready()) return;
    setBusy(true);
    setError("");
    try {
      await invoke<string>("meteor_paste", { captureId, text: pasteText() });
      if (context()?.captureId === captureId) setSubmitted(true);
    } catch (cause) {
      if (context()?.captureId === captureId) {
        setError(`未能启动星移：${String(cause)}`);
        queueMicrotask(() => pasteInput?.focus({ preventScroll: true }));
      }
    } finally {
      if (context()?.captureId === captureId) setBusy(false);
    }
  };

  const continueAnnotating = () => {
    if (!canAdd()) return;
    setActiveIndex(null);
    setError("");
    surface.focus();
  };
  const selectAnnotation = (index: number) => {
    if (locked() || dragging()) return;
    setActiveIndex(index);
    focusInput();
  };
  const addAnnotation = (region: MeteorRegion) => {
    if (!canAdd()) return;
    const index = annotations().length;
    setAnnotations((items) => [...items, { text: "", region }]);
    setActiveIndex(index);
    setError("");
    focusInput();
  };
  const deleteAnnotation = () => {
    const index = activeIndex();
    if (locked() || index === null) return;
    setAnnotations((items) => items.filter((_, i) => i !== index));
    setActiveIndex(annotations().length ? Math.min(index, annotations().length - 1) : null);
    setError("");
    if (active()) focusInput(); else surface.focus();
  };
  const selectCenter = () => {
    const image = context();
    if (!image || !canAdd()) return;
    const monitor = meteorMonitor(image);
    addAnnotation({ x: Math.floor(monitor.x + monitor.width / 2), y: Math.floor(monitor.y + monitor.height / 2), width: 0, height: 0 });
  };

  const selectionAt = (event: PointerEvent) => {
    const image = context();
    return image && dragStart
      ? meteorRegion(dragStart, { x: event.clientX, y: event.clientY }, surface.getBoundingClientRect(), image)
      : null;
  };

  const pointerDown = (event: PointerEvent) => {
    if (paste()) { event.preventDefault(); void cancel(); return; } // Clicking outside the paste box dismisses it.
    if (event.button !== 0 || !event.isPrimary || !canAdd() || dragging()) return;
    event.preventDefault();
    previousActiveIndex = activeIndex();
    setActiveIndex(null);
    dragStart = { x: event.clientX, y: event.clientY };
    pointerId = event.pointerId;
    surface.setPointerCapture(event.pointerId);
    setDragging(true);
    setDraftRegion(selectionAt(event));
  };

  const pointerMove = (event: PointerEvent) => {
    if (event.pointerId === pointerId && dragging()) setDraftRegion(selectionAt(event));
  };

  const pointerUp = (event: PointerEvent) => {
    if (event.pointerId !== pointerId || !dragging()) return;
    const region = selectionAt(event);
    dragStart = null;
    pointerId = null;
    setDraftRegion(null);
    setDragging(false);
    if (surface.hasPointerCapture(event.pointerId)) surface.releasePointerCapture(event.pointerId);
    if (region) addAnnotation(region); else setActiveIndex(previousActiveIndex);
    previousActiveIndex = null;
  };

  const abortSelection = () => {
    if (!dragging()) return;
    const captured = pointerId;
    dragStart = null;
    pointerId = null;
    setDraftRegion(null);
    setDragging(false);
    setActiveIndex(previousActiveIndex);
    previousActiveIndex = null;
    if (captured !== null && surface.hasPointerCapture(captured)) surface.releasePointerCapture(captured);
    focusInput();
  };

  const acceptContext = (value: MeteorContext | null) => {
    if (disposed || !value || value.captureId === context()?.captureId) return;
    if (!value.captureId || !Number.isInteger(value.width) || !Number.isInteger(value.height) || value.width <= 0 || value.height <= 0) {
      setError("屏幕布局无效，请关闭后重新触发 Meteor。");
      return;
    }
    abortSelection();
    panelObserver?.disconnect();
    batch(() => {
      setReady(false);
      setContext(value);
      setAnnotations([]);
      setActiveIndex(null);
      setDraftRegion(null);
      setDragging(false);
      setError("");
      setBusy(false);
      setCancelling(false);
      setSubmitted(false);
      setPasteText("");
      setViewport({ width: window.innerWidth, height: window.innerHeight });
      setReady(true);
    });
    if (value.paste) queueMicrotask(() => pasteInput?.focus({ preventScroll: true }));
    else surface.focus({ preventScroll: true });
  };

  const regionStyle = (selected: MeteorRegion) => {
    const image = context();
    if (!image) return {};
    return {
      left: `${selected.x / image.width * 100}%`,
      top: `${selected.y / image.height * 100}%`,
      width: `${selected.width / image.width * 100}%`,
      height: `${selected.height / image.height * 100}%`,
    };
  };
  const screenRect = (region?: MeteorRegion) => {
    const image = context();
    const view = viewport();
    if (!image) return { x: 0, y: 0, ...view };
    const monitor = meteorMonitor(image, region);
    return { x: monitor.x / image.width * view.width, y: monitor.y / image.height * view.height,
      width: monitor.width / image.width * view.width, height: monitor.height / image.height * view.height };
  };
  const toolbarStyle = createMemo(() => {
    const screen = screenRect();
    return { left: `${screen.x + screen.width / 2}px`, top: `${screen.y + 12}px`, "max-width": `${screen.width - 24}px` };
  });
  const helpStyle = createMemo(() => {
    const screen = screenRect();
    return { left: `${screen.x + screen.width / 2}px`, top: `${screen.y + screen.height - 12}px`, bottom: "auto", transform: "translate(-50%, -100%)", "max-width": `${screen.width - 24}px` };
  });
  const listStyle = createMemo(() => {
    const screen = screenRect(active()?.region);
    return { left: `${screen.x + 12}px`, top: `${screen.y + 72}px`, width: `${screen.width - 24}px` };
  });
  const panelStyle = createMemo(() => {
    const selected = active()?.region;
    const image = context();
    if (!selected || !image) return {};
    const view = viewport();
    const size = panelSize();
    const screen = screenRect(selected);
    const width = Math.min(420, screen.width - 24);
    return {
      width: `${width}px`, "max-height": `${Math.max(40, screen.height - 226)}px`,
      left: `${Math.max(screen.x + 12, Math.min((selected.x + selected.width) / image.width * view.width + 12, screen.x + screen.width - width - 12))}px`,
      top: `${Math.max(screen.y + 148, Math.min((selected.y + selected.height) / image.height * view.height + 12, screen.y + screen.height - size.height - 66))}px`,
    };
  });

  const pasteStyle = createMemo(() => {
    const image = context();
    const screen = screenRect();
    const view = viewport();
    const width = Math.min(420, screen.width - 24);
    const x = image?.pointer ? image.pointer.x / image.width * view.width : screen.x + (screen.width - width) / 2;
    const y = image?.pointer ? image.pointer.y / image.height * view.height : screen.y + screen.height / 3;
    const clamp = (value: number, min: number, max: number) => Math.max(min, Math.min(value, max));
    return {
      width: `${width}px`,
      left: `${clamp(x + 12, screen.x + 12, screen.x + screen.width - width - 12)}px`,
      top: `${clamp(y + 16, screen.y + 12, screen.y + screen.height - panelSize().height - 12)}px`,
    };
  });

  const attachPanel = (element: HTMLElement) => {
    panelObserver?.disconnect();
    panelObserver = new ResizeObserver(() => setPanelSize({ width: element.offsetWidth, height: element.offsetHeight }));
    panelObserver.observe(element);
    queueMicrotask(() => input?.focus({ preventScroll: true }));
  };

  onMount(() => {
    const resize = () => setViewport({ width: window.innerWidth, height: window.innerHeight });
    const keydown = (event: KeyboardEvent) => {
      if (event.isComposing) return;
      if (event.key === "Escape") {
        event.preventDefault();
        void cancel();
      } else if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
        event.preventDefault();
        void (paste() ? submitPaste() : submit());
      } else if ((event.key === "Enter" || event.key === " ") && event.target === surface && activeIndex() === null) {
        event.preventDefault();
        selectCenter();
      }
    };
    window.addEventListener("resize", resize);
    window.addEventListener("keydown", keydown);
    onCleanup(() => {
      disposed = true;
      panelObserver?.disconnect();
      unlistenOpen?.();
      window.removeEventListener("resize", resize);
      window.removeEventListener("keydown", keydown);
    });
    void (async () => {
      let receivedOpen = false;
      const unlisten = await listen<MeteorContext>("meteor:open", (event) => {
        receivedOpen = true;
        acceptContext(event.payload);
      });
      if (disposed) { unlisten(); return; }
      unlistenOpen = unlisten;
      const current = await invoke<MeteorContext | null>("meteor_context");
      // A queued fallback (including prewarm null) must not replace a newer open event.
      if (!receivedOpen) acceptContext(current);
    })().catch((cause) => {
      if (!disposed && !ready()) setError(`无法读取屏幕布局：${String(cause)}`);
    });
  });

  return (
    <main class="meteor-overlay" aria-label="Meteor 屏幕标注" aria-busy={busy()}>
      <div
        ref={surface}
        class="meteor-surface"
        classList={{ "meteor-surface--selectable": canAdd() }}
        role="button"
        tabIndex={0}
        aria-label="单击添加操作要求，拖动框选屏幕区域，或按 Enter 在屏幕中央添加要求"
        aria-disabled={!ready() || locked()}
        onPointerDown={pointerDown}
        onPointerMove={pointerMove}
        onPointerUp={pointerUp}
        onPointerCancel={abortSelection}
        onLostPointerCapture={abortSelection}
        onContextMenu={(event) => event.preventDefault()}
      >
        <Index each={annotations()}>{(item, index) => (
          <div class={item().region.width === 0 ? "meteor-point" : "meteor-region"}
            classList={{ "meteor-mark--active": activeIndex() === index }} style={regionStyle(item().region)} aria-hidden="true" />
        )}</Index>
        <Show when={draftRegion()}>{(region) => (
          <div class={region().width === 0 ? "meteor-point" : "meteor-region"} style={regionStyle(region())} aria-hidden="true" />
        )}</Show>
      </div>
      <Index each={annotations()}>{(item, index) => (
        <button type="button" class="meteor-marker" classList={{ "meteor-marker--active": activeIndex() === index }}
          style={{ left: `clamp(4px, ${item().region.x / (context()?.width ?? 1) * 100}%, calc(100% - 30px))`, top: `clamp(4px, ${item().region.y / (context()?.height ?? 1) * 100}%, calc(100% - 30px))` }}
          aria-label={`选择屏幕标注 ${index + 1}`} disabled={locked() || dragging()} onClick={() => selectAnnotation(index)}>{index + 1}</button>
      )}</Index>

      <Show when={paste() && ready()}>
        <form class="meteor-note" ref={attachPanel} style={pasteStyle()} onSubmit={(event) => { event.preventDefault(); void submitPaste(); }}>
          <label for="meteor-paste">星移 · 怎么处理剪贴板内容？</label>
          <textarea
            ref={pasteInput}
            id="meteor-paste"
            value={pasteText()}
            onInput={(event) => setPasteText(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.shiftKey && !event.ctrlKey && !event.metaKey && !event.isComposing) {
                event.preventDefault();
                void submitPaste();
              }
            }}
            placeholder="留空则根据剪贴板和当前输入框自动推断。例如：翻译成英文 / 只粘贴手机号 / 在浏览器里搜索它"
            rows={3}
            maxLength={METEOR_MAX_TEXT}
            disabled={locked()}
          />
          <Show when={error()}><p class="meteor-error" role="alert">{error()}</p></Show>
          <div class="meteor-note-actions">
            <span>Enter 执行 · Shift+Enter 换行 · Esc 取消</span>
            <button type="submit" class="meteor-submit" disabled={locked()}>{busy() ? "启动中…" : submitted() ? "已启动" : "执行"}</button>
          </div>
        </form>
      </Show>

      <Show when={!paste()}>
      <header class="meteor-toolbar" style={toolbarStyle()}>
        <strong>Meteor</strong>
        <span class="meteor-status" role="status">{submitted() ? "任务已在后台启动" : busy() ? "正在启动桌面 / 浏览器操作…" : ready() ? `${annotations().length} / 20 条标注` : error() ? "屏幕布局不可用" : "等待屏幕标注…"}</span>
        <button type="button" onClick={continueAnnotating} disabled={!canAdd() || dragging()}>继续标注</button>
        <Show when={activeIndex() === null}><button type="button" onClick={selectCenter} disabled={!canAdd() || dragging()}>直接输入</button></Show>
        <Show when={annotations().length > 0 && activeIndex() === null}><button type="button" onClick={() => void submit()} disabled={locked() || dragging() || !request()}>执行全部</button></Show>
        <button type="button" onClick={() => void cancel()} disabled={busy() || cancelling()} aria-label="关闭 Meteor（Esc）">{cancelling() ? "关闭中…" : "取消 · Esc"}</button>
      </header>
      <Show when={annotations().length > 0}>
        <nav class="meteor-annotations" style={listStyle()} aria-label="全部标注">
          <Index each={annotations()}>{(item, index) => (
            <button type="button" aria-label={`编辑标注 ${index + 1}`} aria-pressed={activeIndex() === index}
              disabled={locked() || dragging()} onClick={() => selectAnnotation(index)} title={item().text || "参考区域"}>
              <b>{index + 1}</b><span>{item().text.trim() || "参考区域"}</span>
            </button>
          )}</Index>
        </nav>
      </Show>

      <Show when={active() && !dragging() && ready()}>
        <form class="meteor-note" ref={attachPanel} style={panelStyle()} onSubmit={(event) => { event.preventDefault(); void submit(); }}>
          <div class="meteor-note-title"><label for="meteor-request">标注 {(activeIndex() ?? 0) + 1} · 需要 Nova 帮你做什么？</label>
            <button type="button" onClick={deleteAnnotation} disabled={locked()} aria-label="删除此标注">删除</button></div>
          <textarea
            ref={input}
            id="meteor-request"
            value={active()?.text ?? ""}
            onInput={(event) => { const text = event.currentTarget.value; setAnnotations((items) => items.map((item, index) => index === activeIndex() ? { ...item, text } : item)); }}
            placeholder="例如：帮我填写这份表格，并检查有没有遗漏"
            rows={4}
            disabled={locked()}
            aria-describedby="meteor-reference"
          />
          <p id="meteor-reference">实时标注仅指示大致范围；提交时截取并保留所有屏幕的完整截图，连同全部标注{context()?.clipboard ? "与最近剪贴板" : ""}一次提交。未填说明的标注仅作参考。</p>
          <p classList={{ "meteor-error": textLength() > METEOR_MAX_TEXT }}>{textLength()} / 16000 字 · 最多 20 条标注</p>
          <Show when={error()}><p class="meteor-error" role="alert">{error()}</p></Show>
          <div class="meteor-note-actions">
            <span>{active()?.region.width === 0 ? "已标注位置" : `已框选 ${active()?.region.width} × ${active()?.region.height} px`}</span>
            <button type="submit" class="meteor-submit" disabled={locked() || !request()}>{busy() ? "启动中…" : submitted() ? "已启动" : "执行全部 · Ctrl+Enter"}</button>
          </div>
        </form>
      </Show>
      </Show>

      <Show when={error() && (paste() ? !ready() : !active() || !ready())}>
        <div class="meteor-load-error" style={{ ...toolbarStyle(), top: `${screenRect().y + screenRect().height / 2}px` }} role="alert">{error()}<button type="button" onClick={() => void cancel()} disabled={cancelling()}>关闭</button></div>
      </Show>
      <Show when={!paste()}>
        <footer class="meteor-help" style={helpStyle()}>直接单击 / 拖动空白处继续标注 · Esc 取消 · Ctrl+Enter 执行全部<span>实时标注 · 提交时保留所有屏幕截图 · 位置仅指示大致范围</span></footer>
      </Show>
    </main>
  );
}
