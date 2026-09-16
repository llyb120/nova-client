import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { createSignal, For, onCleanup, onMount, Show } from 'solid-js';

type BrowserSession = { browserId: string; threadId: string; url: string; busy: boolean; status: string; activeTab: string; tabs: { id: string; url: string; title: string }[] };

export default function WorkspaceBrowser(props: { threadId: string }) {
  const [session, setSession] = createSignal<BrowserSession>();
  const [url, setUrl] = createSignal('');
  const [error, setError] = createSignal('');
  let surface!: HTMLDivElement;
  let disposed = false;
  let lastLayout = '';
  let queue = Promise.resolve();
  const call = <T,>(operation: string, args: unknown = {}) => invoke<T>('native_browser_ui', { threadId: props.threadId, operation, args });
  const perform = async (operation: string, args = {}) => {
    setError('');
    try { return await call(operation, args); } catch (e) { setError(String(e)); }
  };
  const syncLayout = () => {
    if (disposed || !session()) return;
    const rect = surface.getBoundingClientRect();
    // Native child views sit above HTML; hide them when a modal covers the surface.
    const hit = document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2);
    const visible = !document.hidden && rect.width > 1 && rect.height > 1 && !!hit && surface.contains(hit)
      && !document.querySelector('.modal-backdrop, [role="dialog"], [aria-modal="true"]');
    const args = { visible, tabId: session()?.activeTab, x: Math.max(0, rect.x), y: Math.max(0, rect.y), width: rect.width, height: rect.height };
    const encoded = JSON.stringify(args);
    if (encoded === lastLayout) return;
    lastLayout = encoded;
    queue = queue.then(async () => { if (!disposed) await call('layout', args); }).catch(e => { lastLayout = ''; if (!disposed) setError(String(e)); });
  };
  onMount(() => {
    let unlisten: (() => void) | undefined;
    void listen<BrowserSession | null>('native-browser:state', event => {
      if (event.payload?.threadId !== props.threadId || disposed) return;
      setSession(event.payload);
      syncLayout();
      if (document.activeElement?.getAttribute('aria-label') !== '网页地址') setUrl(event.payload.url === 'about:blank' ? '' : event.payload.url);
    }).then(stop => { if (disposed) stop(); else unlisten = stop; });
    void call<BrowserSession>('mount').then(value => {
      if (disposed) return;
      setSession(value); setUrl(value.url === 'about:blank' ? '' : value.url); syncLayout();
    }).catch(e => { if (!disposed) setError(String(e)); });
    const resize = new ResizeObserver(syncLayout); resize.observe(surface);
    const mutations = new MutationObserver(syncLayout);
    mutations.observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ['class', 'style'] });
    window.addEventListener('resize', syncLayout);
    document.addEventListener('visibilitychange', syncLayout);
    onCleanup(() => {
      disposed = true; unlisten?.(); resize.disconnect(); mutations.disconnect();
      window.removeEventListener('resize', syncLayout); document.removeEventListener('visibilitychange', syncLayout);
      const tabId = session()?.activeTab;
      void queue.then(() => call('layout', { visible: false, tabId })).catch(() => {});
    });
  });
  const busy = () => session()?.busy;
  return <section class="workspace-browser" aria-label="原生网页浏览器">
    <div class="workspace-browser-tabs" role="tablist" aria-label="网页标签">
      <For each={session()?.tabs}>{tab => <div class="workspace-browser-tab" classList={{ active: session()?.activeTab === tab.id }}>
        <button role="tab" aria-selected={session()?.activeTab === tab.id} title={tab.url} onClick={() => void perform('select_tab', { tabId: tab.id })}>{tab.title || (tab.url === 'about:blank' ? '新标签页' : tab.url)}</button>
        <button aria-label={`关闭 ${tab.title || '标签页'}`} onClick={() => void perform('close_tab', { tabId: tab.id })}>×</button>
      </div>}</For>
      <button title="新标签页" aria-label="新标签页" onClick={() => void perform('new_tab')}>＋</button>
    </div>
    <form class="workspace-toolbar" onSubmit={e => { e.preventDefault(); void perform('goto', { url: url() }); }}>
      <button type="button" title="后退" aria-label="网页后退" disabled={busy()} onClick={() => void perform('back')}>←</button>
      <button type="button" title="前进" aria-label="网页前进" disabled={busy()} onClick={() => void perform('forward')}>→</button>
      <button type="button" title="刷新" aria-label="刷新网页" disabled={busy()} onClick={() => void perform('reload')}>↻</button>
      <input aria-label="网页地址" placeholder="输入网址" value={url()} onInput={e => setUrl(e.currentTarget.value)} />
      <button type="submit" disabled={!session() || busy()}>打开</button>
      <Show when={busy()}><button type="button" title={session()?.status} onClick={() => void perform('stop')}>停止</button></Show>
    </form>
    <Show when={error()}><span class="workspace-error" role="alert">{error()}</span></Show>
    <div ref={surface} class="workspace-browser-surface" aria-label="网页内容" />
  </section>;
}
