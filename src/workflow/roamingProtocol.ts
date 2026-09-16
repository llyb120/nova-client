export const ROAMING_WORKFLOW_PREFIX = "/nova-workflow ";

export function roamingWorkflowPrompt(workflow: { id: string; name: string }, goal: string): string {
  return `${ROAMING_WORKFLOW_PREFIX}${JSON.stringify(workflow)}\n${goal}`;
}

export function parseRoamingWorkflowPrompt(text: string): { id: string; name: string; goal: string } {
  const newline = text.indexOf("\n");
  if (!text.startsWith(ROAMING_WORKFLOW_PREFIX)) throw new Error("漫游工作流请求格式无效");
  const value = JSON.parse(text.slice(ROAMING_WORKFLOW_PREFIX.length, newline < 0 ? undefined : newline));
  if (!value || typeof value.id !== "string" || !value.id.trim() || typeof value.name !== "string") {
    throw new Error("漫游工作流请求格式无效");
  }
  return { id: value.id, name: value.name, goal: newline < 0 ? "" : text.slice(newline + 1) };
}
