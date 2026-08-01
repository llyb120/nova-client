/**
 * 通用工作流运行时：复用 /fire 的「会话接力」模式（一个阶段 = 一个独立会话，turn 事件
 * 驱动推进，支持暂停/恢复与持久化），但阶段与转移由 WorkflowDef 配置决定。
 *
 * 与 store.ts 解耦：store 通过 initWorkflowRuntime 注入少量 UI/状态能力，避免循环依赖。
 * /fire 仍走 store 里的原有专用路径；本运行时驱动 /run 启动的内置与自定义工作流。
 */
import { api } from "../ipc";
import type { Thread, Item } from "../types";
import {
  builtinPromptResolvers,
  builtinResumeResolvers,
  builtinTitleResolvers,
  type BuiltinStageContext,
} from "./builtin";
import { getWorkflow } from "./storage";
import {
  evalTransition,
  isTerminal,
  renderTemplate,
  validateWorkflow,
  WF_DONE,
  type WorkflowDef,
  type WorkflowRunStep,
  type WorkflowStageDef,
} from "./types";

export interface WorkflowHost {
  currentId(): string | null;
  isRunning(threadId: string): boolean;
  setRunning(threadId: string, value: boolean): void;
  refreshThreads(): Promise<void>;
  openThread(threadId: string): Promise<void>;
  bumpScrollToBottom(): void;
  clearProposedPlan(): void;
}

let host: WorkflowHost | null = null;

const activeRuns = new Map<string, WorkflowRunStep>();
const suspendedRuns = new Map<string, WorkflowRunStep>();
const runHistory = new Map<string, WorkflowRunStep>();
const latestThreadByRoot = new Map<string, string>();
const completedRoots = new Set<string>();
const RUNS_KEY = "fd:workflowRuns:v1";

type PersistedRuns = {
  runs: [string, WorkflowRunStep][];
  latest: [string, string][];
  completed: string[];
};

export function initWorkflowRuntime(injected: WorkflowHost): void {
  host = injected;
  restoreRuns();
}

function requireHost(): WorkflowHost {
  if (!host) throw new Error("工作流运行时尚未初始化");
  return host;
}

function persistRuns(): void {
  const runs = new Map(runHistory);
  for (const [id, run] of activeRuns) runs.set(id, run);
  for (const [id, run] of suspendedRuns) runs.set(id, run);
  const snapshot: PersistedRuns = {
    runs: [...runs],
    latest: [...latestThreadByRoot],
    completed: [...completedRoots],
  };
  try {
    localStorage.setItem(RUNS_KEY, JSON.stringify(snapshot));
  } catch {
    // 持久化失败不打断当前流程，本窗口内仍由内存状态跟踪。
  }
}

function restoreRuns(): void {
  try {
    const snapshot = JSON.parse(localStorage.getItem(RUNS_KEY) ?? "null") as PersistedRuns | null;
    if (!snapshot || !Array.isArray(snapshot.runs)) return;
    for (const [threadId, run] of snapshot.runs) {
      if (!threadId || !run?.rootId || !run.workflowId || !run.stageId) continue;
      // 重启时统一按暂停恢复，避免把半截回复直接送去下一阶段。
      suspendedRuns.set(threadId, run);
      runHistory.set(threadId, run);
    }
    for (const [rootId, threadId] of snapshot.latest ?? []) {
      if (rootId && threadId) latestThreadByRoot.set(rootId, threadId);
    }
    for (const rootId of snapshot.completed ?? []) if (rootId) completedRoots.add(rootId);
  } catch {
    localStorage.removeItem(RUNS_KEY);
  }
}

function stageConclusion(thread: Thread): string {
  return (
    [...thread.items]
      .reverse()
      .find(
        (item): item is Extract<Item, { type: "assistant" }> =>
          item.type === "assistant" && !!item.text.trim(),
      )
      ?.text.trim() ?? "（会话没有给出结论）"
  );
}

function resolvePrompt(
  def: WorkflowDef,
  stage: WorkflowStageDef,
  ctx: BuiltinStageContext,
): string {
  const resolver = builtinPromptResolvers[def.id]?.[stage.id];
  if (resolver) return resolver(ctx);
  return renderTemplate(stage.promptTemplate, {
    ...ctx.vars,
    prev: ctx.prev,
    attempt: ctx.attempt,
  });
}

function resolveTitle(
  def: WorkflowDef,
  stage: WorkflowStageDef,
  ctx: BuiltinStageContext,
  status = "",
): string {
  const resolver = builtinTitleResolvers[def.id]?.[stage.id];
  const base = resolver
    ? resolver(ctx)
    : renderTemplate(stage.titleTemplate ?? `[WF] ${stage.name} · 第{{attempt}}次`, {
        ...ctx.vars,
        prev: ctx.prev,
        attempt: ctx.attempt,
      });
  return status ? `${base} · ${status}` : base;
}

