import { createSignal, onMount, Show } from 'solid-js';
import { api } from '../ipc';
import { defaultWorkspaceLayout, setWorkspaceLayout, workspaceLayout } from '../workspaceLayout';

export default function AppearanceLayoutSettings() {
  const [windowLayout, setWindowLayout] = createSignal<Awaited<ReturnType<typeof api.getWindowLayout>>>();
  const [width, setWidth] = createSignal(1280);
  const [height, setHeight] = createSignal(820);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal('');
  const refresh = async () => {
    const layout = await api.getWindowLayout();
    setWindowLayout(layout); setWidth(layout.width); setHeight(layout.height);
  };
  onMount(() => { void refresh().catch(error => setError(String(error))); });
  const apply = async (patch: Parameters<typeof api.setWindowLayout>[0]) => {
    setBusy(true); setError('');
    try {
      await api.setWindowLayout(patch);
      if (patch.reset) setWorkspaceLayout(defaultWorkspaceLayout);
      await refresh();
    } catch (error) { setError(String(error)); }
    finally { setBusy(false); }
  };
  return <>
    <div class="field">
      <span class="field-label">文件侧栏</span>
      <label><input type="checkbox" checked={workspaceLayout.open} onChange={e => setWorkspaceLayout({open:e.currentTarget.checked})} /> 显示文件侧栏（所有会话同步）</label>
      <label><input type="checkbox" checked={workspaceLayout.softWrap} onChange={e => setWorkspaceLayout({softWrap:e.currentTarget.checked})} /> 代码软换行</label>
      <label><input type="checkbox" checked={workspaceLayout.minimap} onChange={e => setWorkspaceLayout({minimap:e.currentTarget.checked})} /> 超过一屏时显示代码缩略图</label>
      <label>侧栏宽度 {workspaceLayout.widthRatio === null ? '自动（默认 480 px）' : `${Math.round(workspaceLayout.widthRatio * 100)}%`}
        <input aria-label="侧栏宽度比例" type="range" min="20" max="70" step="1" value={(workspaceLayout.widthRatio ?? .4) * 100} onInput={e => setWorkspaceLayout({widthRatio:Number(e.currentTarget.value) / 100})} style={{width:'100%'}} />
      </label>
      <span class="field-hint">即时生效并自动记住。拖动侧栏边缘也会更新比例；选区颜色跟随明暗主题。</span>
    </div>
    <div class="field">
      <span class="field-label">窗口布局</span>
      <Show when={windowLayout()}>{layout => <>
        <label><input type="checkbox" checked={layout().remember} disabled={busy()} onChange={e => void apply({remember:e.currentTarget.checked})} /> 记住窗口大小、位置及最大化状态</label>
        <label><input type="checkbox" checked={layout().maximized} disabled={busy()} onChange={e => void apply({maximized:e.currentTarget.checked})} /> 最大化窗口</label>
        <div style={{display:'flex',gap:'12px','align-items':'end','flex-wrap':'wrap'}}>
          <label>窗口宽度<input class="field-input" aria-label="窗口宽度" type="number" min="960" max="16384" value={width()} disabled={busy() || layout().maximized} onInput={e => setWidth(Number(e.currentTarget.value))} /></label>
          <label>窗口高度<input class="field-input" aria-label="窗口高度" type="number" min="600" max="16384" value={height()} disabled={busy() || layout().maximized} onInput={e => setHeight(Number(e.currentTarget.value))} /></label>
          <button type="button" class="btn secondary small" disabled={busy() || layout().maximized || width() < 960 || height() < 600 || width() > 16384 || height() > 16384} onClick={() => void apply({width:width(),height:height()})}>应用窗口尺寸</button>
        </div>
      </>}</Show>
      <span class="field-hint">窗口调整会静默记住。重置会恢复默认窗口大小和位置，以及文件侧栏的开关、宽度、软换行和缩略图设置。</span>
      <button type="button" class="btn secondary small" disabled={busy()} onClick={() => void apply({reset:true})}>重置布局</button>
      <Show when={error()}><span role="alert" class="field-hint">{error()}</span></Show>
    </div>
  </>;
}
