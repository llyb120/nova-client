import { confirm, message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import {
  addClueComment,
  associateClues,
  clearClueOpenRequest,
  clueCardById,
  clueCurrentVersion,
  clueMentionPeers,
  deleteClue,
  disassociateClues,
  markClueMentionRead,
  refreshClueGroups,
  splitClue,
  stackClues,
  startSessionFromClue,
  state,
} from "../store";
import type { ClueCard, ClueComment, ClueNodeGroup } from "../types";
import { ClueCaptureModal } from "./ClueCaptureModal";
import { IconClue, IconMove, IconPlus } from "./icons";
import { MentionPicker } from "./MentionPicker";

type Placement = "update" | "parallel" | "new";
type Point = { x: number; y: number };
type Camera = { x: number; y: number; zoom: number };

type GroupNode = {
  group: ClueNodeGroup;
  cards: ClueCard[];
  role: "start" | "middle" | "end" | "isolated";
  depth: number;
  x: number;
  y: number;
};

type GraphEdge = {
  fromCardId: string;
  toCardIds: string[];
  points: Point[];
};

type GraphLayout = {
  nodes: GroupNode[];
  stages: Array<{ depth: number; x: number }>;
  edges: GraphEdge[];
  cardAnchors: Map<string, Point>;
  maxX: number;
  maxY: number;
};

type GraphStructure = {
  nodes: Array<Omit<GroupNode, "cards">>;
  stages: Array<{ depth: number; x: number }>;
  layoutTop: number;
};

const CARD_WIDTH = 236;
const CARD_HEIGHT = 296;
const STACK_TITLE_PEEK = 48;
const COLUMN_GAP = 150;
const ROW_GAP = 105;
const WORLD_LEFT = 110;
const WORLD_TOP = 104;
const EDGE_LANE_GAP = 14;
const PORT_CENTER_Y = 66.5;
const OUTPUT_ANCHOR_X = CARD_WIDTH + 4.5;
const INPUT_ANCHOR_X = -4.5;
const MIN_ZOOM = 0.42;
const MAX_ZOOM = 1.65;

function fmtTime(ts: number) {
  return new Date(ts).toLocaleString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function excerpt(text: string, max = 170) {
  const compact = text.replace(/\s+/g, " ").trim();
  return compact.length > max ? `${compact.slice(0, max)}…` : compact;
}

function authorName(name?: string) {
  return name?.trim() || "历史";
}

function authorBadge(name?: string) {
  const value = authorName(name);
  const characters = [...value];
  if (characters.length <= 3) return value;
  const words = value.split(/\s+/).filter(Boolean);
  if (words.length > 1) return words.slice(0, 2).map((word) => word[0]).join("").toUpperCase();
  return characters.slice(0, 2).join("");
}

function roleLabel(role: GroupNode["role"]) {
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

function groupRole(group: ClueNodeGroup, parentCardIds: Set<string>): GroupNode["role"] {
  const hasIncoming = group.parentCardIds.length > 0;
  const hasOutgoing = group.cards.some((card) => parentCardIds.has(card.id));
  if (!hasIncoming) return hasOutgoing ? "start" : "isolated";
  return hasOutgoing ? "middle" : "end";
}

function stackCards(group: ClueNodeGroup, selectedCardId: string | null) {
  const selected = group.cards.find((card) => card.id === selectedCardId);
  return selected
    ? [...group.cards.filter((card) => card.id !== selected.id), selected]
    : [...group.cards];
}

function traceRoundedRoute(context: CanvasRenderingContext2D, points: Point[], radius: number) {
  if (points.length < 2) return;
  context.beginPath();
  context.moveTo(points[0].x, points[0].y);
  for (let index = 1; index < points.length - 1; index += 1) {
    const previous = points[index - 1];
    const current = points[index];
    const next = points[index + 1];
    const incoming = Math.min(radius, Math.hypot(current.x - previous.x, current.y - previous.y) / 2);
    const outgoing = Math.min(radius, Math.hypot(next.x - current.x, next.y - current.y) / 2);
    const before = {
      x: current.x - Math.sign(current.x - previous.x) * incoming,
      y: current.y - Math.sign(current.y - previous.y) * incoming,
    };
    const after = {
      x: current.x + Math.sign(next.x - current.x) * outgoing,
      y: current.y + Math.sign(next.y - current.y) * outgoing,
    };
    context.lineTo(before.x, before.y);
    context.quadraticCurveTo(current.x, current.y, after.x, after.y);
  }
  const last = points[points.length - 1];
  context.lineTo(last.x, last.y);
}

function drawArrow(context: CanvasRenderingContext2D, from: Point, to: Point, size: number, color: string) {
  const angle = Math.atan2(to.y - from.y, to.x - from.x);
  context.beginPath();
  context.moveTo(to.x, to.y);
  context.lineTo(
    to.x - size * Math.cos(angle - Math.PI / 6),
    to.y - size * Math.sin(angle - Math.PI / 6),
  );
  context.lineTo(
    to.x - size * Math.cos(angle + Math.PI / 6),
    to.y - size * Math.sin(angle + Math.PI / 6),
  );
  context.closePath();
  context.fillStyle = color;
  context.fill();
}

function distanceToSegment(point: Point, start: Point, end: Point) {
  const dx = end.x - start.x;
  const dy = end.y - start.y;
  if (dx === 0 && dy === 0) return Math.hypot(point.x - start.x, point.y - start.y);
  const progress = Math.max(
    0,
    Math.min(1, ((point.x - start.x) * dx + (point.y - start.y) * dy) / (dx * dx + dy * dy)),
  );
  return Math.hypot(point.x - (start.x + progress * dx), point.y - (start.y + progress * dy));
}

function distanceToEdge(point: Point, edge: GraphEdge) {
  let distance = Number.POSITIVE_INFINITY;
  for (let index = 1; index < edge.points.length; index += 1) {
    distance = Math.min(distance, distanceToSegment(point, edge.points[index - 1], edge.points[index]));
  }
  return distance;
}

export function EvidenceChainView() {
  const [selectedCardId, setSelectedCardId] = createSignal<string | null>(null);
  const [capture, setCapture] = createSignal<{
    placement: Placement;
    targetCardId: string | null;
  } | null>(null);
  const [selectedCardIds, setSelectedCardIds] = createSignal<Set<string>>(new Set());
  const [deletingCardId, setDeletingCardId] = createSignal<string | null>(null);
  const [splittingCardId, setSplittingCardId] = createSignal<string | null>(null);
  const [stacking, setStacking] = createSignal(false);
  const [camera, setCamera] = createSignal<Camera>({ x: 40, y: 40, zoom: 1 });
  const [nodePositions, setNodePositions] = createSignal<Map<string, Point>>(new Map());
  const [draggingGroupId, setDraggingGroupId] = createSignal<string | null>(null);
  const [connecting, setConnecting] = createSignal<{
    fromCardId: string;
    pointerId: number;
    pointer: Point;
  } | null>(null);
  const [connectionTargetId, setConnectionTargetId] = createSignal<string | null>(null);
  const [connectionBusy, setConnectionBusy] = createSignal(false);
  const [edgeMenu, setEdgeMenu] = createSignal<{
    x: number;
    y: number;
    beforeCardId: string;
    afterCardId: string;
  } | null>(null);
  const [edgeBusy, setEdgeBusy] = createSignal(false);
  const [commentText, setCommentText] = createSignal("");
  const [commentMentions, setCommentMentions] = createSignal<string[]>([]);
  const [replyToCommentId, setReplyToCommentId] = createSignal<string | null>(null);
  const [commentBusy, setCommentBusy] = createSignal(false);
  let viewportElement: HTMLDivElement | undefined;
  let canvasElement: HTMLCanvasElement | undefined;
  let commentInputElement: HTMLTextAreaElement | undefined;
  let composingCardId: string | null | undefined;
  let resizeObserver: ResizeObserver | undefined;
  let drawFrame: number | undefined;
  let dragFrame: number | undefined;
  let pendingDragPosition: { groupId: string; point: Point } | undefined;
  let fitted = false;
  let panGesture: {
    pointerId: number;
    startX: number;
    startY: number;
    cameraX: number;
    cameraY: number;
  } | null = null;
  let nodeDrag: {
    pointerId: number;
    groupId: string;
    startX: number;
    startY: number;
    nodeX: number;
    nodeY: number;
  } | null = null;

  const cardToGroup = createMemo(() => {
    const map = new Map<string, ClueNodeGroup>();
    for (const group of state.clueGroups) {
      for (const card of group.cards) map.set(card.id, group);
    }
    return map;
  });

  const cards = createMemo(() => state.clueGroups.flatMap((group) => group.cards));

  // 拓扑排序和自动布局只依赖线索结构。选中卡片、节点拖动不再触发这部分昂贵计算。
  const graphStructure = createMemo<GraphStructure>(() => {
    const byCard = cardToGroup();
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
              const parentGroup = byCard.get(parentId);
              return parentGroup ? depthOf(parentGroup, nextVisiting) + 1 : 0;
            }),
          )
        : 0;
      depthMemo.set(group.id, depth);
      return depth;
    };

    const stageGroups = new Map<number, ClueNodeGroup[]>();
    const parentCardIds = new Set(state.clueGroups.flatMap((group) => group.parentCardIds));
    const parentsByGroup = new Map<string, Set<string>>();
    const childrenByGroup = new Map<string, Set<string>>();
    for (const group of state.clueGroups) {
      const depth = depthOf(group);
      stageGroups.set(depth, [...(stageGroups.get(depth) ?? []), group]);
      for (const parentCardId of group.parentCardIds) {
        const parent = byCard.get(parentCardId);
        if (!parent || parent.id === group.id) continue;
        parentsByGroup.set(group.id, new Set([...(parentsByGroup.get(group.id) ?? []), parent.id]));
        childrenByGroup.set(parent.id, new Set([...(childrenByGroup.get(parent.id) ?? []), group.id]));
      }
    }

    const orderedStages = [...stageGroups.entries()].sort(([left], [right]) => left - right);
    for (const [, groups] of orderedStages) {
      groups.sort((left, right) => left.createdAt - right.createdAt);
    }
    const reorderByNeighbors = (groups: ClueNodeGroup[], neighbors: Map<string, Set<string>>) => {
      const positions = new Map<string, number>();
      for (const [, stage] of orderedStages) {
        stage.forEach((group, index) => positions.set(group.id, (index + 0.5) / stage.length));
      }
      const previousOrder = new Map(groups.map((group, index) => [group.id, index]));
      const score = (group: ClueNodeGroup) => {
        const values = [...(neighbors.get(group.id) ?? [])]
          .map((id) => positions.get(id))
          .filter((value): value is number => value !== undefined);
        return values.length
          ? values.reduce((sum, value) => sum + value, 0) / values.length
          : Number.MAX_SAFE_INTEGER;
      };
      groups.sort((left, right) => {
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

    const longEdgeCount = state.clueGroups.reduce((count, group) => {
      const targetDepth = depthMemo.get(group.id) ?? 0;
      return count + group.parentCardIds.filter((parentCardId) => {
        const parent = byCard.get(parentCardId);
        return parent && targetDepth - (depthMemo.get(parent.id) ?? 0) > 1;
      }).length;
    }, 0);
    const layoutTop = WORLD_TOP + longEdgeCount * EDGE_LANE_GAP;
    const groupHeight = (group: ClueNodeGroup) =>
      CARD_HEIGHT + Math.max(0, group.cards.length - 1) * STACK_TITLE_PEEK;
    const stageHeight = (groups: ClueNodeGroup[]) =>
      groups.reduce((sum, group) => sum + groupHeight(group), 0)
      + Math.max(0, groups.length - 1) * ROW_GAP;
    const groupsById = new Map(state.clueGroups.map((group) => [group.id, group]));
    const unassignedGroupIds = new Set(groupsById.keys());
    const components: ClueNodeGroup[][] = [];
    for (const seed of [...state.clueGroups].sort((left, right) => left.createdAt - right.createdAt)) {
      if (!unassignedGroupIds.delete(seed.id)) continue;
      const component: ClueNodeGroup[] = [];
      const queue = [seed.id];
      for (let index = 0; index < queue.length; index += 1) {
        const groupId = queue[index];
        const group = groupsById.get(groupId);
        if (group) component.push(group);
        const neighbors = new Set([
          ...(parentsByGroup.get(groupId) ?? []),
          ...(childrenByGroup.get(groupId) ?? []),
        ]);
        for (const neighborId of neighbors) {
          if (unassignedGroupIds.delete(neighborId)) queue.push(neighborId);
        }
      }
      components.push(component);
    }

    const groupY = new Map<string, number>();
    let componentTop = layoutTop;
    for (const component of components) {
      const componentIds = new Set(component.map((group) => group.id));
      const componentStages = orderedStages
        .map(([, groups]) => groups.filter((group) => componentIds.has(group.id)))
        .filter((groups) => groups.length > 0);
      const componentHeight = Math.max(...componentStages.map(stageHeight));
      for (const groups of componentStages) {
        let y = componentTop + (componentHeight - stageHeight(groups)) / 2;
        for (const group of groups) {
          groupY.set(group.id, y);
          y += groupHeight(group) + ROW_GAP;
        }
      }
      componentTop += componentHeight + ROW_GAP;
    }

    const nodes: GraphStructure["nodes"] = [];
    const stages: Array<{ depth: number; x: number }> = [];
    for (const [depth, groups] of orderedStages) {
      const x = WORLD_LEFT + depth * (CARD_WIDTH + COLUMN_GAP);
      stages.push({ depth, x });
      groups.forEach((group) => {
        nodes.push({
          group,
          role: groupRole(group, parentCardIds),
          depth,
          x,
          y: groupY.get(group.id) ?? layoutTop,
        });
      });
    }

    return { nodes, stages, layoutTop };
  });

  // 交互层只套用卡片前后顺序和手动坐标，再更新锚点/连线；拖动时不重跑拓扑布局。
  const graphLayout = createMemo<GraphLayout>(() => {
    const structure = graphStructure();
    const positions = nodePositions();
    const selected = selectedCardId();
    const nodes: GroupNode[] = structure.nodes.map((node) => {
      const manualPosition = positions.get(node.group.id);
      return {
        ...node,
        cards: stackCards(node.group, selected),
        x: manualPosition?.x ?? node.x,
        y: manualPosition?.y ?? node.y,
      };
    });
    const { stages, layoutTop } = structure;

    const nodeByCard = new Map<string, GroupNode>();
    const cardAnchors = new Map<string, Point>();
    for (const node of nodes) {
      const frontOffset = Math.max(0, node.cards.length - 1) * STACK_TITLE_PEEK;
      const commonOutput = {
        x: node.x + OUTPUT_ANCHOR_X,
        y: node.y + PORT_CENTER_Y + frontOffset,
      };
      node.cards.forEach((card) => {
        nodeByCard.set(card.id, node);
        cardAnchors.set(card.id, commonOutput);
      });
    }

    const rawEdges: Array<{
      key: string;
      fromCardId: string;
      toCardIds: string[];
      start: Point;
      end: Point;
    }> = [];
    // 同一组 → 同一组只画一条线：堆叠后多张前置卡共享锚点，按卡去重前会画出重合的多条边
    const seenGroupEdges = new Set<string>();
    for (const node of nodes) {
      for (const parentCardId of node.group.parentCardIds) {
        const sourceNode = nodeByCard.get(parentCardId);
        const start = cardAnchors.get(parentCardId);
        if (!sourceNode || !start) continue;
        const groupEdgeKey = `${sourceNode.group.id}->${node.group.id}`;
        if (seenGroupEdges.has(groupEdgeKey)) continue;
        seenGroupEdges.add(groupEdgeKey);
        rawEdges.push({
          key: `${sourceNode.depth}:${node.depth}`,
          fromCardId: parentCardId,
          toCardIds: node.group.cards.map((card) => card.id),
          start,
          end: {
            x: node.x + INPUT_ANCHOR_X,
            y: node.y + PORT_CENTER_Y + Math.max(0, node.cards.length - 1) * STACK_TITLE_PEEK,
          },
        });
      }
    }

    const edges: GraphEdge[] = [];
    const edgeGroups = new Map<string, typeof rawEdges>();
    const longEdges = rawEdges
      .filter((edge) => {
        const [sourceDepth, targetDepth] = edge.key.split(":").map(Number);
        return targetDepth - sourceDepth > 1;
      })
      .sort((left, right) => left.start.y - right.start.y || left.end.y - right.end.y);
    const longEdgeSet = new Set(longEdges);
    for (const edge of rawEdges) {
      if (longEdgeSet.has(edge)) continue;
      edgeGroups.set(edge.key, [...(edgeGroups.get(edge.key) ?? []), edge]);
    }
    for (const groupedEdges of edgeGroups.values()) {
      groupedEdges.sort((left, right) => left.start.y - right.start.y || left.end.y - right.end.y);
      groupedEdges.forEach((edge, index) => {
        const laneOffset = (index - (groupedEdges.length - 1) / 2) * 15;
        const desiredLane = edge.start.x + (edge.end.x - edge.start.x) * 0.5 + laneOffset;
        const laneX = Math.max(edge.start.x + 28, Math.min(edge.end.x - 28, desiredLane));
        edges.push({
          fromCardId: edge.fromCardId,
          toCardIds: edge.toCardIds,
          points: [edge.start, { x: laneX, y: edge.start.y }, { x: laneX, y: edge.end.y }, edge.end],
        });
      });
    }
    longEdges.forEach((edge, index) => {
      const railY = layoutTop - 28 - index * EDGE_LANE_GAP;
      const sourceLaneX = edge.start.x + COLUMN_GAP / 2;
      const targetLaneX = edge.end.x - COLUMN_GAP / 2;
      edges.push({
        fromCardId: edge.fromCardId,
        toCardIds: edge.toCardIds,
        points: [
          edge.start,
          { x: sourceLaneX, y: edge.start.y },
          { x: sourceLaneX, y: railY },
          { x: targetLaneX, y: railY },
          { x: targetLaneX, y: edge.end.y },
          edge.end,
        ],
      });
    });

    const maxX = Math.max(
      520,
      ...nodes.map((node) => node.x + CARD_WIDTH + 80),
    );
    const maxY = Math.max(
      420,
      ...nodes.map(
        (node) => node.y + CARD_HEIGHT + Math.max(0, node.cards.length - 1) * STACK_TITLE_PEEK + 80,
      ),
    );
    return { nodes, stages, edges, cardAnchors, maxX, maxY };
  });

  const selectedCard = createMemo(() => clueCardById(selectedCardId()));
  const selectedGroup = createMemo(() => {
    const id = selectedCardId();
    return id ? cardToGroup().get(id) : undefined;
  });
  const predecessors = createMemo(() =>
    (selectedGroup()?.parentCardIds ?? [])
      .map((id) => clueCardById(id))
      .filter((card): card is ClueCard => !!card),
  );
  const successors = createMemo(() => {
    const id = selectedCardId();
    if (!id) return [];
    return state.clueGroups
      .filter((group) => group.parentCardIds.includes(id))
      .flatMap((group) => group.cards);
  });
  const mentionPeers = createMemo(clueMentionPeers);
  const replyTarget = createMemo(() => {
    const replyId = replyToCommentId();
    return replyId
      ? (selectedCard()?.comments ?? []).find((comment) => comment.id === replyId)
      : undefined;
  });

  const scheduleDraw = () => {
    if (drawFrame !== undefined) cancelAnimationFrame(drawFrame);
    drawFrame = requestAnimationFrame(() => {
      drawFrame = undefined;
      drawCanvas();
    });
  };

  const drawCanvas = () => {
    const viewport = viewportElement;
    const canvas = canvasElement;
    if (!viewport || !canvas) return;
    const width = viewport.clientWidth;
    const height = viewport.clientHeight;
    const pixelRatio = window.devicePixelRatio || 1;
    const targetWidth = Math.round(width * pixelRatio);
    const targetHeight = Math.round(height * pixelRatio);
    if (canvas.width !== targetWidth || canvas.height !== targetHeight) {
      canvas.width = targetWidth;
      canvas.height = targetHeight;
    }
    canvas.style.width = `${width}px`;
    canvas.style.height = `${height}px`;
    const context = canvas.getContext("2d");
    if (!context) return;
    const style = getComputedStyle(viewport);
    const muted = style.getPropertyValue("--text-faint").trim() || "#7d8799";
    const accent = style.getPropertyValue("--accent").trim() || "#3465c8";
    const surface = style.getPropertyValue("--bg-panel").trim() || "#ffffff";
    const currentCamera = camera();

    context.setTransform(pixelRatio, 0, 0, pixelRatio, 0, 0);
    context.clearRect(0, 0, width, height);
    const grid = Math.max(16, 30 * currentCamera.zoom);
    const gridX = ((currentCamera.x % grid) + grid) % grid;
    const gridY = ((currentCamera.y % grid) + grid) % grid;
    context.fillStyle = muted;
    context.globalAlpha = 0.34;
    for (let x = gridX; x < width; x += grid) {
      for (let y = gridY; y < height; y += grid) {
        context.beginPath();
        context.arc(x, y, 1, 0, Math.PI * 2);
        context.fill();
      }
    }
    context.globalAlpha = 1;
    context.translate(currentCamera.x, currentCamera.y);
    context.scale(currentCamera.zoom, currentCamera.zoom);

    const selected = selectedCardId();
    const activeCardIds = new Set(
      selected ? (cardToGroup().get(selected)?.cards.map((card) => card.id) ?? [selected]) : [],
    );
    const edgeIsActive = (edge: GraphEdge) =>
      activeCardIds.has(edge.fromCardId) || edge.toCardIds.some((cardId) => activeCardIds.has(cardId));
    const orderedEdges = [...graphLayout().edges].sort((left, right) => {
      return Number(edgeIsActive(left)) - Number(edgeIsActive(right));
    });
    for (const edge of orderedEdges) {
      const active = edgeIsActive(edge);
      traceRoundedRoute(context, edge.points, 14 / currentCamera.zoom);
      context.strokeStyle = surface;
      context.lineWidth = (active ? 8 : 6) / currentCamera.zoom;
      context.stroke();
      traceRoundedRoute(context, edge.points, 14 / currentCamera.zoom);
      context.strokeStyle = active ? accent : muted;
      context.globalAlpha = active ? 0.95 : 0.56;
      context.lineWidth = (active ? 2.6 : 1.45) / currentCamera.zoom;
      context.stroke();
      context.globalAlpha = 1;
      drawArrow(
        context,
        edge.points[edge.points.length - 2],
        edge.points[edge.points.length - 1],
        (active ? 11 : 9) / currentCamera.zoom,
        active ? accent : muted,
      );
    }

    const pendingConnection = connecting();
    if (pendingConnection) {
      const start = graphLayout().cardAnchors.get(pendingConnection.fromCardId);
      if (start) {
        const end = pendingConnection.pointer;
        const laneX = start.x + Math.max(34, (end.x - start.x) * 0.5);
        const points = [start, { x: laneX, y: start.y }, { x: laneX, y: end.y }, end];
        context.setLineDash([8 / currentCamera.zoom, 6 / currentCamera.zoom]);
        traceRoundedRoute(context, points, 12 / currentCamera.zoom);
        context.strokeStyle = accent;
        context.lineWidth = 2 / currentCamera.zoom;
        context.stroke();
        context.setLineDash([]);
      }
    }
  };

  const fitGraph = () => {
    const viewport = viewportElement;
    const layout = graphLayout();
    if (!viewport || layout.nodes.length === 0 || viewport.clientWidth <= 0 || viewport.clientHeight <= 0) {
      return false;
    }
    const padding = 54;
    const zoom = Math.max(
      MIN_ZOOM,
      Math.min(
        1,
        (viewport.clientWidth - padding * 2) / layout.maxX,
        (viewport.clientHeight - padding * 2) / layout.maxY,
      ),
    );
    setCamera({
      x: Math.max(padding, (viewport.clientWidth - layout.maxX * zoom) / 2),
      y: Math.max(padding, (viewport.clientHeight - layout.maxY * zoom) / 2),
      zoom,
    });
    return true;
  };

  const zoomAt = (nextZoom: number, screenX?: number, screenY?: number) => {
    const viewport = viewportElement;
    if (!viewport) return;
    const current = camera();
    const zoom = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, nextZoom));
    const x = screenX ?? viewport.clientWidth / 2;
    const y = screenY ?? viewport.clientHeight / 2;
    const worldX = (x - current.x) / current.zoom;
    const worldY = (y - current.y) / current.zoom;
    setCamera({ x: x - worldX * zoom, y: y - worldY * zoom, zoom });
  };

  const screenToWorld = (clientX: number, clientY: number): Point => {
    const viewport = viewportElement;
    const current = camera();
    if (!viewport) return { x: 0, y: 0 };
    const rect = viewport.getBoundingClientRect();
    return {
      x: (clientX - rect.left - current.x) / current.zoom,
      y: (clientY - rect.top - current.y) / current.zoom,
    };
  };

  const selectOnly = (cardId: string) => {
    markClueMentionRead(cardId);
    setSelectedCardId(cardId);
    setSelectedCardIds(new Set([cardId]));
  };

  const beginConnection = (cardId: string, event: PointerEvent) => {
    if (connectionBusy()) return;
    event.preventDefault();
    event.stopPropagation();
    selectOnly(cardId);
    setConnecting({
      fromCardId: cardId,
      pointerId: event.pointerId,
      pointer: screenToWorld(event.clientX, event.clientY),
    });
    viewportElement?.setPointerCapture(event.pointerId);
  };

  const beginNodeDrag = (groupId: string, cardId: string, event: PointerEvent) => {
    if (event.button !== 0 || (event.target as HTMLElement).closest(".clue-port")) return;
    if (event.ctrlKey || event.metaKey) return;
    const pendingConnection = connecting();
    if (pendingConnection) {
      event.preventDefault();
      event.stopPropagation();
      if (pendingConnection.fromCardId === cardId) cancelPointerAction();
      else void finishConnection(cardId);
      return;
    }
    const node = graphLayout().nodes.find((item) => item.group.id === groupId);
    if (!node) return;
    event.stopPropagation();
    selectOnly(cardId);
    setDraggingGroupId(groupId);
    nodeDrag = {
      pointerId: event.pointerId,
      groupId,
      startX: event.clientX,
      startY: event.clientY,
      nodeX: node.x,
      nodeY: node.y,
    };
    viewportElement?.setPointerCapture(event.pointerId);
  };

  const finishConnection = async (targetCardId?: string) => {
    const pending = connecting();
    const target = targetCardId ?? connectionTargetId();
    setConnecting(null);
    setConnectionTargetId(null);
    if (!pending || !target || target === pending.fromCardId) return;
    setConnectionBusy(true);
    try {
      await associateClues(pending.fromCardId, target);
      selectOnly(target);
    } catch (error) {
      await message(String(error), { title: "连接失败", kind: "error" });
    } finally {
      setConnectionBusy(false);
    }
  };

  const onPointerDown = (event: PointerEvent) => {
    const target = event.target as HTMLElement;
    if (!target.closest(".clue-edge-menu")) setEdgeMenu(null);
    if (target.closest(".clue-group-node, .clue-canvas-toolbar, .clue-edge-menu")) return;
    if (event.button !== 0 && event.button !== 1) return;
    if (connecting()) {
      cancelPointerAction();
      return;
    }
    const current = camera();
    panGesture = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      cameraX: current.x,
      cameraY: current.y,
    };
    viewportElement?.setPointerCapture(event.pointerId);
  };

  const flushDragPosition = () => {
    dragFrame = undefined;
    const pending = pendingDragPosition;
    pendingDragPosition = undefined;
    if (!pending) return;
    const next = new Map(nodePositions());
    next.set(pending.groupId, pending.point);
    setNodePositions(next);
  };

  const scheduleDragPosition = (groupId: string, point: Point) => {
    pendingDragPosition = { groupId, point };
    if (dragFrame === undefined) dragFrame = requestAnimationFrame(flushDragPosition);
  };

  const onPointerMove = (event: PointerEvent) => {
    const pending = connecting();
    if (pending) {
      setConnecting({ ...pending, pointer: screenToWorld(event.clientX, event.clientY) });
      const target = (document.elementFromPoint(event.clientX, event.clientY) as HTMLElement | null)
        ?.closest(".clue-group-node")
        ?.getAttribute("data-clue-target-id");
      setConnectionTargetId(target && target !== pending.fromCardId ? target : null);
      return;
    }
    if (nodeDrag?.pointerId === event.pointerId) {
      const zoom = camera().zoom;
      scheduleDragPosition(nodeDrag.groupId, {
        x: nodeDrag.nodeX + (event.clientX - nodeDrag.startX) / zoom,
        y: nodeDrag.nodeY + (event.clientY - nodeDrag.startY) / zoom,
      });
      return;
    }
    if (!panGesture || panGesture.pointerId !== event.pointerId) return;
    setCamera((current) => ({
      ...current,
      x: panGesture!.cameraX + event.clientX - panGesture!.startX,
      y: panGesture!.cameraY + event.clientY - panGesture!.startY,
    }));
  };

  const onPointerUp = (event: PointerEvent) => {
    if (viewportElement?.hasPointerCapture(event.pointerId)) {
      viewportElement.releasePointerCapture(event.pointerId);
    }
    if (connecting()?.pointerId === event.pointerId && connectionTargetId()) void finishConnection();
    if (nodeDrag?.pointerId === event.pointerId) {
      if (dragFrame !== undefined) cancelAnimationFrame(dragFrame);
      flushDragPosition();
      nodeDrag = null;
      setDraggingGroupId(null);
    }
    if (panGesture?.pointerId === event.pointerId) panGesture = null;
  };

  const cancelPointerAction = () => {
    if (dragFrame !== undefined) cancelAnimationFrame(dragFrame);
    dragFrame = undefined;
    pendingDragPosition = undefined;
    panGesture = null;
    nodeDrag = null;
    setDraggingGroupId(null);
    setConnecting(null);
    setConnectionTargetId(null);
  };

  const onWheel = (event: WheelEvent) => {
    event.preventDefault();
    const viewport = viewportElement;
    if (!viewport) return;
    const rect = viewport.getBoundingClientRect();
    zoomAt(
      camera().zoom * Math.exp(-event.deltaY * 0.0015),
      event.clientX - rect.left,
      event.clientY - rect.top,
    );
  };

  const onEdgeContextMenu = (event: MouseEvent) => {
    if ((event.target as HTMLElement).closest(".clue-group-node, .clue-canvas-toolbar")) return;
    const point = screenToWorld(event.clientX, event.clientY);
    const edge = graphLayout().edges
      .map((item) => ({ item, distance: distanceToEdge(point, item) }))
      .sort((left, right) => left.distance - right.distance)[0];
    if (!edge || edge.distance > 24 / camera().zoom || edge.item.toCardIds.length === 0) return;
    event.preventDefault();
    const rect = viewportElement?.getBoundingClientRect();
    if (!rect) return;
    setEdgeMenu({
      x: event.clientX - rect.left,
      y: event.clientY - rect.top,
      beforeCardId: edge.item.fromCardId,
      afterCardId: edge.item.toCardIds[0],
    });
  };

  const arrangeGraph = () => {
    setNodePositions(new Map());
    requestAnimationFrame(fitGraph);
  };

  const removeConnection = async () => {
    const edge = edgeMenu();
    if (!edge || edgeBusy()) return;
    setEdgeBusy(true);
    try {
      await disassociateClues(edge.beforeCardId, edge.afterCardId);
      setEdgeMenu(null);
    } catch (error) {
      await message(String(error), { title: "删除连接失败", kind: "error" });
    } finally {
      setEdgeBusy(false);
    }
  };

  const removeCard = async (card: ClueCard) => {
    const title = clueCurrentVersion(card)?.title || "未命名线索";
    const accepted = await confirm(`删除线索「${title}」？下游线索会保留，但不再以它作为前置。`, {
      title: "删除线索",
      kind: "warning",
    });
    if (!accepted) return;
    setDeletingCardId(card.id);
    try {
      await deleteClue(card.id);
    } catch (error) {
      await message(String(error), { title: "删除失败", kind: "error" });
    } finally {
      setDeletingCardId(null);
    }
  };

  const selectCard = (cardId: string, event: MouseEvent) => {
    if (!event.ctrlKey && !event.metaKey) {
      selectOnly(cardId);
      return;
    }
    const next = new Set(selectedCardIds());
    if (next.has(cardId)) next.delete(cardId);
    else next.add(cardId);
    if (next.size === 0) next.add(cardId);
    setSelectedCardIds(next);
    setSelectedCardId(cardId);
  };

  const splitSelectedCard = async (card: ClueCard) => {
    if (splittingCardId()) return;
    setSplittingCardId(card.id);
    try {
      await splitClue(card.id);
      setSelectedCardIds(new Set([card.id]));
      requestAnimationFrame(fitGraph);
    } catch (error) {
      await message(String(error), { title: "拆分失败", kind: "error" });
    } finally {
      setSplittingCardId(null);
    }
  };

  const stackSelectedCards = async () => {
    const cardIds = [...selectedCardIds()];
    if (cardIds.length < 2 || stacking()) return;
    setStacking(true);
    try {
      await stackClues(cardIds);
      const selected = selectedCardId() ?? cardIds[0];
      setSelectedCardIds(new Set([selected]));
      setNodePositions(new Map());
      requestAnimationFrame(fitGraph);
    } catch (error) {
      await message(String(error), { title: "堆叠失败", kind: "error" });
    } finally {
      setStacking(false);
    }
  };

  const beginReply = (comment: ClueComment) => {
    setReplyToCommentId(comment.id);
    const myToken = state.settings?.relayToken ?? "";
    setCommentMentions(
      comment.authorToken && comment.authorToken !== myToken ? [comment.authorToken] : [],
    );
    requestAnimationFrame(() => commentInputElement?.focus());
  };

  const cancelReply = () => {
    setReplyToCommentId(null);
    setCommentMentions([]);
  };

  const submitComment = async () => {
    const card = selectedCard();
    const content = commentText().trim();
    if (!card || !content || commentBusy()) return;
    setCommentBusy(true);
    try {
      await addClueComment(card.id, content, replyToCommentId(), commentMentions());
      setCommentText("");
      setCommentMentions([]);
      setReplyToCommentId(null);
    } catch (error) {
      await message(String(error), { title: "评论失败", kind: "error" });
    } finally {
      setCommentBusy(false);
    }
  };

  createEffect(() => {
    const cardId = selectedCardId();
    if (composingCardId !== undefined && composingCardId !== cardId) {
      setCommentText("");
      setCommentMentions([]);
      setReplyToCommentId(null);
    }
    composingCardId = cardId;
  });

  createEffect(() => {
    const available = cards();
    const request = state.clueOpenRequest;
    if (request && available.some((card) => card.id === request)) {
      setSelectedCardId(request);
      setSelectedCardIds(new Set([request]));
      clearClueOpenRequest(request);
      return;
    }
    const selected = selectedCardId();
    if (selected && available.some((card) => card.id === selected)) {
      const availableIds = new Set(available.map((card) => card.id));
      const next = new Set([...selectedCardIds()].filter((cardId) => availableIds.has(cardId)));
      if (next.size !== selectedCardIds().size) setSelectedCardIds(next);
      return;
    }
    const preferred = state.pendingClueCard?.id;
    const next = (preferred && available.some((card) => card.id === preferred) ? preferred : available[0]?.id) ?? null;
    setSelectedCardId(next);
    setSelectedCardIds(new Set(next ? [next] : []));
  });

  createEffect(() => {
    const layout = graphLayout();
    camera();
    connecting();
    selectedCardId();
    scheduleDraw();
    if (!fitted && layout.nodes.length > 0 && viewportElement) {
      requestAnimationFrame(() => {
        if (!fitted && fitGraph()) fitted = true;
      });
    }
  });

  onMount(() => {
    void refreshClueGroups().then(() => {
      requestAnimationFrame(() => {
        if (fitGraph()) fitted = true;
      });
    });
    resizeObserver = new ResizeObserver(() => {
      scheduleDraw();
      if (!fitted && graphLayout().nodes.length > 0) {
        fitted = fitGraph();
      }
    });
    if (viewportElement) resizeObserver.observe(viewportElement);
    scheduleDraw();
  });

  onCleanup(() => {
    if (drawFrame !== undefined) cancelAnimationFrame(drawFrame);
    if (dragFrame !== undefined) cancelAnimationFrame(dragFrame);
    resizeObserver?.disconnect();
  });

  return (
    <main class="clue-view">
      <header class="clue-head">
        <div>
          <h1 class="clue-title">证据链</h1>
          <p class="clue-sub">拖动空白处平移，滚轮缩放；拖动卡牌调整位置，从右侧连接点建立顺序。</p>
        </div>
        <div class="clue-head-actions">
          <button class="btn primary" onClick={() => setCapture({ placement: "new", targetCardId: null })}>
            <IconPlus size={14} />
            新建线索
          </button>
        </div>
      </header>

      <Show
        when={cards().length > 0}
        fallback={
          <div class="clue-empty">
            <IconClue size={34} />
            <p>还没有线索。</p>
            <span>完成一轮普通会话后点击“生成线索”，或在这里新建第一条线索。</span>
          </div>
        }
      >
        <div class="clue-layout">
          <div
            classList={{ "clue-canvas": true, connecting: !!connecting() }}
            ref={(element) => (viewportElement = element)}
            onPointerDown={onPointerDown}
            onPointerMove={onPointerMove}
            onPointerUp={onPointerUp}
            onPointerCancel={cancelPointerAction}
            onWheel={onWheel}
            onContextMenu={onEdgeContextMenu}
          >
            <canvas class="clue-canvas-lines" ref={(element) => (canvasElement = element)} />
            <div
              class="clue-canvas-world"
              style={{ transform: `translate(${camera().x}px, ${camera().y}px) scale(${camera().zoom})` }}
            >
              <For each={graphLayout().stages}>
                {(stage) => (
                  <div class="clue-canvas-stage" style={{ left: `${stage.x}px`, top: "48px" }}>
                    {stage.depth === 0 ? "起点" : `第 ${stage.depth + 1} 步`}
                  </div>
                )}
              </For>
              <For each={graphLayout().nodes}>
                {(node) => (
                  <section
                    class="clue-group-node"
                    classList={{
                      stacked: node.cards.length > 1,
                      dragging: draggingGroupId() === node.group.id,
                      "connection-source": node.group.cards.some(
                        (card) => connecting()?.fromCardId === card.id,
                      ),
                      "connection-target": node.group.cards.some(
                        (card) => connectionTargetId() === card.id,
                      ),
                    }}
                    data-clue-target-id={node.cards[node.cards.length - 1]?.id}
                    style={{
                      left: `${node.x}px`,
                      top: `${node.y}px`,
                      width: `${CARD_WIDTH}px`,
                      height: `${CARD_HEIGHT + Math.max(0, node.cards.length - 1) * STACK_TITLE_PEEK}px`,
                    }}
                  >
                    <Show when={node.cards.length > 1}>
                      <div class="clue-stack-count">{node.cards.length} 张平行线索</div>
                    </Show>
                    <span
                      class="clue-port input"
                      style={{ top: `${Math.max(0, node.cards.length - 1) * STACK_TITLE_PEEK + 58}px` }}
                      aria-hidden="true"
                      onPointerDown={(event) => {
                        const frontCard = node.cards[node.cards.length - 1];
                        if (!connecting() || !frontCard) return;
                        event.preventDefault();
                        event.stopPropagation();
                        void finishConnection(frontCard.id);
                      }}
                    />
                    <button
                      type="button"
                      class="clue-port output"
                      style={{ top: `${Math.max(0, node.cards.length - 1) * STACK_TITLE_PEEK + 58}px` }}
                      title="拖到另一组线索，建立前置 → 后续"
                      disabled={connectionBusy()}
                      onPointerDown={(event) => {
                        const frontCard = node.cards[node.cards.length - 1];
                        if (frontCard) beginConnection(frontCard.id, event);
                      }}
                    />
                    <For each={node.cards}>
                      {(card, index) => {
                        const version = () => clueCurrentVersion(card);
                        const front = () => index() === node.cards.length - 1;
                        return (
                          <article
                            class={`clue-trading-card role-${node.role}`}
                             classList={{
                               active: selectedCardId() === card.id,
                               selected: selectedCardIds().has(card.id),
                               front: front(),
                               mentioned: state.unreadClueMentions.includes(card.id),
                             }}
                            role="button"
                            tabIndex={0}
                            style={{
                              left: "0",
                              top: `${index() * STACK_TITLE_PEEK}px`,
                              "z-index": index() + 1,
                            }}
                            onPointerDown={(event) => beginNodeDrag(node.group.id, card.id, event)}
                            onClick={(event) => selectCard(card.id, event)}
                            onKeyDown={(event) => {
                              if (event.key === "Enter" || event.key === " ") {
                                selectOnly(card.id);
                              }
                            }}
                          >
                            <div class="clue-card-nameplate">
                              <span
                                class="clue-author-avatar"
                                title={`作者：${authorName(version()?.authorName)}`}
                                aria-label={`作者：${authorName(version()?.authorName)}`}
                              >
                                {authorBadge(version()?.authorName)}
                              </span>
                              <strong>{version()?.title || "未命名线索"}</strong>
                              <span class="clue-version-gem">v{card.versions.length}</span>
                            </div>
                            <div class="clue-card-textbox">
                              <div class="clue-card-kind">{roleLabel(node.role)}</div>
                              <p>{excerpt(version()?.content ?? "")}</p>
                              <div class="clue-card-rule" />
                              <footer>
                                <span>{authorName(version()?.authorName)}</span>
                                <time>{fmtTime(card.updatedAt)}</time>
                              </footer>
                            </div>
                          </article>
                        );
                      }}
                    </For>
                  </section>
                )}
              </For>
            </div>
            <div class="clue-canvas-toolbar" onPointerDown={(event) => event.stopPropagation()}>
              <button title="缩小" onClick={() => zoomAt(camera().zoom / 1.16)}>−</button>
              <button class="zoom-value" title="恢复 100%" onClick={() => zoomAt(1)}>
                {Math.round(camera().zoom * 100)}%
              </button>
              <button title="放大" onClick={() => zoomAt(camera().zoom * 1.16)}>＋</button>
              <button title="适应全部线索" onClick={fitGraph}><IconMove size={14} /></button>
              <button class="arrange" title="清除手动位置并自动整理结构" onClick={arrangeGraph}>
                一键整理
              </button>
            </div>
            <div class="clue-canvas-hint">Ctrl + 点击多选 · 滚轮缩放 · 拖动卡牌移动 · 右侧圆点连接</div>
            <Show when={edgeMenu()}>
              {(menu) => (
                <div
                  class="clue-edge-menu"
                  style={{ left: `${menu().x}px`, top: `${menu().y}px` }}
                  onPointerDown={(event) => event.stopPropagation()}
                >
                  <button disabled={edgeBusy()} onClick={() => void removeConnection()}>
                    {edgeBusy() ? "删除中…" : "删除连接"}
                  </button>
                </div>
              )}
            </Show>
          </div>

          <Show when={selectedCard()}>
            {(card) => {
              const version = () => clueCurrentVersion(card());
              const comments = () => card().comments ?? [];
              const commentById = (id?: string | null) =>
                id ? comments().find((comment) => comment.id === id) : undefined;
              return (
                <aside class="clue-detail">
                  <div class="clue-detail-head">
                    <span class="clue-detail-kicker">ClueCard</span>
                    <h2>{version()?.title || "未命名线索"}</h2>
                    <div class="clue-detail-author">
                      <span class="clue-author-avatar" title={`作者：${authorName(version()?.authorName)}`}>
                        {authorBadge(version()?.authorName)}
                      </span>
                      <span>{authorName(version()?.authorName)}</span>
                    </div>
                    <span class="clue-detail-meta">{fmtTime(card().updatedAt)} · {card().versions.length} 个版本</span>
                  </div>
                  <pre class="clue-detail-content">{version()?.content}</pre>
                  <Show when={(version()?.mentions ?? []).length > 0}>
                    <div class="clue-mention-summary">
                      <span>本次提醒</span>
                      <For each={version()?.mentions ?? []}>
                        {(mention) => <strong>@{mention.name}</strong>}
                      </For>
                    </div>
                  </Show>

                  <div class="clue-detail-actions">
                    <Show when={selectedCardIds().size > 1}>
                      <button class="btn primary" disabled={stacking()} onClick={() => void stackSelectedCards()}>
                        {stacking() ? "堆叠中…" : `堆叠所选（${selectedCardIds().size}）`}
                      </button>
                    </Show>
                    <button class="btn primary" onClick={() => startSessionFromClue(card())}>
                      沿此线索发起会话
                    </button>
                    <button
                      class="btn secondary"
                      onClick={() => setCapture({ placement: "update", targetCardId: card().id })}
                    >
                      更新
                    </button>
                    <button
                      class="btn secondary"
                      onClick={() => setCapture({ placement: "parallel", targetCardId: card().id })}
                    >
                      堆叠线索
                    </button>
                    <Show when={(selectedGroup()?.cards.length ?? 0) > 1}>
                      <button
                        class="btn secondary"
                        disabled={splittingCardId() === card().id}
                        onClick={() => void splitSelectedCard(card())}
                      >
                        {splittingCardId() === card().id ? "拆分中…" : "拆分"}
                      </button>
                    </Show>
                    <button
                      class="btn secondary"
                      onClick={() => setCapture({ placement: "new", targetCardId: card().id })}
                    >
                      开启下一条
                    </button>
                    <button
                      class="btn danger"
                      disabled={deletingCardId() === card().id}
                      onClick={() => void removeCard(card())}
                    >
                      {deletingCardId() === card().id ? "删除中…" : "删除线索"}
                    </button>
                  </div>

                  <div class="clue-comments">
                    <div class="clue-comments-head">
                      <div class="clue-section-title">评论与回复</div>
                      <span>{comments().length}</span>
                    </div>
                    <Show
                      when={comments().length > 0}
                      fallback={<div class="clue-comments-empty">还没有评论</div>}
                    >
                      <div class="clue-comment-list">
                        <For each={comments()}>
                          {(item) => {
                            const parent = () => commentById(item.parentCommentId);
                            return (
                              <article
                                class="clue-comment"
                                classList={{ reply: !!item.parentCommentId }}
                              >
                                <div class="clue-comment-head">
                                  <span
                                    class="clue-author-avatar"
                                    title={`作者：${authorName(item.authorName)}`}
                                  >
                                    {authorBadge(item.authorName)}
                                  </span>
                                  <strong>{authorName(item.authorName)}</strong>
                                  <time>{fmtTime(item.createdAt)}</time>
                                </div>
                                <Show when={parent()}>
                                  {(target) => (
                                    <blockquote class="clue-comment-quote">
                                      <strong>@{authorName(target().authorName)}</strong>
                                      <span>{target().content}</span>
                                    </blockquote>
                                  )}
                                </Show>
                                <Show when={(item.mentions ?? []).length > 0}>
                                  <div class="clue-comment-mentions">
                                    <For each={item.mentions ?? []}>
                                      {(mention) => <span>@{mention.name}</span>}
                                    </For>
                                  </div>
                                </Show>
                                <p>{item.content}</p>
                                <button
                                  type="button"
                                  class="clue-comment-reply"
                                  onClick={() => beginReply(item)}
                                >
                                  回复
                                </button>
                              </article>
                            );
                          }}
                        </For>
                      </div>
                    </Show>

                    <div class="clue-comment-composer">
                      <Show when={replyTarget()}>
                        {(target) => (
                          <div class="clue-comment-replying">
                            <span>回复 @{authorName(target().authorName)}</span>
                            <button type="button" onClick={cancelReply}>
                              取消回复
                            </button>
                          </div>
                        )}
                      </Show>
                      <textarea
                        ref={commentInputElement}
                        class="field-input"
                        rows={3}
                        value={commentText()}
                        disabled={commentBusy()}
                        placeholder={replyTarget() ? "写下回复…" : "写下评论…"}
                        onInput={(event) => setCommentText(event.currentTarget.value)}
                      />
                      <MentionPicker
                        peers={mentionPeers()}
                        selectedTokens={commentMentions()}
                        disabled={commentBusy() || mentionPeers().length === 0}
                        placeholder={mentionPeers().length > 0 ? "@ 提醒团队成员" : "暂无可提醒的团队成员"}
                        onChange={setCommentMentions}
                      />
                      <div class="clue-comment-submit-row">
                        <span class="field-hint">回复会自动 @ 原评论作者。</span>
                        <button
                          type="button"
                          class="btn primary small"
                          disabled={commentBusy() || !commentText().trim()}
                          onClick={() => void submitComment()}
                        >
                          {commentBusy() ? "发送中…" : replyTarget() ? "发送回复" : "发表评论"}
                        </button>
                      </div>
                    </div>
                  </div>

                  <Show when={predecessors().length > 0}>
                    <div class="clue-links">
                      <div class="clue-section-title">前置线索</div>
                      <For each={predecessors()}>
                        {(item) => (
                          <button onClick={() => selectOnly(item.id)}>
                            {clueCurrentVersion(item)?.title || "未命名线索"}
                          </button>
                        )}
                      </For>
                    </div>
                  </Show>
                  <Show when={successors().length > 0}>
                    <div class="clue-links">
                      <div class="clue-section-title">后续线索</div>
                      <For each={successors()}>
                        {(item) => (
                          <button onClick={() => selectOnly(item.id)}>
                            {clueCurrentVersion(item)?.title || "未命名线索"}
                          </button>
                        )}
                      </For>
                    </div>
                  </Show>

                  <Show when={card().versions.length > 1}>
                    <div class="clue-history">
                      <div class="clue-section-title">版本记录</div>
                      <For each={[...card().versions].reverse()}>
                        {(item, index) => (
                          <div class="clue-history-item">
                            <span>v{card().versions.length - index()}</span>
                            <strong>{item.title}</strong>
                            <time>{fmtTime(item.createdAt)}</time>
                          </div>
                        )}
                      </For>
                    </div>
                  </Show>
                </aside>
              );
            }}
          </Show>
        </div>
      </Show>

      <Show when={capture()}>
        {(value) => (
          <ClueCaptureModal
            initialPlacement={value().placement}
            initialTargetCardId={value().targetCardId}
            onClose={() => setCapture(null)}
          />
        )}
      </Show>
    </main>
  );
}
