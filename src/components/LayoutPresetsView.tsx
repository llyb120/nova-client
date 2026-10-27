import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { confirm } from "@tauri-apps/plugin-dialog";
import { layoutApi, layoutState, layoutSrcdoc, loadLayouts } from "../layoutStore";
import "./LayoutPanel.css";
import "./LayoutPresetsView.css";

/** 预设布局：预先设计好，会话里可直接套用（也可让主会话按名称使用）。 */
export default function LayoutPresetsView() {
  const [selectedId, setSelectedId] = createSignal<string | null>(null);
  const [name, setName] = createSignal("");
  const [request, setRequest] = createSignal("");
  const [sample, setSample] = createSignal("{}");
  const [error, setError] = createSignal("");
  let frame: HTMLIFrameElement | undefined;

  onMount(() => void loadLayouts(""));
  const selected = createMemo(() => layoutState.presets.find((p) => p.id === selectedId()));

  const post = () => {
    try {
      frame?.contentWindow?.postMessage({ type: "nova-layout-data", data: JSON.parse(sample() || "{}") }, "*");
      setError("");
    } catch {
      setError("测试数据不是合法 JSON");
    }
  };
  createEffect(() => { sample(); post(); });
  const onMessage = (event: MessageEvent) => {
    if (event.source === frame?.contentWindow && event.data?.type === "nova-layout-ready") post();
  };
  window.addEventListener("message", onMessage);
  onCleanup(() => window.removeEventListener("message", onMessage));

  const design = async () => {
    const text = request().trim();
    if (!text) return;
    try {
      const res = (await layoutApi.presetDesign(text, { presetId: selectedId() ?? undefined, name: name().trim() || undefined })) as { presetId: string };
      setSelectedId(res.presetId);
      setRequest("");
      setError("");
    } catch (e) {
      setError(String(e));
    }
  };
  const remove = async () => {
    const preset = selected();
    if (preset && (await confirm(`删除预设「${preset.name}」？`, { title: "删除预设", kind: "warning" }))) {
      await layoutApi.presetRemove(preset.id);
      setSelectedId(null);
    }
  };

  return (
    <main class="layout-presets">
      <aside class="layout-presets-list">
        <div class="layout-presets-title">预设布局</div>
        <button type="button" class="layout-presets-item new" classList={{ active: selectedId() === null }} onClick={() => setSelectedId(null)}>＋ 新建预设</button>
        <For each={layoutState.presets}>
          {(preset) => (
            <button type="button" class="layout-presets-item" classList={{ active: preset.id === selectedId() }} onClick={() => { setSelectedId(preset.id); setName(preset.name); }}>
              {preset.name}
              <Show when={preset.designing}><small>设计中…</small></Show>
            </button>
          )}
        </For>
      </aside>
      <section class="layout-presets-main">
        <div class="layout-presets-head">
          <input value={name()} placeholder={selected() ? "预设名称" : "新预设名称"} onInput={(e) => setName(e.currentTarget.value)}
            onBlur={() => { const p = selected(); if (p && name().trim() && name().trim() !== p.name) void layoutApi.presetRename(p.id, name().trim()); }} />
          <Show when={selected()}><button type="button" class="layout-icon-btn" onClick={() => void remove()}>删除</button></Show>
        </div>
        <div class="layout-presets-preview">
          <Show when={selected()?.html} fallback={<div class="layout-empty">{selected()?.designing ? "设计会话正在设计…" : "在下方描述你想要的布局，设计会话会并行完成。预设可在任意会话里直接使用。"}</div>}>
            <iframe ref={frame} title="预览" sandbox="allow-scripts" referrerpolicy="no-referrer" srcdoc={layoutSrcdoc(selected()!.html)} onLoad={post} />
          </Show>
        </div>
        <Show when={selected()?.schema}>
          <details class="layout-presets-schema">
            <summary>数据结构与测试数据</summary>
            <pre>{selected()!.schema}</pre>
            <textarea rows={4} value={sample()} onInput={(e) => setSample(e.currentTarget.value)} placeholder='预览用测试数据，如 {"title":"示例"}' />
          </details>
        </Show>
        <Show when={error()}><div class="layout-presets-error">{error()}</div></Show>
        <div class="layout-presets-compose">
          <textarea rows={3} value={request()} onInput={(e) => setRequest(e.currentTarget.value)}
            placeholder={selected() ? "描述要如何调整这个预设…" : "描述你想要的布局，如：左侧任务清单，右侧进度卡片与日志…"}
            onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); void design(); } }} />
          <button type="button" class="btn" disabled={!request().trim() || !!selected()?.designing} onClick={() => void design()}>{selected() ? "调整" : "设计"}</button>
        </div>
      </section>
    </main>
  );
}
