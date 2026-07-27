// theme.ts — canvas 绘制的主题来源：读取 :root 上的 CSS 变量（与 DOM 完全同一份配色），
// 主题切换时失效缓存。颜色混合实现 CSS color-mix(in srgb, ...) 的子集，供 app.css 中
// 大量 color-mix 声明在 canvas 侧复算。

export interface Palette {
  bg: string;
  bgSidebar: string;
  bgPanel: string;
  bgHover: string;
  bgActive: string;
  bgFloat: string;
  bgInput: string;
  border: string;
  borderLight: string;
  borderStrong: string;
  cardBorder: string;
  text: string;
  textDim: string;
  textMuted: string;
  textFaint: string;
  accent: string;
  accentDim: string;
  accent2: string;
  onAccent: string;
  blue: string;
  red: string;
  yellow: string;
  green: string;
  violet: string;
  cyan: string;
  seal: string;
  scroll: string;
  scrollHover: string;
  mono: string;
  sans: string;
  rSm: number;
  rMd: number;
  rLg: number;
}

type RGBA = [number, number, number, number];

export function parseColor(input: string): RGBA | null {
  const s = input.trim();
  let m = /^#([0-9a-f]{3,8})$/i.exec(s);
  if (m) {
    const h = m[1];
    if (h.length === 3 || h.length === 4) {
      const r = parseInt(h[0] + h[0], 16);
      const g = parseInt(h[1] + h[1], 16);
      const b = parseInt(h[2] + h[2], 16);
      const a = h.length === 4 ? parseInt(h[3] + h[3], 16) / 255 : 1;
      return [r, g, b, a];
    }
    if (h.length === 6 || h.length === 8) {
      const r = parseInt(h.slice(0, 2), 16);
      const g = parseInt(h.slice(2, 4), 16);
      const b = parseInt(h.slice(4, 6), 16);
      const a = h.length === 8 ? parseInt(h.slice(6, 8), 16) / 255 : 1;
      return [r, g, b, a];
    }
    return null;
  }
  m = /^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)(?:[\s,/]+([\d.]+%?))?\s*\)$/i.exec(s);
  if (m) {
    let a = 1;
    if (m[4] !== undefined) {
      a = m[4].endsWith("%") ? parseFloat(m[4]) / 100 : parseFloat(m[4]);
    }
    return [+m[1], +m[2], +m[3], a];
  }
  if (s === "transparent") return [0, 0, 0, 0];
  return null;
}

export function rgbaString([r, g, b, a]: RGBA): string {
  const cl = (v: number) => Math.max(0, Math.min(255, Math.round(v)));
  const ca = Math.max(0, Math.min(1, a));
  return a >= 1
    ? `rgb(${cl(r)},${cl(g)},${cl(b)})`
    : `rgba(${cl(r)},${cl(g)},${cl(b)},${Math.round(ca * 1000) / 1000})`;
}

/** color-mix(in srgb, a w%, b)：w 为 a 的占比（0-1）。b 缺省为 transparent。
 * CSS 语义：预乘 alpha 混合后按结果 alpha 反归一——与透明色混合只缩放 alpha 不加深。 */
export function mix(a: string, w: number, b = "transparent"): string {
  const ca = parseColor(a) ?? [0, 0, 0, 0];
  const cb = parseColor(b) ?? [0, 0, 0, 0];
  const wa = w;
  const wb = 1 - w;
  const alpha = ca[3] * wa + cb[3] * wb;
  if (alpha <= 0) return "rgba(0,0,0,0)";
  const r = (ca[0] * ca[3] * wa + cb[0] * cb[3] * wb) / alpha;
  const g = (ca[1] * ca[3] * wa + cb[1] * cb[3] * wb) / alpha;
  const bl = (ca[2] * ca[3] * wa + cb[2] * cb[3] * wb) / alpha;
  return rgbaString([r, g, bl, alpha]);
}

/** 把颜色压到指定透明度：color-mix(in srgb, c a%, transparent) 的简写 */
export function fade(c: string, alpha: number): string {
  const p = parseColor(c);
  if (!p) return c;
  return rgbaString([p[0], p[1], p[2], p[3] * alpha]);
}

let cached: Palette | null = null;
const listeners = new Set<() => void>();

function readPalette(): Palette {
  const cs = getComputedStyle(document.documentElement);
  const v = (name: string) => cs.getPropertyValue(name).trim();
  const px = (name: string) => parseFloat(v(name)) || 0;
  cached = {
    bg: v("--bg"),
    bgSidebar: v("--bg-sidebar"),
    bgPanel: v("--bg-panel"),
    bgHover: v("--bg-hover"),
    bgActive: v("--bg-active"),
    bgFloat: v("--bg-float"),
    bgInput: v("--bg-input"),
    border: v("--border"),
    borderLight: v("--border-light"),
    borderStrong: v("--border-strong"),
    cardBorder: v("--card-border"),
    text: v("--text"),
    textDim: v("--text-dim"),
    textMuted: v("--text-muted"),
    textFaint: v("--text-faint"),
    accent: v("--accent"),
    accentDim: v("--accent-dim"),
    accent2: v("--accent-2"),
    onAccent: v("--on-accent"),
    blue: v("--blue"),
    red: v("--red"),
    yellow: v("--yellow"),
    green: v("--green"),
    violet: v("--violet"),
    cyan: v("--cyan"),
    seal: v("--seal"),
    scroll: v("--scroll"),
    scrollHover: v("--scroll-hover"),
    mono: v("--mono") || "monospace",
    sans: v("--sans") || "sans-serif",
    rSm: px("--r-sm"),
    rMd: px("--r-md"),
    rLg: px("--r-lg"),
  };
  return cached;
}

export function theme(): Palette {
  return cached ?? readPalette();
}

export function onThemeChange(fn: () => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

let observed = false;
/** 启动主题监听（documentElement[data-theme] 变化时刷新色板并通知）。 */
export function watchTheme(): void {
  if (observed || typeof MutationObserver === "undefined") return;
  observed = true;
  new MutationObserver(() => {
    cached = null;
    readPalette();
    for (const fn of listeners) fn();
  }).observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
}

/** Web 字体（Inter/Noto/JetBrains）加载完成后触发一次重排：之前的测量不准。 */
export function onFontsReady(fn: () => void): void {
  if (typeof document !== "undefined" && document.fonts?.ready) {
    void document.fonts.ready.then(fn);
  }
}

/**
 * 字体「后续加载完成」监听：fonts.ready 只覆盖首屏在用字体；会话中后到的字重/字族
 * 加载完成时（FontFaceSet loadingdone）再触发一次——测量缓存必须失效重排，
 * 否则后备字体（偏窄）的测量结果会让行内代码背景/后续片段错位。
 */
export function onFontsLoadingDone(fn: () => void): () => void {
  if (typeof document === "undefined" || !document.fonts?.addEventListener) {
    return () => {};
  }
  const handler = () => fn();
  document.fonts.addEventListener("loadingdone", handler);
  return () => document.fonts.removeEventListener("loadingdone", handler);
}