function runContext(run: WorkflowRunStep, prev: string): BuiltinStageContext {
  return { vars: run.vars, prev, attempt: run.attempts[run.stageId] ?? 1 };
}

function isViewingChain(rootId: string): boolean {
  const currentId = requireHost().currentId();
  if (!currentId) return false;
  if (currentId === rootId) return true;
  const run =
    runHistory.get(currentId) ?? activeRuns.get(currentId) ?? suspendedRuns.get(currentId);
  return run?.rootId === rootId;
}

async function createStageThread(
  run: WorkflowRunStep,
  stage: WorkflowStageDef,
  title: string,
  prompt: string,
): Promise<void> {
  const h = requireHost();
  const root = await api.getThread(run.rootId);
  const thread = await api.createThread(
    root.cwd,
    root.agentKind,
    root.model ?? null,
    stage.mode ?? "build",
    null,
    false,
    false,
    null,
    null,
    null,
    run.rootId,
  );
  await api.renameThread(thread.id, title);
  activeRuns.set(thread.id, run);
  runHistory.set(thread.id, run);
  latestThreadByRoot.set(run.rootId, thread.id);
  persistRuns();
  await h.refreshThreads();
  if (isViewingChain(run.rootId)) await h.openThread(thread.id);
  h.setRunning(thread.id, true);
  await api.sendPrompt(thread.id, prompt, []);
}

/** turn 正常结束后推进流程。 */
async function advanceWorkflow(threadId: string): Promise<void> {
  const h = requireHost();
  const run = activeRuns.get(threadId);
  if (!run) return;
  activeRuns.delete(threadId);
  persistRuns();

  const def = getWorkflow(run.workflowId);
  const stage = def?.stages.find((s) => s.id === run.stageId);
  if (!def || !stage) return;

  const thread = await api.getThread(threadId);
  const conclusion = stageConclusion(thread);
  const transition = evalTransition(stage, conclusion);

  if (!transition) {
    // 没有任何转移命中：停在当前阶段等用户补充。
    suspendedRuns.set(threadId, run);
    persistRuns();
    await api.renameThread(threadId, resolveTitle(def, stage, runContext(run, conclusion), "待补充"));
    await h.refreshThreads();
    return;
  }

  if (isTerminal(transition.to)) {
    const success = transition.to === WF_DONE;
    completedRoots.add(run.rootId);
    persistRuns();
    await api.renameThread(
      threadId,
      resolveTitle(def, stage, runContext(run, conclusion), success ? "完成" : "已停止"),
    );
    await h.refreshThreads();
    void api.notifyWorkflowDone(threadId, success).catch(() => {});
    return;
  }

  const next = def.stages.find((s) => s.id === transition.to);
  if (!next) return;

  if (run.stageCount + 1 > def.maxTotalStages) {
    completedRoots.add(run.rootId);
    persistRuns();
    await api.renameThread(
      threadId,
      resolveTitle(def, stage, runContext(run, conclusion), "已停止"),
    );
    await h.refreshThreads();
    void api.notifyWorkflowDone(threadId, false).catch(() => {});
    return;
  }

  run.stageId = next.id;
  run.stageCount += 1;
  run.attempts[next.id] = (run.attempts[next.id] ?? 0) + 1;
  const ctx = runContext(run, conclusion);
  await createStageThread(run, next, resolveTitle(def, next, ctx), resolvePrompt(def, next, ctx));
}

function suspendWorkflow(threadId: string, manual: boolean): void {
  const run = activeRuns.get(threadId);
  if (!run) return;
  activeRuns.delete(threadId);
  suspendedRuns.set(threadId, run);
  persistRuns();
  const def = getWorkflow(run.workflowId);
  const stage = def?.stages.find((s) => s.id === run.stageId);
  if (def && stage) {
    void api
      .renameThread(threadId, resolveTitle(def, stage, runContext(run, ""), manual ? "已暂停" : "异常暂停"))
      .then(() => requireHost().refreshThreads())
      .catch(() => {});
  }
}

