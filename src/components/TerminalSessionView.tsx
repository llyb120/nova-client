import { listen } from "@tauri-apps/api/event";
import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createSignal, onCleanup, onSettled, Show, untrack } from "solid-js";
import {
  buildAgyAskKeySteps,
  buildAgyAskSkipSteps,
  clampAgySplitRatio,
  DEFAULT_AGY_SPLIT_RATIO,
  findActivePendingAgyAsk,
  loadAgySplitRatio,
  mergeAgyTurnsWithOptimistic,
  reconcileOptimisticTurns,
  saveAgySplitRatio,
  type AgyAskSubmissionItem,
  type OptimisticAgyTurn,
  type TerminalKeyStep,
} from "../agyTerminalPrompt";
import { api } from "../ipc";
import { hasTerminalDraft } from "../promptDraft";
import { modelChoices, refreshAntigravityQuota, refreshThreads, setState, showToast, state } from "../store";
import { getTerminalGroup, sendTerminalCancel, sendTerminalInput, sendTerminalKeySteps } from "../terminalSessions";
import type { AgyAskBlock, AgyTurn } from "../types";
import { AntigravityQuotaBadge } from "./AntigravityQuotaBadge";
import { isRunningAgyPhase, TerminalAgyConversation, type AgyAgentPhase } from "./TerminalAgyConversation";
import { TerminalComposer } from "./TerminalComposer";
import { TitleBarPortal } from "./TitleBar";
import { TypewriterText } from "./TypewriterText";
import "./HtmlTurn.css";
import WorkspaceTerminal, { type TerminalAgentTone } from "./WorkspaceTerminal";
import { IconFolder, IconStar, IconStopwatch, IconTerminal } from "./icons";

