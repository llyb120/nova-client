import { createSignal, For, onCleanup, Show, type JSX } from "solid-js";
import { api } from "../ipc";
import { setSearchReveal, state } from "../store";
import { agentLabel, agentShort } from "../utils";
import { IconSearch } from "./icons";

type Result = Awaited<ReturnType<typeof api.searchThreads>>[number];

/** 把 text 中与 query 相同（忽略大小写）的片段包成 <mark>。 */
function highlight(text: string, query: string) {
  const needle = query.toLowerCase();
  const lower = text.toLowerCase();
  const out: JSX.Element[] = [];
  let i = 0;
  // ponytail: toLowerCase 改变长度的少数字符（如 İ）会让下标错位，只影响高亮位置。
  for (let at = needle ? lower.indexOf(needle) : -1; at >= 0; at = lower.indexOf(needle, i)) {
    out.push(text.slice(i, at), <mark>{text.slice(at, at + needle.length)}</mark>);
    i = at + needle.length;
  }
  out.push(text.slice(i));
  return out;
}

/** 会话搜索弹框：只搜标题、用户输入和 AI 正文（后端过滤掉思考与工具）。 */
export function SessionSearchModal(props: { onOpen: (id: string) => Promise<void>; onClose: () => void }) {
  const [query, setQuery] = createSignal("");
  const [results, setResults] = createSignal<Result[]>([]);
  const [searched, setSearched] = createSignal(false);
  let timer: number | undefined;
  let seq = 0;
  onCleanup(() => window.clearTimeout(timer));

  const search = (value: string) => {
    setQuery(value);
    window.clearTimeout(timer);
    const current = ++seq;
    if (!value.trim()) {
      setResults([]);
      setSearched(false);
      return;
    }
    timer = window.setTimeout(() => {
      void api.searchThreads(value.trim())
        .catch(() => [])
        .then((hits) => {
          if (current !== seq) return;
          setResults(hits);
          setSearched(true);
        });
    }, 200);
  };

  const open = async (threadId: string, itemId?: number) => {
    const q = query().trim();
    props.onClose();
    await props.onOpen(threadId);
    if (itemId != null) setSearchReveal({ threadId, itemId, query: q });
  };

  return (
    <div class="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && props.onClose()}>
      <div class="modal session-search-modal" role="dialog" aria-label="搜索会话">
        <label class="session-search-input">
          <IconSearch size={15} />
          <input
            type="search"
            placeholder="搜索标题、提问和回复…"
            value={query()}
            ref={(el) => queueMicrotask(() => el.focus())}
            onInput={(e) => search(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") props.onClose();
              const first = results()[0];
              if (e.key === "Enter" && first) void open(first.threadId, first.hits[0]?.itemId);
            }}
          />
        </label>
        <div class="session-search-results">
          <For
            each={results()}
            fallback={<Show when={searched()}><div class="session-search-empty">没有匹配的会话</div></Show>}
          >
            {(r) => {
              const meta = () => state.threads.find((t) => t.id === r.threadId);
              const title = () => meta()?.title || "未命名会话";
              return (
                <div class="session-search-thread">
                  <button type="button" class="session-search-title" onClick={() => void open(r.threadId)}>
                    <Show when={meta()}>
                      {(m) => (
                        <span class={`thread-agent ${m().agentKind}`} title={agentLabel(m().agentKind)}>
                          {agentShort(m().agentKind)}
                        </span>
                      )}
                    </Show>
                    <span class="session-search-title-text">
                      {r.titleHit ? highlight(title(), query().trim()) : title()}
                    </span>
                    <Show when={r.total > 0}>
                      <span class="session-search-count">{r.total} 处</span>
                    </Show>
                  </button>
                  <For each={r.hits}>
                    {(hit) => (
                      <button type="button" class="session-search-snippet" onClick={() => void open(r.threadId, hit.itemId)}>
                        {highlight(hit.snippet, query().trim())}
                      </button>
                    )}
                  </For>
                  <Show when={r.total > r.hits.length}>
                    <div class="session-search-more">还有 {r.total - r.hits.length} 处命中</div>
                  </Show>
                </div>
              );
            }}
          </For>
        </div>
      </div>
    </div>
  );
}
