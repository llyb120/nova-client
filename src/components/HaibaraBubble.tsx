import { createSignal, Show } from "solid-js";
import { HAIBARA_QUOTES } from "../haibaraQuotes";

const pick = () => HAIBARA_QUOTES[Math.floor(Math.random() * HAIBARA_QUOTES.length)];

/** 新会话首页气泡：组件随 HomeView 每次挂载重新随机一句（点新对话/返回首页即触发）。
 *  × 关闭本轮展示。 */
export function HaibaraBubble() {
  const [quote, setQuote] = createSignal<string | null>(pick());

  return (
    <Show when={quote() !== null}>
      <div class="haibara-bubble" role="status">
        <button
          type="button"
          class="haibara-bubble-close"
          title="关闭"
          onClick={() => setQuote(null)}
        >
          ×
        </button>
        <p class="haibara-bubble-text">{quote()}</p>
      </div>
    </Show>
  );
}
