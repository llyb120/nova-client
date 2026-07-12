import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { api } from "../ipc";
import { compactThread, chatScrollToBottomSignal, setState, state } from "../store";
import type { Item } from "../types";
import { agentLabel } from "../utils";
import { Composer } from "./Composer";
import { IconBroadcast, IconCompress, IconDownload, IconShare } from "./icons";
import { PermissionCard } from "./PermissionCard";
import { PlanActionCard } from "./PlanActionCard";
import { PlanCard } from "./PlanCard";
import { ShareModal } from "./ShareModal";
import { TypewriterText } from "./TypewriterText";
import { fmtTokens, type Group, groupItems, TurnGroup } from "./TurnGroup";

/**
 * transcript 虚拟化包裹层：长会话若把每一轮（含 Markdown 结论、工具卡片、diff）都常驻
 * DOM，节点数随会话线性增长，WebView2 渲染进程内存单调上涨直至崩溃。这里给每个轮次套一层
 * 轻量 wrapper（始终存在，成本仅一个 div），用 IntersectionObserver 判断是否临近视口：
 * 远离视口时卸载内部重内容、用等高占位撑住（滚动位置不跳），滚回来再挂载。
 * 正在流式输出的当前轮（active）永不卸载，避免高度剧变导致抖动。
 */
function VirtualGroup(props: {
  group: Group;
  active: boolean;
  scrollEl: () => HTMLElement | undefined;
}) {
  let ref: HTMLDivElement | undefined;
  const [visible, setVisible] = createSignal(true);
  const [height, setHeight] = createSignal(0);
  const mounted = () => visible() || props.active;

  onMount(() => {
    if (!ref) return;
    const io = new IntersectionObserver(
      (entries) => {
        const entry = entries[0];
        if (!entry) return;
        if (entry.isIntersecting) {
          setVisible(true);
        } else {
          // 卸载前记录真实高度（此刻内容仍挂载）：用等高占位替身，滚动条不跳。
          const h = entry.boundingClientRect.height;
          if (h > 0) setHeight(h);
          setVisible(false);
        }
      },
      // 视口上下各留 1200px 缓冲，减少快速滚动时的空白闪烁
      { root: props.scrollEl(), rootMargin: "1200px 0px" },
    );
    io.observe(ref);
    onCleanup(() => io.disconnect());
  });

  return (
    <div
      ref={ref}
      class="vgroup"
      style={!mounted() && height() > 0 ? { height: `${height()}px` } : undefined}
    >
      <Show when={mounted()}>
        <TurnGroup group={props.group} active={props.active} />
      </Show>
    </div>
  );
}

