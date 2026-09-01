/**
 * 可配置工作流：类型与纯逻辑（模板渲染、转移求值、校验）。
 *
 * 设计复用现有 /fire 的「会话接力」运行时：一个节点 = 一个独立会话。引擎自动把
 * 上一节点结论交给下一节点。连线有两种判断模式：正常模式（前一节点输出引擎注入的
 * 路由标识才走该连线）与提示词模式（轻量模型按前一节点结论判断）；两种模式运行所需
 * 的提示词均由引擎自动补全，用户只需配置节点提示词与连线名称。
 */
import type { AgentKind } from "../types";

/** 终止伪节点：只表示流程到达结束状态。 */
export const WF_DONE = "$done";
/** 旧数据兼容别名；画布与新配置不再暴露失败结束节点。 */
export const WF_FAIL = "$fail";

/**
 * 工作流触发条件：满足任一即自动启动该工作流（无需 /run）。
 * - slash：用户输入以 `/<command>` 开头，如 command="fix" 匹配 `/fix 修登录`。
 * - contains：提示词包含某串（大小写不敏感）。
 * - regex：提示词匹配正则（大小写不敏感）。
 */
export type WorkflowTrigger =
  | { kind: "slash"; command: string }
  | { kind: "contains"; text: string }
  | { kind: "regex"; pattern: string };

export type WorkflowTransitionWhen =
  | { kind: "always" }
  /** 结论以该标记结尾（大小写不敏感），同 FIRE_ACCEPTED 语义。 */
  | { kind: "marker"; value: string }
  /** 自定义正则（大小写不敏感）匹配结论。 */
  | { kind: "regex"; value: string };

export interface WorkflowTransition {
  id: string;
  /** 旧版显式匹配规则；新建连线由引擎自动维护，不需要用户配置。 */
  when: WorkflowTransitionWhen;
  /** 目标节点 id，或 $done。旧数据中的 $fail 视为同一个结束状态。 */
  to: string;
  /** 跳转依据：告诉引擎/模型什么情况下走这条连线。正常模式用于生成路由标识要求，提示词模式作为轻量模型判断的候选描述；留空时回退用连线名称。 */
  prompt?: string;
  /** 连线名称（显示在线上）。 */
  label?: string;
}

export interface WorkflowStageDef {
  id: string;
  /** 显示名，也用于生成会话标题。 */
  name: string;
  /**
   * 提示词模板。支持变量：{{goal}} {{criteria}} {{prev}}（上一阶段结论，首阶段为空）
   * {{attempt}}（当前阶段第几次进入）。内置工作流可改用代码里的 resolver 覆盖。
   */
  promptTemplate: string;
  /** 可选：覆盖会话标题模板，变量同上；默认 `[WF] {name} · 第{attempt}次`。 */
  titleTemplate?: string;
  /** 可选：覆盖该阶段会话模式（如 build / plan）。 */
  mode?: string;
  /** 可选：覆盖该阶段的后端；缺省跟随启动会话。仅换模型不换后端时无需设置。 */
  agentKind?: AgentKind;
  /** 可选：覆盖该阶段的模型；缺省跟随启动会话（设置了 agentKind 而未设模型时用该后端默认模型）。 */
  model?: string | null;
  /** 节点完成后暂停，由用户人工选择下一条连线；引擎不自动判断。 */
  manualReview?: boolean;
  /** 旧配置兼容字段。 */
  reviewOnly?: boolean;
  transitions: WorkflowTransition[];
  /** 画布坐标。 */
  x: number;
  y: number;
}

export interface WorkflowDef {
  id: string;
  name: string;
  version: number;
  /** 内置工作流（代码定义，不可删除，可复制为自定义）。 */
  builtin?: boolean;
  /** 团队分享：分享该工作流的队友展示名（接收后带上，标识团队来源）。 */
  sharedBy?: string;
  /** 接收团队分享的时间戳（ms），仅用于展示。 */
  sharedAt?: number;
  /** 是否启用（可被选择/触发/运行）；字段缺失视为启用。开关独立持久化，内置工作流也可切换。 */
  enabled?: boolean;
  /** 起始阶段 id。 */
  entry: string;
  /** 全局保险丝：整条链最多创建的阶段会话总数。 */
  maxTotalStages: number;
  /** 触发条件：满足任一即自动启动（与 /run 并存）。 */
  triggers?: WorkflowTrigger[];
  stages: WorkflowStageDef[];
}

