import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import {
  isTerminal,
  newWorkflowId,
  WF_DONE,
  WF_FAIL,
  type WorkflowDef,
  type WorkflowStageDef,
  type WorkflowTransition,
} from "../workflow/types";

const NODE_W = 156;
const NODE_H = 56;
const TERM_W = 110;
const TERM_H = 40;
const LOOP_OFFSET = 58;

interface Props {
  def: WorkflowDef;
  onChange: (def: WorkflowDef) => void;
  selectedStageId: string | null;
  selectedTransitionId: string | null;
  onSelectStage: (id: string | null) => void;
  onSelectTransition: (id: string | null) => void;
  onDeleteStage: (id: string) => void;
  onDeleteTransition: (stageId: string, transitionId: string) => void;
  onEditStage?: (id: string) => void;
  onEditTransition?: (stageId: string, transitionId: string) => void;
}

type Point = { x: number; y: number };
type RouteKind = "forward" | "back" | "down" | "up" | "arcR" | "arcL" | "arcTop" | "arcBottom";

interface EdgeView {
  transition: WorkflowTransition;
  stageId: string;
  d: string;
  label: Point;
}

type ContextMenu =
  | { x: number; y: number; kind: "stage"; stageId: string }
  | { x: number; y: number; kind: "edge"; stageId: string; transitionId: string }
  | null;