export default function TerminalSessionView(props: { threadId: string }) {
  const currentMeta = () => state.threads.find((thread) => thread.id === props.threadId);
  const cwd = () => state.cwd || currentMeta()?.cwd || "";
  const title = () => state.title || currentMeta()?.title || "终端";
  const isAgyMode = () => currentMeta()?.terminalConfig?.mode === "antigravity";
  const agyAccountId = () => currentMeta()?.terminalConfig?.agyAccountId;
  const isAgySession = () => isAgyMode() || !!agyAccountId() || !!currentMeta()?.agyConversationId;
  const hasForwardedDraft = untrack(() => hasTerminalDraft(props.threadId));
  const showComposer = () => isAgySession() || hasForwardedDraft;
  const agyBypass = () => currentMeta()?.terminalConfig?.bypassPermissions !== false;
  const agyModel = () => currentMeta()?.terminalConfig?.model?.trim() || "";
  const agyModelLabel = () => {
    const id = agyModel();
    if (!id) return "";
    return modelChoices("antigravity").find((m) => m.value === id)?.name || id;
  };
  const agyStatusSummary = () => {
    const parts = ["Antigravity CLI"];
    if (agyAccountId()) {
      const account = state.antigravityAccounts?.accounts.find((acc) => acc.id === agyAccountId());
      parts.push(account?.label || account?.email || "独立账号");
    }
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
  const [idleSessionId, setIdleSessionId] = createSignal<string>();
  const [terminalInteracted, setTerminalInteracted] = createSignal(false);
  const [completedOnce, setCompletedOnce] = createSignal(false);
  const [splitOpen, setSplitOpen] = createSignal(true);
  const showWorldline = () => isAgySession() && splitOpen();
  const [splitRatio, setSplitRatio] = createSignal(loadAgySplitRatio());
  const [draggingSplit, setDraggingSplit] = createSignal(false);
  const [dragPreviewRatio, setDragPreviewRatio] = createSignal<number | null>(null);
  const [agyTurns, setAgyTurns] = createSignal<AgyTurn[]>([]);
  const [optimisticByThread, setOptimisticByThread] = createSignal<Record<string, OptimisticAgyTurn[]>>({});
  const [worldlineHintTurn, setWorldlineHintTurn] = createSignal(0);
  const [submittingAskKey, setSubmittingAskKey] = createSignal<string | null>(null);
  const [touchedAskKey, setTouchedAskKey] = createSignal<string | null>(null);
  let workspaceRef: HTMLDivElement | undefined;
  let terminalBodyRef: HTMLDivElement | undefined;
  let turnRequestSeq = 0;
  let optimisticSeq = 0;
  let optimisticPruneTimer: number | undefined;
  let askSubmitTimer: number | undefined;
  let stopSplitDrag: (() => void) | undefined;

  const pruneOptimisticForThread = (tid: string, turns: AgyTurn[]) => {
    setOptimisticByThread((prev) => {
      const list = prev[tid];
      if (!list || list.length === 0) return prev;
      const nextList = reconcileOptimisticTurns(turns, list);
      if (nextList.length === list.length) return prev;
      const next = { ...prev };
      if (nextList.length === 0) delete next[tid];
      else next[tid] = nextList;
      return next;
    });
  };

  const clearOptimisticForThread = (tid: string) => {
    setOptimisticByThread((prev) => {
      if (!prev[tid]?.length) return prev;
      const next = { ...prev };
      delete next[tid];
      return next;
    });
  };

  onCleanup(() => {
    stopSplitDrag?.();
    if (optimisticPruneTimer !== undefined) {
      window.clearTimeout(optimisticPruneTimer);
      optimisticPruneTimer = undefined;
    }
    if (askSubmitTimer !== undefined) {
      window.clearTimeout(askSubmitTimer);
      askSubmitTimer = undefined;
    }
  });

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
      if (!agy) {
        setAgyTurns([]);
        return;
      }
      const seq = ++turnRequestSeq;
      void api.getAgyTerminalTurns(tid)
        .then((turns) => {
          if (seq !== turnRequestSeq || props.threadId !== tid) return;
          setAgyTurns(turns);
          pruneOptimisticForThread(tid, turns);
        })
        .catch(() => {
          if (seq !== turnRequestSeq || props.threadId !== tid) return;
          setAgyTurns([]);
        });
    },
  );

  onSettled(() => {
    let disposed = false;
    const terminalBody = terminalBodyRef;
    const cancelAutoSend = () => setTerminalInteracted(true);
    const onTerminalKeyDown = (e: KeyboardEvent) => {
      setTerminalInteracted(true);
      const key = untrack(activeAskKey);
      if (!key) return;
      if (e.key === "Shift" || e.key === "Control" || e.key === "Alt" || e.key === "Meta") return;
      if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === "c" && activeTerminalTab()?.terminal.hasSelection()) {
        return;
      }
      setTouchedAskKey(key);
    };
    const onTerminalPaste = () => {
      setTerminalInteracted(true);
      const key = untrack(activeAskKey);
      if (key) setTouchedAskKey(key);
    };
    terminalBody?.addEventListener("keydown", onTerminalKeyDown, true);
    terminalBody?.addEventListener("paste", onTerminalPaste, true);
    terminalBody?.addEventListener("pointerdown", cancelAutoSend, true);
    let unlistenState: (() => void) | undefined;
    let unlistenTurns: (() => void) | undefined;
    void listen<{ threadId: string; sessionId: string; state: AgyAgentPhase }>(
      "terminal:agy-state",
      (event) => {
        if (disposed || event.payload.threadId !== props.threadId) return;
        const tid = event.payload.threadId;
        const next = event.payload.state;
        const prev = untrack(agyPhase);
        if (isRunningAgyPhase(next)) {
          setCompletedOnce(false);
          if (optimisticPruneTimer !== undefined) {
            window.clearTimeout(optimisticPruneTimer);
            optimisticPruneTimer = undefined;
          }
        } else if (next === "idle" && isRunningAgyPhase(prev)) {
          setCompletedOnce(true);
          void refreshAntigravityQuota(false, untrack(agyAccountId));
          if (optimisticPruneTimer !== undefined) window.clearTimeout(optimisticPruneTimer);
          optimisticPruneTimer = window.setTimeout(() => {
            optimisticPruneTimer = undefined;
            if (!disposed) clearOptimisticForThread(tid);
          }, 1500);
        } else if (next === "error") {
          setCompletedOnce(false);
          if (optimisticPruneTimer !== undefined) window.clearTimeout(optimisticPruneTimer);
          optimisticPruneTimer = window.setTimeout(() => {
            optimisticPruneTimer = undefined;
            if (!disposed) clearOptimisticForThread(tid);
          }, 1500);
        }
        setIdleSessionId(next === "idle" ? event.payload.sessionId : undefined);
        setAgyPhase(next);
      },
    )
      .then((dispose) => {
        if (disposed) dispose();
        else unlistenState = dispose;
      })
      .catch(() => {});
    void listen<{ threadId: string; conversationId: string; turns: AgyTurn[] }>(
      "terminal:agy-turns",
      (event) => {
        if (disposed || event.payload.threadId !== props.threadId) return;
        const tid = event.payload.threadId;
        const nextTurns = event.payload.turns ?? [];
        const prevTurns = untrack(agyTurns);
        const prevPendingId = findActivePendingAgyAsk(prevTurns)?.id;
        const nextPendingId = findActivePendingAgyAsk(nextTurns)?.id;
        if ((nextTurns.length > prevTurns.length || (nextPendingId && nextPendingId !== prevPendingId)) && !untrack(splitOpen)) {
          setWorldlineHintTurn((n) => n + 1);
        }
        setAgyTurns(nextTurns);
        pruneOptimisticForThread(tid, nextTurns);
      },
    )
      .then((dispose) => {
        if (disposed) dispose();
        else unlistenTurns = dispose;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      terminalBody?.removeEventListener("keydown", onTerminalKeyDown, true);
      terminalBody?.removeEventListener("paste", onTerminalPaste, true);
      terminalBody?.removeEventListener("pointerdown", cancelAutoSend, true);
      unlistenState?.();
      unlistenTurns?.();
    };
  });

  const startSplitResize = (e: PointerEvent) => {
    if (e.button !== 0 || !workspaceRef) return;
    e.preventDefault();
    stopSplitDrag?.();
    setDraggingSplit(true);
    setDragPreviewRatio(untrack(splitRatio));
    const onMove = (ev: PointerEvent) => {
      if (!workspaceRef) return;
      const rect = workspaceRef.getBoundingClientRect();
      if (rect.width <= 0) return;
      const next = clampAgySplitRatio((ev.clientX - rect.left) / rect.width);
      setDragPreviewRatio(next);
    };
    const cleanup = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onEnd);
      window.removeEventListener("pointercancel", onEnd);
      stopSplitDrag = undefined;
      setDraggingSplit(false);
    };
    const onEnd = () => {
      const finalRatio = untrack(dragPreviewRatio);
      cleanup();
      setDragPreviewRatio(null);
      if (finalRatio !== null) {
        setSplitRatio(finalRatio);
        saveAgySplitRatio(finalRatio);
      }
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onEnd);
    window.addEventListener("pointercancel", onEnd);
    stopSplitDrag = cleanup;
  };

  const resetSplitRatio = () => {
    setSplitRatio(DEFAULT_AGY_SPLIT_RATIO);
    saveAgySplitRatio(DEFAULT_AGY_SPLIT_RATIO);
  };

  const activeTerminalTab = () => getTerminalGroup(`thread:${props.threadId}`).tabs()[0];
  const terminalReady = () => activeTerminalTab()?.status() === "running";
  const autoSendState = () => {
    const tab = activeTerminalTab();
    if (terminalInteracted() || tab?.error() || tab?.status() === "error" || tab?.status() === "exited") return "cancelled" as const;
    // shortcut: 用已打开的会话和当前 PTY 的空闲事件判断就绪，有 CLI 专用输入信号后再放宽。
    return isAgyMode() && tab?.status() === "running" && idleSessionId() === tab.id && !!currentMeta()?.agyConversationId
      ? "ready" as const : "waiting" as const;
  };

  createEffect(
    () => [props.threadId, activeTerminalTab()?.status()] as const,
    ([tid, status]) => {
      if (status === "exited" || status === "error") {
        clearOptimisticForThread(tid);
      }
    },
  );

  const pendingOptimistic = () =>
    reconcileOptimisticTurns(agyTurns(), optimisticByThread()[props.threadId] ?? []);
  const displayedTurns = () =>
    mergeAgyTurnsWithOptimistic(agyTurns(), optimisticByThread()[props.threadId] ?? []);
  const hasPendingOptimistic = () => pendingOptimistic().length > 0;
  const canStop = () => isRunningAgyPhase(agyPhase()) || hasPendingOptimistic();
  const activePendingAsk = () => findActivePendingAgyAsk(displayedTurns());
  const activeAskKey = () => {
    const ask = activePendingAsk();
    return ask ? `${props.threadId}:${ask.id}` : null;
  };
  const threadScopedAskId = (scoped: string | null) => {
    const prefix = `${props.threadId}:`;
    return scoped && scoped.startsWith(prefix) ? scoped.slice(prefix.length) : null;
  };

  createEffect(
    () => [activeAskKey(), agyPhase()] as const,
    ([key, phase]) => {
      const sub = untrack(submittingAskKey);
      if (sub && (sub !== key || phase === "idle" || phase === "error")) {
        setSubmittingAskKey(null);
        if (askSubmitTimer !== undefined) {
          window.clearTimeout(askSubmitTimer);
          askSubmitTimer = undefined;
        }
      }
      const touched = untrack(touchedAskKey);
      if (touched && touched !== key) {
        setTouchedAskKey(null);
      }
    },
  );

  const sendAskSteps = async (ask: AgyAskBlock, steps: TerminalKeyStep[]) => {
    const tab = activeTerminalTab();
    const tid = props.threadId;
    const key = `${tid}:${ask.id}`;
    if (!tab || tab.status() !== "running" || agyPhase() !== "reviewing") {
      showToast("反重力 CLI 当前未处于等待回答状态");
      return;
    }
    if (activeAskKey() !== key || submittingAskKey() === key || touchedAskKey() === key) {
      return;
    }
    setSubmittingAskKey(key);
    if (askSubmitTimer !== undefined) window.clearTimeout(askSubmitTimer);
    askSubmitTimer = window.setTimeout(() => {
      askSubmitTimer = undefined;
      if (
        untrack(submittingAskKey) === key &&
        untrack(activeAskKey) === key &&
        untrack(agyPhase) === "reviewing"
      ) {
        setSubmittingAskKey(null);
        showToast("未检测到 CLI 响应，请在右侧终端确认");
      }
    }, 6000);
    const ok = await sendTerminalKeySteps(tab, steps);
    if (!ok) {
      if (askSubmitTimer !== undefined) {
        window.clearTimeout(askSubmitTimer);
        askSubmitTimer = undefined;
      }
      if (untrack(submittingAskKey) === key) setSubmittingAskKey(null);
      showToast("未能写入终端，请在右侧终端回答");
    }
  };

  const handleSubmitAsk = (ask: AgyAskBlock, submissions: AgyAskSubmissionItem[]) =>
    sendAskSteps(ask, buildAgyAskKeySteps(ask.questions, submissions));

  const handleSkipAsk = (ask: AgyAskBlock) =>
    sendAskSteps(ask, buildAgyAskSkipSteps(ask.questions.length));

  const handleFocusTerminal = () => {
    activeTerminalTab()?.terminal.focus();
  };

  const handleComposerSend = async (payload: string, submit: boolean): Promise<boolean> => {
    const tab = activeTerminalTab();
    if (!tab) return false;
    const tid = props.threadId;
    const agy = isAgySession();
    const timestamp = Date.now();
    const realLen = untrack(() => agyTurns().length);
    const sent = await sendTerminalInput(tab, payload, { submit, useTerminalPaste: !agy });
    if (sent && agy && submit && props.threadId === tid && !payload.trim().startsWith("/")) {
      setOptimisticByThread((prev) => {
        const current = reconcileOptimisticTurns(untrack(agyTurns), prev[tid] ?? []);
        const last = current[current.length - 1];
        const baseTurnCount = Math.max(realLen, last ? last.baseTurnCount + 1 : 0);
        return {
          ...prev,
          [tid]: [
            ...current,
            {
              id: ++optimisticSeq,
              prompt: payload,
              timestamp,
              baseTurnCount,
            },
          ],
        };
      });
    }
    return sent;
  };

  const handleStop = () => {
    const tab = activeTerminalTab();
    if (!tab) return;
    if (sendTerminalCancel(tab) && !isRunningAgyPhase(untrack(agyPhase))) {
      clearOptimisticForThread(props.threadId);
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
              <AntigravityQuotaBadge model={agyModel()} accountId={agyAccountId()} />
              <button
                type="button"
                class={["chat-action-btn", "terminal-worldline-toggle", { active: splitOpen() }]}
                title={splitOpen() ? "收起世界线分屏" : "展开世界线分屏（查看用户输入与 AI 最终结论）"}
                aria-expanded={splitOpen() ? "true" : "false"}
                onClick={() => setSplitOpen((open) => !open)}
              >
                <span class="chat-action-chevron" aria-hidden="true">
                  <IconStopwatch size={14} />
                  <Show keyed when={!splitOpen() && worldlineHintTurn()}>
                    <span class="repo-time-toggle-hand" />
                  </Show>
                </span>
                <span>世界线</span>
                <Show when={displayedTurns().length > 0}>
                  <span class="terminal-worldline-count">{displayedTurns().length}</span>
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
        <div ref={workspaceRef} class={["terminal-agy-workspace", { "split-open": showWorldline() }]}>
          <Show when={showComposer()}>
            <section
              class="terminal-agy-pane"
              style={{ width: showWorldline() ? `${(splitRatio() * 100).toFixed(2)}%` : undefined }}
              aria-label={showWorldline() ? "反重力 CLI 对话式世界线" : undefined}
            >
              <Show when={showWorldline()}>
                <TerminalAgyConversation
                  threadId={props.threadId}
                  turns={displayedTurns()}
                  agyPhase={agyPhase()}
                  hasConversationId={!!currentMeta()?.agyConversationId}
                  hasPendingOptimistic={hasPendingOptimistic()}
                  activeAskId={activePendingAsk()?.id ?? null}
                  submittingAskId={threadScopedAskId(submittingAskKey())}
                  touchedAskId={threadScopedAskId(touchedAskKey())}
                  terminalReady={terminalReady()}
                  onSubmitAsk={handleSubmitAsk}
                  onSkipAsk={handleSkipAsk}
                  onFocusTerminal={handleFocusTerminal}
                  onStop={handleStop}
                  onCollapse={() => setSplitOpen(false)}
                />
              </Show>
              <div class={["terminal-composer-dock", { "is-agy": isAgySession() }]}>
                <TerminalComposer
                  threadId={props.threadId}
                  agy={isAgySession()}
                  autoFocus={showWorldline()}
                  autoSendState={autoSendState()}
                  terminalReady={terminalReady()}
                  waitingAsk={!!activePendingAsk() && agyPhase() === "reviewing"}
                  canStop={canStop()}
                  onStop={handleStop}
                  onSend={handleComposerSend}
                />
              </div>
            </section>
          </Show>
          <Show when={showWorldline()}>
            <div
              class={["terminal-split-resizer", { dragging: draggingSplit() }]}
              role="separator"
              aria-orientation="vertical"
              aria-label="调整 CLI 与世界线分屏比例"
              title="拖拽调整分屏宽度，双击恢复 2:1"
              onPointerDown={startSplitResize}
              onDblClick={resetSplitRatio}
            />
            <Show when={dragPreviewRatio() !== null}>
              <div
                class="terminal-split-guide"
                style={{ left: `${(dragPreviewRatio()! * 100).toFixed(2)}%` }}
                aria-hidden="true"
              />
            </Show>
          </Show>
          <div ref={terminalBodyRef} class="terminal-session-body">
            <WorkspaceTerminal
              threadId={props.threadId}
              cwd={cwd()}
              initCommand={initCommand()}
              autoFocus={!showWorldline()}
              statusSummary={isAgyMode() ? agyStatusSummary() : undefined}
              agentStateLabel={agentState()?.label}
              agentStateTone={agentState()?.tone}
              fontSize={state.settings?.terminalFontSize ?? 13}
            />
          </div>
        </div>
      </div>
    </main>
  );
}
