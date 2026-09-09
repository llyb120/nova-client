/**
 * 会话画布背景：静态底（纯色 + 柔光 + 点阵）叠加低频更新的星图，按尺寸/主题缓存成位图，
 * 热路径只做一次 drawImage。颜色从 CSS 变量解析，与 DOM 版主题（深/浅色）一致。
 */

import { paintStarMap } from "./starMap";

/** 星图背景低频更新间隔；恒星位移很小，30 秒步进不会产生可见跳动。 */
export const STAR_MAP_UPDATE_MS = 30_000;

/** 装饰性背景最多 4M 像素 / 2x；文字画布保持自己的原生分辨率。 */
export function backdropPixelRatio(width: number, height: number): number {
  return Math.min(window.devicePixelRatio || 1, 2, Math.sqrt(4_000_000 / Math.max(1, width * height)));
}

const SKY_BUCKET_MS = 30_000;

export interface BackdropTheme {
  bg: string;
  glowAccent: string;
  glowCyan: string;
  glowCorner: string;
  gridDot: string;
}

/** 从 CSS 变量读取背景主题，与 DOM 版主题（深/浅色）一致；星图仅暗色主题绘制。 */
export function readBackdropTheme(): BackdropTheme {
  const s = getComputedStyle(document.documentElement);
  const v = (n: string, fb: string) => s.getPropertyValue(n).trim() || fb;
  return {
    bg: v("--bg", "#1e1e1e"),
    glowAccent: v("--canvas-glow-accent", "rgba(0,122,204,.12)"),
    glowCyan: v("--canvas-glow-cyan", "rgba(79,193,233,.06)"),
    glowCorner: v("--canvas-glow-corner", "rgba(0,122,204,.05)"),
    gridDot: v("--grid-dot", "rgba(255,255,255,.05)"),
  };
}

const backdropCache = new Map<string, HTMLCanvasElement>();
const pendingBackdrops = new Set<string>();

type IdleWindow = Window & {
  requestIdleCallback?: (callback: IdleRequestCallback, options?: IdleRequestOptions) => number;
};

/**
 * 缓存里每张都是整幅设备位图：1080p@2x 约 20MB，4K 约 33MB。按「张数」限 8 会放行
 * 上百 MB，所以按实际像素内存回收，最多 4 张（够容纳两种画布尺寸各一个时桶）。
 */
const BACKDROP_CACHE_MAX_BYTES = 72 * 1024 * 1024;
const BACKDROP_CACHE_MAX_ENTRIES = 4;

function backdropBytes(surface: HTMLCanvasElement): number {
  return surface.width * surface.height * 4;
}

/** Map 保持插入序，从头删就是最旧的那张；至少留一张，避免新旧交替时空窗重建。 */
function trimBackdropCache(): void {
  let bytes = 0;
  for (const surface of backdropCache.values()) bytes += backdropBytes(surface);
  while (
    backdropCache.size > 1 &&
    (bytes > BACKDROP_CACHE_MAX_BYTES || backdropCache.size > BACKDROP_CACHE_MAX_ENTRIES)
  ) {
    const oldestKey = backdropCache.keys().next().value!;
    bytes -= backdropBytes(backdropCache.get(oldestKey)!);
    backdropCache.delete(oldestKey);
  }
}

function cacheBackdrop(groupKey: string, key: string, surface: HTMLCanvasElement): void {
  // 每个尺寸/主题只保留最新恒星时桶，避免旧桶占满小缓存后规律性 miss。
  for (const cachedKey of backdropCache.keys()) {
    if (cachedKey.startsWith(`${groupKey}|`) && cachedKey !== key) backdropCache.delete(cachedKey);
  }
  if (backdropCache.has(key)) backdropCache.delete(key);
  backdropCache.set(key, surface);
  trimBackdropCache();
}

