import { listen } from "@tauri-apps/api/event";
import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createSignal, onCleanup, onSettled, Show, untrack } from "solid-js";
import {
  buildAgyAskKeySteps,
  buildAgyAskSkipSteps,
  clampAgySplitRatio,
  DEFAULT_AGY_SPLIT_RATIO,
  detectDroppedFirstChar,
  findActivePendingAgyAsk,
  formatTerminalPromptWithPaths,
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
import {
  dispatchTerminalQueuedPrompt,
  enqueueTerminalPrompt,
  holdTerminalPromptQueue,
  registerTerminalQueueDispatcher,
  releaseTerminalPromptQueue,
  removeTerminalQueuedPrompt,
  setTerminalQueueViewBusy,
  terminalQueueDispatchingIds,
  terminalQueueFailedIds,
  terminalQueueHeldThreadIds,
  terminalQueuedPrompts,
  type TerminalQueuedPrompt,
} from "../terminalPromptQueue";
import {
  getTerminalGroup,
  sendTerminalCancel,
  sendTerminalInput,
  sendTerminalKeySteps,
  waitForTerminalIdleSettle,
  type TerminalTab,
} from "../terminalSessions";
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
  const isDevinMode = () => currentMeta()?.terminalConfig?.mode === "devin";
  const agyAccountId = () => currentMeta()?.terminalConfig?.agyAccountId;
  const cliKind = () =>
    isAgyMode() || !!agyAccountId() || !!currentMeta()?.agyConversationId
      ? ("antigravity" as const)
      : isDevinMode() || !!currentMeta()?.devinSessionId
        ? ("devin" as const)
        : null;
  const isAgySession = () => cliKind() === "antigravity";
  const isDevinSession = () => cliKind() === "devin";
  const isCliSession = () => cliKind() !== null;
  const cliDisplayName = () => (isDevinSession() ? "Devin CLI" : "反重力 CLI");
  const cliConversationId = () =>
    isDevinSession() ? currentMeta()?.devinSessionId : currentMeta()?.agyConversationId;
  const hasForwardedDraft = untrack(() => hasTerminalDraft(props.threadId));
  const showComposer = () => isCliSession() || hasForwardedDraft;
  const agyBypass = () => currentMeta()?.terminalConfig?.bypassPermissions !== false;
  const agyModel = () => currentMeta()?.terminalConfig?.model?.trim() || "";
  const [devinModelLabels, setDevinModelLabels] = createSignal<Record<string, string>>({});
  const agyModelLabel = () => {
    const id = agyModel();
    if (!id) return "";
    return modelChoices("antigravity").find((m) => m.value === id)?.name || id;
  };
  const devinModelLabel = () => {
    const id = agyModel();
    if (!id) return "";
    return devinModelLabels()[id] || id;
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
  const devinStatusSummary = () => {
    const parts = ["Devin CLI"];
    if (devinModelLabel()) parts.push(devinModelLabel());
    if (agyBypass()) parts.push("Bypass 模式");
    return parts.join(" · ");
  };
  const initCommand = () => {
    if (isAgyMode() || isDevinMode()) return "";
    const cfg = currentMeta()?.terminalConfig;
    return cfg != null ? (cfg.initCommand ?? "") : (state.settings?.terminalInitCommand ?? "");
  };
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [starUpdating, setStarUpdating] = createSignal(false);
  const [agyPhase, setAgyPhase] = createSignal<AgyAgentPhase | null>(null);
  const [idleSessionId, setIdleSessionId] = createSignal<string>();
  const [settledIdleSessionId, setSettledIdleSessionId] = createSignal<string>();
  const [terminalInteracted, setTerminalInteracted] = createSignal(false);
  const [completedOnce, setCompletedOnce] = createSignal(false);
  const [splitOpen, setSplitOpen] = createSignal(true);
  const showWorldline = () => isCliSession() && splitOpen();
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
  let lastSentPrompt: {
    threadId: string;
    payload: string;
    sentAt: number;
    diagnostics: ReturnType<TerminalTab["getInputDiagnostics"]>;
  } | null = null;

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
      void api.getTerminalCliPhase(tid)
        .then((phase) => {
          if (props.threadId !== tid) return;
          if (!phase) {
            if (!untrack(() => !!state.running[tid])) {
              setAgyPhase(null);
            }
            return;
          }
          if (phase === "idle" || phase === "working" || phase === "tool_use" || phase === "reviewing" || phase === "error") {
            setAgyPhase(phase);
            if (phase === "idle") {
              const tab = untrack(activeTerminalTab);
              if (tab) setIdleSessionId(tab.id);
            }
          }
        })
        .catch(() => {});
    },
  );

  createEffect(
    () => [isDevinSession(), agyModel()] as const,
    ([devin, modelId]) => {
      if (!devin || !modelId || untrack(devinModelLabels)[modelId]) return;
      void api.getDevinTerminalModels(false)
        .then((opts) => {
          const map: Record<string, string> = {};
          for (const opt of opts?.configOptions?.[0]?.options ?? []) {
            if (opt?.value) map[opt.value] = opt.name || opt.value;
          }
          setDevinModelLabels(map);
        })
        .catch(() => {});
    },
  );

  createEffect(
    () => [props.threadId, cliConversationId(), isCliSession()] as const,
    ([tid, , cli]) => {
      if (!cli) {
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
          if (untrack(isAgySession)) {
            void refreshAntigravityQuota(false, untrack(agyAccountId));
          }
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
        if (nextTurns.length > prevTurns.length && lastSentPrompt) {
          const pending = lastSentPrompt;
          const now = Date.now();
          if (pending.threadId === tid && now - pending.sentAt < 15_000) {
            const latestPrompt = nextTurns[nextTurns.length - 1]?.prompt ?? "";
            if (detectDroppedFirstChar(pending.payload, latestPrompt)) {
              const diag = pending.diagnostics;
              console.warn("[agy-first-char-dropped]", {
                threadId: tid,
                firstChar: Array.from(pending.payload.trimStart())[0] ?? "",
                sentLength: pending.payload.length,
                elapsedMs: now - pending.sentAt,
                sinceLastOutputMs: diag.lastOutputAt > 0 ? diag.now - diag.lastOutputAt : null,
                sinceLastResizeMs: diag.lastResizeAt > 0 ? diag.now - diag.lastResizeAt : null,
                sinceLastCancelMs: diag.lastCancelAt > 0 ? diag.now - diag.lastCancelAt : null,
                sinceLastControlReplyMs: diag.lastControlReplyAt > 0 ? diag.now - diag.lastControlReplyAt : null,
                isConpty: diag.isConpty,
                sendFocusMode: diag.sendFocusMode,
                bracketedPasteMode: diag.bracketedPasteMode,
              });
            }
          }
          lastSentPrompt = null;
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

  createEffect(
    () =>
      [
        props.threadId,
        isAgyMode(),
        isDevinMode(),
        activeTerminalTab(),
        activeTerminalTab()?.status(),
        idleSessionId(),
        terminalInteracted(),
      ] as const,
    ([, agy, devin, tab, status, idleId, interacted]) => {
      setSettledIdleSessionId(undefined);
      if ((!agy && !devin) || !tab || status !== "running" || idleId !== tab.id || interacted) {
        return;
      }
      let cancelled = false;
      onCleanup(() => {
        cancelled = true;
      });
      void waitForTerminalIdleSettle(tab, {
        minDelayMs: 320,
        quietMs: 180,
        maxWaitMs: 1200,
        isCancelled: () => cancelled,
      }).then((settled) => {
        if (!cancelled && settled) {
          setSettledIdleSessionId(tab.id);
        }
      });
    },
  );

  const autoSendState = () => {
    const tab = activeTerminalTab();
    if (terminalInteracted() || tab?.error() || tab?.status() === "error" || tab?.status() === "exited") return "cancelled" as const;
    // shortcut: 除空闲信号外额外等待短暂输出静默，避开 TUI 初始化/resize 未稳态导致首字被吞。
    const readyByMode = isAgyMode() || isDevinMode();
    return readyByMode && tab?.status() === "running" && settledIdleSessionId() === tab.id
      ? "ready" as const : "waiting" as const;
  };

  createEffect(
    () => [props.threadId, activeTerminalTab()?.status()] as const,
    ([tid, status]) => {
      if (status === "exited" || status === "error") {
        clearOptimisticForThread(tid);
        setAgyPhase(null);
        setIdleSessionId(undefined);
      }
    },
  );

  const pendingOptimistic = () =>
    reconcileOptimisticTurns(agyTurns(), optimisticByThread()[props.threadId] ?? []);
  const displayedTurns = () =>
    mergeAgyTurnsWithOptimistic(agyTurns(), optimisticByThread()[props.threadId] ?? []);
  const hasPendingOptimistic = () => pendingOptimistic().length > 0;
  const canStop = () => isRunningAgyPhase(agyPhase()) || hasPendingOptimistic();
  const currentQueuedPrompts = () =>
    terminalQueuedPrompts().filter((item) => item.threadId === props.threadId);
  const currentQueueHeld = () => terminalQueueHeldThreadIds().has(props.threadId);

  createEffect(
    () => [props.threadId, canStop()] as const,
    ([tid, busy]) => {
      setTerminalQueueViewBusy(tid, busy);
      return () => {
        setTerminalQueueViewBusy(tid, false);
      };
    },
  );
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
    if (!tab || tab.status() !== "running" || agyPhase() === "idle" || agyPhase() === "error") {
      showToast(`${cliDisplayName()} 当前未处于等待回答状态`);
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
        untrack(agyPhase) !== "idle" &&
        untrack(agyPhase) !== "error"
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

  const isAgyStartupSettled = (tab: TerminalTab) => {
    const phase = untrack(agyPhase);
    return (
      untrack(settledIdleSessionId) === tab.id ||
      (phase !== null && phase !== "idle") ||
      untrack(agyTurns).length > 0
    );
  };

  const waitForAgyStartupReady = async (tid: string, tab: TerminalTab): Promise<boolean> => {
    if (!(untrack(isAgyMode) || untrack(isDevinMode)) || isAgyStartupSettled(tab)) return true;
    const deadline = Date.now() + 6000;
    while (Date.now() < deadline) {
      if (
        props.threadId !== tid ||
        tab.disposed ||
        tab.status() === "exited" ||
        tab.status() === "error" ||
        activeTerminalTab()?.id !== tab.id
      ) {
        return false;
      }
      if (isAgyStartupSettled(tab)) return true;
      await new Promise<void>((resolve) => window.setTimeout(resolve, 60));
    }
    return !tab.disposed && tab.status() === "running" && props.threadId === tid;
  };

  const sendPayloadToTerminal = async (
    payload: string,
    submitMode: boolean | "steer",
  ): Promise<boolean> => {
    const tab = activeTerminalTab();
    if (!tab) return false;
    const tid = props.threadId;
    if ((isAgyMode() || isDevinMode()) && !isAgyStartupSettled(tab)) {
      const settled = await waitForAgyStartupReady(tid, tab);
      if (!settled) return false;
    }
    const agy = isCliSession();
    const shouldSubmit = submitMode !== false;
    const timestamp = Date.now();
    const realLen = untrack(() => agyTurns().length);
    const trackDrop = agy && shouldSubmit && !payload.trim().startsWith("/");
    if (trackDrop) {
      lastSentPrompt = {
        threadId: tid,
        payload,
        sentAt: timestamp,
        diagnostics: tab.getInputDiagnostics(),
      };
    }
    const sent = await sendTerminalInput(tab, payload, {
      submit: submitMode,
      useTerminalPaste: !agy,
    });
    if (!sent && trackDrop && lastSentPrompt?.sentAt === timestamp) {
      lastSentPrompt = null;
    }
    if (sent && agy && shouldSubmit && props.threadId === tid && !payload.trim().startsWith("/")) {
      setTerminalQueueViewBusy(tid, true);
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

  const handleComposerSend = (payload: string, submit: boolean): Promise<boolean> => {
    if (submit && isCliSession()) {
      releaseTerminalPromptQueue(props.threadId);
    }
    return sendPayloadToTerminal(payload, submit);
  };

  const dispatchQueuedItem = async (
    item: TerminalQueuedPrompt,
    steerNow: boolean,
  ): Promise<boolean> => {
    if (item.threadId !== props.threadId) return false;
    const paths = item.images.length > 0
      ? await api.saveTerminalAttachments(item.images.map((image) => ({ ...image })))
      : [];
    if (paths.length !== item.images.length) {
      throw new Error("部分附件未能写入终端草稿");
    }
    const payload = formatTerminalPromptWithPaths(item.text, paths);
    if (!payload) return false;
    const submitMode: boolean | "steer" =
      steerNow && untrack(isAgySession) && untrack(canStop) ? "steer" : true;
    return sendPayloadToTerminal(payload, submitMode);
  };

  createEffect(
    () => props.threadId,
    (tid) => {
      if (!tid) return;
      return registerTerminalQueueDispatcher(tid, dispatchQueuedItem);
    },
  );

  const handleStop = () => {
    const tab = activeTerminalTab();
    if (!tab) return;
    holdTerminalPromptQueue(props.threadId);
    if (sendTerminalCancel(tab) && !isRunningAgyPhase(untrack(agyPhase))) {
      clearOptimisticForThread(props.threadId);
    }
  };

  const agentState = (): { label: string; tone: TerminalAgentTone } | undefined => {
    const phase = agyPhase();
    if (!isAgyMode() && !isDevinMode() && phase === null) return undefined;
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
                  : isDevinMode()
                    ? `Devin CLI（${agyBypass() ? "Bypass 模式" : "标准权限"}${devinModelLabel() ? ` · 模型 ${devinModelLabel()}` : ""}）${
                        currentMeta()?.devinSessionId
                          ? ` · 已绑定会话 ${currentMeta()!.devinSessionId}（下次进入自动恢复）`
                          : ""
                      }`
                    : currentMeta()?.agyConversationId
                      ? `终端会话 · 已绑定 Antigravity 会话 ${currentMeta()!.agyConversationId!.slice(0, 8)}（下次进入自动恢复）`
                      : currentMeta()?.devinSessionId
                        ? `终端会话 · 已绑定 Devin 会话 ${currentMeta()!.devinSessionId}（下次进入自动恢复）`
                        : "终端会话"
              }
            >
              {isAgyMode() ? "反重力 CLI" : isDevinMode() ? "Devin CLI" : "终端"}
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
            </Show>
            <Show when={isCliSession()}>
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
              aria-label={showWorldline() ? `${cliDisplayName()} 对话式世界线` : undefined}
            >
              <Show when={showWorldline()}>
                <TerminalAgyConversation
                  threadId={props.threadId}
                  turns={displayedTurns()}
                  cliLabel={cliDisplayName()}
                  allowInteractiveAsk={isAgySession()}
                  agyPhase={agyPhase()}
                  hasConversationId={!!cliConversationId()}
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
              <div class={["terminal-composer-dock", { "is-agy": isCliSession() }]}>
                <TerminalComposer
                  threadId={props.threadId}
                  agy={isCliSession()}
                  cliLabel={cliDisplayName()}
                  autoFocus={showWorldline()}
                  autoSendState={autoSendState()}
                  terminalReady={terminalReady()}
                  waitingAsk={!!activePendingAsk() && agyPhase() !== "idle" && agyPhase() !== "error"}
                  running={canStop()}
                  supportsSteer={isAgySession()}
                  queuedItems={currentQueuedPrompts()}
                  queueHeld={currentQueueHeld()}
                  dispatchingQueueIds={terminalQueueDispatchingIds()}
                  failedQueueIds={terminalQueueFailedIds()}
                  canStop={canStop()}
                  onStop={handleStop}
                  onSend={handleComposerSend}
                  onEnqueue={(text, images) => enqueueTerminalPrompt(props.threadId, text, images)}
                  onWithdrawQueued={(item) => removeTerminalQueuedPrompt(item.id)}
                  onSendQueuedNow={(item, steerNow) => {
                    void dispatchTerminalQueuedPrompt(item, steerNow);
                  }}
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
              statusSummary={isAgyMode() ? agyStatusSummary() : isDevinMode() ? devinStatusSummary() : undefined}
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
