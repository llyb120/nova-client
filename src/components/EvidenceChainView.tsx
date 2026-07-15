import { confirm, message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import {
  associateClues,
  clueCardById,
  clueCurrentVersion,
  deleteClue,
  refreshClueGroups,
  startSessionFromClue,
  state,
} from "../store";
import type { ClueCard, ClueNodeGroup } from "../types";
import { ClueAssociateModal } from "./ClueAssociateModal";
import { ClueCaptureModal } from "./ClueCaptureModal";
import { IconClue, IconMove, IconPlus } from "./icons";

type Placement = "update" | "parallel" | "new";
type Point = { x: number; y: number };
type Camera = { x: number; y: number; zoom: number };

type GroupNode = {
  group: ClueNodeGroup;
  cards: ClueCard[];
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

const CARD_WIDTH = 236;
const CARD_HEIGHT = 296;
const STACK_X = 13;
const STACK_Y = 11;
const MAX_STACK_OFFSET = 4;
const COLUMN_GAP = 150;
const ROW_GAP = 105;
const WORLD_LEFT = 110;
const WORLD_TOP = 104;
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

function cardVariant(id: string) {
  let hash = 0;
  for (const char of id) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  return ["amber", "azure", "violet", "jade"][hash % 4];
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

export function EvidenceChainView() {
  const [selectedCardId, setSelectedCardId] = createSignal<string | null>(null);
  const [capture, setCapture] = createSignal<{
    placement: Placement;
    targetCardId: string | null;
  } | null>(null);
  const [showAssociate, setShowAssociate] = createSignal(false);
  const [deletingCardId, setDeletingCardId] = createSignal<string | null>(null);
  const [camera, setCamera] = createSignal<Camera>({ x: 40, y: 40, zoom: 1 });
  const [connecting, setConnecting] = createSignal<{
    fromCardId: string;
    pointerId: number;
    pointer: Point;
  } | null>(null);
  const [connectionTargetId, setConnectionTargetId] = createSignal<string | null>(null);
  const [connectionBusy, setConnectionBusy] = createSignal(false);
  let viewportElement: HTMLDivElement | undefined;
  let canvasElement: HTMLCanvasElement | undefined;
  let resizeObserver: ResizeObserver | undefined;
  let drawFrame: number | undefined;
  let fitted = false;
  let panGesture: {
    pointerId: number;
    startX: number;
    startY: number;
    cameraX: number;
    cameraY: number;
  } | null = null;

  const cardToGroup = createMemo(() => {
    const map = new Map<string, ClueNodeGroup>();
    for (const group of state.clueGroups) {
      for (const card of group.cards) map.set(card.id, group);
    }
    return map;
  });

  const cards = createMemo(() => state.clueGroups.flatMap((group) => group.cards));

  const graphLayout = createMemo<GraphLayout>(() => {
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
    for (const group of state.clueGroups) {
      const depth = depthOf(group);
      stageGroups.set(depth, [...(stageGroups.get(depth) ?? []), group]);
    }

    const groupOrder = new Map<string, number>();
    const nodes: GroupNode[] = [];
    const stages: Array<{ depth: number; x: number }> = [];
    for (const [depth, groups] of [...stageGroups.entries()].sort(([left], [right]) => left - right)) {
      const groupScores = new Map(
        groups.map((group) => {
          const parentOrders = group.parentCardIds
            .map((parentId) => byCard.get(parentId))
            .filter((parent): parent is ClueNodeGroup => !!parent)
            .map((parent) => groupOrder.get(parent.id))
            .filter((order): order is number => order !== undefined);
          const score = parentOrders.length
            ? parentOrders.reduce((sum, order) => sum + order, 0) / parentOrders.length
            : Number.MAX_SAFE_INTEGER;
          return [group.id, score] as const;
        }),
      );
      groups.sort((left, right) => {
        const difference = (groupScores.get(left.id) ?? 0) - (groupScores.get(right.id) ?? 0);
        return difference || left.createdAt - right.createdAt;
      });
      const x = WORLD_LEFT + depth * (CARD_WIDTH + COLUMN_GAP + MAX_STACK_OFFSET * STACK_X);
      stages.push({ depth, x });
      groups.forEach((group, index) => {
        groupOrder.set(group.id, index);
        nodes.push({
          group,
          cards: stackCards(group, selectedCardId()),
          depth,
          x,
          y: WORLD_TOP + index * (CARD_HEIGHT + ROW_GAP + MAX_STACK_OFFSET * STACK_Y),
        });
      });
    }

    const nodeByCard = new Map<string, GroupNode>();
    const cardAnchors = new Map<string, Point>();
    for (const node of nodes) {
      node.cards.forEach((card, index) => {
        nodeByCard.set(card.id, node);
        const offset = Math.min(index, MAX_STACK_OFFSET);
        cardAnchors.set(card.id, {
          x: node.x + CARD_WIDTH + offset * STACK_X + 8,
          y: node.y + 68 + offset * STACK_Y,
        });
      });
    }

    const rawEdges: Array<{
      key: string;
      fromCardId: string;
      toCardIds: string[];
      start: Point;
      end: Point;
    }> = [];
    for (const node of nodes) {
      for (const parentCardId of node.group.parentCardIds) {
        const sourceNode = nodeByCard.get(parentCardId);
        const start = cardAnchors.get(parentCardId);
        if (!sourceNode || !start) continue;
        rawEdges.push({
          key: `${sourceNode.depth}:${node.depth}`,
          fromCardId: parentCardId,
          toCardIds: node.group.cards.map((card) => card.id),
          start,
          end: { x: node.x - 12, y: node.y + 68 },
        });
      }
    }

    const edges: GraphEdge[] = [];
    const edgeGroups = new Map<string, typeof rawEdges>();
    for (const edge of rawEdges) edgeGroups.set(edge.key, [...(edgeGroups.get(edge.key) ?? []), edge]);
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

    const maxX = Math.max(
      520,
      ...nodes.map((node) => node.x + CARD_WIDTH + MAX_STACK_OFFSET * STACK_X + 80),
    );
    const maxY = Math.max(
      420,
      ...nodes.map((node) => node.y + CARD_HEIGHT + MAX_STACK_OFFSET * STACK_Y + 80),
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
    const orderedEdges = [...graphLayout().edges].sort((left, right) => {
      const active = (edge: GraphEdge) =>
        edge.fromCardId === selected || edge.toCardIds.includes(selected ?? "");
      return Number(active(left)) - Number(active(right));
    });
    for (const edge of orderedEdges) {
      const active = edge.fromCardId === selected || edge.toCardIds.includes(selected ?? "");
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
    if (!viewport || layout.nodes.length === 0) return;
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
    setSelectedCardId(cardId);
    setConnecting({
      fromCardId: cardId,
      pointerId: event.pointerId,
      pointer: screenToWorld(event.clientX, event.clientY),
    });
    viewportElement?.setPointerCapture(event.pointerId);
  };

  const finishConnection = async () => {
    const pending = connecting();
    const target = connectionTargetId();
    setConnecting(null);
    setConnectionTargetId(null);
    if (!pending || !target || target === pending.fromCardId) return;
    setConnectionBusy(true);
    try {
      await associateClues(pending.fromCardId, target);
      setSelectedCardId(target);
    } catch (error) {
      await message(String(error), { title: "连接失败", kind: "error" });
    } finally {
      setConnectionBusy(false);
    }
  };

  const onPointerDown = (event: PointerEvent) => {
    const target = event.target as HTMLElement;
    if (target.closest(".clue-group-node, .clue-canvas-toolbar")) return;
    if (event.button !== 0 && event.button !== 1) return;
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

  const onPointerMove = (event: PointerEvent) => {
    const pending = connecting();
    if (pending) {
      setConnecting({ ...pending, pointer: screenToWorld(event.clientX, event.clientY) });
      const target = (document.elementFromPoint(event.clientX, event.clientY) as HTMLElement | null)
        ?.closest("[data-clue-card-id]")
        ?.getAttribute("data-clue-card-id");
      setConnectionTargetId(target && target !== pending.fromCardId ? target : null);
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
    if (connecting()?.pointerId === event.pointerId) void finishConnection();
    if (panGesture?.pointerId === event.pointerId) panGesture = null;
  };

  const cancelPointerAction = () => {
    panGesture = null;
    setConnecting(null);
    setConnectionTargetId(null);
  };

  const onWheel = (event: WheelEvent) => {
    event.preventDefault();
    const viewport = viewportElement;
    if (!viewport) return;
    if (event.ctrlKey || event.metaKey) {
      const rect = viewport.getBoundingClientRect();
      zoomAt(
        camera().zoom * Math.exp(-event.deltaY * 0.0015),
        event.clientX - rect.left,
        event.clientY - rect.top,
      );
      return;
    }
    setCamera((current) => ({
      ...current,
      x: current.x - event.deltaX,
      y: current.y - event.deltaY,
    }));
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

  createEffect(() => {
    const available = cards();
    const selected = selectedCardId();
    if (selected && available.some((card) => card.id === selected)) return;
    const preferred = state.pendingClueCard?.id;
    setSelectedCardId(
      (preferred && available.some((card) => card.id === preferred) ? preferred : available[0]?.id) ?? null,
    );
  });

  createEffect(() => {
    const layout = graphLayout();
    camera();
    connecting();
    selectedCardId();
    scheduleDraw();
    if (!fitted && layout.nodes.length > 0 && viewportElement) {
      fitted = true;
      requestAnimationFrame(fitGraph);
    }
  });

  onMount(() => {
    void refreshClueGroups();
    resizeObserver = new ResizeObserver(() => {
      scheduleDraw();
      if (!fitted && graphLayout().nodes.length > 0) {
        fitted = true;
        fitGraph();
      }
    });
    if (viewportElement) resizeObserver.observe(viewportElement);
    scheduleDraw();
  });

  onCleanup(() => {
    if (drawFrame !== undefined) cancelAnimationFrame(drawFrame);
    resizeObserver?.disconnect();
  });

  return (
    <main class="clue-view">
      <header class="clue-head">
        <div>
          <h1 class="clue-title">证据链</h1>
          <p class="clue-sub">拖动画布浏览，按住 Ctrl 滚轮缩放；从卡片右侧连接点拖到另一张卡建立顺序。</p>
        </div>
        <div class="clue-head-actions">
          <Show when={cards().length > 1}>
            <button class="btn secondary" onClick={() => setShowAssociate(true)}>
              选择关联
            </button>
          </Show>
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
            onDblClick={(event) => {
              if (!(event.target as HTMLElement).closest(".clue-group-node")) fitGraph();
            }}
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
                    classList={{ stacked: node.cards.length > 1 }}
                    style={{
                      left: `${node.x}px`,
                      top: `${node.y}px`,
                      width: `${CARD_WIDTH + Math.min(node.cards.length - 1, MAX_STACK_OFFSET) * STACK_X}px`,
                      height: `${CARD_HEIGHT + Math.min(node.cards.length - 1, MAX_STACK_OFFSET) * STACK_Y}px`,
                    }}
                  >
                    <Show when={node.cards.length > 1}>
                      <div class="clue-stack-count">{node.cards.length} 张平行线索</div>
                    </Show>
                    <For each={node.cards}>
                      {(card, index) => {
                        const version = () => clueCurrentVersion(card);
                        const offset = () => Math.min(index(), MAX_STACK_OFFSET);
                        const front = () => index() === node.cards.length - 1;
                        return (
                          <article
                            class={`clue-trading-card variant-${cardVariant(card.id)}`}
                            classList={{
                              active: selectedCardId() === card.id,
                              front: front(),
                              "connection-source": connecting()?.fromCardId === card.id,
                              "connection-target": connectionTargetId() === card.id,
                            }}
                            data-clue-card-id={card.id}
                            role="button"
                            tabIndex={0}
                            style={{
                              left: `${offset() * STACK_X}px`,
                              top: `${offset() * STACK_Y}px`,
                              "z-index": index() + 1,
                            }}
                            onClick={() => setSelectedCardId(card.id)}
                            onKeyDown={(event) => {
                              if (event.key === "Enter" || event.key === " ") setSelectedCardId(card.id);
                            }}
                          >
                            <span class="clue-port input" aria-hidden="true" />
                            <button
                              type="button"
                              class="clue-port output"
                              title="拖到另一张线索，建立前置 → 后续"
                              disabled={connectionBusy()}
                              onPointerDown={(event) => beginConnection(card.id, event)}
                            />
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
                            <div class="clue-card-art">
                              <span class="clue-card-sigil"><IconClue size={34} /></span>
                              <span class="clue-card-kind">CLUE · EVIDENCE</span>
                            </div>
                            <div class="clue-card-textbox">
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
            </div>
            <div class="clue-canvas-hint">拖动空白处平移 · Ctrl + 滚轮缩放 · 拖动卡牌右侧圆点连接</div>
          </div>

          <Show when={selectedCard()}>
            {(card) => {
              const version = () => clueCurrentVersion(card());
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

                  <div class="clue-detail-actions">
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
                      平行后续
                    </button>
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

                  <Show when={predecessors().length > 0}>
                    <div class="clue-links">
                      <div class="clue-section-title">前置线索</div>
                      <For each={predecessors()}>
                        {(item) => (
                          <button onClick={() => setSelectedCardId(item.id)}>
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
                          <button onClick={() => setSelectedCardId(item.id)}>
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
      <Show when={showAssociate()}>
        <ClueAssociateModal
          initialBeforeCardId={selectedCardId()}
          onClose={() => setShowAssociate(false)}
        />
      </Show>
    </main>
  );
}
