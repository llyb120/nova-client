/**
 * 用户自定义工作流的持久化（localStorage）。内置工作流来自代码，不存这里。
 */
import { BUILTIN_WORKFLOWS, getBuiltinWorkflow } from "./builtin";
import type { WorkflowDef, WorkflowTrigger } from "./types";

const WORKFLOWS_KEY = "fd:workflows:v1";

function readUserWorkflows(): WorkflowDef[] {
  try {
    const raw = JSON.parse(localStorage.getItem(WORKFLOWS_KEY) ?? "[]");
    return Array.isArray(raw) ? (raw as WorkflowDef[]) : [];
  } catch {
    return [];
  }
}

function writeUserWorkflows(list: WorkflowDef[]) {
  localStorage.setItem(WORKFLOWS_KEY, JSON.stringify(list));
}

/** 所有可用工作流：内置 + 用户自定义。 */
export function listWorkflows(): WorkflowDef[] {
  return [...BUILTIN_WORKFLOWS, ...readUserWorkflows()];
}

export function getWorkflow(id: string): WorkflowDef | undefined {
  return getBuiltinWorkflow(id) ?? readUserWorkflows().find((w) => w.id === id);
}

/** 按名称查找（/run 命令用），大小写不敏感。 */
export function findWorkflowByName(name: string): WorkflowDef | undefined {
  const needle = name.trim().toLowerCase();
  return listWorkflows().find((w) => w.name.trim().toLowerCase() === needle);
}

export function saveWorkflow(def: WorkflowDef): void {
  if (def.builtin) throw new Error("内置工作流不可覆盖，请复制为自定义后修改");
  const list = readUserWorkflows();
  const index = list.findIndex((w) => w.id === def.id);
  if (index >= 0) list[index] = def;
  else list.push(def);
  writeUserWorkflows(list);
}

export function deleteUserWorkflow(id: string): void {
  writeUserWorkflows(readUserWorkflows().filter((w) => w.id !== id));
}

// ---------------------------------------------------------------------------
// 触发条件匹配
// ---------------------------------------------------------------------------

function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export interface TriggerHit {
  id: string;
  name: string;
  goal: string;
}

/** 单个触发器是否命中；命中返回用作 goal 的文本，否则 null。 */
export function matchTrigger(text: string, trigger: WorkflowTrigger): string | null {
  const trimmed = text.trim();
  if (!trimmed) return null;
  if (trigger.kind === "slash") {
    const cmd = trigger.command.trim().replace(/^\/+/, "");
    if (!cmd || /[\s/]/.test(cmd)) return null;
    const m = trimmed.match(new RegExp(`^\\/${escapeRegExp(cmd)}(?:\\s|$)`, "i"));
    if (!m) return null;
    return trimmed.slice(m[0].length).trim();
  }
  if (trigger.kind === "contains") {
    const needle = trigger.text.trim();
    return needle && trimmed.toLowerCase().includes(needle.toLowerCase()) ? trimmed : null;
  }
  try {
    return new RegExp(trigger.pattern, "i").test(trimmed) ? trimmed : null;
  } catch {
    return null;
  }
}

/**
 * 扫描所有工作流的触发条件。slash 指令优先于 contains/regex，避免宽泛包含误抢指令。
 * 返回首个命中的工作流与 goal；无命中返回 null。
 */
export function findTriggeredWorkflow(text: string): TriggerHit | null {
  const list = listWorkflows();
  for (const wf of list) {
    for (const trigger of wf.triggers ?? []) {
      if (trigger.kind !== "slash") continue;
      const goal = matchTrigger(text, trigger);
      if (goal !== null) return { id: wf.id, name: wf.name, goal };
    }
  }
  for (const wf of list) {
    for (const trigger of wf.triggers ?? []) {
      if (trigger.kind === "slash") continue;
      const goal = matchTrigger(text, trigger);
      if (goal !== null) return { id: wf.id, name: wf.name, goal };
    }
  }
  return null;
}
