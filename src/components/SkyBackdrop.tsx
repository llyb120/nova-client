import { onCleanup, onMount } from "solid-js";
import { backdropPixelRatio, paintCanvasBackdrop, readBackdropTheme, STAR_MAP_UPDATE_MS } from "../canvasTranscript/base";

/**
 * 全局星野背景：整扇窗口只有这一张画布，压在应用层之下，侧栏（历史会话）也能透出银河。
 * 暗色主题才画星图（paintStarMap 按底色亮度自行跳过浅色主题）。
 */
export function SkyBackdrop() {
  let canvasEl!: HTMLCanvasElement;
  onMount(() => {
    let resizeTimer: number | undefined;
    let disposed = false;
    const paint = () => {
      if (disposed || document.hidden) return;
      const canvas = canvasEl;
      const w = window.innerWidth;
      const h = window.innerHeight;
      if (!w || !h) return;
      // 画布正好铺满视口，投影不用再做坐标系换算（参考系即自身）。
      const dpr = backdropPixelRatio(w, h);
      const pixelW = Math.max(1, Math.round(w * dpr));
      const pixelH = Math.max(1, Math.round(h * dpr));
      if (canvas.width !== pixelW || canvas.height !== pixelH) {
        canvas.width = pixelW;
        canvas.height = pixelH;
      }
      const ctx = canvas.getContext("2d", { alpha: false });
      if (!ctx) return;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      // 星图按 30s 时桶缓存，新桶 idle 构建完成后经 onReady 重绘。
      paintCanvasBackdrop(ctx, w, h, readBackdropTheme(), Date.now(), paint, () => !disposed && !document.hidden);
    };
    paint();
    // 拖动窗口时先让 CSS 拉伸旧位图，停手后再按新尺寸重绘一次，避免连续重建整幅星图。
    const onResize = () => {
      if (resizeTimer !== undefined) window.clearTimeout(resizeTimer);
      resizeTimer = window.setTimeout(() => {
        resizeTimer = undefined;
        paint();
      }, 120);
    };
    window.addEventListener("resize", onResize);
    document.addEventListener("visibilitychange", paint);
    // 主题切换时立即按新配色重绘，不等低频星图定时器。
    const mo = new MutationObserver(paint);
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    const starMapTimer = window.setInterval(() => {
      if (!document.hidden) paint();
    }, STAR_MAP_UPDATE_MS);
    onCleanup(() => {
      disposed = true;
      if (resizeTimer !== undefined) window.clearTimeout(resizeTimer);
      window.removeEventListener("resize", onResize);
      document.removeEventListener("visibilitychange", paint);
      mo.disconnect();
      window.clearInterval(starMapTimer);
    });
  });

  return (
    <>
      <canvas ref={canvasEl} class="sky-backdrop" aria-hidden="true" />
      <div class="dawn-backdrop" aria-hidden="true" />
    </>
  );
}
