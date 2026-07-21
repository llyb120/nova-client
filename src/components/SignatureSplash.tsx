import { onCleanup, onMount, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { api } from "../ipc";
import { restoreSettled } from "../store";
import {
  setSignatureProgress,
  setSignatureVisible,
  signatureProgress,
} from "./signatureOverlay";
import "./SignatureSplash.css";

/** 描笔时长；之后稍作停留再固化。 */
const REVEAL_MS = 2600;
const SETTLE_HOLD_MS = 500;

/**
 * 启动签名：在输入框水印原位置、原样式上描笔——水印本体被斜边软刷从左到右揭开，
 * 笔尖光点沿基线游走；签完进度置回 null，水印就地固化。颜色与位置即水印本身。
 * 升级重启会先恢复之前的会话：等恢复有结论后，在最终显示的输入框水印上签。
 */
export function SignatureSplash() {
  let markEl: HTMLElement | null = null;
  let nameEl: Element | null = null;
  let raf = 0;
  let holdTimer: number | undefined;
  let waitInterval: number | undefined;
  let started = false;

  const stop = () => {
    cancelAnimationFrame(raf);
    window.clearTimeout(holdTimer);
    window.clearInterval(waitInterval);
    setSignatureProgress(null);
  };

  const beginStroke = () => {
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
    setSignatureVisible(true);
    raf = requestAnimationFrame(tick);
  };

  const start = () => {
    if (started) return;
    started = true;
    // 恢复结论刚出来时目标视图可能尚未挂载（如 ChatView 正在打开会话），逐帧重试定位水印。
    const locate = (attempts: number) => {
      const mark = document.querySelector<HTMLElement>(".composer-engraved-watermark");
      const r = mark?.getBoundingClientRect();
      if (mark && r && r.width > 0 && r.height > 0) {
        markEl = mark;
        nameEl = mark.querySelector(".engraved-number-mark-name");
        beginStroke();
        return;
      }
      if (attempts > 0) {
        raf = requestAnimationFrame(() => locate(attempts - 1));
      } else {
        setSignatureVisible(true);
      }
    };
    locate(60);
  };

  onMount(() => {
    void api
      .signaturePending()
      .then((value) => {
        if (!value) {
          setSignatureVisible(true);
          return;
        }
        // 只在升级恢复完成或确认无需恢复后定位目标水印，绝不提前在主页签名。
        waitInterval = window.setInterval(() => {
          if (restoreSettled()) {
            window.clearInterval(waitInterval);
            start();
          }
        }, 100);
      })
      .catch(() => setSignatureVisible(true));
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