export function ChatView() {
  let scrollRef: HTMLDivElement | undefined;
  let innerRef: HTMLDivElement | undefined;
  let stickToBottom = true;

  const permissions = createMemo(() =>
    state.permissions.filter((p) => p.threadId === state.currentId),
  );

  const groups = createMemo<ReturnType<typeof groupItems>>(
    (prev) => groupItems(state.items as Item[], prev),
    [],
  );
  const isRunning = () => !!(state.currentId && state.running[state.currentId]);

  // 流式更新高频触发，用 rAF 去重：每帧至多读一次 scrollHeight（强制重排），避免抖动
  let scrollPending = false;
  const scrollToBottom = () => {
    if (scrollPending) return;
    scrollPending = true;
    requestAnimationFrame(() => {
      scrollPending = false;
      if (scrollRef && stickToBottom) scrollRef.scrollTop = scrollRef.scrollHeight;
    });
  };

  /** 发送新提示词：无论当前是否吸底，立刻跳到底部（无平滑滚动） */
  const jumpToBottomNow = () => {
    stickToBottom = true;
    if (!scrollRef) return;
    scrollRef.scrollTop = scrollRef.scrollHeight;
    // 新消息入 DOM 后高度可能再变，下一帧再钉一次
    requestAnimationFrame(() => {
      if (scrollRef && stickToBottom) scrollRef.scrollTop = scrollRef.scrollHeight;
    });
  };

  // 会话累计 token 用量
  const totalTokens = createMemo(() =>
    state.items.reduce(
      (sum, it) => (it.type === "turn" && it.totalTokens ? sum + it.totalTokens : sum),
      0,
    ),
  );

  const onScroll = () => {
    if (!scrollRef) return;
    const dist = scrollRef.scrollHeight - scrollRef.scrollTop - scrollRef.clientHeight;
    stickToBottom = dist < 60;
  };

  // 内容变化时自动吸底
  createEffect(() => {
    void state.items.length;
    const last = state.items[state.items.length - 1];
    if (last && "text" in last) void (last as { text: string }).text.length;
    void permissions().length;
    if (stickToBottom && scrollRef) {
      scrollToBottom();
    }
  });

  // 高度变化吸底兜底：Markdown 平滑出字的动画在 store 更新之后仍会持续增高内容，
  // （图片加载、工具卡片展开同理），仅靠上面的 store effect 会漏。用 ResizeObserver
  // 观察内容容器，任何来源的高度变化都触发吸底（scrollToBottom 内部已做 rAF 去重）。
  onMount(() => {
    if (!innerRef) return;
    const ro = new ResizeObserver(() => {
      if (stickToBottom) scrollToBottom();
    });
    ro.observe(innerRef);
    onCleanup(() => ro.disconnect());
  });

  // 切换线程时滚到底
  createEffect(() => {
    void state.currentId;
    stickToBottom = true;
    scrollToBottom();
  });

  // 会话中继续发送提示词：未在底部时也立刻跳到底（无过渡）
  createEffect(() => {
    const tick = chatScrollToBottomSignal();
    if (tick === 0) return;
    jumpToBottomNow();
  });

  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [showShare, setShowShare] = createSignal(false);

  const currentMeta = createMemo(() =>
    state.threads.find((t) => t.id === state.currentId),
  );
  const roamingRole = () => currentMeta()?.roamingRole ?? null;
  // worktree 会话的 cwd 是 uuid 工作目录，展示时用源仓库路径更直观
  const cwdDisplay = () => currentMeta()?.worktree?.repo || state.cwd;

  const startRename = () => {
    setDraft(state.title);
    setEditing(true);
  };

  // 漫游 guest：召回会话——host 自动把完整快照 Flow 回来，收件箱里选项目接收
  const [recalling, setRecalling] = createSignal(false);
  const recall = async () => {
    const id = state.currentId;
    if (!id || recalling()) return;
    setRecalling(true);
    try {
      await api.recallRoamingThread(id);
    } catch (e) {
      await message(String(e), { kind: "error" });
    } finally {
      setRecalling(false);
    }
  };

  const commitRename = async () => {
    setEditing(false);
    const id = state.currentId;
    const title = draft().trim();
    if (!id || !title || title === state.title) return;
    await api.renameThread(id, title);
    setState("title", title);
  };

  return (
    <main class="chat">
      <header class="chat-head">
        <Show
          when={editing()}
          fallback={
            <div class="chat-title" onDblClick={startRename} title="双击重命名">
              <TypewriterText
                text={state.title}
                title={state.title}
                animate={!!state.currentId && state.titleTyping[state.currentId]}
              />
            </div>
          }
        >
          <input
            class="chat-title-input"
            value={draft()}
            onInput={(e) => setDraft(e.currentTarget.value)}
            onBlur={() => void commitRename()}
            onKeyDown={(e) => {
              if (e.key === "Enter") void commitRename();
              if (e.key === "Escape") setEditing(false);
            }}
            ref={(el) => queueMicrotask(() => el.focus())}
          />
        </Show>
        <span class={`agent-badge ${state.agentKind}`}>
          {agentLabel(state.agentKind)}
        </span>
        <Show when={roamingRole()}>
          <span
            class={`roaming-badge ${roamingRole()}`}
            title={
              roamingRole() === "guest"
                ? `漫游中：在 ${currentMeta()?.roamingPeerName ?? "队友"} 的机器上执行`
                : `漫游中：替 ${currentMeta()?.roamingPeerName ?? "队友"} 在本机执行`
            }
          >
            <IconBroadcast size={11} />
            {roamingRole() === "guest"
              ? `漫游 @ ${currentMeta()?.roamingPeerName ?? "队友"}`
              : `代执行 · ${currentMeta()?.roamingPeerName ?? "队友"}`}
          </span>
        </Show>
        <div
          class="chat-cwd"
          title={
            currentMeta()?.worktree
              ? `源仓库：${currentMeta()!.worktree!.repo}\n分支：${currentMeta()!.worktree!.branch}${
                  state.cwd && state.cwd !== currentMeta()!.worktree!.repo
                    ? `\n工作目录：${state.cwd}`
                    : ""
                }`
              : state.cwd
          }
        >
          <Show when={currentMeta()?.worktree} fallback={state.cwd}>
            <span class="chat-cwd-repo">{currentMeta()!.worktree!.repo}</span>
            <span class="chat-cwd-wt">⎇ {currentMeta()!.worktree!.branch}</span>
          </Show>
        </div>
        <Show when={totalTokens() > 0}>
          <span class="chat-tokens" title="本会话累计 token 用量">
            {fmtTokens(totalTokens())} tokens
          </span>
        </Show>
        <Show when={state.currentId && state.running[state.currentId!]}>
          <span class="chat-running">
            <span class="spinner small" />
            运行中
          </span>
        </Show>
        <Show
          when={
            state.agentKind === "codex" &&
            !!state.currentId &&
            state.items.length > 0 &&
            roamingRole() !== "guest"
          }
        >
          <button
            class="chat-compact-btn"
            title="压缩上下文：把当前长历史浓缩为摘要，后续仅基于摘要继续，加快响应"
            disabled={isRunning()}
            onClick={() => void compactThread()}
          >
            <IconCompress size={14} />
            压缩
          </button>
        </Show>
        <Show when={state.relay.connected && state.currentId && roamingRole() !== "guest"}>
          <button
            class="chat-share-btn"
            title="用 Flow 把这个对话分享给队友"
            onClick={() => setShowShare(true)}
          >
            <IconShare size={14} />
            Flow
          </button>
        </Show>
        <Show when={state.relay.connected && state.currentId && roamingRole() === "guest"}>
          <button
            class="chat-share-btn"
            title={`把这段漫游会话拿回本机：${currentMeta()?.roamingPeerName ?? "对方"} 会自动回传完整快照（等价于对方 Flow 给你），到收件箱选择本地项目即可接收`}
            disabled={recalling()}
            onClick={() => void recall()}
          >
            <IconDownload size={14} />
            {recalling() ? "召回中…" : "召回"}
          </button>
        </Show>
      </header>
      <Show when={showShare() && state.currentId}>
        <ShareModal threadId={state.currentId!} onClose={() => setShowShare(false)} />
      </Show>

      <div class="transcript" ref={scrollRef} onScroll={onScroll}>
        <div class="transcript-inner" ref={innerRef}>
          <Show when={state.items.length === 0 && !state.loadingThread}>
            <div class="transcript-hint">
              在下方输入任务，{agentLabel(state.agentKind)} 将在{" "}
              <code>{cwdDisplay()}</code> 中工作。
            </div>
          </Show>
          <Show keyed when={state.currentId}>
            <For each={groups()}>
              {(g) => (
                <VirtualGroup
                  group={g}
                  // 运行中所有尚未闭合的轮次都保持活跃：补充提示词会新开一组，
                  // 若只标最后一组，前面仍在跑的工具/输出会像已停止。
                  active={isRunning() && !g.turn}
                  scrollEl={() => scrollRef}
                />
              )}
            </For>
          </Show>
          <For each={permissions()}>{(req) => <PermissionCard req={req} />}</For>
        </div>
      </div>

      <footer class="chat-foot">
        <Show when={state.plan && state.plan.length > 0}>
          <PlanCard plan={state.plan!} />
        </Show>
        <PlanActionCard />
        <Composer />
      </footer>
    </main>
  );
}
