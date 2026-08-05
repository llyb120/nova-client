import type { ClueCard, ClueNodeGroup } from "./types";

/** 证据链共享计算与展示工具：画布与列表两种视图都基于这套结构。 */

export type ClueRole = "start" | "middle" | "end" | "isolated";

export interface ClueStageGroup {
  group: ClueNodeGroup;
  role: ClueRole;
  depth: number;
}

export interface ClueStage {
  depth: number;
  groups: ClueStageGroup[];
}

export interface ClueStructure {
  /** 按拓扑深度分层、层内按邻居重心排序后的组。 */
  stages: ClueStage[];
  /** 所有被引用为前置的卡片 id。 */
  parentCardIds: Set<string>;
  cardToGroup: Map<string, ClueNodeGroup>;
  depthByGroup: Map<string, number>;
  parentsByGroup: Map<string, Set<string>>;
  childrenByGroup: Map<string, Set<string>>;
}

export function fmtTime(ts: number) {
  return new Date(ts).toLocaleString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function excerpt(text: string, max = 170) {
  const compact = text.replace(/\s+/g, " ").trim();
  return compact.length > max ? `${compact.slice(0, max)}…` : compact;
}

export function authorName(name?: string) {
  return name?.trim() || "历史";
}

export function authorBadge(name?: string) {
  const value = authorName(name);
  const characters = [...value];
  if (characters.length <= 3) return value;
  const words = value.split(/\s+/).filter(Boolean);
  if (words.length > 1) return words.slice(0, 2).map((word) => word[0]).join("").toUpperCase();
  return characters.slice(0, 2).join("");
}

export function roleLabel(role: ClueRole) {
  switch (role) {
    case "start":
      return "START · 起始";
    case "end":
      return "END · 末尾";
    case "isolated":
      return "ISOLATED · 孤立";
    default:
      return "CLUE · 证据";
  }
}

export function groupRole(group: ClueNodeGroup, parentCardIds: Set<string>): ClueRole {
  const hasIncoming = group.parentCardIds.length > 0;
  const hasOutgoing = group.cards.some((card) => parentCardIds.has(card.id));
  if (!hasIncoming) return hasOutgoing ? "start" : "isolated";
  return hasOutgoing ? "middle" : "end";
}

export function stackCards(group: ClueNodeGroup, selectedCardId: string | null) {
  const selected = group.cards.find((card) => card.id === selectedCardId);
  return selected
    ? [...group.cards.filter((card) => card.id !== selected.id), selected]
    : [...group.cards];
}

/**
 * 计算证据链的层级结构：拓扑深度分层 + 层内按邻居重心排序（减少交叉）。
 * 纯函数，画布在此基础上再算几何坐标，列表直接用它渲染大纲。
 */
export function computeClueStructure(groups: ClueNodeGroup[]): ClueStructure {
  const cardToGroup = new Map<string, ClueNodeGroup>();
  for (const group of groups) {
    for (const card of group.cards) cardToGroup.set(card.id, group);
  }

  const depthMemo = new Map<string, number>();
  const depthOf = (group: ClueNodeGroup, visiting = new Set<string>()): number => {
    const cached = depthMemo.get(group.id);
    if (cached !== undefined) return cached;
    if (visiting.has(group.id)) return 0;
    const nextVisiting = new Set(visiting);
    nextVisiting.add(group.id);
    const depth = group.parentCardIds.length
      ? Math.max(
          0,
          ...group.parentCardIds.map((parentId) => {
            const parentGroup = cardToGroup.get(parentId);
            return parentGroup ? depthOf(parentGroup, nextVisiting) + 1 : 0;
          }),
        )
      : 0;
    depthMemo.set(group.id, depth);
    return depth;
  };

  const stageGroups = new Map<number, ClueNodeGroup[]>();
  const parentCardIds = new Set(groups.flatMap((group) => group.parentCardIds));
  const parentsByGroup = new Map<string, Set<string>>();
  const childrenByGroup = new Map<string, Set<string>>();
  for (const group of groups) {
    const depth = depthOf(group);
    stageGroups.set(depth, [...(stageGroups.get(depth) ?? []), group]);
    for (const parentCardId of group.parentCardIds) {
      const parent = cardToGroup.get(parentCardId);
      if (!parent || parent.id === group.id) continue;
      parentsByGroup.set(group.id, new Set([...(parentsByGroup.get(group.id) ?? []), parent.id]));
      childrenByGroup.set(parent.id, new Set([...(childrenByGroup.get(parent.id) ?? []), group.id]));
    }
  }

  const orderedStages = [...stageGroups.entries()].sort(([left], [right]) => left - right);
  for (const [, stageGroupList] of orderedStages) {
    stageGroupList.sort((left, right) => left.createdAt - right.createdAt);
  }
  const reorderByNeighbors = (stageGroupList: ClueNodeGroup[], neighbors: Map<string, Set<string>>) => {
    const positions = new Map<string, number>();
    for (const [, stage] of orderedStages) {
      stage.forEach((group, index) => positions.set(group.id, (index + 0.5) / stage.length));
    }
    const previousOrder = new Map(stageGroupList.map((group, index) => [group.id, index]));
    const score = (group: ClueNodeGroup) => {
      const values = [...(neighbors.get(group.id) ?? [])]
        .map((id) => positions.get(id))
        .filter((value): value is number => value !== undefined);
      return values.length
        ? values.reduce((sum, value) => sum + value, 0) / values.length
        : Number.MAX_SAFE_INTEGER;
    };
    stageGroupList.sort((left, right) => {
      const difference = score(left) - score(right);
      return difference || (previousOrder.get(left.id) ?? 0) - (previousOrder.get(right.id) ?? 0);
    });
  };
  for (let iteration = 0; iteration < 4; iteration += 1) {
    for (let index = 1; index < orderedStages.length; index += 1) {
      reorderByNeighbors(orderedStages[index][1], parentsByGroup);
    }
    for (let index = orderedStages.length - 2; index >= 0; index -= 1) {
      reorderByNeighbors(orderedStages[index][1], childrenByGroup);
    }
  }

  const stages: ClueStage[] = orderedStages.map(([depth, stageGroupList]) => ({
    depth,
    groups: stageGroupList.map((group) => ({ group, role: groupRole(group, parentCardIds), depth })),
  }));

  return { stages, parentCardIds, cardToGroup, depthByGroup: depthMemo, parentsByGroup, childrenByGroup };
}