/** 运行态：某条工作流链当前所在阶段。持久化在 localStorage。 */
export interface WorkflowRunStep {
  rootId: string;
  workflowId: string;
  stageId: string;
  /** 已创建阶段会话总数（含根），用于 maxTotalStages 保险丝。 */
  stageCount: number;
  /** 各阶段被进入的次数（stageId -> count），作为模板里的 {{attempt}}。 */
  attempts: Record<string, number>;
  /** 流程变量：goal / criteria 及用户自定义。 */
  vars: Record<string, string>;
  /** 「跟随会话」节点的锚点：启动工作流时用户选择的会话后端/模型（首节点覆盖前）。
   *  缺省时回退到 root 会话当前值。 */
  followAgentKind?: string;
  followModel?: string | null;
}

function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** 引擎注入的分支标识。用户无需看到或配置。 */
export function workflowRouteMarker(transitionId: string): string {
  return `[[NOVA_WORKFLOW_ROUTE:${transitionId}]]`;
}

/** 连线用于模型判断的自然语言提示；兼容旧 label / marker / regex 配置。 */
export function workflowTransitionPrompt(transition: WorkflowTransition): string {
  const prompt = transition.prompt?.trim() || transition.label?.trim();
  if (prompt) return prompt;
  if (transition.when.kind === "marker") return `结论以 ${transition.when.value} 结束`;
  if (transition.when.kind === "regex") return `结论匹配 ${transition.when.value}`;
  return "";
}

/** 渲染 {{var}} 模板；未知变量替换为空串。 */
export function renderTemplate(
  template: string,
  vars: Record<string, string | number | undefined>,
): string {
  return template.replace(/\{\{\s*(\w+)\s*\}\}/g, (_match, key: string) => {
    const value = vars[key];
    return value === undefined || value === null ? "" : String(value);
  });
}

/**
 * 按声明顺序求值阶段转移，返回第一条命中的转移；无命中返回 null（流程停在当前阶段，
 * 视为异常，由运行时按暂停处理）。
 */
export function evalTransition(
  stage: WorkflowStageDef,
  conclusion: string,
): WorkflowTransition | null {
  // 新版隐式路由优先。即使旧数据中的 when=always，也不会抢走模型已选择的分支。
  for (const transition of stage.transitions) {
    if (conclusion.trimEnd().endsWith(workflowRouteMarker(transition.id))) return transition;
  }
  const usesImplicitRouting =
    stage.transitions.length > 1 && stage.transitions.some((transition) => transition.prompt !== undefined);
  for (const transition of stage.transitions) {
    const when = transition.when;
    if (when.kind === "always") {
      if (!usesImplicitRouting) return transition;
      continue;
    }
    if (when.kind === "marker") {
      const marker = when.value.trim();
      if (marker && new RegExp(`${escapeRegExp(marker)}\\s*$`, "i").test(conclusion)) {
        return transition;
      }
    } else if (when.kind === "regex") {
      try {
        if (new RegExp(when.value, "i").test(conclusion)) return transition;
      } catch {
        // 非法正则跳过，避免一条坏规则卡死整条流程。
      }
    }
  }
  return null;
}

export function isTerminal(target: string): boolean {
  return target === WF_DONE || target === WF_FAIL;
}

/** 校验工作流定义，返回错误信息数组（空 = 合法）。 */
export function validateWorkflow(def: WorkflowDef): string[] {
  const errors: string[] = [];
  if (!def.name.trim()) errors.push("工作流名称不能为空");
  if (def.stages.length === 0) {
    errors.push("至少需要一个阶段");
    return errors;
  }
  const ids = new Set<string>();
  for (const stage of def.stages) {
    if (!stage.id.trim()) errors.push("存在未命名的阶段");
    if (ids.has(stage.id)) errors.push(`阶段 id 重复：${stage.id}`);
    ids.add(stage.id);
    if (!stage.promptTemplate.trim()) {
      errors.push(`阶段「${stage.name || stage.id}」的提示词模板为空`);
    }
    if (stage.manualReview && stage.transitions.length === 0) {
      errors.push(`人工审核节点「${stage.name || stage.id}」至少需要一条连线`);
    }
  }
  if (!ids.has(def.entry)) errors.push(`起始阶段不存在：${def.entry}`);
  for (const stage of def.stages) {
    for (const transition of stage.transitions) {
      if (!isTerminal(transition.to) && !ids.has(transition.to)) {
        errors.push(`阶段「${stage.name}」的转移指向不存在的阶段：${transition.to}`);
      }
      if (stage.transitions.length > 1 && !workflowTransitionPrompt(transition)) {
        errors.push(`节点「${stage.name}」存在多个出口，请填写每条连线的判断提示词`);
      }
    }
  }
  return errors;
}

