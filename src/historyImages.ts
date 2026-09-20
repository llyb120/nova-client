import { convertFileSrc } from '@tauri-apps/api/core';
import { api } from './ipc';
import type { PromptImage } from './types';

export const IMAGE_CACHE_BYTES = 32 * 1024 * 1024;
export const IMAGE_CACHE_ENTRIES = 48;
export const IMAGE_LOAD_CONCURRENCY = 2;
export interface ImageDimensions { naturalWidth: number; naturalHeight: number }
export interface TranscriptImage extends ImageDimensions {
  drawable: CanvasImageSource;
  bytes: number;
  dispose(): void;
}
export function localFilePath(uri: string): string {
  let path: string;
  try { path = decodeURIComponent(uri.replace(/^file:\/\//i, '')); }
  catch { path = uri.replace(/^file:\/\//i, ''); }
  return /^\/[A-Za-z]:\//.test(path) ? path.slice(1) : path;
}
export function assetUrl(uri: string): string {
  return /^file:\/\//i.test(uri) ? convertFileSrc(localFilePath(uri)) : uri;
}
const promptSources = new WeakMap<PromptImage, { data?: string; uri?: string; source: string }>();
export function promptImageSource(image: PromptImage): string {
  const known = promptSources.get(image);
  if (known && known.data === image.data && known.uri === image.uri) return known.source;
  const source = image.data ? `data:${image.mimeType};base64,${image.data}` : image.uri ?? '';
  promptSources.set(image, { data: image.data, uri: image.uri, source });
  return source;
}
export async function originalImageUrl(source: string): Promise<string> {
  return assetUrl(source.startsWith('nova-history://') ? (await api.getHistoryImage(source, 480, true)).uri : source);
}

async function decodeImage(source: string, edge: number): Promise<TranscriptImage> {
  let width: number | null = null, height: number | null = null;
  if (source.startsWith('nova-history://')) {
    const info = await api.getHistoryImage(source, edge);
    source = info.thumbnailUri ?? info.uri;
    width = info.width; height = info.height;
  }
  const image = new Image();
  image.decoding = 'async';
  await new Promise<void>((resolve, reject) => {
    image.onload = () => resolve();
    image.onerror = () => reject(new Error('图片无法加载，原图仍可单独打开'));
    image.src = assetUrl(source);
  });
  const naturalWidth = width ?? image.naturalWidth, naturalHeight = height ?? image.naturalHeight;
  if (!image.naturalWidth || !image.naturalHeight) { image.src = ''; throw new Error('图片尺寸无效'); }
  const scale = Math.min(1, edge / Math.max(image.naturalWidth, image.naturalHeight));
  const w = Math.max(1, Math.round(image.naturalWidth * scale)), h = Math.max(1, Math.round(image.naturalHeight * scale));
  // Retain a bounded bitmap, not a multi-megapixel HTMLImageElement. Originals
  // are resolved only for an explicit preview/copy/edit, never on transcript paint.
  if (typeof createImageBitmap === 'function') {
    try {
      const bitmap = await createImageBitmap(image, { resizeWidth: w, resizeHeight: h, resizeQuality: 'high' });
      image.onload = image.onerror = null; image.src = '';
      return { drawable: bitmap, naturalWidth, naturalHeight, bytes: w * h * 4, dispose: () => bitmap.close() };
    } catch { /* Older WebViews may not resize ImageBitmap; use a bounded canvas. */ }
  }
  const canvas = document.createElement('canvas'); canvas.width = w; canvas.height = h;
  const context = canvas.getContext('2d');
  if (!context) { image.src = ''; throw new Error('无法创建图片预览'); }
  context.drawImage(image, 0, 0, w, h);
  image.onload = image.onerror = null; image.src = '';
  return { drawable: canvas, naturalWidth, naturalHeight, bytes: w * h * 4, dispose: () => { canvas.width = canvas.height = 1; } };
}

type Load = (source: string, edge: number) => Promise<TranscriptImage>;
type Entry = { key: string; source: string; epoch: number; state: 'queued' | 'loading' | 'ready' | 'error'; image?: TranscriptImage; error?: string; used: number };
/** Pixel-budgeted LRU plus a two-slot queue. Only requested visible images load.
 * Metadata survives bitmap eviction, so revisiting a thumbnail does not relayout.
 * In-flight native calls are allowed to finish but never accumulate on switching.
 */
export class TranscriptImages {
  private entries = new Map<string, Entry>();
  private metadata = new Map<string, ImageDimensions>();
  private wanted = new Set<string>();
  private aliases = new Map<string, string>();
  private epoch = 0;
  private sequence = 0;
  private active = 0;
  private bytes = 0;
  private timer: ReturnType<typeof setTimeout> | undefined;
  constructor(private changed: (dimensionsChanged: boolean) => void, private load: Load = decodeImage,
    private budget = IMAGE_CACHE_BYTES, private capacity = IMAGE_CACHE_ENTRIES) {}
  private key(source: string): string {
    if (source.length <= 1024) return source;
    let key = this.aliases.get(source);
    if (!key) { key = `inline:${++this.sequence}`; this.aliases.set(source, key); }
    return key;
  }
  beginFrame() { this.wanted.clear(); }
  dimensions(source: string): ImageDimensions | null {
    const value = this.metadata.get(this.key(source));
    return value ?? null;
  }
  request(source: string): TranscriptImage | null {
    if (!source) return null;
    const key = this.key(source); this.wanted.add(key);
    let entry = this.entries.get(key);
    if (!entry) {
      if (this.entries.size >= this.capacity) {
        const victim = [...this.entries.values()].filter(e => e.state !== 'loading' && !this.wanted.has(e.key)).sort((a, b) => a.used - b.used)[0];
        if (victim) this.remove(victim);
        if (this.entries.size >= this.capacity) return null;
      }
      entry = { key, source, epoch: this.epoch, state: 'queued', used: ++this.sequence };
      this.entries.set(key, entry);
      this.schedule();
    }
    entry.used = ++this.sequence;
    return entry.image ?? null;
  }
  error(source: string): string | undefined { return this.entries.get(this.key(source))?.error ?? (!this.entries.has(this.key(source)) && this.entries.size >= this.capacity ? '同屏图片较多，请打开原图' : undefined); }
  endFrame() {
    for (const [key, entry] of this.entries) {
      if (entry.state === 'queued' && !this.wanted.has(key)) this.entries.delete(key);
    }
    this.evict();
  }
  private schedule() {
    if (this.timer !== undefined) return;
    // Leave the current frame/input task before starting any decoding work.
    this.timer = setTimeout(() => { this.timer = undefined; this.pump(); }, 0);
  }
  private pump() {
    while (this.active < IMAGE_LOAD_CONCURRENCY) {
      const entry = [...this.entries.values()].find(e => e.state === 'queued' && this.wanted.has(e.key));
      if (!entry) break;
      entry.state = 'loading'; this.active++;
      const dpr = typeof devicePixelRatio === 'number' ? devicePixelRatio : 1;
      const edge = dpr > 1 ? 960 : 480;
      void this.load(entry.source, edge).then(image => {
        if (entry.epoch !== this.epoch || this.entries.get(entry.key) !== entry) { image.dispose(); return; }
        const old = this.metadata.get(entry.key);
        const changed = !old || old.naturalWidth !== image.naturalWidth || old.naturalHeight !== image.naturalHeight;
        this.metadata.delete(entry.key);
        this.metadata.set(entry.key, { naturalWidth: image.naturalWidth, naturalHeight: image.naturalHeight });
        while (this.metadata.size > 512) this.metadata.delete(this.metadata.keys().next().value!);
        if (image.bytes > this.budget) { image.dispose(); throw new Error('图片超出预览预算，请打开原图'); }
        entry.state = 'ready'; entry.image = image; this.bytes += image.bytes;
        this.evict();
        if (this.wanted.has(entry.key)) this.changed(changed);
      }).catch(error => {
        if (entry.epoch !== this.epoch) return;
        entry.state = 'error'; entry.error = String(error);
        if (this.wanted.has(entry.key)) this.changed(false);
      }).finally(() => { this.active--; this.schedule(); });
    }
  }
  private evict() {
    const candidates = [...this.entries.values()].filter(e => e.state === 'ready' && !this.wanted.has(e.key)).sort((a, b) => a.used - b.used);
    for (const entry of candidates) {
      if (this.bytes <= this.budget && this.entries.size <= this.capacity) break;
      this.remove(entry);
    }
    // A pathological viewport must not pin unlimited decoded pixels. Preserve
    // already-visible pictures and decline extra ones rather than reload-thrash.
    for (const entry of [...this.entries.values()].filter(e => e.state === 'ready').sort((a, b) => b.used - a.used)) {
      if (this.bytes <= this.budget) break;
      this.bytes -= entry.image!.bytes; entry.image!.dispose(); entry.image = undefined;
      entry.state = 'error'; entry.error = '同屏图片超过预览预算，请单独打开原图';
    }
    const disposable = [...this.entries.values()].filter(e => e.state === 'error' && !this.wanted.has(e.key));
    for (const entry of disposable) if (this.entries.size > this.capacity) this.entries.delete(entry.key);
    // Large inline sources are compatibility-only. Do not retain their strings
    // after leaving the current cache/metadata window.
    for (const [source, key] of this.aliases) if (!this.entries.has(key)) { this.aliases.delete(source); this.metadata.delete(key); }
  }
  private remove(entry: Entry) {
    if (entry.image) { this.bytes -= entry.image.bytes; entry.image.dispose(); }
    this.entries.delete(entry.key);
  }
  clear() {
    this.epoch++;
    if (this.timer !== undefined) clearTimeout(this.timer);
    this.timer = undefined;
    for (const entry of this.entries.values()) if (entry.image) entry.image.dispose();
    this.entries.clear(); this.metadata.clear(); this.aliases.clear(); this.wanted.clear(); this.bytes = 0;
  }
  stats() { return { entries: this.entries.size, bytes: this.bytes, active: this.active,
    queued: [...this.entries.values()].filter(e => e.state === 'queued').length, metadata: this.metadata.size }; }
}
