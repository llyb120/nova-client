import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { confirm } from "@tauri-apps/plugin-dialog";
import { layoutApi, layoutState, layoutSrcdoc, loadLayouts } from "../layoutStore";
import "./LayoutPanel.css";
import "./LayoutPresetsView.css";

const EXAMPLES = [
  "左侧任务清单，右侧进度卡片与日志",
  "一个简洁的数据看板：顶部指标卡 + 下方趋势列表",
  "看板视图：待办 / 进行中 / 已完成三列",
  "单页报告：标题、摘要、要点列表与引用来源",
];

/** 看板预设：在侧栏看板页的「配置」里设计和管理，会话里只能选择。 */
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
  // 预览默认用设计会话给的 mock 数据，可在「数据结构与测试数据」里改。
  createEffect(on(() => selected()?.mock, (mock) => setSample(JSON.stringify(mock ?? {}, null, 2))));
  const onMessage = (event: MessageEvent) => {
    if (event.source === frame?.contentWindow && event.data?.type === "nova-layout-ready") post();
  };
  window.addEventListener("message", onMessage);
  onCleanup(() => window.removeEventListener("message", onMessage));

  const select = (id: string | null) => {
    setSelectedId(id);
    setName(layoutState.presets.find((p) => p.id === id)?.name ?? "");
    setError("");
  };
  const design = async () => {
    const text = request().trim();
    if (!text) return;
    try {
      const res = (await layoutApi.presetDesign(text, { presetId: selectedId() ?? undefined, name: selected() ? undefined : name().trim() || undefined })) as { presetId: string };
      setSelectedId(res.presetId);
      setRequest("");
      setError("");
    } catch (e) {
      setError(String(e));
    }
  };
  const rename = () => {
    const p = selected();
    const next = name().trim();
    if (p && next && next !== p.name) void layoutApi.presetRename(p.id, next);
    else if (p) setName(p.name);
  };
  const remove = async () => {
    const preset = selected();
    if (preset && (await confirm(`删除预设「${preset.name}」？`, { title: "删除预设", kind: "warning" }))) {
      await layoutApi.presetRemove(preset.id);
      select(null);
    }
  };

  return (
    <main class="layout-presets">
      <aside class="layout-presets-list">
        <button type="button" class="layout-presets-new" classList={{ active: selectedId() === null }} onClick={() => select(null)}>＋ 新建预设</button>
        <div class="layout-presets-cards">
          <For each={layoutState.presets} fallback={<div class="layout-presets-hint">还没有预设。<br />描述一个看板，让设计会话帮你做出来。</div>}>
            {(preset) => (
              <button type="button" class="layout-card" classList={{ active: preset.id === selectedId() }} onClick={() => select(preset.id)}>
                <div class="layout-card-thumb">
                  <Show when={preset.html} fallback={<span class="layout-spinner" />}>
                    <iframe tabindex="-1" aria-hidden="true" title={preset.name} sandbox="allow-scripts" referrerpolicy="no-referrer" srcdoc={layoutSrcdoc(preset.html, preset.mock)} />
                  </Show>
                </div>
                <div class="layout-card-name">
                  <span>{preset.name}</span>
                  <Show when={preset.designing}><small>设计中…</small></Show>
                </div>
              </button>
            )}
          </For>
        </div>
      </aside>
      <section class="layout-presets-main">
        <header class="layout-presets-head">
          <input value={name()} placeholder={selected() ? "预设名称" : "新预设名称（可选）"} onInput={(e) => setName(e.currentTarget.value)}
            onBlur={rename} onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }} />
          <span class="layout-presets-tip">在「＋ 添加看板」中选择</span>
          <Show when={selected()}><button type="button" class="layout-icon-btn danger" onClick={() => void remove()}>删除</button></Show>
        </header>
        <div class="layout-presets-preview">
          <Show
            when={selected()?.html}
            fallback={
              <div class="layout-empty">
                <Show when={selected()?.designing} fallback={
                  <>
                    <div class="layout-empty-title">{selected() ? "这个预设还没有内容" : "描述你想要的看板"}</div>
                    <div>设计会话会在后台并行完成，完成后这里会出现实时预览。</div>
                    <Show when={!selected()}>
                      <div class="layout-examples">
                        <For each={EXAMPLES}>{(ex) => <button type="button" onClick={() => setRequest(ex)}>{ex}</button>}</For>
                      </div>
                    </Show>
                  </>
                }>
                  <span class="layout-spinner" /><div>设计会话正在设计…</div>
                </Show>
              </div>
            }
          >
            <iframe ref={frame} title="预览" sandbox="allow-scripts" referrerpolicy="no-referrer" srcdoc={layoutSrcdoc(selected()!.html, selected()!.mock)} onLoad={post} />
            <Show when={selected()?.designing}><div class="layout-presets-busy"><span class="layout-spinner" />正在调整…</div></Show>
          </Show>
        </div>
        <Show when={selected()}>
          {(p) => (
            <label class="layout-presets-prompt">
              <span>生成数据的附加提示词</span>
              <textarea rows={3} value={p().prompt ?? ""} placeholder="如：重点列出风险和待办；结论不超过 50 字…（失焦保存，对使用该预设的看板立即生效）"
                onBlur={(e) => { const v = e.currentTarget.value.trim(); if (v !== (p().prompt ?? "")) void layoutApi.presetPrompt(p().id, v).catch((err) => setError(String(err))); }} />
            </label>
          )}
        </Show>
        <Show when={selected()?.schema}>
          <details class="layout-presets-schema">
            <summary>数据结构与测试数据</summary>
            <pre>{selected()!.schema}</pre>
            <textarea value={sample()} onInput={(e) => setSample(e.currentTarget.value)} rows={8} placeholder='预览用测试数据（默认为设计时给出的示例数据）' />
          </details>
        </Show>
        <Show when={error()}><div class="layout-presets-error">{error()}</div></Show>
        <div class="layout-presets-compose">
          <textarea rows={2} value={request()} onInput={(e) => setRequest(e.currentTarget.value)}
            placeholder={selected() ? "描述要如何调整这个预设…  Enter 发送，Shift+Enter 换行" : "描述你想要的看板，如：左侧任务清单，右侧进度卡片与日志…"}
            onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); void design(); } }} />
          <button type="button" class="btn primary" disabled={!request().trim() || !!selected()?.designing} onClick={() => void design()}>{selected() ? "调整" : "设计"}</button>
        </div>
      </section>
    </main>
  );
}