/** 创建不含星图的静态底图（纯色底 + 柔光 + 点阵）。 */
function createBackdrop(
  width: number,
  height: number,
  pixelW: number,
  pixelH: number,
  dpr: number,
  theme: BackdropTheme,
): HTMLCanvasElement {
  const surface = document.createElement("canvas");
  surface.width = pixelW;
  surface.height = pixelH;
  const bg = surface.getContext("2d", { alpha: false })!;
  bg.setTransform(dpr, 0, 0, dpr, 0, 0);
  bg.fillStyle = theme.bg;
  bg.fillRect(0, 0, width, height);

  const radialEllipse = (
    x: number,
    y: number,
    radiusX: number,
    radiusY: number,
    color: string,
    fadeAt: number,
  ) => {
    bg.save();
    bg.translate(x, y);
    bg.scale(radiusX, radiusY);
    const gradient = bg.createRadialGradient(0, 0, 0, 0, 0, 1);
    gradient.addColorStop(0, color);
    gradient.addColorStop(fadeAt, "transparent");
    gradient.addColorStop(1, "transparent");
    bg.fillStyle = gradient;
    bg.fillRect(-2, -2, 4, 4);
    bg.restore();
  };
  // Canvas 是不透明表面，不能直接裁切位于其下方的 body::before；按相同比例在会话区复现。
  radialEllipse(width * 0.28, height * -0.14, 1000, 520, theme.glowAccent, 0.7);
  radialEllipse(width * 0.86, height * -0.1, 780, 460, theme.glowCyan, 0.7);
  radialEllipse(width * 1.04, height * 1.12, 820, 560, theme.glowCorner, 0.72);

  bg.fillStyle = theme.gridDot;
  const dot = 1 / dpr;
  for (let y = 0; y < height; y += 26) {
    for (let x = 0; x < width; x += 26) bg.fillRect(x, y, dot, dot);
  }
  return surface;
}

/** 创建已合成静态层和星图的背景位图。 */
function createCompleteBackdrop(
  width: number,
  height: number,
  pixelW: number,
  pixelH: number,
  dpr: number,
  theme: BackdropTheme,
  now: number,
): HTMLCanvasElement {
  const surface = createBackdrop(width, height, pixelW, pixelH, dpr, theme);
  const surfaceCtx = surface.getContext("2d", { alpha: false })!;
  surfaceCtx.setTransform(dpr, 0, 0, dpr, 0, 0);
  paintStarMap(surfaceCtx, width, height, theme, now);
  return surface;
}

/**
 * 绘制独立背景 Canvas：星图与静态层共用缓存位图，热路径只做一次 drawImage。
 * 分桶失效时先沿用旧位图，并在 idle 时生成新桶；首次绘制同步生成以避免空白。
 */
export function paintCanvasBackdrop(
  ctx: CanvasRenderingContext2D,
  width: number,
  height: number,
  theme: BackdropTheme,
  now = Date.now(),
  onReady?: () => void,
  canBuild: () => boolean = () => true,
): void {
  const dpr = backdropPixelRatio(width, height);
  const pixelW = Math.max(1, Math.round(width * dpr));
  const pixelH = Math.max(1, Math.round(height * dpr));
  const themeKey = [theme.bg, theme.glowAccent, theme.glowCyan, theme.glowCorner, theme.gridDot].join("|");
  const groupKey = [pixelW, pixelH, themeKey].join("|");
  const bucket = Math.floor(now / SKY_BUCKET_MS);
  const key = `${groupKey}|${bucket}`;
  let surface = backdropCache.get(key);
  if (!surface) {
    const previous = [...backdropCache.entries()].find(([cachedKey]) => cachedKey.startsWith(`${groupKey}|`))?.[1];
    if (!previous) {
      surface = createCompleteBackdrop(width, height, pixelW, pixelH, dpr, theme, now);
      cacheBackdrop(groupKey, key, surface);
    } else {
      surface = previous;
      if (!pendingBackdrops.has(key)) {
        pendingBackdrops.add(key);
        const build = () => {
          if (!canBuild()) {
            pendingBackdrops.delete(key);
            return;
          }
          const next = createCompleteBackdrop(width, height, pixelW, pixelH, dpr, theme, now);
          pendingBackdrops.delete(key);
          cacheBackdrop(groupKey, key, next);
          onReady?.();
        };
        const idleWindow = window as IdleWindow;
        if (idleWindow.requestIdleCallback) idleWindow.requestIdleCallback(build, { timeout: 2_000 });
        else window.setTimeout(build, 50);
      }
    }
  }
  ctx.drawImage(surface, 0, 0, pixelW, pixelH, 0, 0, width, height);
}
