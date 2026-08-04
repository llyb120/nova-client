/**
 * 工作流编辑器内置示例。
 *
 * 这里只展示“节点提示词 + 连线判断提示词 + 会话结论接力”，不实现也不接管真正的
 * `/fire`。`/fire` 始终由 store.ts 中的专用 Fire Relay 状态机处理。
 */
import { WF_DONE, type WorkflowDef } from "./types";

export const WORKFLOW_DEMO_ID = "builtin:workflow-demo";

/** 数字员工默认工作流：复刻原来员工 Wake → Do 两步走（开工预检路由 + 开发执行）。 */
export const WORKFLOW_WAKE_DO_ID = "builtin:wake-do";

export const WORKFLOW_WAKE_DO: WorkflowDef = {
  id: WORKFLOW_WAKE_DO_ID,
  name: "开工预检 → 开发（Wake → Do）",
  version: 1,
  builtin: true,
  entry: "wake",
  maxTotalStages: 12,
  stages: [
    {
      id: "wake",
      name: "开工预检",
      titleTemplate: "[Wake] 开工预检 · 第{{attempt}}次",
      promptTemplate:
        "你是数字员工的开工预检（Wake）阶段：只做只读侦察和决策，不要修改任何文件。\n\n{{context}}\n\n请围绕下面的工作目标侦察现状（相关代码、git 状态、已有进展与结论），然后判断这一轮是否需要动手开发：\n- 需要动手：说明切入点、大致方案与注意事项，选择「进入开发」；\n- 无需动手（目标已满足 / 信息不足 / 不在职责范围）：说明理由，选择「直接收尾」。\n\n工作目标：\n{{goal}}",
      mode: "build",
      x: 80,
      y: 160,
      transitions: [
        {
          id: "wake_to_do",
          when: { kind: "always" },
          to: "do",
          prompt: "需要动手开发，进入执行",
        },
        {
          id: "wake_done",
          when: { kind: "always" },
          to: WF_DONE,
          prompt: "无需开发或目标已满足，直接收尾",
        },
      ],
    },
    {
      id: "do",
      name: "开发执行",
      titleTemplate: "[Do] 开发执行 · 第{{attempt}}次",
      promptTemplate:
        "你是数字员工的开发执行（Do）阶段。请基于开工预检的结论动手完成工作：遵循仓库现有规范，控制改动范围，完成后自查并给出可核实的结论（改了什么、如何验证）。\n\n{{context}}",
      mode: "build",
      x: 360,
      y: 160,
      transitions: [
        {
          id: "do_done",
          when: { kind: "always" },
          to: WF_DONE,
          prompt: "本轮开发完成，收尾汇报",
        },
      ],
    },
  ],
};

export const WORKFLOW_DEMO: WorkflowDef = {
  id: WORKFLOW_DEMO_ID,
  name: "工作流示例",
  version: 1,
  builtin: true,
  entry: "execute",
  maxTotalStages: 30,
  stages: [
    {
      id: "execute",
      name: "执行",
      promptTemplate: "完成工作流目标。检查现状后直接实施，并在结尾给出可核实的完成结论。",
      mode: "build",
      x: 80,
      y: 160,
      transitions: [
        {
          id: "execute_to_review",
          when: { kind: "always" },
          to: "review",
          prompt: "执行完成后进入检查",
        },
      ],
    },
    {
      id: "review",
      name: "检查",
      promptTemplate: "根据工作流目标和上一节点结论检查实际结果，只做判断并指出未满足项。",
      mode: "build",
      x: 360,
      y: 160,
      transitions: [
        {
          id: "review_done",
          when: { kind: "always" },
          to: WF_DONE,
          prompt: "目标已经满足",
        },
        {
          id: "review_retry",
          when: { kind: "always" },
          to: "execute",
          prompt: "仍有未满足项，需要继续执行",
        },
      ],
    },
  ],
};

export const BUILTIN_WORKFLOWS: WorkflowDef[] = [WORKFLOW_WAKE_DO, WORKFLOW_DEMO];

export function getBuiltinWorkflow(id: string): WorkflowDef | undefined {
  return BUILTIN_WORKFLOWS.find((workflow) => workflow.id === id);
}
