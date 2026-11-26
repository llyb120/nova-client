import { listen } from "@tauri-apps/api/event";
import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createSignal, For, onSettled, Show, untrack } from "solid-js";
import { api } from "../ipc";
import { modelChoices, refreshAntigravityQuota, refreshThreads, setState, state } from "../store";
import type { AgyUserPrompt } from "../types";
import { AntigravityQuotaBadge } from "./AntigravityQuotaBadge";
import { TitleBarPortal } from "./TitleBar";
import { TypewriterText } from "./TypewriterText";
import WorkspaceTerminal, { type TerminalAgentTone } from "./WorkspaceTerminal";
import { IconCopy, IconFolder, IconStar, IconStopwatch, IconTerminal } from "./icons";

type AgyAgentPhase = "idle" | "working" | "tool_use" | "reviewing" | "error";

function isRunningAgyPhase(phase: AgyAgentPhase | null): boolean {
  return phase === "working" || phase === "tool_use" || phase === "reviewing";
}

function formatPromptTime(ts: number): string {
  if (!ts) return "";
  const date = new Date(ts);
  if (Number.isNaN(date.getTime())) return "";
  const now = new Date();
  const hh = String(date.getHours()).padStart(2, "0");
  const mm = String(date.getMinutes()).padStart(2, "0");
  if (
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate()
  ) {
    return `${hh}:${mm}`;
  }
  const mon = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${mon}-${day} ${hh}:${mm}`;
}

export default function TerminalSessionView(props: { threadId: string }) {
  const currentMeta = () => state.threads.find((thread) => thread.id === props.threadId);
  const cwd = () => state.cwd || currentMeta()?.cwd || "";
  const title = () => state.title || currentMeta()?.title || "终端";
  const isAgyMode = () => currentMeta()?.terminalConfig?.mode === "antigravity";
  const isAgySession = () => isAgyMode() || !!currentMeta()?.agyConversationId;
  const agyBypass = () => currentMeta()?.terminalConfig?.bypassPermissions !== false;
  const agyModel = () => currentMeta()?.terminalConfig?.model?.trim() || "";
  const agyModelLabel = () => {
    const id = agyModel();
    if (!id) return "";
    return modelChoices("antigravity").find((m) => m.value === id)?.name || id;
  };
  const agyStatusSummary = () => {
    const parts = ["Antigravity CLI"];
    if (agyModelLabel()) parts.push(agyModelLabel());
    if (agyBypass()) parts.push("Bypass 模式");
    return parts.join(" · ");
  };
  const initCommand = () => {
    if (isAgyMode()) return "";
    const cfg = currentMeta()?.terminalConfig;
    return cfg != null ? (cfg.initCommand ?? "") : (state.settings?.terminalInitCommand ?? "");
  };
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [starUpdating, setStarUpdating] = createSignal(false);
  const [agyPhase, setAgyPhase] = createSignal<AgyAgentPhase | null>(null);
  const [completedOnce, setCompletedOnce] = createSignal(false);
  const [worldlineOpen, setWorldlineOpen] = createSignal(false);
  const [agyPrompts, setAgyPrompts] = createSignal<AgyUserPrompt[]>([]);
  const [expandedPromptIndex, setExpandedPromptIndex] = createSignal<number | null>(null);
  const [copiedPromptIndex, setCopiedPromptIndex] = createSignal<number | null>(null);
  const [worldlineHintTurn, setWorldlineHintTurn] = createSignal(0);
  let worldlineListRef: HTMLDivElement | undefined;
  let copyTimer: number | undefined;
  let promptRequestSeq = 0;

  createEffect(
    () => props.threadId,
    (tid) => {
      const running = untrack(() => !!state.running[tid]);
      setAgyPhase(running ? "working" : null);
      setCompletedOnce(false);
    },
  );

  createEffect(
    () => [props.threadId, currentMeta()?.agyConversationId, isAgySession()] as const,
    ([tid, , agy]) => {
      setExpandedPromptIndex(null);
      setCopiedPromptIndex(null);
      if (!agy) {
        setAgyPrompts([]);
        setWorldlineOpen(false);
        return;
      }
      const seq = ++promptRequestSeq;
      void api.getAgyTerminalPrompts(tid)
        .then((prompts) => {
          if (seq !== promptRequestSeq || props.threadId !== tid) return;
          setAgyPrompts(prompts);
        })
        .catch(() => {
          if (seq !== promptRequestSeq || props.threadId !== tid) return;
          setAgyPrompts([]);
        });
    },
  );

  createEffect(
    () => [agyPrompts().length, worldlineOpen()] as const,
    ([len, open]) => {
      if (open && len > 0 && worldlineListRef) {
        queueMicrotask(() => {
          if (worldlineListRef) worldlineListRef.scrollTop = worldlineListRef.scrollHeight;
        });
      }
    },
  );

  onSettled(() => {
    let disposed = false;
    let unlistenState: (() => void) | undefined;
    let unlistenPrompts: (() => void) | undefined;
    void listen<{ threadId: string; sessionId: string; state: AgyAgentPhase }>(
      "terminal:agy-state",
      (event) => {
        if (disposed || event.payload.threadId !== props.threadId) return;
        const next = event.payload.state;
        const prev = untrack(agyPhase);
        if (isRunningAgyPhase(next)) {
          setCompletedOnce(false);
        } else if (next === "idle" && isRunningAgyPhase(prev)) {
          setCompletedOnce(true);
          void refreshAntigravityQuota(false);
        } else if (next === "error") {
          setCompletedOnce(false);
        }
        setAgyPhase(next);
      },
    )
      .then((dispose) => {
        if (disposed) dispose();
        else unlistenState = dispose;
      })
      .catch(() => {});
    void listen<{ threadId: string; conversationId: string; prompts: AgyUserPrompt[] }>(
      "terminal:agy-prompts",
      (event) => {
        if (disposed || event.payload.threadId !== props.threadId) return;
        const nextPrompts = event.payload.prompts ?? [];
        const prevLen = untrack(() => agyPrompts().length);
        if (nextPrompts.length > prevLen && !untrack(worldlineOpen)) {
          setWorldlineHintTurn((n) => n + 1);
        }
        setAgyPrompts(nextPrompts);
      },
    )
      .then((dispose) => {
        if (disposed) dispose();
        else unlistenPrompts = dispose;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      unlistenState?.();
      unlistenPrompts?.();
      if (copyTimer !== undefined) window.clearTimeout(copyTimer);
    };
  });

  const copyPrompt = async (prompt: AgyUserPrompt) => {
    if (!navigator.clipboard) return;
    try {
      await navigator.clipboard.writeText(prompt.text);
      setCopiedPromptIndex(prompt.index);
      if (copyTimer !== undefined) window.clearTimeout(copyTimer);
      copyTimer = window.setTimeout(() => {
        copyTimer = undefined;
        setCopiedPromptIndex(null);
      }, 1500);
    } catch (error) {
      void message(String(error), { kind: "error" });
    }
  };

  const agentState = (): { label: string; tone: TerminalAgentTone } | undefined => {
    const phase = agyPhase();
    if (!isAgyMode() && phase === null) return undefined;
    switch (phase) {
      case "working":
        return { label: "执行中", tone: "running" };
      case "tool_use":
        return { label: "调用工具中", tone: "running" };
      case "reviewing":
        return { label: "等待确认", tone: "waiting" };
      case "error":
        return { label: "执行出错", tone: "error" };
      case "idle":
        return completedOnce()
          ? { label: "已完成", tone: "done" }
          : { label: "空闲", tone: "idle" };
      default:
        return { label: "空闲", tone: "idle" };
    }
  };

  const startRename = () => {
    setDraft(title());
    setEditing(true);
  };

  const commitRename = async () => {
    setEditing(false);
    const next = draft().trim();
    if (!next || next === title()) return;
    try {
      await api.renameThread(props.threadId, next);
      setState(s => {
        if (s.currentId === props.threadId) s.title = next;
        const thread = s.threads.find((item) => item.id === props.threadId);
        if (thread) thread.title = next;
      });
    } catch (error) {
      void message(String(error), { kind: "error" });
    }
  };

  const toggleStar = async () => {
    const meta = currentMeta();
    if (!meta || starUpdating()) return;
    const starred = !meta.starred;
    setStarUpdating(true);
    setState(s => {
      const thread = s.threads.find((item) => item.id === meta.id);
      if (thread) thread.starred = starred;
    });
    try {
      await api.setThreadStarred(meta.id, starred);
      void refreshThreads();
    } catch (error) {
      setState(s => {
        const thread = s.threads.find((item) => item.id === meta.id);
        if (thread) thread.starred = !starred;
      });
      void message(String(error), { kind: "error" });
    } finally {
      setStarUpdating(false);
    }
  };

  const openExternalTerminal = () => {
    const dir = cwd();
    if (!dir) return;
    void api.openInTerminal(dir).catch((error) => {
      void message(String(error), { kind: "error" });
    });
  };

  const openExplorer = () => {
    const dir = cwd();
    if (!dir) return;
    void api.openInExplorer(dir).catch((error) => {
      void message(String(error), { kind: "error" });
    });
  };

  return (
    <main class="chat terminal-session-view">
      <TitleBarPortal>
        <header class="chat-head">
          <div class="chat-context">
            <Show
              when={editing()}
              fallback={
                <div class="chat-title" onDblClick={startRename} title="双击重命名">
                  <TypewriterText
                    text={title()}
                    title={title()}
                    animate={!!state.titleTyping[props.threadId]}
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
            <Show when={currentMeta() && !currentMeta()?.roamingRole}>
              <button
                type="button"
                class={["chat-star", { starred: !!currentMeta()?.starred }]}
                title={currentMeta()?.starred ? "取消星标" : "加星标并在项目内置顶"}
                aria-pressed={!!currentMeta()?.starred ? "true" : "false"}
                onClick={() => void toggleStar()}
              >
                <IconStar size={15} filled={!!currentMeta()?.starred} />
              </button>
            </Show>
            <span
              class="agent-badge terminal"
              title={
                isAgyMode()
                  ? `反重力 CLI（${agyBypass() ? "Bypass 模式" : "标准权限"}${agyModelLabel() ? ` · 模型 ${agyModelLabel()}` : ""}）${
                      currentMeta()?.agyConversationId
                        ? ` · 已绑定会话 ${currentMeta()!.agyConversationId!.slice(0, 8)}（下次进入自动恢复）`
                        : ""
                    }`
                  : currentMeta()?.agyConversationId
                    ? `终端会话 · 已绑定 Antigravity 会话 ${currentMeta()!.agyConversationId!.slice(0, 8)}（下次进入自动恢复）`
                    : "终端会话"
              }
            >
              {isAgyMode() ? "反重力 CLI" : "终端"}
            </span>
            <div
              class="chat-cwd"
              title={
                currentMeta()?.worktree
                  ? `源仓库：${currentMeta()!.worktree!.repo}\n分支：${currentMeta()!.worktree!.branch}${
                      cwd() && cwd() !== currentMeta()!.worktree!.repo
                        ? `\n工作目录：${cwd()}`
                        : ""
                    }`
                  : cwd()
              }
            >
              <Show when={currentMeta()?.worktree} fallback={cwd()}>
                <span class="chat-cwd-repo">{currentMeta()!.worktree!.repo}</span>
                <span class="chat-cwd-wt">⎇ {currentMeta()!.worktree!.branch}</span>
              </Show>
            </div>
          </div>
          <div class="chat-actions">
            <Show when={isAgySession()}>
              <AntigravityQuotaBadge model={agyModel()} />
              <button
                type="button"
                class={["chat-action-btn", "terminal-worldline-toggle", { active: worldlineOpen() }]}
                title={worldlineOpen() ? "收起世界线" : "展开世界线（记录本会话的用户输入）"}
                aria-expanded={worldlineOpen() ? "true" : "false"}
                onClick={() => setWorldlineOpen((v) => !v)}
              >
                <span class="chat-action-chevron" aria-hidden="true">
                  <IconStopwatch size={14} />
                  <Show keyed when={!worldlineOpen() && worldlineHintTurn()}>
                    <span class="repo-time-toggle-hand" />
                  </Show>
                </span>
                <span>世界线</span>
                <Show when={agyPrompts().length > 0}>
                  <span class="terminal-worldline-count">{agyPrompts().length}</span>
                </Show>
              </button>
            </Show>
            <button
              type="button"
              class="chat-action-btn"
              title="在外部终端窗口中打开当前目录"
              disabled={!cwd()}
              onClick={openExternalTerminal}
            >
              <IconTerminal size={14} />
              <span>外部终端</span>
            </button>
            <button
              type="button"
              class="chat-action-btn"
              title="在资源管理器中打开当前目录"
              disabled={!cwd()}
              onClick={openExplorer}
            >
              <IconFolder size={14} />
              <span>资源管理器</span>
            </button>
          </div>
        </header>
      </TitleBarPortal>
      <div class="chat-shell">
        <div class="terminal-session-body">
          <WorkspaceTerminal
            threadId={props.threadId}
            cwd={cwd()}
            initCommand={initCommand()}
            statusSummary={
              isAgyMode() ? agyStatusSummary() : undefined
            }
            agentStateLabel={agentState()?.label}
            agentStateTone={agentState()?.tone}
            fontSize={state.settings?.terminalFontSize ?? 13}
          />
        </div>
        <Show when={isAgySession()}>
          <aside
            class={["repo-time-machine", "terminal-worldline", { collapsed: !worldlineOpen(), expanded: worldlineOpen() }]}
            aria-label="反重力 CLI 用户输入世界线"
          >
            <div class="repo-time-machine-label terminal-worldline-head" aria-hidden={!worldlineOpen() ? "true" : "false"}>
              <span class="terminal-worldline-title">世界线</span>
              <span class="terminal-worldline-meta">{agyPrompts().length} 条输入</span>
              <button
                type="button"
                class="repo-time-magic-clock"
                title="收起世界线"
                aria-label="收起世界线"
                tabindex={worldlineOpen() ? 0 : -1}
                onClick={() => setWorldlineOpen(false)}
              >
                <span class="repo-time-clock-face" aria-hidden="true">
                  <IconStopwatch size={17} />
                  <span class="repo-time-clock-hand" />
                </span>
              </button>
            </div>
            <div ref={worldlineListRef} class="terminal-worldline-list">
              <Show
                when={agyPrompts().length > 0}
                fallback={
                  <div class="terminal-worldline-empty">
                    {currentMeta()?.agyConversationId ? "暂无用户输入记录" : "发送首条消息后自动记录用户输入"}
                  </div>
                }
              >
                <div class="terminal-worldline-spine" aria-hidden="true" />
                <For each={agyPrompts()}>
                  {(prompt) => {
                    const expanded = () => expandedPromptIndex() === prompt.index;
                    const copied = () => copiedPromptIndex() === prompt.index;
                    return (
                      <div class={["terminal-worldline-item", { expanded: expanded() }]}>
                        <button
                          type="button"
                          class="terminal-worldline-card"
                          title={prompt.text}
                          onClick={() => setExpandedPromptIndex((cur) => (cur === prompt.index ? null : prompt.index))}
                        >
                          <span class="repo-time-dot terminal-worldline-dot">{prompt.index}</span>
                          <span class="terminal-worldline-body">
                            <span class="terminal-worldline-text">{prompt.text}</span>
                            <Show when={formatPromptTime(prompt.timestamp)}>
                              {(t) => <span class="terminal-worldline-time">{t()}</span>}
                            </Show>
                          </span>
                        </button>
                        <button
                          type="button"
                          class={["terminal-worldline-copy", { copied: copied() }]}
                          title="复制该条输入"
                          aria-label={`复制第 ${prompt.index} 条输入`}
                          tabindex={worldlineOpen() ? 0 : -1}
                          onClick={(e) => {
                            e.stopPropagation();
                            void copyPrompt(prompt);
                          }}
                        >
                          <Show when={copied()} fallback={<IconCopy size={12} />}>
                            <span>已复制</span>
                          </Show>
                        </button>
                      </div>
                    );
                  }}
                </For>
              </Show>
            </div>
          </aside>
        </Show>
      </div>
    </main>
  );
}
