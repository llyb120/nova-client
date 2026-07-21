import { onCleanup, onMount, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { api } from "../ipc";
import { setSignatureProgress, signatureProgress } from "./signatureOverlay";
import "./SignatureSplash.css";

/** 描笔时长；之后稍作停留再固化。 */
const REVEAL_MS = 2600;
const SETTLE_HOLD_MS = 500;

/**
 * 启动签名：在输入框水印原位置、原样式上描笔——水印本体被斜边软刷从左到右揭开，
 * 笔尖光点沿基线游走；签完进度置回 null，水印就地固化。颜色与位置即水印本身。
 */
export function SignatureSplash() {
  let markEl: HTMLElement | null = null;
  let nameEl: Element | null = null;
  let raf = 0;
  let holdTimer: number | undefined;

  const stop = () => {
    cancelAnimationFrame(raf);
    window.clearTimeout(holdTimer);
    setSignatureProgress(null);
  };

  onMount(() => {
    void api.signaturePending().then((value) => {
      if (!value) return;
      // 等一帧让首页水印渲染完再开始。
      requestAnimationFrame(() => {
        markEl = document.querySelector<HTMLElement>(".composer-engraved-watermark");
        if (!markEl) return;
        const r = markEl.getBoundingClientRect();
        if (r.width <= 0 || r.height <= 0) return;
        nameEl = markEl.querySelector(".engraved-number-mark-name");
        const start = performance.now();
        const tick = (now: number) => {
          const p = Math.min(1, (now - start) / REVEAL_MS);
          setSignatureProgress(p);
          if (p < 1) {
            raf = requestAnimationFrame(tick);
          } else {
            holdTimer = window.setTimeout(stop, SETTLE_HOLD_MS);
          }
        };
        setSignatureProgress(0);
        raf = requestAnimationFrame(tick);
      });
    });
  });

  onCleanup(stop);

  /**
   * 笔尖位置：跟随描笔前沿，纵向带轻微手抖的正弦起伏。
   * 每帧实时量水印位置——首页内容加载会引起布局位移，缓存坐标会跑偏。
   */
  const pen = () => {
    const p = signatureProgress();
    if (p === null || !markEl || !markEl.isConnected) return null;
    const r = markEl.getBoundingClientRect();
    const nr = (nameEl ?? markEl).getBoundingClientRect();
    return {
      x: r.left + p * r.width,
      y: nr.top + nr.height * 0.52 + Math.sin(p * Math.PI * 5) * nr.height * 0.14,
      done: p >= 1,
    };
  };

  return (
    // Portal 到 body：position:fixed 的参照系不被 .app 等祖先的 transform/filter 带偏。
    <Portal>
      <Show when={pen()}>
        {(pos) => (
          <div
            class={`signature-pen${pos().done ? " done" : ""}`}
            style={{ transform: `translate(${pos().x.toFixed(1)}px, ${pos().y.toFixed(1)}px)` }}
          />
        )}
      </Show>
    </Portal>
  );
}
