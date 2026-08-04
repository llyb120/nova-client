/**
 * 可配置工作流：类型与纯逻辑（模板渲染、转移求值、校验）。
 *
 * 设计复用现有 /fire 的「会话接力」运行时：一个节点 = 一个独立会话。引擎自动把
 * 上一节点结论交给下一节点，并为分支连线补充不可见路由标识；用户只需配置节点提示词
 * 与连线判断提示词。本文件只包含与 store 无关的纯逻辑。
 */

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
  /** 连线判断提示词。存在多个出口时，引擎据此要求当前节点选择下一跳。 */
  prompt?: string;
  /** 旧版画布标签，继续兼容已有工作流。 */
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

let idCounter = 0;
/** 生成短 id（画布新建节点/转移用）。 */
export function newWorkflowId(prefix: string): string {
  idCounter += 1;
  return `${prefix}_${Date.now().toString(36)}_${idCounter.toString(36)}`;
}
