import { createMemo, createResource, createSignal, For, Show } from 'solid-js';
import { Portal } from 'solid-js/web';
import { api } from '../ipc';
import { originalImageUrl, promptImageSource } from '../historyImages';
import type { Item } from '../types';
import './HistoryDetails.css';

/** Details are opt-in and live outside the bounded transcript window. They are
 * disposed on close/switch; merely scrolling never loads full raw tool payloads.
 */
export function HistoryDetails(props: { threadId: string; item: Item; generation?: string; onClose(): void }) {
  const [result] = createResource(() => [props.threadId, props.item.id, props.generation] as const,
    async ([threadId, itemId, generation]) => {
      try { return { value: await api.getThreadItemDetail(threadId, itemId, generation), error: "" }; }
      catch (error) { return { value: null, error: String(error) }; }
    });
  const item = () => result()?.value;
  const [limit, setLimit] = createSignal(64 * 1024);
  const [copied, setCopied] = createSignal('');
  const text = createMemo(() => { const value = item(); return value ? ('text' in value ? value.text : JSON.stringify(value, null, 2)) : ''; });
  return <Portal><div class="history-overlay" onClick={e => { if (e.target === e.currentTarget) props.onClose(); }}>
    <section class="history-dialog" role="dialog" aria-modal="true" aria-label="完整消息" onKeyDown={e => { e.stopPropagation(); if (e.key === 'Escape') props.onClose(); }}>
      <header><strong>完整消息</strong><button onClick={props.onClose} aria-label="关闭完整消息">关闭</button></header>
      <Show when={result.loading}><p role="status">正在读取这一条消息…</p></Show>
      <Show when={result()?.error}><p role="alert">{String(result()?.error)}</p></Show>
      <Show when={item()}>
        <pre>{text().slice(0, limit())}</pre>
        <Show when={text().length > limit()}><button onClick={() => setLimit(n => n + 64 * 1024)}>继续显示（原文共 {text().length} 字符）</button></Show>
        <button onClick={() => void navigator.clipboard.writeText(text()).then(() => setCopied('已复制')).catch(error => setCopied(String(error)))}>复制完整内容</button><span role="status">{copied()}</span>
        <Show when={item()?.type === 'user'}><div class="history-attachments"><For each={(item() as Extract<Item, { type: 'user' }>)?.images?.filter(image => image.mimeType.startsWith('image/')) ?? []}>{image =>
          <button onClick={() => window.dispatchEvent(new CustomEvent('nova:history-image', { detail: promptImageSource(image) }))}>{image.name || '打开原图'}</button>
        }</For></div></Show>
      </Show>
    </section>
  </div></Portal>;
}
export function HistoryImagePreview(props: { source: string; onClose(): void }) {
  const [result] = createResource(() => props.source, async source => {
    try { return { url: await originalImageUrl(source), error: "" }; }
    catch (error) { return { url: "", error: String(error) }; }
  });
  return <Portal><div class="history-overlay" onClick={e => { if (e.target === e.currentTarget) props.onClose(); }}>
    <section class="history-dialog history-image-dialog" role="dialog" aria-modal="true" aria-label="原图预览" onKeyDown={e => { e.stopPropagation(); if (e.key === 'Escape') props.onClose(); }}>
      <header><strong>原图预览</strong><button onClick={props.onClose} aria-label="关闭原图预览">关闭</button></header>
      <Show when={result.loading}><p role="status">正在加载原图…</p></Show>
      <Show when={result()?.error}><p role="alert">{String(result()?.error)}</p></Show>
      <Show when={result()?.url}>{src => <img src={src()} alt="会话原图" decoding="async" />}</Show>
    </section>
  </div></Portal>;
}
