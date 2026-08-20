import { createSignal, onCleanup, onMount, For, Show } from "solid-js";
import {
  type BrowserAction,
  analyzeScreenshot,
  browserInfo,
  captureRegion,
  closeBrowser,
  compilePlan,
  liveEvents,
  openBrowser,
  pauseRecording,
  refreshInfo,
  resumeRecording,
  runPlanWithAgent,
  saveShot,
  startRecording,
  stopRecording,
  subscribeBrowser,
  type ClipMark,
  type PlayPlan,
  type PlanRunResult,
  type RecordEvent,
  type RecordedClip,
} from "../browser";
import { enabledAgentKinds, ensureModelOptions, lastUsed, state } from "../store";
import type { AgentKind } from "../types";
import { ModelPicker } from "./ConfigSelects";
import { IconGlobe, IconPlus, IconStop, IconTrash, IconPencil, IconCheck } from "./icons";

const STORAGE_KEY = "nova.browser.clips";

function loadClips(): RecordedClip[] {
  try {
    return JSON.parse(localStorage.getItem(STORAGE_KEY) || "[]");
  } catch {
    return [];
  }
}
function saveClips(clips: RecordedClip[]) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(clips));
}

export default function BrowserView() {
  const [url, setUrl] = createSignal("https://www.bing.com");
  const [clips, setClips] = createSignal<RecordedClip[]>(loadClips());
  const [activeClipId, setActiveClipId] = createSignal<string | null>(null);
  const [browserOn, setBrowserOn] = createSignal(false);
  const [opening, setOpening] = createSignal(false);
  const [analyzing, setAnalyzing] = createSignal(false);
  const [running, setRunning] = createSignal(false);
  const [runResult, setRunResult] = createSignal<PlanRunResult | null>(null);
  // 编辑中的事件（步骤）
  const [editingEventId, setEditingEventId] = createSignal<number | null>(null);
  const [editSelector, setEditSelector] = createSignal("");
  const [editValue, setEditValue] = createSignal("");
  // 截图标记的备注输入
  const [pendingMark, setPendingMark] = createSignal<{ dataUrl: string; path: string } | null>(null);
  const [markNote, setMarkNote] = createSignal("");
  const [newAction, setNewAction] = createSignal<BrowserAction>("click");
  const [notice, setNotice] = createSignal<{ text: string; kind: "info" | "error" } | null>(null);
  const [draggingStepId, setDraggingStepId] = createSignal<number | null>(null);
  const [dragOverStepId, setDragOverStepId] = createSignal<number | null>(null);

  let unsubs: (() => void)[] = [];
  let noticeTimer: ReturnType<typeof setTimeout> | undefined;
  let pointerDrag: { draggedId: number; targetId: number; placeAfter: boolean; pointerId: number } | null = null;

  function notify(text: string, kind: "info" | "error" = "info", persistent = false) {
    if (noticeTimer) clearTimeout(noticeTimer);
    setNotice({ text, kind });
    if (!persistent) noticeTimer = setTimeout(() => setNotice(null), 5000);
  }

  const recording = () => browserInfo()?.recording ?? false;
  const paused = () => browserInfo()?.paused ?? false;
  const activeClip = () => clips().find((c) => c.id === activeClipId()) ?? null;
  const currentClip = () => activeClip()!;

  async function open() {
    if (opening()) return;
    setOpening(true);
    try {
      await openBrowser();
      setBrowserOn(true);
      void refreshInfo();
    } catch (e) {
      notify(`打开浏览器失败：${e}`, "error");
    } finally {
      setOpening(false);
    }
  }

  async function close() {
    try {
      await closeBrowser();
    } catch { /* ignore */ }
    setBrowserOn(false);
  }

  function updateClip(id: string, updater: (clip: RecordedClip) => RecordedClip) {
    const next = clips().map((clip) => (clip.id === id ? updater(clip) : clip));
    setClips(next);
    saveClips(next);
  }

  function addManualStep() {
    const clip = activeClip();
    if (!clip) return;
    const action = newAction();
    const event = {
      id: Date.now(),
      ts: Date.now(),
      url: clip.startUrl,
      kind: action,
      target: { selector: "" },
      data: action === "record"
        ? { recordContent: "", outputName: `记录${clip.events.filter((item) => item.kind === "record").length + 1}`, imagePaths: [] }
        : {},
    };
    updateClip(clip.id, (value) => ({ ...value, events: [...value.events, event] }));
  }

  function updateStep(evId: number, patch: Partial<RecordEvent>) {
    const clip = activeClip();
    if (!clip) return;
    updateClip(clip.id, (value) => ({
      ...value,
      events: value.events.map((event) => event.id === evId ? { ...event, ...patch } : event),
    }));
  }

  function moveStep(evId: number, delta: number) {
    const clip = activeClip();
    if (!clip) return;
    updateClip(clip.id, (value) => {
      const events = [...value.events];
      const from = events.findIndex((event) => event.id === evId);
      const to = from + delta;
      if (from < 0 || to < 0 || to >= events.length) return value;
      [events[from], events[to]] = [events[to], events[from]];
      return { ...value, events };
    });
  }

  function reorderStep(draggedId: number, targetId: number, placeAfter: boolean) {
    const clip = activeClip();
    if (!clip || draggedId === targetId) return;
    updateClip(clip.id, (value) => {
      const events = [...value.events];
      const from = events.findIndex((event) => event.id === draggedId);
      const target = events.findIndex((event) => event.id === targetId);
      if (from < 0 || target < 0) return value;
      const [dragged] = events.splice(from, 1);
      let insertion = events.findIndex((event) => event.id === targetId);
      if (placeAfter) insertion += 1;
      events.splice(insertion, 0, dragged);
      return { ...value, events };
    });
  }

  function finishStepDrag(commit = false) {
    const drag = pointerDrag;
    pointerDrag = null;
    if (commit && drag && drag.draggedId !== drag.targetId) {
      reorderStep(drag.draggedId, drag.targetId, drag.placeAfter);
    }
    setDraggingStepId(null);
    setDragOverStepId(null);
    document.body.classList.remove("be-step-dragging");
  }

  function updatePointerDrag(clientX: number, clientY: number) {
    if (!pointerDrag) return;
    const element = document.elementFromPoint(clientX, clientY)?.closest<HTMLElement>(".be-step[data-step-id]");
    const targetId = Number(element?.dataset.stepId);
    if (!element || !Number.isFinite(targetId) || targetId === pointerDrag.draggedId) {
      pointerDrag.targetId = pointerDrag.draggedId;
      setDragOverStepId(null);
      return;
    }
    const rect = element.getBoundingClientRect();
    pointerDrag.targetId = targetId;
    pointerDrag.placeAfter = clientY > rect.top + rect.height / 2;
    setDragOverStepId(targetId);
  }

  function startStepDrag(event: PointerEvent, stepId: number) {
    if (event.button !== 0) return;
    event.preventDefault();
    pointerDrag = { draggedId: stepId, targetId: stepId, placeAfter: false, pointerId: event.pointerId };
    setDraggingStepId(stepId);
    document.body.classList.add("be-step-dragging");

    const onMove = (moveEvent: PointerEvent) => {
      if (!pointerDrag || moveEvent.pointerId !== pointerDrag.pointerId) return;
      moveEvent.preventDefault();
      updatePointerDrag(moveEvent.clientX, moveEvent.clientY);
      const steps = document.querySelector<HTMLElement>(".be-steps");
      if (steps) {
        const rect = steps.getBoundingClientRect();
        if (moveEvent.clientY < rect.top + 36) steps.scrollTop -= 14;
        else if (moveEvent.clientY > rect.bottom - 36) steps.scrollTop += 14;
      }
    };
    const onUp = (upEvent: PointerEvent) => {
      if (!pointerDrag || upEvent.pointerId !== pointerDrag.pointerId) return;
      document.removeEventListener("pointermove", onMove);
      document.removeEventListener("pointerup", onUp);
      document.removeEventListener("pointercancel", onCancel);
      finishStepDrag(true);
    };
    const onCancel = () => {
      document.removeEventListener("pointermove", onMove);
      document.removeEventListener("pointerup", onUp);
      document.removeEventListener("pointercancel", onCancel);
      finishStepDrag(false);
    };
    document.addEventListener("pointermove", onMove, { passive: false });
    document.addEventListener("pointerup", onUp);
    document.addEventListener("pointercancel", onCancel);
  }

  function setAnalysisPrompt(text: string) {
    const clip = activeClip();
    if (clip) updateClip(clip.id, (value) => ({ ...value, analysisPrompt: text }));
  }

  function setAnalysisRecordRef(name: string, checked: boolean) {
    const clip = activeClip();
    if (!clip) return;
    updateClip(clip.id, (value) => {
      const current = value.analysisRecordRefs || [];
      return {
        ...value,
        analysisRecordRefs: checked
          ? [...new Set([...current, name])]
          : current.filter((item) => item !== name),
      };
    });
  }

  async function pasteTargetImages(eventId: number, event: ClipboardEvent) {
    const files = Array.from(event.clipboardData?.files || []).filter((file) => file.type.startsWith("image/"));
    if (!files.length) return;
    event.preventDefault();
    const paths: string[] = [];
    for (const file of files) {
      const dataUrl = await new Promise<string>((resolve, reject) => {
        const reader = new FileReader();
        reader.onload = () => resolve(String(reader.result));
        reader.onerror = () => reject(reader.error);
        reader.readAsDataURL(file);
      });
      paths.push(await saveShot(dataUrl, `target-${Date.now()}-${paths.length}`));
    }
    const current = activeClip()?.events.find((item) => item.id === eventId);
    updateStep(eventId, { target: { ...current?.target, imagePaths: [...(current?.target?.imagePaths || []), ...paths] } });
  }

  function removeTargetImage(eventId: number, path: string) {
    const current = activeClip()?.events.find((item) => item.id === eventId);
    updateStep(eventId, { target: { ...current?.target, imagePaths: (current?.target?.imagePaths || []).filter((item) => item !== path) } });
  }

  async function pasteRecordImages(eventId: number, event: ClipboardEvent) {
    const files = Array.from(event.clipboardData?.files || []).filter((file) => file.type.startsWith("image/"));
    if (!files.length) return;
    event.preventDefault();
    const paths: string[] = [];
    for (const file of files) {
      const dataUrl = await new Promise<string>((resolve, reject) => {
        const reader = new FileReader();
        reader.onload = () => resolve(String(reader.result));
        reader.onerror = () => reject(reader.error);
        reader.readAsDataURL(file);
      });
      paths.push(await saveShot(dataUrl, `record-${Date.now()}-${paths.length}`));
    }
    const clip = activeClip();
    const current = clip?.events.find((item) => item.id === eventId);
    updateStep(eventId, { data: { ...current?.data, imagePaths: [...(current?.data?.imagePaths || []), ...paths] } });
  }

  function removeRecordImage(eventId: number, path: string) {
    const clip = activeClip();
    const current = clip?.events.find((item) => item.id === eventId);
    updateStep(eventId, { data: { ...current?.data, imagePaths: (current?.data?.imagePaths || []).filter((item) => item !== path) } });
  }

  // ---------- 录制 ----------
  async function newClip() {
    if (recording()) {
      const events = await stopRecording();
      const info = browserInfo();
      const clip: RecordedClip = {
        id: crypto.randomUUID(),
        name: `片段 ${clips().length + 1}`,
        createdAt: Date.now(),
        startUrl: info?.url || url(),
        events,
        marks: [],
      };
      const next = [...clips(), clip];
      setClips(next);
      saveClips(next);
      setActiveClipId(clip.id);
      notify(`已保存 ${clip.name}（${events.length} 步）`);
    } else {
      try {
        await startRecording();
        notify("开始录制，在浏览器里的操作会记录为步骤");
      } catch (e) {
        notify(`启动录制失败：${e}`, "error");
      }
    }
  }

  async function togglePause() {
    if (paused()) await resumeRecording();
    else await pauseRecording();
    void refreshInfo();
  }

  function deleteClip(id: string) {
    const next = clips().filter((c) => c.id !== id);
    setClips(next);
    saveClips(next);
    if (activeClipId() === id) setActiveClipId(null);
  }

  // ---------- 步骤编辑 ----------
  function startEditEvent(evId: number, selector: string, value: string) {
    setEditingEventId(evId);
    setEditSelector(selector);
    setEditValue(value);
  }

  function saveEditEvent() {
    const clipId = activeClipId();
    const evId = editingEventId();
    if (!clipId || evId == null) return;
    const next = clips().map((c) =>
      c.id === clipId
        ? {
            ...c,
            events: c.events.map((ev) =>
              ev.id === evId
                ? { ...ev, target: { ...ev.target, selector: editSelector() }, data: { ...ev.data, value: editValue() } }
                : ev,
            ),
          }
        : c,
    );
    setClips(next);
    saveClips(next);
    setEditingEventId(null);
  }

  function deleteEvent(evId: number) {
    const clipId = activeClipId();
    if (!clipId) return;
    const next = clips().map((c) =>
      c.id === clipId ? { ...c, events: c.events.filter((ev) => ev.id !== evId) } : c,
    );
    setClips(next);
    saveClips(next);
  }

  // ---------- 截图标记（用户自己框选截图，agent 分析元素位置） ----------
  async function addShotMark() {
    if (!browserOn()) {
      notify("请先打开浏览器");
      return;
    }
    try {
      const { image } = await captureRegion();
      const path = await saveShot(image, `mark-${Date.now()}`);
      setPendingMark({ dataUrl: image, path });
    } catch (e) {
      // 取消或失败都不弹错，除非是真错误
      if (String(e).includes("取消")) return;
      notify(`截图失败：${e}`, "error");
    }
  }

  async function analyzeMark() {
    const shot = pendingMark();
    const clipId = activeClipId();
    if (!shot || !clipId) return;
    setAnalyzing(true);
    try {
      const raw = await analyzeScreenshot(state.cwd || ".", shot.dataUrl, markNote());
      const m = raw.match(/\{[\s\S]*\}/);
      const parsed = m ? JSON.parse(m[0]) : {};
      const mark: ClipMark = {
        id: crypto.randomUUID(),
        note: markNote(),
        selector: parsed.selector || undefined,
        confidence: typeof parsed.confidence === "number" ? parsed.confidence : undefined,
        imagePath: shot.path,
      };
      const next = clips().map((c) => (c.id === clipId ? { ...c, marks: [...c.marks, mark] } : c));
      setClips(next);
      saveClips(next);
      notify(mark.selector ? `已定位：${mark.selector}` : "未能确定元素位置", mark.selector ? "info" : "error");
      setPendingMark(null);
      setMarkNote("");
    } catch (e) {
      notify(`分析失败：${e}`, "error");
    } finally {
      setAnalyzing(false);
    }
  }

  function removeMark(clipId: string, markId: string) {
    const next = clips().map((c) =>
      c.id === clipId ? { ...c, marks: c.marks.filter((m) => m.id !== markId) } : c,
    );
    setClips(next);
    saveClips(next);
  }

  // ---------- 运行 ----------
  async function runClip(clip: RecordedClip) {
    setRunning(true);
    setRunResult(null);
    try {
      const plan: PlayPlan = await compilePlan(clip.events);
      plan.analysisPrompt = clip.analysisPrompt || "";
      plan.analysisRecordRefs = clip.analysisRecordRefs || [];
      plan.headless = clip.headless ?? true;
      const runAgentKind = clip.runAgentKind || lastUsed.agentKind();
      const raw = await runPlanWithAgent(state.cwd || ".", plan, runAgentKind, clip.runModel || lastUsed.model(runAgentKind));
      const conclusion = raw.trim();
      const failed = /失败|未完成|阻断|无法|error|failed/i.test(conclusion);
      setRunResult({
        planId: clip.id,
        ok: !failed,
        steps: [],
        final: { ok: !failed, error: failed ? conclusion : undefined, summary: conclusion },
      });
      notify(conclusion || "执行已结束，但 Agent 未返回结论", failed ? "error" : "info", true);
    } catch (e) {
      notify(`运行失败：${e}`, "error", true);
    } finally {
      setRunning(false);
    }
  }

  onMount(() => {
    for (const kind of enabledAgentKinds()) void ensureModelOptions(kind);
    unsubs = subscribeBrowser({
      onNavigate: (u) => u && setUrl(u),
      onClosed: () => setBrowserOn(false),
      onError: (e) => notify(`浏览器错误：${e}`, "error"),
    });
    void refreshInfo();
  });

  onCleanup(() => {
    unsubs.forEach((u) => u());
    if (noticeTimer) clearTimeout(noticeTimer);
  });

  return (
    <div class="browser-view">
      <div class="browser-main">
        <div class="browser-bar">
          <button class="browser-btn primary" disabled={opening()} onClick={() => void open()}>
            {opening() ? "打开中…" : browserOn() ? "浏览器已打开" : "打开浏览器"}
          </button>
          <Show when={browserOn()}>
            <button class="browser-btn" onClick={() => void close()} title="关闭浏览器">关闭</button>
          </Show>
          <Show when={notice()}>
            {(item) => (
              <div class="browser-notice" classList={{ error: item().kind === "error" }} title={item().text}>
                <span>{item().text}</span>
                <button type="button" onClick={() => setNotice(null)} aria-label="关闭提示">×</button>
              </div>
            )}
          </Show>
        </div>
        <div class="browser-editor">
          <Show
            when={activeClipId()}
            fallback={
              <div class="browser-placeholder-main">
                <IconGlobe size={56} />
                <p class="bp-title">Playwright 浏览器</p>
                <p class="bp-sub">先录制或选择右侧片段，再在这里编辑完整步骤。</p>
                <Show when={recording()}>
                  <div class="bp-recording">● 录制中 · 已记录 {liveEvents().length} 步 {paused() ? "（已暂停）" : ""}</div>
                </Show>
              </div>
            }
          >
            {(_activeId) => (
              <>
                <div class="be-head">
                  <div>
                    <input
                      class="be-title"
                      value={currentClip().name}
                      onInput={(e) => updateClip(currentClip().id, (value) => ({ ...value, name: e.currentTarget.value }))}
                    />
                    <div class="bt-hint">{currentClip().events.length} 个步骤 · {currentClip().marks.length} 个截图标记</div>
                  </div>
                  <div class="bt-row">
                    <select class="bt-input" value={newAction()} onChange={(e) => setNewAction(e.currentTarget.value as BrowserAction)}>
                      <option value="click">点击</option>
                      <option value="input">输入</option>
                      <option value="navigate">访问网址</option>
                      <option value="key">按键</option>
                      <option value="record">记录</option>
                    </select>
                    <button class="bt-btn primary" onClick={addManualStep}><IconPlus size={13} /> 新建步骤</button>
                  </div>
                </div>

                <div class="be-steps">
                  <For each={currentClip().events} fallback={<div class="be-empty">暂无步骤，可从上方手动新建或重新录制。</div>}>
                    {(event, index) => (
                      <div
                        class="be-step"
                        data-step-id={event.id}
                        classList={{ dragging: draggingStepId() === event.id, "drag-over": dragOverStepId() === event.id }}
                      >
                        <div
                          class="be-drag-handle"
                          title="拖动排序"
                          role="button"
                          aria-label={`拖动步骤 ${index() + 1} 排序`}
                          onPointerDown={(e) => startStepDrag(e, event.id)}
                        >⋮⋮</div>
                        <div class="be-step-index">{index() + 1}</div>
                        <div class="be-step-body">
                          <div class="be-step-top">
                            <select
                              class="be-action"
                              value={event.kind}
                              onChange={(e) => updateStep(event.id, { kind: e.currentTarget.value as BrowserAction })}
                            >
                              <option value="click">点击</option>
                              <option value="input">输入</option>
                              <option value="change">选择</option>
                              <option value="key">按键</option>
                              <option value="navigate">访问网址</option>
                              <option value="record">记录</option>
                            </select>
                            <div class="be-step-actions">
                              <button class="bt-icon-btn" title="上移" onClick={() => moveStep(event.id, -1)}>↑</button>
                              <button class="bt-icon-btn" title="下移" onClick={() => moveStep(event.id, 1)}>↓</button>
                              <button class="bt-icon-btn danger" title="删除" onClick={() => deleteEvent(event.id)}><IconTrash size={12} /></button>
                            </div>
                          </div>

                          <Show when={event.kind === "navigate"} fallback={
                            <Show when={event.kind === "record"} fallback={
                              <>
                                <label class="be-field be-target-field"><span>目标元素</span><div class="be-target-input"><input value={event.target?.selector || ""} onChange={(e) => updateStep(event.id, { target: { ...event.target, selector: e.currentTarget.value } })} onPaste={(e) => void pasteTargetImages(event.id, e)} placeholder="输入 selector，或直接粘贴图片辅助定位" /><Show when={(event.target?.imagePaths || []).length > 0}><div class="be-target-images"><For each={event.target?.imagePaths || []}>{(path) => <div class="be-record-image"><span>{path}</span><button class="bt-icon-btn danger" onClick={() => removeTargetImage(event.id, path)}><IconTrash size={11} /></button></div>}</For></div></Show></div></label>
                                <Show when={event.kind === "input" || event.kind === "change"}>
                                  <label class="be-field"><span>输入值</span><input value={event.data?.value || ""} onChange={(e) => updateStep(event.id, { data: { ...event.data, value: e.currentTarget.value } })} /></label>
                                </Show>
                                <Show when={event.kind === "key"}>
                                  <label class="be-field"><span>按键</span><input value={event.data?.key || "Enter"} onChange={(e) => updateStep(event.id, { data: { ...event.data, key: e.currentTarget.value } })} /></label>
                                </Show>
                              </>
                            }>
                              <div class="be-record-simple">
                                <label class="be-field"><span>记录名</span><input value={event.data?.outputName || ""} onChange={(e) => updateStep(event.id, { data: { ...event.data, outputName: e.currentTarget.value } })} placeholder="用于最终分析引用此记录结果" /></label>
                                <label class="be-field be-record-content"><span>补充要求</span><textarea value={event.data?.recordContent || ""} onChange={(e) => updateStep(event.id, { data: { ...event.data, recordContent: e.currentTarget.value } })} onPaste={(e) => void pasteRecordImages(event.id, e)} placeholder="补充说明这条记录的定位线索、截图范围或其它要求；也可粘贴参考图片" /></label>
                                <Show when={(event.data?.imagePaths || []).length > 0}>
                                  <div class="be-record-images">
                                    <For each={event.data?.imagePaths || []}>{(path) => <div class="be-record-image"><span>{path}</span><button class="bt-icon-btn danger" onClick={() => removeRecordImage(event.id, path)}><IconTrash size={11} /></button></div>}</For>
                                  </div>
                                </Show>
                              </div>
                            </Show>
                          }>
                            <div class="be-record-simple">
                              <label class="be-field"><span>网址</span><input value={event.url || ""} onChange={(e) => updateStep(event.id, { url: e.currentTarget.value })} placeholder="https://..." /></label>
                              <label class="be-field be-storage-toggle">
                                <span>访问前设置</span>
                                <span class="be-inline-check">
                                  <input
                                    type="checkbox"
                                    checked={event.data?.navigateStorageEnabled ?? false}
                                    onChange={(e) => updateStep(event.id, { data: { ...event.data, navigateStorageEnabled: e.currentTarget.checked } })}
                                  />
                                  sessionStorage
                                </span>
                              </label>
                              <Show when={event.data?.navigateStorageEnabled}>
                                <label class="be-field"><span>键</span><input value={event.data?.storageKey || ""} onChange={(e) => updateStep(event.id, { data: { ...event.data, storageKey: e.currentTarget.value } })} placeholder="sessionStorage key" /></label>
                                <label class="be-field"><span>值</span><textarea value={event.data?.storageValue || ""} onChange={(e) => updateStep(event.id, { data: { ...event.data, storageValue: e.currentTarget.value } })} placeholder="sessionStorage value" /></label>
                              </Show>
                            </div>
                          </Show>
                        </div>
                      </div>
                    )}
                  </For>
                </div>

                <div class="be-analysis">
                  <div class="bt-title">最终分析</div>
                  <Show when={currentClip().events.some((event) => event.kind === "record" && event.data?.outputName?.trim())}>
                    <div class="be-record-simple">
                      <span class="bt-hint">引用记录结果</span>
                      <div class="bt-row">
                        <For each={currentClip().events.filter((event) => event.kind === "record" && event.data?.outputName?.trim())}>
                          {(event) => (
                            <label class="be-inline-check">
                              <input
                                type="checkbox"
                                checked={(currentClip().analysisRecordRefs || []).includes(event.data!.outputName!.trim())}
                                onChange={(e) => setAnalysisRecordRef(event.data!.outputName!.trim(), e.currentTarget.checked)}
                              />
                              {event.data!.outputName!.trim()}
                            </label>
                          )}
                        </For>
                      </div>
                    </div>
                  </Show>
                  <textarea
                    value={currentClip().analysisPrompt || ""}
                    onInput={(e) => setAnalysisPrompt(e.currentTarget.value)}
                    placeholder={'输入完成全部步骤后要如何分析。Agent 只会基于“记录”步骤保存的页面块截图生成结论。'}
                  />
                  <div class="be-analysis-run-options">
                    <label>
                      <span>执行模型</span>
                      <ModelPicker
                        agentKind={currentClip().runAgentKind || lastUsed.agentKind()}
                        agentKinds={enabledAgentKinds()}
                        model={currentClip().runModel || lastUsed.model(currentClip().runAgentKind || lastUsed.agentKind())}
                        onPickModel={(agentKind: AgentKind, model: string) => updateClip(currentClip().id, (value) => ({ ...value, runAgentKind: agentKind, runModel: model }))}
                        portal
                      />
                    </label>
                    <label class="be-headless-check">
                      <input
                        type="checkbox"
                        checked={currentClip().headless ?? true}
                        onChange={(e) => updateClip(currentClip().id, (value) => ({ ...value, headless: e.currentTarget.checked }))}
                      />
                      启用无头模式
                    </label>
                  </div>
                  <div class="be-analysis-foot">
                    <span class="bt-hint">所有“记录”都以页面块截图作为唯一结果；DOM 仅辅助定位。</span>
                    <button class="bt-btn primary" disabled={running()} onClick={() => void runClip(currentClip())}>▶ 执行并分析</button>
                  </div>
                </div>
              </>
            )}
          </Show>
        </div>
      </div>

      <div class="browser-toolbar">
        <div class="bt-section">
          <div class="bt-title">录制</div>
          <div class="bt-row">
            <button class="bt-btn primary" onClick={() => void newClip()} disabled={!browserOn() && !recording()}>
              <Show when={recording()} fallback={<><IconPlus size={13} /> 开始录制</>}>
                <IconStop size={13} /> 停止并保存
              </Show>
            </button>
            <Show when={recording()}>
              <button class="bt-btn" onClick={() => void togglePause()}>{paused() ? "继续" : "暂停"}</button>
            </Show>
          </div>
        </div>

        <div class="bt-section">
          <div class="bt-title">片段 ({clips().length})</div>
          <div class="bt-clips">
            <For each={clips()} fallback={<div class="bt-empty">还没有片段。打开浏览器后开始录制。</div>}>
              {(clip) => (
                <div class="bt-clip" classList={{ active: activeClipId() === clip.id }} onClick={() => setActiveClipId(clip.id)}>
                  <div class="bt-clip-head">
                    <input
                      class="bt-clip-name-input"
                      value={clip.name}
                      onClick={(e) => e.stopPropagation()}
                      onInput={(e) => updateClip(clip.id, (value) => ({ ...value, name: e.currentTarget.value }))}
                      aria-label="片段名称"
                    />
                    <span class="bt-clip-count">{clip.events.length} 步</span>
                  </div>
                </div>
              )}
            </For>
          </div>
        </div>

        <Show when={pendingMark()}>
          {(shot) => (
            <div class="bt-section">
              <div class="bt-title">新截图标记</div>
              <div class="bt-shot">
                <img src={shot().dataUrl} class="bt-shot-img" alt="截图" />
                <textarea
                  class="bt-note"
                  placeholder="说明要关注什么（如：登录按钮）"
                  value={markNote()}
                  onInput={(e) => setMarkNote(e.currentTarget.value)}
                />
                <div class="bt-row">
                  <button class="bt-btn primary" disabled={analyzing()} onClick={() => void analyzeMark()}>
                    {analyzing() ? "分析中…" : "分析元素位置并保存"}
                  </button>
                  <button class="bt-btn" onClick={() => setPendingMark(null)}>取消</button>
                </div>
              </div>
            </div>
          )}
        </Show>

        <Show when={running()}>
          <div class="bt-section">
            <div class="bt-title">运行中</div>
            <div class="bt-hint">Agent 正在用 Playwright 执行并验证计划…</div>
          </div>
        </Show>
        <Show when={runResult()}>
          {(res) => (
            <div class="bt-section">
              <div class="bt-title">运行结果</div>
              <div class="bt-hint" classList={{ ok: res().ok, err: !res().ok }}>
                {res().final.summary || (res().ok ? "执行完成" : `未完成：${res().final.error || "未知错误"}`)}
              </div>
            </div>
          )}
        </Show>
      </div>
    </div>
  );
}
