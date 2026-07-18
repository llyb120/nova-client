import { state } from "../store";

/** 渔翁垂钓的水墨剪影（扁舟 + 斗笠渔翁 + 钓竿钓线 + 微澜），供船身与水中倒影复用 */
function FishingBoat(props: { class?: string }) {
  return (
    <svg
      class={`ink-boat ${props.class ?? ""}`}
      viewBox="0 0 140 72"
      fill="none"
      stroke="currentColor"
      stroke-width="2.2"
      stroke-linecap="round"
      stroke-linejoin="round"
    >
      {/* 钓竿与垂下的钓线 */}
      <path d="M66 36 L122 13" />
      <path d="M122 13 L122 47" stroke-width="1.1" />
      {/* 渔翁：斗笠与蜷坐的身躯 */}
      <path d="M52 33 Q64 23 78 33 Q65 30 52 33 Z" fill="currentColor" stroke="none" />
      <path d="M63 32 L67 30 L71 32 L68 33 Z" fill="currentColor" stroke="none" />
      <path
        d="M61 34 Q57 45 66 47 Q72 47 71 39 Q70 35 67 34 Z"
        fill="currentColor"
        stroke="none"
      />
      {/* 一叶扁舟 */}
      <path
        d="M20 50 Q70 67 116 49 Q92 56 70 56 Q44 56 24 50 Z"
        fill="currentColor"
        stroke="none"
      />
      {/* 落钩处的一圈微澜 */}
      <path d="M114 49 Q122 52 130 49" stroke-width="1.1" opacity="0.7" />
    </svg>
  );
}

/**
 * 水墨山水背景：远山静立，海面上潮起潮落。
 * 纯装饰层：fixed 铺满、置于内容之下、pointer-events:none 不拦截任何交互；
 * 动画只用 transform / opacity（合成层，几乎不耗性能），并尊重 prefers-reduced-motion。
 * 夜间海面添一叶垂钓渔舟，仅在首页显现。
 */
export function AmbientScene() {
  return (
    <div
      class="ink-scene"
      classList={{ "is-home": state.currentId === null }}
      aria-hidden="true"
    >
      <div class="ink-mountains" />
      <div class="ink-sea">
        <div class="ink-wave ink-wave-far">
          <span class="ink-wave-tile" />
        </div>
        <div class="ink-wave ink-wave-near">
          <span class="ink-wave-tile" />
        </div>
        <div class="ink-boat-wrap">
          <FishingBoat />
          <FishingBoat class="ink-boat-reflection" />
        </div>
      </div>
    </div>
  );
}
