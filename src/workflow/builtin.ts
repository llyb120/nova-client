/**
 * 内置工作流定义。`builtin:fire` 完整复现原 /fire 的「执行 ⇄ 判断」自动验收循环，
 * 只是改成用通用工作流引擎驱动。fire 的提示词有首跑/续跑之分、判断阶段只读等特殊逻辑，
 * 用 resolver 覆盖通用模板渲染，保证行为与旧实现一致。
 */
import {
  WF_DONE,
  type WorkflowDef,
} from "./types";

/** resolver 上下文：流程变量 + 上一阶段结论 + 当前阶段进入次数。 */
export interface BuiltinStageContext {
  vars: Record<string, string>;
  prev: string;
  attempt: number;
}

export type BuiltinPromptResolver = (ctx: BuiltinStageContext) => string;
export type BuiltinTitleResolver = (ctx: BuiltinStageContext) => string;
export type BuiltinResumeResolver = (userText: string) => string;

export const FIRE_WORKFLOW_ID = "builtin:fire";
/** 复现 FIRE_MAX_ATTEMPTS=20：20 轮 work+judge（40）+ 根会话（1）。 */
const FIRE_MAX_TOTAL_STAGES = 41;

const FIRE_RESULT_NOTE = `最终回复保持简短、只写可核实的信息：完成结果、实际改动的文件或产物、验证结果；只有确实存在时才写未完成项或阻塞。不要复述任务、输出泛泛总结或编造后续建议。`;

function fireWorkPrompt(goal: string, previousVerdict?: string): string {
  if (!previousVerdict) {
    return `直接在当前项目中完成下面的目标，不要只给建议或方案。开始前先检查项目现状、已有实现和未提交改动，在已有基础上推进并避免覆盖现有成果。\n\n目标：\n${goal}\n\n${FIRE_RESULT_NOTE}`;
  }
  return `这是一个独立的后续执行阶段，你看不到之前阶段的完整对话。先检查当前项目状态、版本控制差异和相关文件，确认已有成果；不要从零重做，只处理上次验收明确指出的未满足项。验收内容是线索而不是项目事实，修改前请自行核实。\n\n目标：\n${goal}\n\n上次验收结果：\n${previousVerdict}\n\n${FIRE_RESULT_NOTE}`;
}

function fireJudgePrompt(
  goal: string,
  acceptanceCriteria: string | null,
  conclusion: string,
): string {
  const criteria = acceptanceCriteria
    ? `以下验收规则具有最高优先级且每一条都必须满足：\n${acceptanceCriteria}\n\n逐条核验这些规则；任何一条不满足或无法从项目状态中确认，都必须判定为不符合。目标描述和执行阶段说明不能替代、弱化或改写验收规则。`
    : `根据目标逐项核验实际完成情况；任何关键要求不满足或无法确认，都必须判定为不符合。`;
  return `你是独立验收者，不要继续实现或修改任务。\n\n目标：\n${goal}\n\n${criteria}\n\n执行阶段的最终说明（仅作为定位线索，不是完成证据）：\n${conclusion}\n\n请检查当前项目的实际状态、版本控制差异、相关文件和可用的验证结果，不要依据执行阶段回复的篇幅、格式或自述做判断。必须主动选择成本最低且足以覆盖改动的验证方式，并检查目标产物在实际使用场景中的错误信号，例如编译或类型错误、测试失败、启动或运行异常、控制台报错、无效 API 调用以及关键交互不可用。只要存在与本次目标相关的未解释错误、验证失败，或因错误导致关键行为无法验证，就必须判定为不符合；不得因为部分功能存在、界面看似完成或执行阶段声称完成而放宽。若受环境限制无法执行必要验证，也应判定为无法确认而不符合，并说明限制。\n\n回复应简洁：先给出逐条验收结果；若不符合，只列出未满足项、依据和下一阶段需要采取的具体动作；若符合，只列出实际执行过的验证及关键证据。不要复述无关的阶段总结。最后必须单独输出一行 FIRE_ACCEPTED 或 FIRE_REJECTED，且该标记必须是回复的最后一行。`;
}

