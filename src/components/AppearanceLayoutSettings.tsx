import { createSignal, onSettled, Show } from 'solid-js';
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
  onSettled(() => { void refresh().catch(error => setError(String(error))); });
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
    <section class="settings-group">
      <h3 class="settings-group-title">文件侧栏</h3>
      <p class="settings-group-desc">即时生效；开合按会话记住，其余全局记住。拖动侧栏边缘也会更新比例；选区颜色跟随明暗主题。</p>
      <div class="appearance-toggles">
        <label class="backend-switch appearance-switch-row">
          <input type="checkbox" checked={workspaceLayout.open} onChange={e => setWorkspaceLayout({open:e.currentTarget.checked})} />
          <span>在当前会话显示文件侧栏</span>
        </label>
        <label class="backend-switch appearance-switch-row">
          <input type="checkbox" checked={workspaceLayout.softWrap} onChange={e => setWorkspaceLayout({softWrap:e.currentTarget.checked})} />
          <span>代码软换行</span>
        </label>
        <label class="backend-switch appearance-switch-row">
          <input type="checkbox" checked={workspaceLayout.minimap} onChange={e => setWorkspaceLayout({minimap:e.currentTarget.checked})} />
          <span>超过一屏时显示代码缩略图</span>
        </label>
      </div>
      <div class="field appearance-slider-field">
        <div class="appearance-slider-head">
          <span class="field-label">侧栏宽度</span>
          <span class="appearance-slider-val">{workspaceLayout.widthRatio === null ? '自动（默认 480 px）' : `${Math.round(workspaceLayout.widthRatio * 100)}%`}</span>
        </div>
        <input class="appearance-range" aria-label="侧栏宽度比例" type="range" min="20" max="70" step="1" value={(workspaceLayout.widthRatio ?? .4) * 100} onInput={e => setWorkspaceLayout({widthRatio:Number(e.currentTarget.value) / 100})} />
      </div>
    </section>
    <section class="settings-group">
      <h3 class="settings-group-title">窗口布局</h3>
      <p class="settings-group-desc">窗口调整会静默记住。重置会恢复默认窗口大小和位置，以及文件侧栏的开关、宽度、软换行和缩略图设置。</p>
      <Show when={windowLayout()}>{layout => <>
        <div class="appearance-toggles">
          <label class="backend-switch appearance-switch-row">
            <input type="checkbox" checked={layout().remember} disabled={busy()} onChange={e => void apply({remember:e.currentTarget.checked})} />
            <span>记住窗口大小、位置及最大化状态</span>
          </label>
          <label class="backend-switch appearance-switch-row">
            <input type="checkbox" checked={layout().maximized} disabled={busy()} onChange={e => void apply({maximized:e.currentTarget.checked})} />
            <span>最大化窗口</span>
          </label>
        </div>
        <div class="appearance-window-row">
          <label class="field">
            <span class="field-label">窗口宽度</span>
            <input class="field-input" aria-label="窗口宽度" type="number" min="960" max="16384" value={width()} disabled={busy() || layout().maximized} onInput={e => setWidth(Number(e.currentTarget.value))} />
          </label>
          <label class="field">
            <span class="field-label">窗口高度</span>
            <input class="field-input" aria-label="窗口高度" type="number" min="600" max="16384" value={height()} disabled={busy() || layout().maximized} onInput={e => setHeight(Number(e.currentTarget.value))} />
          </label>
          <button type="button" class="btn secondary small" disabled={busy() || layout().maximized || width() < 960 || height() < 600 || width() > 16384 || height() > 16384} onClick={() => void apply({width:width(),height:height()})}>应用窗口尺寸</button>
        </div>
      </>}</Show>
      <div class="appearance-reset-row">
        <button type="button" class="btn secondary small" disabled={busy()} onClick={() => void apply({reset:true})}>重置布局</button>
      </div>
      <Show when={error()}><span role="alert" class="field-hint">{error()}</span></Show>
    </section>
  </>;
}