function layoutGeneratedStages(stages: WorkflowStageDef[], entry: string): void {
  const byId = new Map(stages.map((stage) => [stage.id, stage]));
  const depth = new Map<string, number>([[entry, 0]]);
  const visitOrder = new Map<string, number>([[entry, 0]]);
  const queue = [entry];
  let nextOrder = 1;

  // 用从入口首次到达的层级排布主流程；回环不会把节点反复推远。
  while (queue.length > 0) {
    const id = queue.shift()!;
    const stage = byId.get(id);
    if (!stage) continue;
    const nextDepth = (depth.get(id) ?? 0) + 1;
    for (const transition of stage.transitions) {
      if (isTerminal(transition.to) || !byId.has(transition.to) || depth.has(transition.to)) continue;
      depth.set(transition.to, nextDepth);
      visitOrder.set(transition.to, nextOrder++);
      queue.push(transition.to);
    }
  }

  let fallbackDepth = Math.max(0, ...depth.values()) + 1;
  for (const stage of stages) {
    if (depth.has(stage.id)) continue;
    depth.set(stage.id, fallbackDepth++);
    visitOrder.set(stage.id, nextOrder++);
  }

  const columns = new Map<number, WorkflowStageDef[]>();
  for (const stage of stages) {
    const d = depth.get(stage.id) ?? 0;
    const column = columns.get(d) ?? [];
    column.push(stage);
    columns.set(d, column);
  }

  const maxRows = Math.max(1, ...[...columns.values()].map((column) => column.length));
  const rowGap = 150;
  const centerY = Math.max(180, 80 + ((maxRows - 1) * rowGap) / 2);
  for (const [d, column] of [...columns.entries()].sort((a, b) => a[0] - b[0])) {
    column.sort((a, b) => (visitOrder.get(a.id) ?? 0) - (visitOrder.get(b.id) ?? 0));
    const startY = centerY - ((column.length - 1) * rowGap) / 2;
    column.forEach((stage, row) => {
      stage.x = 80 + d * 270;
      stage.y = Math.round(startY + row * rowGap);
    });
  }
}

