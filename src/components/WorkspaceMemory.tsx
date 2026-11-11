import { createEffect, createSignal, Show } from "solid-js";
import { api } from "../ipc";
import { state } from "../store";
import { MessageBlock } from "./WorkspaceContext";
import VirtualRows from "./VirtualRows";

type Inspect = Awaited<ReturnType<typeof api.inspectProjectMemory>>;

function fmtTime(ms: number): string {
  return new Date(ms).toLocaleString("zh-CN", { hour12: false });
}

export default function WorkspaceMemory(props: { threadId: string }) {
  const [data, setData] = createSignal<Inspect | null>(null);
  const [error, setError] = createSignal("");
  const [loading, setLoading] = createSignal(false);

  const load = async () => {
    setLoading(true);
    setError("");
    try {
      setData(await api.inspectProjectMemory(props.threadId));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setData(null);
    } finally {
      setLoading(false);
    }
  };

  createEffect(
    () => [props.threadId, state.cwd, !!state.dreaming[props.threadId]] as const,
    () => { void load(); },
  );

  return (
    <div class="workspace-context">
      <header class="workspace-toolbar">
        <span class="workspace-context-title">项目记忆</span>
        <button type="button" disabled={loading()} onClick={() => void load()}>刷新</button>
      </header>
      <div class="workspace-context-scroll">
        <Show when={error()}>
          <p class="workspace-empty workspace-error">{error()}</p>
        </Show>
        <Show when={!error() && data()}>
          <>
            <Show when={data()!.projectKey}>
              <p class="workspace-context-stats">
                项目 {data()!.projectKey}{data()!.updatedAt ? ` · 更新于 ${fmtTime(data()!.updatedAt!)}` : ""}
              </p>
            </Show>
            <Show when={data()!.emptyReason}>
              <p class="workspace-empty">{data()!.emptyReason}</p>
            </Show>
            <Show when={!data()!.emptyReason}>
              <>
                <details class="workspace-context-section workspace-context-frozen" open>
                  <summary>冻结区<span class="workspace-context-hint">跨会话复用，用到足够多次才会改</span></summary>
                  <Show when={data()!.frozen.length} fallback={<p class="workspace-context-note">还没有冻结记忆。</p>}>
                    <VirtualRows
                      items={data()!.frozen}
                      row={message => <MessageBlock message={message} open={message.text.length <= 4000} />}
                    />
                  </Show>
                </details>
                <details class="workspace-context-section workspace-context-shallow" open>
                  <summary>浅层记忆<span class="workspace-context-hint">用够几次才写入冻结区，一直没再用到的会被淘汰</span></summary>
                  <Show when={data()!.shallow.length} fallback={<p class="workspace-context-note">还没有浅层记忆。</p>}>
                    <VirtualRows
                      items={data()!.shallow}
                      row={message => <MessageBlock message={message} open={message.text.length <= 4000} />}
                    />
                  </Show>
                </details>
              </>
            </Show>
          </>
        </Show>
      </div>
    </div>
  );
}
