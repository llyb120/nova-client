/**
 * 工作流编辑器内置示例。
 *
 * 这里只展示“节点提示词 + 连线判断提示词 + 会话结论接力”，不实现也不接管真正的
 * `/fire`。`/fire` 始终由 store.ts 中的专用 Fire Relay 状态机处理。
 */
import { WF_DONE, type WorkflowDef } from "./types";

export const WORKFLOW_DEMO_ID = "builtin:workflow-demo";

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

export const BUILTIN_WORKFLOWS: WorkflowDef[] = [WORKFLOW_DEMO];

export function getBuiltinWorkflow(id: string): WorkflowDef | undefined {
  return BUILTIN_WORKFLOWS.find((workflow) => workflow.id === id);
}
