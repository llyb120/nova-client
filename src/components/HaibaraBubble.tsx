import { createSignal, Show } from "solid-js";
import { HAIBARA_QUOTES } from "../haibaraQuotes";

const pick = () => HAIBARA_QUOTES[Math.floor(Math.random() * HAIBARA_QUOTES.length)];

/** 新会话首页气泡：组件随 HomeView 每次挂载重新随机一句（点新对话/返回首页即触发）。
 *  × 关闭本轮展示，⟳ 换一句。 */
export function HaibaraBubble() {
  const [quote, setQuote] = createSignal<string | null>(pick());

  return (
    <Show when={quote() !== null}>
      <div class="haibara-bubble" role="status">
        <div class="haibara-bubble-head">
          <span class="haibara-bubble-name">灰原哀</span>
          <span class="haibara-bubble-hint">新会话问候</span>
          <span class="haibara-bubble-actions">
            <button type="button" title="换一句" onClick={() => setQuote(pick())}>
              ⟳
            </button>
            <button type="button" title="关闭" onClick={() => setQuote(null)}>
              ×
            </button>
          </span>
        </div>
        <p class="haibara-bubble-text">{quote()}</p>
      </div>
    </Show>
  );
}
