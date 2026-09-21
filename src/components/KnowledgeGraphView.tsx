import { createEffect, createMemo, createResource, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { buildKnowledgeGraph, graphEdgePath, type GraphNode, type KnowledgeGroup, type GraphEdge } from "../knowledgeGraph";
import "./KnowledgeGraphView.css";

const toolName = (tool: string) => ({ jianlai: "剑来", chrome: "Chrome", webview: "WebView" })[tool] ?? tool;
export default function KnowledgeGraphView() {
  const [loadError, setLoadError] = createSignal("");
  const [groups, { refetch }] = createResource(async () => {
    setLoadError("");
    try { return await invoke<KnowledgeGroup[]>("knowledge_graph"); }
    catch (error) { setLoadError(String(error)); return []; }
  });
  const graph = createMemo(() => buildKnowledgeGraph(groups() ?? []));
  const byId = createMemo(() => new Map(graph().nodes.map(node => [node.id, node])));
  const edgePairs = createMemo(() => new Set(graph().edges.map(edge => JSON.stringify([edge.source, edge.target]))));
  const [query, setQuery] = createSignal("");
  const [selectedId, setSelectedId] = createSignal("");
  const [edgeId, setEdgeId] = createSignal("");
  const [routeKey, setRouteKey] = createSignal("");
  const selected = () => byId().get(selectedId());
  const selectedEdge = () => graph().edges.find(edge => edge.id === edgeId());
  const activeRoute = () => graph().routes.find(route => route.key === routeKey());
  const routeNodes = createMemo(() => new Set(activeRoute()?.nodeIds ?? []));
  const [camera, setCamera] = createSignal({ x: 0, y: 0, scale: 1 });
  const [size, setSize] = createSignal({ width: 800, height: 600 });
  let canvas!: HTMLDivElement;
  let drag: { x: number; y: number; camera: ReturnType<typeof camera> } | undefined;
  const source = (node: GraphNode) => node.kind === "goal" ? "共同目标 · " + node.routeKeys.length + " 条路径" : toolName(node.tool) + " · " + node.scope;
  const matches = createMemo(() => {
    const text = query().trim().toLocaleLowerCase();
    const routes = new Map(graph().routes.map(route => [route.key, route]));
    return text ? graph().nodes.filter(node => (node.title + " " + source(node) + " " + node.routeKeys.map(key => routes.get(key)?.task).join(" ")).toLocaleLowerCase().includes(text)) : [];
  });
  const matchIds = createMemo(() => new Set(matches().map(node => node.id)));
  const selectedRoutes = () => graph().routes.filter(route => selected()?.routeKeys.includes(route.key));
  const incoming = () => graph().edges.filter(edge => edge.target === selectedId() && (!routeKey() || edge.occurrences.some(item => item.routeKey === routeKey())));
  const outgoing = () => graph().edges.filter(edge => edge.source === selectedId() && (!routeKey() || edge.occurrences.some(item => item.routeKey === routeKey())));
  const edgeRoutes = () => graph().routes.filter(route => selectedEdge()?.occurrences.some(item => item.routeKey === route.key));
  const outgoingCounts = createMemo(() => {
    const counts = new Map<string, number>();
    for (const edge of graph().edges) if (edge.kind === "step") counts.set(edge.source, (counts.get(edge.source) ?? 0) + 1);
    return counts;
  });
  const fit = () => {
    const nodes = graph().nodes;
    if (!nodes.length) { setCamera({ x: 0, y: 0, scale: 1 }); return; }
    const left = Math.min(...nodes.map(node => node.x - node.radius)) - 80;
    const right = Math.max(...nodes.map(node => node.x + node.radius)) + 80;
    const top = Math.min(...nodes.map(node => node.y - node.radius * 2.4)) - 35;
    const bottom = Math.max(...nodes.map(node => node.y + node.radius)) + 80;
    const scale = Math.min(1, size().width / (right - left), size().height / (bottom - top));
    setCamera({ x: -(left + right) / 2 * scale, y: -(top + bottom) / 2 * scale, scale });
  };
  const locate = (node: GraphNode) => {
    if (routeKey() && !node.routeKeys.includes(routeKey())) setRouteKey("");
    setEdgeId(""); setSelectedId(node.id); setCamera({ x: -node.x, y: -node.y, scale: 1 });
  };
  const selectEdge = (edge: GraphEdge) => { locate(byId().get(edge.source)!); setEdgeId(edge.id); };
  const overview = () => { setSelectedId(""); setEdgeId(""); setRouteKey(""); setQuery(""); fit(); };
  const zoom = (factor: number) => setCamera(value => {
    const scale = Math.max(.005, Math.min(2, value.scale * factor));
    return { x: value.x * scale / value.scale, y: value.y * scale / value.scale, scale };
  });
  createEffect(() => {
    graph(); size();
    const node = selected();
    if (node) setCamera({ x: -node.x, y: -node.y, scale: 1 }); else fit();
  });
  onMount(() => {
    const observer = new ResizeObserver(([entry]) => setSize({ width: entry.contentRect.width, height: entry.contentRect.height }));
    observer.observe(canvas); onCleanup(() => observer.disconnect());
  });
  return <main class="knowledge-page">
    <header class="knowledge-header"><div><h1>知识图谱</h1><p>所有起点、操作与目标，在一张图里连接</p></div><button class="btn" disabled={groups.loading} onClick={() => void refetch()}>刷新记录</button></header>
    <div class="knowledge-toolbar"><button class="btn" onClick={overview}>全图总览</button>
      <input type="search" aria-label="搜索全部图谱" placeholder="搜索步骤、目标、应用或网站…" value={query()} onInput={event => setQuery(event.currentTarget.value)} />
      <span class="knowledge-count">全部来源 · {graph().starts.length} 个起点 · {graph().nodes.length} 个节点 · {graph().edges.length} 条连接 · {graph().routes.length} 条路径</span>
    </div>
    <Show when={activeRoute()}>{route => <div class="knowledge-breadcrumb"><span>高亮路径：{route().task} · {toolName(route().tool)} · {route().scope}</span><button onClick={() => setRouteKey("")}>取消路径高亮</button></div>}</Show>
    <Show when={loadError()}><p class="knowledge-error" role="alert">读取失败：{loadError()}。请点击刷新重试。</p></Show>
    <div class="knowledge-body">
      <div ref={canvas} class="knowledge-canvas" aria-label="完整知识图谱"
        onWheel={event => { event.preventDefault(); zoom(event.deltaY < 0 ? 1.12 : 1 / 1.12); }}
        onPointerDown={event => { if (event.button !== 0 || (event.target as Element).closest('button, [role="button"]')) return; drag = { x: event.clientX, y: event.clientY, camera: camera() }; event.currentTarget.setPointerCapture(event.pointerId); }}
        onPointerMove={event => { if (drag) setCamera({ ...drag.camera, x: drag.camera.x + event.clientX - drag.x, y: drag.camera.y + event.clientY - drag.y }); }}
        onPointerUp={() => { drag = undefined; }} onPointerCancel={() => { drag = undefined; }}>
        <Show when={graph().nodes.length && !loadError()} fallback={<div class="knowledge-empty"><h2>{loadError() ? "图谱读取失败" : groups.loading ? "正在读取图谱…" : "还没有操作路径"}</h2><p>已保存的操作路径会在这里显示全部起点与连接。</p></div>}>
          <div class="knowledge-world" style={{ "--label-scale": Math.max(1, Math.min(1.3, 1 / camera().scale)), transform: 'translate(' + (size().width / 2 + camera().x) + 'px, ' + (size().height / 2 + camera().y) + 'px) scale(' + camera().scale + ')' }}>
            <svg class="knowledge-edges"><defs><marker id="knowledge-arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path d="M 0 0 L 10 5 L 0 10 z" /></marker></defs>
              <For each={graph().edges}>{edge => <path role="button" tabindex="0" aria-label={byId().get(edge.source)!.title + ' → ' + byId().get(edge.target)!.title}
                classList={{ selected: edgeId() === edge.id || (!!routeKey() ? edge.occurrences.some(item => item.routeKey === routeKey()) : edge.source === selectedId() || edge.target === selectedId()), goal: edge.kind === "goal" }}
                d={graphEdgePath(edge, byId(), edgePairs().has(JSON.stringify([edge.target, edge.source])))} marker-end="url(#knowledge-arrow)"
                onClick={() => selectEdge(edge)} onKeyDown={event => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); selectEdge(edge); } }}>
                <title>{edge.kind === "goal" ? "达成目标" : "已记录的操作顺序"} · {new Set(edge.occurrences.map(item => item.routeKey)).size} 条来源路径</title>
              </path>}</For>
            </svg>
            <For each={graph().nodes}>{node => <button class="knowledge-node" data-kind={node.kind} data-node-id={node.id} data-color={node.color}
              classList={{ root: node.kind === "start", selected: selectedId() === node.id, match: matchIds().has(node.id), "route-member": routeNodes().has(node.id) }}
              style={{ left: node.x + 'px', top: node.y + 'px', width: node.radius * 2 + 'px', height: node.radius * 2 + 'px' }} title={node.title + ' · ' + source(node)} aria-pressed={selectedId() === node.id} onClick={() => locate(node)}>
              <span class="knowledge-node-symbol" aria-hidden="true">{node.kind === "start" ? "⌂" : node.kind === "goal" ? "✓" : "⑂"}</span>
              <span class="knowledge-node-title">{node.title}</span><small>{node.kind === "start" ? "起点 · " + toolName(node.tool) : node.kind === "goal" ? "目标 · " + node.routeKeys.length + " 条路径" : (outgoingCounts().get(node.id) ?? 0) + " 个后续操作"}</small>
            </button>}</For>
          </div>
        </Show>
        <div class="knowledge-controls"><button aria-label="缩小" onClick={() => zoom(1 / 1.25)}>−</button><span>{Math.round(camera().scale * 100)}%</span><button aria-label="放大" onClick={() => zoom(1.25)}>＋</button><button onClick={fit}>适应画布</button></div>
        <div class="knowledge-help">全量展示 · 拖动平移 · 滚轮缩放 · 点击节点或连线查看来源</div>
      </div>
      <aside class="knowledge-detail" classList={{ idle: !selected() && !query().trim() }} aria-label="操作详情">
        <Show when={query().trim()}><section class="knowledge-search-results" aria-label="搜索结果"><h2 aria-live="polite">全部图谱 · {matches().length} 项结果</h2>
          <For each={matches()} fallback={<p>没有找到匹配的步骤或目标，请换个关键词。</p>}>{node => <button class="knowledge-next knowledge-search-result" onClick={() => locate(node)}><strong>{node.title}</strong><small>{source(node)}</small><span>定位到此处 ↗</span></button>}</For>
        </section></Show>
        <Show when={selectedEdge()}>{edge => <section class="knowledge-edge-detail"><h2>连接来源</h2><p>{byId().get(edge().source)!.title} → {byId().get(edge().target)!.title}</p><p>{edge().kind === "goal" ? "目标关系" : "操作顺序"}</p><For each={edgeRoutes()}>{route => <button class="knowledge-next" onClick={() => setRouteKey(route.key)}>{route.task} · 第 {edge().occurrences.filter(item => item.routeKey === route.key).map(item => item.step).join("、")} 步</button>}</For></section>}</Show>
        <Show when={selected()} fallback={<><h2>全部起点 · {graph().starts.length}</h2><p>实线表示记录中的操作顺序，虚线连接目标。同一应用内同名操作汇聚，不同来源可通过共同目标关联。</p>
          <For each={graph().starts}>{node => <button class="knowledge-next knowledge-search-result" onClick={() => locate(node)}><strong>{node.title}</strong><small>{source(node)}</small></button>}</For>
        </>}>
          {node => <><h2>{node().title}</h2><p>{source(node())}</p>
            <h3>从哪里来 · {incoming().length}</h3><For each={incoming()}>{edge => <button class="knowledge-next" onClick={() => locate(byId().get(edge.source)!)}>{byId().get(edge.source)!.title}<span>定位</span></button>}</For>
            <h3>接下来 · {outgoing().length}</h3><For each={outgoing()} fallback={<p>已到记录终点，请核对下方成功检查点。</p>}>{edge => <button class="knowledge-next" onClick={() => locate(byId().get(edge.target)!)}>{byId().get(edge.target)!.title}<span>{edge.kind === "goal" ? "查看目标" : "定位下一步"}</span></button>}</For>
            <h3>来源路径与检查点</h3><p>同名操作表示共同概念。选择一条来源路径，核对入口条件与完整顺序。</p>
            <For each={selectedRoutes()}>{route => <details class="knowledge-route"><summary>{route.task}<small>{toolName(route.tool)} · {route.scope} · {route.confidence === "reused" ? "多会话复用" : "首次验证"}</small></summary>
              <button class="btn" onClick={() => setRouteKey(route.key)}>高亮这条路径</button><p>入口：{route.conditions.join("；")}</p>
              <ol><For each={route.steps}>{(step, i) => <li><button class="knowledge-step-link" onClick={() => locate(byId().get(route.nodeIds[i() + 1])!)}>{step}</button></li>}</For></ol>
              <strong>成功检查点</strong><ul><For each={route.checks}>{check => <li>{check}</li>}</For></ul>
              <Show when={route.pitfalls.length}><strong>已知注意事项</strong><ul><For each={route.pitfalls}>{pitfall => <li>{pitfall}</li>}</For></ul></Show>
            </details>}</For>
          </>}
        </Show>
      </aside>
    </div>
  </main>;
}
