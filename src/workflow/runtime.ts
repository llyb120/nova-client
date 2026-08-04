/**
 * 通用工作流运行时：复用 /fire 的「会话接力」模式（一个阶段 = 一个独立会话，turn 事件
 * 驱动推进，支持暂停/恢复与持久化），但阶段与转移由 WorkflowDef 配置决定。
 *
 * 与 store.ts 解耦：store 通过 initWorkflowRuntime 注入少量 UI/状态能力，避免循环依赖。
 * /fire 仍走 store 里的原有专用路径；本运行时驱动 /run 启动的内置与自定义工作流。
 */
import { createSignal } from "solid-js";
import { api } from "../ipc";
import type { PromptImage, Thread, Item } from "../types";
import { getWorkflow, isWorkflowEnabled } from "./storage";
import {
  evalTransition,
  isTerminal,
  renderTemplate,
  validateWorkflow,
  workflowRouteMarker,
  workflowTransitionPrompt,
  type WorkflowDef,
  type WorkflowRunStep,
  type WorkflowStageDef,
  type WorkflowTransition,
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
const pendingManualReviews = new Set<string>();
const [workflowReviewRevision, setWorkflowReviewRevision] = createSignal(0);
export { workflowReviewRevision };
const RUNS_KEY = "fd:workflowRuns:v1";

type PersistedRuns = {
  runs: [string, WorkflowRunStep][];
  latest: [string, string][];
  completed: string[];
  manualReviews?: string[];
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
    manualReviews: [...pendingManualReviews],
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
    for (const threadId of snapshot.manualReviews ?? []) {
      if (threadId) pendingManualReviews.add(threadId);
    }
    setWorkflowReviewRevision((value) => value + 1);
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

type WorkflowStageContext = {
  vars: Record<string, string>;
  prev: string;
  attempt: number;
};

function resolvePrompt(
  _def: WorkflowDef,
  stage: WorkflowStageDef,
  ctx: WorkflowStageContext,
): string {
  const base = renderTemplate(stage.promptTemplate, {
    ...ctx.vars,
    prev: ctx.prev,
    attempt: ctx.attempt,
  }).trim();
  const handoff = [
    ctx.vars.context?.trim() ? ctx.vars.context.trim() : "",
    ctx.vars.goal?.trim() ? `工作流目标：\n${ctx.vars.goal.trim()}` : "",
    ctx.prev.trim() ? `上一节点结论：\n${ctx.prev.trim()}` : "",
  ].filter(Boolean);
  const routes = stage.transitions;
  const routing = routes.length > 1 && !stage.manualReview
    ? `完成当前节点任务后，必须根据实际结论选择且只选择一个下一跳。将对应标识单独放在回复最后一行，标识后不要再输出内容：\n${routes
        .map((transition) => `- ${workflowTransitionPrompt(transition)}\n  ${workflowRouteMarker(transition.id)}`)
        .join("\n")}`
    : "";
  return [base, ...handoff, routing].filter(Boolean).join("\n\n");
}

function resolveTitle(
  _def: WorkflowDef,
  stage: WorkflowStageDef,
  ctx: WorkflowStageContext,
  status = "",
): string {
  const base = renderTemplate(stage.titleTemplate ?? `[WF] ${stage.name} · 第{{attempt}}次`, {
    ...ctx.vars,
    prev: ctx.prev,
    attempt: ctx.attempt,
  });
  return status ? `${base} · ${status}` : base;
}

function runContext(run: WorkflowRunStep, prev: string): WorkflowStageContext {
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
  originThreadId?: string,
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
  // 竞态保护：若在等待期间用户的干预消息已把原阶段重新挂回流程，放弃新阶段，
  // 避免同一运行态被两个会话同时驱动导致链分叉；原阶段回合结束后会重新推进。
  if (originThreadId && activeRuns.get(originThreadId) === run) {
    activeRuns.delete(thread.id);
    runHistory.delete(thread.id);
    latestThreadByRoot.set(run.rootId, originThreadId);
    persistRuns();
    await api.deleteThread(thread.id).catch(() => {});
    await h.refreshThreads();
    return;
  }
  if (isViewingChain(run.rootId)) await h.openThread(thread.id);
  h.setRunning(thread.id, true);
  await api.sendPrompt(thread.id, prompt, []);
}

async function followTransition(
  threadId: string,
  run: WorkflowRunStep,
  def: WorkflowDef,
  stage: WorkflowStageDef,
  conclusion: string,
  transition: WorkflowTransition,
): Promise<void> {
  const h = requireHost();
  if (isTerminal(transition.to)) {
    completedRoots.add(run.rootId);
    pendingManualReviews.delete(threadId);
    persistRuns();
    setWorkflowReviewRevision((value) => value + 1);
    await api.renameThread(threadId, resolveTitle(def, stage, runContext(run, conclusion), "完成"));
    await h.refreshThreads();
    void api.notifyWorkflowDone(threadId, true).catch(() => {});
    return;
  }

  const next = def.stages.find((candidate) => candidate.id === transition.to);
  if (!next) return;
  if (run.stageCount + 1 > def.maxTotalStages) {
    completedRoots.add(run.rootId);
    pendingManualReviews.delete(threadId);
    persistRuns();
    setWorkflowReviewRevision((value) => value + 1);
    await api.renameThread(threadId, resolveTitle(def, stage, runContext(run, conclusion), "已停止"));
    await h.refreshThreads();
    void api.notifyWorkflowDone(threadId, false).catch(() => {});
    return;
  }

  pendingManualReviews.delete(threadId);
  suspendedRuns.delete(threadId);
  run.stageId = next.id;
  run.stageCount += 1;
  run.attempts[next.id] = (run.attempts[next.id] ?? 0) + 1;
  persistRuns();
  setWorkflowReviewRevision((value) => value + 1);
  const ctx = runContext(run, conclusion);
  await createStageThread(
    run,
    next,
    resolveTitle(def, next, ctx),
    resolvePrompt(def, next, ctx),
    threadId,
  );
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
  if (!def || !stage) {
    // 工作流定义已被删除/修改：清掉幽灵条目，避免后续消息被挂到不存在的流程上。
    runHistory.delete(threadId);
    persistRuns();
    return;
  }

  const thread = await api.getThread(threadId);
  // 用户中途干预（排队消息已把本阶段重新挂回）时让位给那一回合：
  // 新结论会并入干预内容，回合结束后会再次走到这里推进。
  if (activeRuns.has(threadId)) return;
  const conclusion = stageConclusion(thread);
  if (stage.manualReview) {
    suspendedRuns.set(threadId, run);
    pendingManualReviews.add(threadId);
    persistRuns();
    setWorkflowReviewRevision((value) => value + 1);
    await api.renameThread(threadId, resolveTitle(def, stage, runContext(run, conclusion), "待人工审核"));
    await h.refreshThreads();
    return;
  }

  // 单出口直接接力；多出口由引擎注入的标识选择。旧工作流仍兼容 marker/regex。
  const transition = stage.transitions.length === 1
    ? stage.transitions[0]
    : evalTransition(stage, conclusion);

  if (!transition) {
    // 没有任何转移命中：停在当前阶段等用户补充。
    suspendedRuns.set(threadId, run);
    persistRuns();
    await api.renameThread(threadId, resolveTitle(def, stage, runContext(run, conclusion), "待补充"));
    await h.refreshThreads();
    return;
  }

  await followTransition(threadId, run, def, stage, conclusion, transition);
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
  const wasManualReview = pendingManualReviews.delete(threadId);
  activeRuns.set(threadId, run);
  if (wasSuspended || wasCompleted || wasManualReview) {
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
  if (wasManualReview) setWorkflowReviewRevision((value) => value + 1);
  return run;
}

// ---------------------------------------------------------------------------
// 对 store 暴露的接口
// ---------------------------------------------------------------------------

export function startWorkflow(
  workflowId: string,
  vars: Record<string, string>,
  rootId: string,
  /** 首节点附件：会话输入即首节点输入，图片随首阶段提示词一起发送。 */
  images: PromptImage[] = [],
): Promise<void> {
  const h = requireHost();
  const def = getWorkflow(workflowId);
  if (!def) return Promise.reject(new Error("找不到工作流"));
  if (!isWorkflowEnabled(def)) {
    return Promise.reject(new Error(`工作流「${def.name}」已停用，请在工作流页启用后再运行`));
  }
  const errors = validateWorkflow(def);
  if (errors.length > 0) return Promise.reject(new Error(errors.join("；")));
  const entry = def.stages.find((s) => s.id === def.entry);
  if (!entry) return Promise.reject(new Error("起始阶段不存在"));
  // 该会话属于未完成的工作流链时禁止再次启动（/run 或自动触发器）：
  // 否则会覆盖现有运行态，旧链尖端仍会继续推进，造成同一 root 下双链交缠。
  const existing =
    activeRuns.get(rootId) ?? suspendedRuns.get(rootId) ?? runHistory.get(rootId);
  if (existing && !completedRoots.has(existing.rootId)) {
    const existingName = getWorkflow(existing.workflowId)?.name ?? "未完成的工作流";
    return Promise.reject(
      new Error(
        `该会话仍在工作流「${existingName}」中：继续流程请在链的最新会话直接补充消息；要启动新工作流请新建会话`,
      ),
    );
  }

  return (async () => {
    const root = await api.getThread(rootId);
    // 漫游/额度会话的执行位置不在本机，无法由本地工作流驱动；员工会话允许作为工作流根。
    if (root.roamingRole || root.quotaPeerName) {
      throw new Error("工作流仅支持本地会话");
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
      await api.sendPrompt(rootId, resolvePrompt(def, entry, ctx), images);
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

/** 用户在工作流最新会话补充消息时重新挂回流程；非工作流会话返回 null。 */
export function preparePrompt(threadId: string, text: string): string | null {
  return reattach(threadId) ? text : null;
}

export interface ManualWorkflowReview {
  stageName: string;
  transitions: { id: string; label: string }[];
}

export function manualWorkflowReview(threadId: string): ManualWorkflowReview | null {
  if (!pendingManualReviews.has(threadId)) return null;
  const run = suspendedRuns.get(threadId) ?? runHistory.get(threadId);
  const def = run ? getWorkflow(run.workflowId) : undefined;
  const stage = def?.stages.find((candidate) => candidate.id === run?.stageId);
  if (!run || !def || !stage?.manualReview) return null;
  return {
    stageName: stage.name,
    transitions: stage.transitions.map((transition) => ({
      id: transition.id,
      label: workflowTransitionPrompt(transition) || (isTerminal(transition.to) ? "结束" : def.stages.find((candidate) => candidate.id === transition.to)?.name ?? "下一步"),
    })),
  };
}

export async function chooseManualWorkflowTransition(
  threadId: string,
  transitionId: string,
): Promise<void> {
  if (!pendingManualReviews.has(threadId)) throw new Error("当前节点不在等待人工审核");
  const run = suspendedRuns.get(threadId) ?? runHistory.get(threadId);
  const def = run ? getWorkflow(run.workflowId) : undefined;
  const stage = def?.stages.find((candidate) => candidate.id === run?.stageId);
  const transition = stage?.transitions.find((candidate) => candidate.id === transitionId);
  if (!run || !def || !stage || !transition) throw new Error("人工审核选项已失效");
  const thread = await api.getThread(threadId);
  await followTransition(threadId, run, def, stage, stageConclusion(thread), transition);
}

export function isActive(threadId: string): boolean {
  return activeRuns.has(threadId);
}

/** 所有进行中/暂停运行的链根会话 id（数字员工按员工会话去重轮次用）。 */
export function runRootIds(): string[] {
  const roots = new Set<string>();
  for (const run of activeRuns.values()) roots.add(run.rootId);
  for (const run of suspendedRuns.values()) roots.add(run.rootId);
  return [...roots];
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