export function normalizeGeneratedWorkflow(raw: unknown): WorkflowDef {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    throw new Error("Agent 未返回有效工作流");
  }
  const source = raw as Partial<WorkflowDef>;
  const inputStages = Array.isArray(source.stages) ? source.stages.slice(0, 6) : [];
  if (inputStages.length === 0) throw new Error("Agent 生成的工作流没有阶段");

  const stageIds = inputStages.map((stage, index) => {
    const candidate = stage && typeof stage === "object" ? String((stage as WorkflowStageDef).id ?? "").trim() : "";
    return candidate || `stage_${index + 1}`;
  });
  const uniqueIds = stageIds.map((id, index) =>
    stageIds.indexOf(id) === index ? id : `${id}_${index + 1}`,
  );
  const idMap = new Map(stageIds.map((id, index) => [id, uniqueIds[index]]));

  const terminalAliases = new Set(["done", "end", "finish", "finished", "complete", "completed", "结束", "完成"]);
  const stages: WorkflowStageDef[] = inputStages.map((value, index) => {
    const stage = (value && typeof value === "object" ? value : {}) as Partial<WorkflowStageDef>;
    const transitions = Array.isArray(stage.transitions) ? stage.transitions.slice(0, 4) : [];
    return {
      id: uniqueIds[index],
      name: String(stage.name ?? `阶段 ${index + 1}`).trim() || `阶段 ${index + 1}`,
      promptTemplate: String(stage.promptTemplate ?? "").trim() || `完成当前阶段负责的任务。目标：{{goal}}\n上一阶段结论：{{prev}}`,
      mode: "build",
      manualReview: !!stage.manualReview,
      transitions: transitions.map((value, transitionIndex) => {
        const transition = (value && typeof value === "object" ? value : {}) as Partial<WorkflowTransition>;
        const target = String(transition.to ?? "").trim();
        const normalizedTarget = target === WF_FAIL
          ? WF_FAIL
          : target === WF_DONE || (!idMap.has(target) && terminalAliases.has(target.toLowerCase()))
            ? WF_DONE
            : (idMap.get(target) ?? target);
        return {
          id: `route_${index + 1}_${transitionIndex + 1}`,
          when: { kind: "always" },
          to: normalizedTarget,
          prompt: String(transition.prompt ?? transition.label ?? "").trim(),
          label: String(transition.label ?? "").trim() || undefined,
        };
      }),
      x: 80 + (index % 3) * 280,
      y: 140 + Math.floor(index / 3) * 180,
    };
  });

  const entryRaw = String(source.entry ?? stageIds[0]).trim();
  const workflow: WorkflowDef = {
    id: newWorkflowId("hard"),
    name: String(source.name ?? "Hard 工作流").trim() || "Hard 工作流",
    version: 1,
    enabled: true,
    entry: idMap.get(entryRaw) ?? stages[0].id,
    maxTotalStages: Math.min(12, Math.max(stages.length, Number(source.maxTotalStages) || 12)),
    stages,
  };
  // 生成模型常把终点写成普通节点、漏写出口，或产出只有回环没有退出路径的图。
  // Hard 模式应自动补成可收敛工作流，而不是把原始 JSON 和校验错误直接暴露给用户。
  const stageIdSet = new Set(stages.map((stage) => stage.id));
  for (const stage of stages) {
    for (const transition of stage.transitions) {
      if (!isTerminal(transition.to) && !stageIdSet.has(transition.to)) transition.to = WF_DONE;
    }
    if (stage.transitions.length === 0) {
      stage.transitions.push({
        id: `route_auto_done_${stage.id}`,
        when: { kind: "always" },
        to: WF_DONE,
        prompt: "当前阶段完成后结束工作流",
        label: "完成",
      });
    }
  }

  const canFinish = new Set<string>();
  let changed = true;
  while (changed) {
    changed = false;
    for (const stage of stages) {
      if (canFinish.has(stage.id)) continue;
      if (stage.transitions.some((transition) =>
        isTerminal(transition.to) || canFinish.has(transition.to))) {
        canFinish.add(stage.id);
        changed = true;
      }
    }
  }
  for (const stage of stages) {
    if (canFinish.has(stage.id)) continue;
    stage.transitions.push({
      id: `route_auto_exit_${stage.id}`,
      when: { kind: "always" },
      to: WF_DONE,
      prompt: "当前回环已完成目标或继续迭代不再产生收益时结束工作流",
      label: "完成并退出",
    });
  }
  for (const stage of stages) {
    if (stage.transitions.length <= 1) continue;
    for (const transition of stage.transitions) {
      if (!workflowTransitionPrompt(transition)) {
        transition.prompt = isTerminal(transition.to) ? "目标已完成时结束" : `需要进入下一阶段 ${transition.to}`;
      }
    }
  }

  const reachable = new Set<string>();
  const pending = [workflow.entry];
  while (pending.length > 0) {
    const id = pending.shift()!;
    if (reachable.has(id)) continue;
    reachable.add(id);
    const stage = stages.find((candidate) => candidate.id === id);
    if (!stage) continue;
    for (const transition of stage.transitions) {
      if (!isTerminal(transition.to)) pending.push(transition.to);
    }
  }
  // 与入口断开的孤立节点不参与执行，自动移除，避免一块无关子图阻塞整个 Hard 流程。
  for (let index = stages.length - 1; index >= 0; index--) {
    if (!reachable.has(stages[index].id)) stages.splice(index, 1);
  }
  // Hard 设计只采用生成内容，不采用模型坐标：保持配置画布原有视觉，
  // 仅按入口层级重新放置节点，让主流程从左到右、分支上下展开、回环走画布绕线。
  layoutGeneratedStages(stages, workflow.entry);
  const errors = validateWorkflow(workflow);
  if (errors.length > 0) throw new Error(`Agent 生成的工作流不可运行：${errors.join("；")}`);
  return workflow;
}

let idCounter = 0;
/** 生成短 id（画布新建节点/转移用）。 */
export function newWorkflowId(prefix: string): string {
  idCounter += 1;
  return `${prefix}_${Date.now().toString(36)}_${idCounter.toString(36)}`;
}
