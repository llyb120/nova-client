import { createSignal } from "solid-js";
import { splitEffort } from "./modelEfforts";
import type { AgentKind } from "./types";

const KEY = "fd:recentModels";
const LIMIT = 24;

function read(): string[] {
  try {
    const value = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
  } catch {
    return [];
  }
}

/** 最近选过的模型（新→旧），键与收藏 id 相同：`<agentKind>:<encodeURIComponent(模型)>` */
export const [recentModelIds, setRecentModelIds] = createSignal(read());

/** 名次表，0 最靠前；模型选择器据此把最近用过的模型排到前面。 */
export function recentModelRank(): Map<string, number> {
  return new Map(recentModelIds().map((id, index) => [id, index]));
}

/** 记下这次选择。同时记基础模型，让折叠了强度的父级也能排到前面。 */
export function rememberModelPick(agentKind: AgentKind, model: string) {
  const base = splitEffort(model, agentKind === "lyra")?.[0] ?? model;
  const ids = [...new Set([model, base].map((value) => `${agentKind}:${encodeURIComponent(value)}`))];
  const next = [...ids, ...recentModelIds().filter((id) => !ids.includes(id))].slice(0, LIMIT);
  setRecentModelIds(next);
  localStorage.setItem(KEY, JSON.stringify(next));
}