/** 类 BPMN 工作流画布：智能连线路由、节点/连线右键删除、平移缩放、连线预览。纯 SVG。 */
export function WorkflowCanvas(props: Props) {
  let svgRef: SVGSVGElement | undefined;
  const [pan, setPan] = createSignal({ x: 24, y: 24 });
  const [zoom, setZoom] = createSignal(1);
  const [connectFrom, setConnectFrom] = createSignal<string | null>(null);
  const [connectPos, setConnectPos] = createSignal<Point | null>(null);
  const [menu, setMenu] = createSignal<ContextMenu>(null);

  const readonly = () => props.def.builtin === true;
  const stages = () => props.def.stages;
  const stageById = (id: string) => stages().find((s) => s.id === id);

  const center = (s: WorkflowStageDef): Point => ({ x: s.x + NODE_W / 2, y: s.y + NODE_H / 2 });

  function terminalPos(target: string): Point {
    const maxX = stages().reduce((m, s) => Math.max(m, s.x + NODE_W), 0);
    const baseX = Math.max(maxX + 140, 440);
    return target === WF_FAIL ? { x: baseX, y: 250 } : { x: baseX, y: 110 };
  }
  const terminalCenter = (target: string): Point => {
    const p = terminalPos(target);
    return { x: p.x + TERM_W / 2, y: p.y + TERM_H / 2 };
  };

  function toWorld(clientX: number, clientY: number): Point {
    const rect = svgRef?.getBoundingClientRect();
    const left = rect?.left ?? 0;
    const top = rect?.top ?? 0;
    return { x: (clientX - left - pan().x) / zoom(), y: (clientY - top - pan().y) / zoom() };
  }

  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setMenu(null);
        setConnectFrom(null);
        setConnectPos(null);
      }
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  // --- 连线路由：按源/目标相对位置选择端口与绕行方式，避免回边/同列边交叉 ---
  function bezMid(p0: Point, p1: Point, p2: Point, p3: Point): Point {
    return {
      x: 0.125 * p0.x + 0.375 * p1.x + 0.375 * p2.x + 0.125 * p3.x,
      y: 0.125 * p0.y + 0.375 * p1.y + 0.375 * p2.y + 0.125 * p3.y,
    };
  }

  function buildRoute(from: WorkflowStageDef, toCenter: Point, kind: RouteKind, isTerm: boolean): { d: string; label: Point } {
    const out = (side: "r" | "l" | "t" | "b"): Point =>
      side === "r" ? { x: from.x + NODE_W, y: from.y + NODE_H / 2 }
        : side === "l" ? { x: from.x, y: from.y + NODE_H / 2 }
        : side === "t" ? { x: from.x + NODE_W / 2, y: from.y }
        : { x: from.x + NODE_W / 2, y: from.y + NODE_H };
    const inAt = (side: "r" | "l" | "t" | "b", c: Point): Point =>
      side === "r" ? { x: c.x + NODE_W / 2, y: c.y }
        : side === "l" ? { x: c.x - NODE_W / 2, y: c.y }
        : side === "t" ? { x: c.x, y: c.y - NODE_H / 2 }
        : { x: c.x, y: c.y + NODE_H / 2 };
    const termIn = (c: Point): Point => ({ x: c.x - TERM_W / 2, y: c.y });
    const inp = (side: "r" | "l" | "t" | "b", c: Point) => (isTerm ? termIn(c) : inAt(side, c));

    if (kind === "forward") {
      const o = out("r"); const i = inp("l", toCenter);
      const dx = Math.max(44, Math.abs(i.x - o.x) / 2);
      return { d: `M ${o.x} ${o.y} C ${o.x + dx} ${o.y}, ${i.x - dx} ${i.y}, ${i.x} ${i.y}`, label: bezMid(o, { x: o.x + dx, y: o.y }, { x: i.x - dx, y: i.y }, i) };
    }
    if (kind === "back") {
      const o = out("l"); const i = inp("r", toCenter);
      const dx = Math.max(44, Math.abs(o.x - i.x) / 2);
      return { d: `M ${o.x} ${o.y} C ${o.x - dx} ${o.y}, ${i.x + dx} ${i.y}, ${i.x} ${i.y}`, label: bezMid(o, { x: o.x - dx, y: o.y }, { x: i.x + dx, y: i.y }, i) };
    }
    if (kind === "down" || kind === "up") {
      const o = out(kind === "down" ? "b" : "t"); const i = inAt(kind === "down" ? "t" : "b", toCenter);
      const dy = Math.max(44, Math.abs(i.y - o.y) / 2);
      const p1 = { x: o.x, y: o.y + (kind === "down" ? dy : -dy) };
      const p2 = { x: i.x, y: i.y + (kind === "down" ? -dy : dy) };
      const mid = bezMid(o, p1, p2, i);
      return { d: `M ${o.x} ${o.y} C ${p1.x} ${p1.y}, ${p2.x} ${p2.y}, ${i.x} ${i.y}`, label: { x: mid.x + 12, y: mid.y } };
    }
    if (kind === "arcR") {
      const o = out("r"); const i = inp("r", toCenter);
      const loopX = Math.max(o.x, i.x) + LOOP_OFFSET;
      return { d: `M ${o.x} ${o.y} C ${loopX} ${o.y}, ${loopX} ${i.y}, ${i.x} ${i.y}`, label: { x: loopX + 6, y: (o.y + i.y) / 2 } };
    }
    if (kind === "arcL") {
      const o = out("l"); const i = inp("l", toCenter);
      const loopX = Math.min(o.x, i.x) - LOOP_OFFSET;
      return { d: `M ${o.x} ${o.y} C ${loopX} ${o.y}, ${loopX} ${i.y}, ${i.x} ${i.y}`, label: { x: loopX - 6, y: (o.y + i.y) / 2 } };
    }
    if (kind === "arcTop") {
      const o = out("t"); const i = inAt("t", toCenter);
      const loopY = Math.min(o.y, i.y) - LOOP_OFFSET;
      return { d: `M ${o.x} ${o.y} C ${o.x} ${loopY}, ${i.x} ${loopY}, ${i.x} ${i.y}`, label: { x: (o.x + i.x) / 2, y: loopY - 6 } };
    }
    const o = out("b"); const i = inAt("b", toCenter);
    const loopY = Math.max(o.y, i.y) + LOOP_OFFSET;
    return { d: `M ${o.x} ${o.y} C ${o.x} ${loopY}, ${i.x} ${loopY}, ${i.x} ${i.y}`, label: { x: (o.x + i.x) / 2, y: loopY + 14 } };
  }

  function chooseKind(from: WorkflowStageDef, toCenter: Point, pairCount: number, pairIndex: number): RouteKind {
    const sc = center(from);
    const dx = toCenter.x - sc.x;
    const dy = toCenter.y - sc.y;
    if (pairCount >= 2) {
      if (Math.abs(dy) >= Math.abs(dx)) return pairIndex === 0 ? "arcR" : "arcL";
      return pairIndex === 0 ? "arcTop" : "arcBottom";
    }
    if (Math.abs(dx) > NODE_W * 0.4) return dx > 0 ? "forward" : "back";
    return dy >= 0 ? "down" : "up";
  }

  function edges(): EdgeView[] {
    const pairCount = new Map<string, number>();
    const pairKey = (a: string, b: string) => [a, b].sort().join("|");
    type Flat = { from: WorkflowStageDef; transition: WorkflowTransition; toId: string; toCenter: Point; isTerm: boolean };
    const flat: Flat[] = [];
    for (const stage of stages()) {
      for (const transition of stage.transitions) {
        const isTerm = isTerminal(transition.to);
        const toCenter = isTerm ? terminalCenter(transition.to) : center(stageById(transition.to)!);
        if (!toCenter) continue;
        flat.push({ from: stage, transition, toId: transition.to, toCenter, isTerm });
        const key = pairKey(stage.id, transition.to);
        pairCount.set(key, (pairCount.get(key) ?? 0) + 1);
      }
    }
    const pairIndex = new Map<string, number>();
    return flat.map(({ from, transition, toId, toCenter, isTerm }) => {
      const key = pairKey(from.id, toId);
      const idx = pairIndex.get(key) ?? 0;
      pairIndex.set(key, idx + 1);
      const kind = chooseKind(from, toCenter, pairCount.get(key) ?? 1, idx);
      const route = buildRoute(from, toCenter, kind, isTerm);
      return { transition, stageId: from.id, d: route.d, label: route.label };
    });
  }

  // --- 节点拖拽 ---
  let dragStage: string | null = null;
  let dragOffset = { x: 0, y: 0 };
  let moved = false;

  function onNodePointerDown(e: PointerEvent, stage: WorkflowStageDef) {
    e.stopPropagation();
    setMenu(null);
    (e.target as Element).setPointerCapture?.(e.pointerId);
    if (connectFrom()) { finishConnect(stage.id); return; }
    const world = toWorld(e.clientX, e.clientY);
    dragStage = stage.id;
    dragOffset = { x: world.x - stage.x, y: world.y - stage.y };
    moved = false;
  }

  function onPointerMove(e: PointerEvent) {
    if (connectFrom()) setConnectPos(toWorld(e.clientX, e.clientY));
    if (dragStage) {
      const world = toWorld(e.clientX, e.clientY);
      const id = dragStage;
      moved = true;
      props.onChange({
        ...props.def,
        stages: stages().map((s) =>
          s.id === id ? { ...s, x: Math.round(world.x - dragOffset.x), y: Math.round(world.y - dragOffset.y) } : s,
        ),
      });
    } else if (panDrag) {
      panMoved = true;
      setPan({ x: panStart.x + (e.clientX - panOrigin.x), y: panStart.y + (e.clientY - panOrigin.y) });
    }
  }

  function onNodePointerUp(e: PointerEvent, stage: WorkflowStageDef) {
    e.stopPropagation();
    if (dragStage && !moved) props.onSelectStage(stage.id);
    dragStage = null;
  }

  // --- 背景平移 / 取消 ---
  let panDrag = false;
  let panStart = { x: 0, y: 0 };
  let panOrigin = { x: 0, y: 0 };
  let panMoved = false;

  function onBackgroundPointerDown(e: PointerEvent) {
    setMenu(null);
    if (connectFrom()) { setConnectFrom(null); setConnectPos(null); return; }
    panDrag = true; panMoved = false;
    panStart = pan(); panOrigin = { x: e.clientX, y: e.clientY };
  }
  function onBackgroundPointerUp() {
    if (panDrag && !panMoved) { props.onSelectStage(null); props.onSelectTransition(null); }
    panDrag = false;
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    const factor = e.deltaY < 0 ? 1.1 : 1 / 1.1;
    setZoom((z) => Math.min(2, Math.max(0.4, z * factor)));
  }

  // --- 连线 ---
  function startConnect(stageId: string) { setConnectFrom(stageId); setConnectPos(null); props.onSelectStage(null); props.onSelectTransition(null); }
  function finishConnect(to: string) {
    const from = connectFrom();
    setConnectFrom(null); setConnectPos(null);
    if (!from || from === to) return;
    const transition: WorkflowTransition = { id: newWorkflowId("t"), when: { kind: "always" }, to, label: "" };
    props.onChange({ ...props.def, stages: stages().map((s) => (s.id === from ? { ...s, transitions: [...s.transitions, transition] } : s)) });
    props.onSelectTransition(transition.id);
  }
  function onTerminalPointerDown(e: PointerEvent, target: string) {
    e.stopPropagation();
    if (connectFrom()) finishConnect(target);
  }

  // --- 右键菜单 ---
  function openStageMenu(e: MouseEvent, stageId: string) { e.preventDefault(); e.stopPropagation(); setMenu({ x: e.clientX, y: e.clientY, kind: "stage", stageId }); }
  function openEdgeMenu(e: MouseEvent, stageId: string, transitionId: string) { e.preventDefault(); e.stopPropagation(); setMenu({ x: e.clientX, y: e.clientY, kind: "edge", stageId, transitionId }); }

  const preview = createMemo(() => {
    const id = connectFrom();
    const p = connectPos();
    const s = id ? stageById(id) : undefined;
    return s && p ? { s, p } : null;
  });

  return (
    <div class="wf-canvas-wrap">
      <div class="wf-canvas-toolbar">
        <Show when={connectFrom()}>
          <span class="wf-connect-hint">点击目标阶段或终止节点完成连线 · Esc / 点空白取消</span>
        </Show>
        <span class="wf-zoom-spacer" />
        <button class="btn small secondary" onClick={() => setZoom((z) => Math.max(0.4, z / 1.1))}>－</button>
        <span class="wf-zoom-label">{Math.round(zoom() * 100)}%</span>
        <button class="btn small secondary" onClick={() => setZoom((z) => Math.min(2, z * 1.1))}>＋</button>
        <button class="btn small secondary" onClick={() => { setZoom(1); setPan({ x: 24, y: 24 }); }}>复位</button>
      </div>
      <svg
        ref={svgRef}
        class="wf-canvas"
        classList={{ connecting: !!connectFrom() }}
        onPointerMove={onPointerMove}
        onPointerUp={() => { onBackgroundPointerUp(); dragStage = null; }}
        onWheel={onWheel}
        onContextMenu={(e) => { e.preventDefault(); setMenu(null); }}
      >
        <defs>
          <marker id="wf-arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
            <path d="M 0 0 L 10 5 L 0 10 z" class="wf-arrow-head" />
          </marker>
        </defs>
        <rect class="wf-canvas-bg" x={0} y={0} width="100%" height="100%" onPointerDown={onBackgroundPointerDown} />
        <g transform={`translate(${pan().x},${pan().y}) scale(${zoom()})`}>
          <For each={edges()}>
            {(edge) => (
              <g
                class="wf-edge"
                classList={{ selected: props.selectedTransitionId === edge.transition.id }}
                onPointerDown={(e) => { e.stopPropagation(); props.onSelectTransition(edge.transition.id); props.onSelectStage(null); setMenu(null); }}
                onDblClick={(e) => { e.stopPropagation(); props.onEditTransition?.(edge.stageId, edge.transition.id); }}
                onContextMenu={(e) => openEdgeMenu(e, edge.stageId, edge.transition.id)}
              >
                <path class="wf-edge-hit" d={edge.d} />
                <path class="wf-edge-line" d={edge.d} marker-end="url(#wf-arrow)" />
                <Show when={edge.transition.label}>
                  <text class="wf-edge-label" x={edge.label.x} y={edge.label.y}>{edge.transition.label}</text>
                </Show>
              </g>
            )}
          </For>

          {/* 连线预览 */}
          <Show when={preview()}>
            {(v) => {
              const o = { x: v().s.x + NODE_W, y: v().s.y + NODE_H / 2 };
              const p = v().p;
              const dx = Math.max(30, Math.abs(p.x - o.x) / 2);
              return <path class="wf-connect-preview" d={`M ${o.x} ${o.y} C ${o.x + dx} ${o.y}, ${p.x - dx} ${p.y}, ${p.x} ${p.y}`} />;
            }}
          </Show>

          <For each={[WF_DONE, WF_FAIL]}>
            {(target) => {
              const pos = () => terminalPos(target);
              return (
                <g class="wf-terminal" classList={{ fail: target === WF_FAIL }} transform={`translate(${pos().x},${pos().y})`} onPointerDown={(e) => onTerminalPointerDown(e, target)}>
                  <rect class="wf-terminal-box" width={TERM_W} height={TERM_H} rx={20} />
                  <text class="wf-terminal-text" x={TERM_W / 2} y={25}>{target === WF_FAIL ? "失败结束" : "成功结束"}</text>
                </g>
              );
            }}
          </For>

          <For each={stages()}>
            {(stage) => (
              <g
                class="wf-node"
                classList={{ selected: props.selectedStageId === stage.id, entry: props.def.entry === stage.id, "connect-source": connectFrom() === stage.id }}
                transform={`translate(${stage.x},${stage.y})`}
                onPointerDown={(e) => onNodePointerDown(e, stage)}
                onPointerUp={(e) => onNodePointerUp(e, stage)}
                onDblClick={(e) => { e.stopPropagation(); props.onEditStage?.(stage.id); }}
                onContextMenu={(e) => openStageMenu(e, stage.id)}
              >
                <rect class="wf-node-box" width={NODE_W} height={NODE_H} rx={10} />
                <text class="wf-node-name" x={NODE_W / 2} y={24}>{stage.name || "（未命名）"}</text>
                <text class="wf-node-sub" x={NODE_W / 2} y={42}>{stage.reviewOnly ? "只读核验" : `${stage.transitions.length} 条转移`}</text>
                <Show when={props.def.entry === stage.id}><text class="wf-node-entry" x={10} y={-6}>起点</text></Show>
                <circle class="wf-node-handle" cx={NODE_W} cy={NODE_H / 2} r={8} onPointerDown={(e) => { e.stopPropagation(); startConnect(stage.id); }}>
                  <title>拖出连线</title>
                </circle>
              </g>
            )}
          </For>
        </g>
      </svg>

      <Show when={menu()}>
        {(m) => (
          <>
            <div class="wf-ctx-scrim" onPointerDown={() => setMenu(null)} onContextMenu={(e) => { e.preventDefault(); setMenu(null); }} />
            <div class="wf-ctx" style={{ left: `${m().x}px`, top: `${m().y}px` }}>
              <div class="wf-ctx-title">{m().kind === "stage" ? "阶段" : "连线"}</div>
              <button
                class="wf-ctx-item danger"
                disabled={readonly()}
                onClick={() => {
                  const cur = m();
                  setMenu(null);
                  if (!cur || readonly()) return;
                  if (cur.kind === "stage") props.onDeleteStage(cur.stageId);
                  else props.onDeleteTransition(cur.stageId, cur.transitionId);
                }}
              >
                {readonly() ? "内置只读，无法删除" : "删除"}
              </button>
            </div>
          </>
        )}
      </Show>
    </div>
  );
}
