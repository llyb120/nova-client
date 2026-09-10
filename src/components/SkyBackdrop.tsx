import { For, Show, onCleanup, onMount } from "solid-js";
import { backdropPixelRatio, paintCanvasBackdrop, readBackdropTheme, STAR_MAP_UPDATE_MS } from "../canvasTranscript/base";

/** 每个公式或图形独占一个格子，统一漂移保证动画中也不相互遮挡。 */
const SCIENCE_TILES: Array<{ formula?: string; shape?: "axes" | "cube" | "polyhedron" }> = [
  { formula: "eⁱπ + 1 = 0" },
  { shape: "axes" },
  { formula: "E = mc²" },
  { formula: "∫₋∞⁺∞ e⁻ˣ² dx = √π" },
  { shape: "cube" },
  { formula: "Δx · Δp ≥ ℏ/2" },
  { formula: "a² + b² = c²" },
  { shape: "polyhedron" },
  { formula: "F = Gm₁m₂/r²" },
  { formula: "iℏ ∂ψ/∂t = Ĥψ" },
  { shape: "axes" },
  { formula: "∇ × E = −∂B/∂t" },
  { formula: "P(A | B) = P(B | A)P(A)/P(B)" },
  { shape: "polyhedron" },
  { formula: "S = kᵦ ln Ω" },
  { formula: "∑ₙ₌₀∞ xⁿ/n! = eˣ" },
  { formula: "F = ma" },
  { formula: "γ = 1/√(1 − v²/c²)" },
  { shape: "cube" },
  { formula: "∂²u/∂t² = c²∇²u" },
];

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
      <div class="dawn-backdrop" aria-hidden="true">
        <div class="science-field">
          <For each={SCIENCE_TILES}>
            {(tile) => (
              <svg class="science-tile" viewBox="0 0 400 160" fill="none">
                <Show when={tile.formula}>
                  <text class="science-equation" x="200" y="86" text-anchor="middle" font-size={(tile.formula?.length ?? 0) > 25 ? "21" : "30"}>
                    {tile.formula}
                  </text>
                </Show>
                <Show when={tile.shape === "axes"}>
                  <g class="science-geometry">
                    <path d="M90 118H315M200 140V20M307 113L315 118L307 123M195 28L200 20L205 28M120 114V122M160 114V122M240 114V122M280 114V122M196 48H204M196 82H204" />
                    <path d="M104 40Q200 194 296 40" />
                    <path class="science-hidden-edge" d="M110 82H290M160 30V135M240 30V135" />
                  </g>
                </Show>
                <Show when={tile.shape === "cube"}>
                  <g class="science-geometry">
                    <path d="M150 58L230 58L230 134L150 134ZM150 58L187 26L267 26L230 58M267 26V102L230 134" />
                    <path class="science-hidden-edge" d="M150 134L187 102L267 102M187 102V26" />
                  </g>
                </Show>
                <Show when={tile.shape === "polyhedron"}>
                  <g class="science-geometry">
                    <path d="M132 57L198 20L265 53L284 105L217 141L147 121ZM132 57L204 70L198 20M204 70L265 53M204 70L217 141M204 70L147 121M204 70L284 105" />
                    <path class="science-hidden-edge" d="M132 57L213 103L265 53M213 103L217 141M198 20L213 103" />
                  </g>
                </Show>
              </svg>
            )}
          </For>
        </div>
      </div>
    </>
  );
}