/** turn 开始或用户补充消息时重新挂回流程；返回该 thread 的运行态（非工作流会话返回 null）。 */
function reattach(threadId: string): WorkflowRunStep | null {
  const run =
    activeRuns.get(threadId) ?? suspendedRuns.get(threadId) ?? runHistory.get(threadId);
  if (!run || latestThreadByRoot.get(run.rootId) !== threadId) return null;

  const wasCompleted = completedRoots.delete(run.rootId);
  for (const [id, r] of activeRuns) {
    if (id !== threadId && r.rootId === run.rootId) {
      activeRuns.delete(id);
      suspendedRuns.set(id, r);
    }
  }
  const wasSuspended = suspendedRuns.delete(threadId);
  activeRuns.set(threadId, run);
  if (wasSuspended || wasCompleted) {
    const def = getWorkflow(run.workflowId);
    const stage = def?.stages.find((s) => s.id === run.stageId);
    if (def && stage) {
      void api
        .renameThread(threadId, resolveTitle(def, stage, runContext(run, "")))
        .then(() => requireHost().refreshThreads())
        .catch(() => {});
    }
  }
  persistRuns();
  return run;
}

// ---------------------------------------------------------------------------
// 对 store 暴露的接口
// ---------------------------------------------------------------------------

export function startWorkflow(
  workflowId: string,
  vars: Record<string, string>,
  rootId: string,
): Promise<void> {
  const h = requireHost();
  const def = getWorkflow(workflowId);
  if (!def) return Promise.reject(new Error("找不到工作流"));
  const errors = validateWorkflow(def);
  if (errors.length > 0) return Promise.reject(new Error(errors.join("；")));
  const entry = def.stages.find((s) => s.id === def.entry);
  if (!entry) return Promise.reject(new Error("起始阶段不存在"));

  return (async () => {
    const root = await api.getThread(rootId);
    if (root.employeeId || root.roamingRole || root.quotaPeerName) {
      throw new Error("工作流仅支持本地普通会话");
    }
    if (h.isRunning(rootId)) throw new Error("请等待当前会话结束后再启动工作流");

    if (entry.mode) {
      await api.setThreadMode(rootId, entry.mode);
    }
    const run: WorkflowRunStep = {
      rootId,
      workflowId,
      stageId: entry.id,
      stageCount: 1,
      attempts: { [entry.id]: 1 },
      vars,
    };
    activeRuns.set(rootId, run);
    runHistory.set(rootId, run);
    suspendedRuns.delete(rootId);
    completedRoots.delete(rootId);
    latestThreadByRoot.set(rootId, rootId);
    persistRuns();

    const ctx = runContext(run, "");
    await api.renameThread(rootId, resolveTitle(def, entry, ctx));
    await h.refreshThreads();
    if (h.currentId() === rootId) h.bumpScrollToBottom();
    h.clearProposedPlan();
    h.setRunning(rootId, true);
    try {
      await api.sendPrompt(rootId, resolvePrompt(def, entry, ctx), []);
    } catch (e) {
      if (activeRuns.get(rootId) === run) {
        activeRuns.delete(rootId);
        suspendedRuns.set(rootId, run);
        persistRuns();
      }
      h.setRunning(rootId, false);
      throw e;
    }
  })();
}

/** acp:turn running=true：重新挂回流程。 */
export function handleTurnStart(threadId: string): void {
  reattach(threadId);
}

/** acp:turn running=false：正常结束则推进，否则暂停。返回是否为本运行时管理的会话。 */
export function handleTurnEnd(threadId: string, stopReason: string | null | undefined): boolean {
  if (!activeRuns.has(threadId)) return false;
  const manual = stopReason === "cancelled" || stopReason === "force_cancelled";
  const normal = stopReason === "end_turn" || stopReason === "max_turn_requests";
  const action = normal ? advanceWorkflow(threadId) : Promise.resolve(suspendWorkflow(threadId, manual));
  void action.catch((error) => console.error("Workflow advance failed", error));
  return true;
}

/**
 * 用户在某会话补充消息时调用：重新挂回流程，并对只读阶段做续跑提示改写。
 * 返回应发送的文本；非工作流会话返回 null。
 */
export function preparePrompt(threadId: string, text: string): string | null {
  const run = reattach(threadId);
  if (!run) return null;
  const def = getWorkflow(run.workflowId);
  const stage = def?.stages.find((s) => s.id === run.stageId);
  if (def && stage?.reviewOnly) {
    const resolver = builtinResumeResolvers[def.id]?.[stage.id];
    if (resolver) return resolver(text);
  }
  return text;
}

export function isActive(threadId: string): boolean {
  return activeRuns.has(threadId);
}

/** sendPrompt 失败时由 store 调用，把当前阶段挂起。 */
export function suspendActive(threadId: string): void {
  suspendWorkflow(threadId, false);
}

/** 某 root 链当前尖端会话（侧栏分组/跳转用）。 */
export function latestStageThread(rootId: string): string | undefined {
  return latestThreadByRoot.get(rootId);
}

export function isWorkflowThread(threadId: string): boolean {
  return activeRuns.has(threadId) || suspendedRuns.has(threadId) || runHistory.has(threadId);
}
