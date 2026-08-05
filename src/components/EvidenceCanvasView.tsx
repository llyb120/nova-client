import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import type { ClueStageGroup, ClueStructure } from "../clueGraph";
import { authorBadge, authorName, excerpt, fmtTime, roleLabel, stackCards } from "../clueGraph";
import { associateClues, disassociateClues, clueCurrentVersion, state } from "../store";
import type { ClueNodeGroup } from "../types";
import { IconMove } from "./icons";

type Point = { x: number; y: number };
type Camera = { x: number; y: number; zoom: number };

type GroupNode = {
  group: ClueNodeGroup;
  cards: ReturnType<typeof stackCards>;
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

type Props = {
  structure: ClueStructure;
  selectedCardId: string | null;
  onSelectCard: (cardId: string, event?: MouseEvent) => void;
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

export function EvidenceCanvasView(props: Props) {
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
  let viewportElement: HTMLDivElement | undefined;
  let canvasElement: HTMLCanvasElement | undefined;
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

  // 几何布局：在共享层级结构之上计算坐标。拖动时不重跑拓扑布局。
  const geometry = createMemo<GraphStructure>(() => {
    const structure = props.structure;
    const allGroups = structure.stages.flatMap((stage) => stage.groups.map((item) => item.group));
    const longEdgeCount = allGroups.reduce((count, group) => {
      const targetDepth = structure.depthByGroup.get(group.id) ?? 0;
      return count + group.parentCardIds.filter((parentCardId) => {
        const parent = structure.cardToGroup.get(parentCardId);
        return parent && targetDepth - (structure.depthByGroup.get(parent.id) ?? 0) > 1;
      }).length;
    }, 0);
    const layoutTop = WORLD_TOP + longEdgeCount * EDGE_LANE_GAP;
    const groupHeight = (group: ClueNodeGroup) =>
      CARD_HEIGHT + Math.max(0, group.cards.length - 1) * STACK_TITLE_PEEK;
    const stageHeight = (items: ClueStageGroup[]) =>
      items.reduce((sum, item) => sum + groupHeight(item.group), 0)
      + Math.max(0, items.length - 1) * ROW_GAP;
    const groupsById = new Map(allGroups.map((group) => [group.id, group]));
    const unassignedGroupIds = new Set(groupsById.keys());
    const components: ClueNodeGroup[][] = [];
    for (const seed of [...allGroups].sort((left, right) => left.createdAt - right.createdAt)) {
      if (!unassignedGroupIds.delete(seed.id)) continue;
      const component: ClueNodeGroup[] = [];
      const queue = [seed.id];
      for (let index = 0; index < queue.length; index += 1) {
        const groupId = queue[index];
        const group = groupsById.get(groupId);
        if (group) component.push(group);
        const neighbors = new Set([
          ...(structure.parentsByGroup.get(groupId) ?? []),
          ...(structure.childrenByGroup.get(groupId) ?? []),
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
      const componentStages = structure.stages
        .map((stage) => stage.groups.filter((item) => componentIds.has(item.group.id)))
        .filter((items) => items.length > 0);
      const componentHeight = Math.max(...componentStages.map(stageHeight));
      for (const items of componentStages) {
        let y = componentTop + (componentHeight - stageHeight(items)) / 2;
        for (const item of items) {
          groupY.set(item.group.id, y);
          y += groupHeight(item.group) + ROW_GAP;
        }
      }
      componentTop += componentHeight + ROW_GAP;
    }

    const nodes: GraphStructure["nodes"] = [];
    const stages: Array<{ depth: number; x: number }> = [];
    for (const stage of structure.stages) {
      const x = WORLD_LEFT + stage.depth * (CARD_WIDTH + COLUMN_GAP);
      stages.push({ depth: stage.depth, x });
      for (const item of stage.groups) {
        nodes.push({
          group: item.group,
          role: item.role,
          depth: item.depth,
          x,
          y: groupY.get(item.group.id) ?? layoutTop,
        });
      }
    }

    return { nodes, stages, layoutTop };
  });

  // 交互层只套用卡片前后顺序和手动坐标，再更新锚点/连线；拖动时不重跑拓扑布局。
  const graphLayout = createMemo<GraphLayout>(() => {
    const structure = geometry();
    const positions = nodePositions();
    const selected = props.selectedCardId;
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

    const selected = props.selectedCardId;
    const activeCardIds = new Set(
      selected ? (props.structure.cardToGroup.get(selected)?.cards.map((card) => card.id) ?? [selected]) : [],
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

  const beginConnection = (cardId: string, event: PointerEvent) => {
    if (connectionBusy()) return;
    event.preventDefault();
    event.stopPropagation();
    props.onSelectCard(cardId);
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
    props.onSelectCard(cardId);
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
      props.onSelectCard(target);
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

  createEffect(() => {
    const layout = graphLayout();
    camera();
    connecting();
    props.selectedCardId;
    scheduleDraw();
    if (!fitted && layout.nodes.length > 0 && viewportElement) {
      requestAnimationFrame(() => {
        if (!fitted && fitGraph()) fitted = true;
      });
    }
  });

  onMount(() => {
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
                         active: props.selectedCardId === card.id,
                         selected: props.selectedCardId === card.id,
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
                      onClick={(event) => props.onSelectCard(card.id, event)}
                      onKeyDown={(event) => {
                        if (event.key === "Enter" || event.key === " ") {
                          props.onSelectCard(card.id);
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
  );
}
