/**
 * 可配置工作流：类型与纯逻辑（模板渲染、转移求值、校验）。
 *
 * 设计复用现有 /fire 的「会话接力」运行时：一个阶段 = 一个独立会话，阶段之间靠
 * 上一阶段的最终结论文本 + 标记/正则做转移判断。本文件只包含与 store 无关的纯逻辑，
 * 运行时（建会话、发提示词、turn 事件驱动、暂停/恢复、持久化）在 store.ts。
 */

/** 终止伪阶段：流程成功结束。 */
export const WF_DONE = "$done";
/** 终止伪阶段：流程以失败结束。 */
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
  when: WorkflowTransitionWhen;
  /** 目标阶段 id，或 $done / $fail。 */
  to: string;
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
  /** 该阶段只做核验、不写代码；用户在其上补充消息时走「只读续跑」提示。 */
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
  for (const transition of stage.transitions) {
    const when = transition.when;
    if (when.kind === "always") return transition;
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
  }
  if (!ids.has(def.entry)) errors.push(`起始阶段不存在：${def.entry}`);
  for (const stage of def.stages) {
    for (const transition of stage.transitions) {
      if (!isTerminal(transition.to) && !ids.has(transition.to)) {
        errors.push(`阶段「${stage.name}」的转移指向不存在的阶段：${transition.to}`);
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