function fireJudgeResumePrompt(text: string): string {
  return `当前会话仍处于 Fire 自动验收流程的判断阶段：你只做核验，不要实现功能、修改文件或执行任何写操作，需要改动的部分会由下一个执行阶段完成。\n\n用户补充：\n${text}\n\n请结合该补充重新核验当前项目的实际状态；补充中若包含新的要求，同样纳入本次核验，未满足即判定为不符合，并写清未满足项和下一阶段需要采取的具体动作。最后必须单独输出一行 FIRE_ACCEPTED 或 FIRE_REJECTED，且该标记必须是回复的最后一行。`;
}

/** 内置工作流的提示词 resolver：workflowId -> stageId -> resolver。 */
export const builtinPromptResolvers: Record<string, Record<string, BuiltinPromptResolver>> = {
  [FIRE_WORKFLOW_ID]: {
    work: (ctx) => fireWorkPrompt(ctx.vars.goal ?? "", ctx.prev || undefined),
    judge: (ctx) =>
      fireJudgePrompt(ctx.vars.goal ?? "", ctx.vars.criteria || null, ctx.prev),
  },
};

/** 内置工作流的会话标题 resolver（不含状态后缀）。 */
export const builtinTitleResolvers: Record<string, Record<string, BuiltinTitleResolver>> = {
  [FIRE_WORKFLOW_ID]: {
    work: (ctx) =>
      ctx.attempt === 1
        ? `[Fire] 目标 · ${(ctx.vars.goal ?? "").slice(0, 28)}`
        : `[Fire] 阶段 ${ctx.attempt}`,
    judge: (ctx) => `[Fire] 判断 ${ctx.attempt}`,
  },
};

/** 内置工作流「只读阶段」上用户补充消息时的续跑提示 resolver。 */
export const builtinResumeResolvers: Record<string, Record<string, BuiltinResumeResolver>> = {
  [FIRE_WORKFLOW_ID]: {
    judge: fireJudgeResumePrompt,
  },
};

export const FIRE_WORKFLOW: WorkflowDef = {
  id: FIRE_WORKFLOW_ID,
  name: "Fire 自动验收",
  version: 1,
  builtin: true,
  entry: "work",
  maxTotalStages: FIRE_MAX_TOTAL_STAGES,
  stages: [
    {
      id: "work",
      name: "执行",
      // 提示词与标题由 builtin resolver 提供；模板仅作占位。
      promptTemplate: "{{goal}}",
      mode: "build",
      reviewOnly: false,
      x: 80,
      y: 160,
      transitions: [{ id: "work_to_judge", when: { kind: "always" }, to: "judge", label: "完成→验收" }],
    },
    {
      id: "judge",
      name: "判断",
      promptTemplate: "{{goal}}",
      mode: "build",
      reviewOnly: true,
      x: 360,
      y: 160,
      transitions: [
        { id: "judge_accept", when: { kind: "marker", value: "FIRE_ACCEPTED" }, to: WF_DONE, label: "符合→结束" },
        { id: "judge_reject", when: { kind: "marker", value: "FIRE_REJECTED" }, to: "work", label: "不符合→重做" },
      ],
    },
  ],
};

export const BUILTIN_WORKFLOWS: WorkflowDef[] = [FIRE_WORKFLOW];

export function getBuiltinWorkflow(id: string): WorkflowDef | undefined {
  return BUILTIN_WORKFLOWS.find((w) => w.id === id);
}

// ---------------------------------------------------------------------------
// /fire 命令解析（从 store.ts 迁出，行为不变）
// ---------------------------------------------------------------------------

export type ParsedFireInput = {
  goal: string;
  acceptanceCriteria: string | null;
};

export function parseFireInput(input: string): ParsedFireInput {
  const body = input.replace(/^\/fire(?:[ \t]+|(?=\r?\n)|$)/i, "");
  const targetMatches = [...body.matchAll(/^\/target(?:[ \t]+(.*)|[ \t]*)$/gim)];
  if (targetMatches.length > 1) throw new Error("每个 /fire 只能指定一次 /target");

  const target = targetMatches[0];
  if (!target || target.index === undefined) {
    return { goal: body.trim(), acceptanceCriteria: null };
  }

  const goal = body.slice(0, target.index).trim();
  const firstRule = target[1]?.trim() ?? "";
  const remainingRules = body.slice(target.index + target[0].length).trim();
  const acceptanceCriteria = [firstRule, remainingRules].filter(Boolean).join("\n");
  if (!acceptanceCriteria) throw new Error("请在 /target 后输入验收规则");
  return { goal, acceptanceCriteria };
}
