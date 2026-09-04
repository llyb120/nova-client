import type { JSX } from "solid-js";
import { createEffect, onCleanup, onMount } from "solid-js";

/**
 * 会话运行中的标题跳动：逐字上下波浪动画，节奏跟随 token 输出速率。
 * 用单个共享 rAF 逐帧积分相位而不是 CSS animation：CSS 改
 * animation-duration 会重算相位导致跳帧，速率每 0.5s 上报一次时尤其明显；
 * 这里周期只做平滑插值，相位永远连续，快慢切换都顺滑。
 */
const MAX_BOUNCING_CHARS = 30;
/** 相邻字符的相位差（周期比例），形成波浪推进感 */
const STEP_RATIO = 0.064;
const BASE_S = 1.4;
const MIN_S = 0.45;
const MAX_S = 1.6;
/** ≈45 tok/s 时周期为慢速档的一半 */
const HALF_RATE = 45;
const AMP_PX = 2.5;
/** 起跳段占整周期比例，其余时间停留（对齐原 keyframes 的 0→45% 起落、其余静止） */
const HOP_FRACTION = 0.45;

const periodFor = (rate: number) =>
  Math.min(MAX_S, Math.max(MIN_S, BASE_S / (1 + Math.max(0, rate) / HALF_RATE)));

/** 单周期波形：前半段起跳回落（sin 半波），后半段停留 */
const hop = (u: number) => (u < HOP_FRACTION ? Math.sin(Math.PI * (u / HOP_FRACTION)) : 0);

interface Bouncer {
  els: HTMLElement[];
  phase: number;
  period: number;
  rate: () => number;
}

const bouncers = new Set<Bouncer>();
let frame = 0;
let last = 0;
const reduceMotion =
  typeof matchMedia === "function" ? matchMedia("(prefers-reduced-motion: reduce)") : null;

function tick(now: number) {
  const dt = Math.min(0.05, (now - last) / 1000 || 0);
  last = now;
  for (const b of bouncers) {
    // 周期向目标值平滑插值（约 1s 收敛），速率上报的台阶被抹平成连续变速
    b.period += (periodFor(b.rate()) - b.period) * Math.min(1, dt * 3);
    b.phase = (b.phase + dt / b.period) % 1;
    for (let i = 0; i < b.els.length; i++) {
      const el = b.els[i];
      if (!el) continue;
      const u = b.phase - i * STEP_RATIO;
      const y = hop(u - Math.floor(u)) * -AMP_PX;
      el.style.transform = `translateY(${y.toFixed(2)}px)`;
    }
  }
  frame = bouncers.size ? requestAnimationFrame(tick) : 0;
}

export function TitleBounce(props: {
  text: string;
  title?: string;
  class?: string;
  /** 当前会话输出速率（tokens/s），0 / 缺省 = 慢速基础档 */
  tokensPerSec?: number;
}) {
  const chars = () => Array.from(props.text ?? "").slice(0, MAX_BOUNCING_CHARS);
  let bouncer: Bouncer | undefined;
  let host: HTMLSpanElement | undefined;
  const span = (ch: string, i: number, cls: string): JSX.Element => (
    <span class={cls}>{ch === " " ? "\u00a0" : ch}</span>
  );
  const collect = () => {
    if (!bouncer || !host) return;
    bouncer.els = Array.from(host.querySelectorAll<HTMLElement>(".tb-char"));
  };
  onMount(() => {
    if (reduceMotion?.matches) return;
    bouncer = { els: [], phase: Math.random(), period: periodFor(props.tokensPerSec ?? 0), rate: () => props.tokensPerSec ?? 0 };
    bouncers.add(bouncer);
    collect();
    if (!frame) {
      last = performance.now();
      frame = requestAnimationFrame(tick);
    }
  });
  // 标题变化时重建字符节点，重新收集引用
  createEffect(() => {
    chars();
    queueMicrotask(collect);
  });
  onCleanup(() => {
    if (bouncer) bouncers.delete(bouncer);
    if (!bouncers.size && frame) {
      cancelAnimationFrame(frame);
      frame = 0;
    }
  });
  return (
    <span class={`thread-title-bounce ${props.class ?? ""}`} title={props.title ?? props.text} ref={(el) => (host = el)}>
      {chars().map((ch, i) => span(ch, i, "tb-char"))}
      {props.text.length > MAX_BOUNCING_CHARS
        ? span("\u2026", MAX_BOUNCING_CHARS - 1, "tb-tail")
        : null}
    </span>
  );
}
