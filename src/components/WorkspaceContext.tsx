import { createEffect, createMemo, createSignal, onCleanup, Show } from "solid-js";
import { api } from "../ipc";
import { state } from "../store";
import VirtualRows from "./VirtualRows";

type InspectMsg = { role: string; text: string };
type Inspect = Awaited<ReturnType<typeof api.inspectThreadContext>>;

function roleLabel(role: string): string {
  switch (role) {
    case "user": return "用户";
    case "assistant": return "助手";
    case "system": return "系统";
    case "toolResult": return "工具结果";
    case "tool": return "工具";
    default: return role || "消息";
  }
}

function fmt(n: number): string {
  return n.toLocaleString("zh-CN");
}

export function MessageBlock(props: { message: InspectMsg; open: boolean }) {
  // 正文只在展开时才进 DOM：一格几百 KB 的工具结果展开着挂上去，整个面板就会被拖卡。
  const [shown, setShown] = createSignal(props.open);
  return (
    <details
      class="workspace-context-message"
      open={props.open}
      onToggle={event => setShown((event.currentTarget as HTMLDetailsElement).open)}
    >
      <summary>
        <span class="workspace-context-role">{roleLabel(props.message.role)}</span>
        <span class="workspace-context-preview">{props.message.text.slice(0, 80) || "（空）"}</span>
      </summary>
      <Show when={shown()}><pre>{props.message.text || "（空）"}</pre></Show>
    </details>
  );
}

export default function WorkspaceContext(props: { threadId: string }) {
  const [data, setData] = createSignal<Inspect | null>(null);
  const [error, setError] = createSignal("");
  const [loading, setLoading] = createSignal(false);

  const load = async () => {
    setLoading(true);
    setError("");
    try {
      setData(await api.inspectThreadContext(props.threadId));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setData(null);
    } finally {
      setLoading(false);
    }
  };

  let timer: ReturnType<typeof setTimeout> | undefined;
  createEffect(
    () => [props.threadId, state.items.length, !!state.running[props.threadId]] as const,
    () => {
      clearTimeout(timer);
      if (!data()) { void load(); return; }
      // 每多一条消息就整份重取（后端要读整个会话文件并重新计 token）会拖卡界面，
      // 合并成一次尾部刷新；需要即时结果时还有刷新按钮。
      timer = setTimeout(() => void load(), 400);
    },
  );
  onCleanup(() => clearTimeout(timer));

  const memory = () => data()?.projectMemory;
  // 只带 toolCall、没有文字的消息（每次工具调用都有一条）显示出来只是「助手（空）」，跳过不显示。
  const shown = (messages: InspectMsg[] = []) => messages.filter(message => message.text.trim());
  const prefix = createMemo(() => shown(data()?.prefix));
  const workspace = createMemo(() => shown(data()?.workspace));

  return (
    <div class="workspace-context">
      <header class="workspace-toolbar">
        <span class="workspace-context-title">模型当前上下文</span>
        <button type="button" disabled={loading()} onClick={() => void load()}>刷新</button>
      </header>
      <div class="workspace-context-scroll">
        <Show when={error()}>
          <p class="workspace-empty workspace-error">{error()}</p>
        </Show>
        <Show when={!error() && data()?.emptyReason}>
          <p class="workspace-empty">{data()!.emptyReason}</p>
        </Show>
        <Show when={!error() && data() && !data()!.emptyReason}>
          <>
            <Show when={memory()}>
              {mem => (
                <p class="workspace-context-stats">
                  项目 {mem().projectKey} · 前缀 {fmt(mem().prefixTokens)} · 工作区 {fmt(mem().workspaceUsed)} / {fmt(mem().workspaceWindow)}
                </p>
              )}
            </Show>
            <Show when={data()!.memoryNote}>
              <p class="workspace-context-note">{data()!.memoryNote}</p>
            </Show>
            <details class="workspace-context-section" open>
              <summary>系统提示</summary>
              <Show when={data()!.systemPrompt} fallback={<p class="workspace-context-note">系统提示尚未记录。新开一轮对话后会出现在这里。</p>}>
                <pre class="workspace-context-system">{data()!.systemPrompt}</pre>
              </Show>
            </details>
            <details class="workspace-context-section workspace-context-prefix" open>
              <summary>项目记忆前缀<span class="workspace-context-hint">本会话内固定，不会随对话改写</span></summary>
              <Show when={prefix().length} fallback={<p class="workspace-context-note">没有项目记忆前缀。</p>}>
                <VirtualRows items={prefix()} row={message => <MessageBlock message={message} open />} />
              </Show>
            </details>
            <details class="workspace-context-section workspace-context-workspace" open>
              <summary>工作区<span class="workspace-context-hint">会随对话增长或压缩变化</span></summary>
              <Show when={workspace().length} fallback={<p class="workspace-context-note">工作区还是空的。</p>}>
                <VirtualRows
                  items={workspace()}
                  row={message => <MessageBlock message={message} open={message.text.length <= 800} />}
                />
              </Show>
            </details>
          </>
        </Show>
      </div>
    </div>
  );
}
