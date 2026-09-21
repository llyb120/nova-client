export interface KnowledgeRoute {
  id: string;
  task: string;
  conditions: string[];
  steps: string[];
  checks: string[];
  pitfalls: string[];
  confidence: string;
  successes: number;
}
export interface KnowledgeGroup {
  tool: string;
  scope: string;
  graph: { routes: KnowledgeRoute[] };
}
export interface GraphRoute extends KnowledgeRoute {
  key: string;
  tool: string;
  scope: string;
  nodeIds: string[];
}
export interface GraphNode {
  id: string;
  kind: 'start' | 'action' | 'goal';
  title: string;
  tool: string;
  scope: string;
  routeKeys: string[];
  x: number;
  y: number;
  radius: number;
  color: number;
}
export interface GraphEdge {
  id: string;
  source: string;
  target: string;
  kind: 'step' | 'goal';
  occurrences: { routeKey: string; step: number }[];
}

export function buildKnowledgeGraph(groups: KnowledgeGroup[]) {
  const nodes = new Map<string, GraphNode>();
  const edges = new Map<string, GraphEdge>();
  const routes: GraphRoute[] = [];
  for (const group of groups) for (const route of group.graph.routes) {
    const key = JSON.stringify([group.tool, group.scope, route.id]);
    const add = (kind: GraphNode['kind'], identity: unknown, title: string) => {
      // Actions are concepts within one tool/site, not assertions of identical page state.
      // A shared goal links sources by topic; only recorded step edges imply order.
      const id = JSON.stringify(kind === 'goal' ? [kind, identity] : [kind, group.tool, group.scope, identity]);
      let node = nodes.get(id);
      if (!node) {
        const hash = Array.from(title).reduce((hash, char) => (hash * 31 + char.charCodeAt(0)) >>> 0, 0);
        node = { id, kind, title, tool: kind === 'goal' ? '' : group.tool, scope: kind === 'goal' ? '' : group.scope,
          routeKeys: [], x: 0, y: 0, radius: kind === 'start' ? 78 : 64, color: kind === 'start' ? 0 : kind === 'goal' ? 3 : 1 + hash % 4 };
        nodes.set(id, node);
      }
      if (!node.routeKeys.includes(key)) node.routeKeys.push(key);
      return id;
    };
    const nodeIds = [add('start', route.conditions, route.conditions.join('；') || '无额外入口条件'),
      ...route.steps.map(step => add('action', step, step)), add('goal', route.task, route.task)];
    for (let step = 1; step < nodeIds.length; step++) {
      const source = nodeIds[step - 1], target = nodeIds[step];
      const kind = step === nodeIds.length - 1 ? 'goal' : 'step';
      const id = JSON.stringify([source, target, kind]);
      let edge = edges.get(id);
      if (!edge) { edge = { id, source, target, kind, occurrences: [] }; edges.set(id, edge); }
      edge.occurrences.push({ routeKey: key, step });
    }
    routes.push({ ...route, key, tool: group.tool, scope: group.scope, nodeIds });
  }
  const allNodes = [...nodes.values()], allEdges = [...edges.values()];
  const adjacent = new Map(allNodes.map(node => [node.id, new Set<string>()]));
  for (const edge of allEdges) {
    adjacent.get(edge.source)!.add(edge.target);
    adjacent.get(edge.target)!.add(edge.source);
  }
  for (const node of allNodes) if (node.kind === 'action') node.radius = Math.min(85, 62 + adjacent.get(node.id)!.size * 3);
  const seen = new Set<string>();
  const components: GraphNode[][] = [];
  for (const node of allNodes) {
    if (seen.has(node.id)) continue;
    const component = [node]; seen.add(node.id);
    for (let i = 0; i < component.length; i++) for (const id of adjacent.get(component[i].id)!) {
      if (!seen.has(id)) { seen.add(id); component.push(nodes.get(id)!); }
    }
    components.push(component);
  }
  const boxes = components.map(component => {
    const index = new Map(component.map((node, i) => [node.id, i]));
    const links = allEdges.filter(edge => index.has(edge.source) && edge.source !== edge.target);
    component.forEach((node, i) => {
      node.x = Math.cos(i * 2.399963) * Math.sqrt(i) * 190;
      node.y = Math.sin(i * 2.399963) * Math.sqrt(i) * 190;
    });
    // ponytail: fixed 160 relaxation passes; spatial buckets bound local repulsion.
    // Dense graphs may retain edge crossings; use a dedicated layout engine if routing is needed.
    for (let pass = 0; pass < 160; pass++) {
      const forces = component.map(() => ({ x: 0, y: 0 }));
      const cells = new Map<string, number[]>();
      component.forEach((node, i) => {
        const key = Math.floor(node.x / 240) + ',' + Math.floor(node.y / 240);
        const cell = cells.get(key) ?? []; cell.push(i); cells.set(key, cell);
      });
      component.forEach((node, i) => {
        const cx = Math.floor(node.x / 240), cy = Math.floor(node.y / 240);
        for (let x = cx - 1; x <= cx + 1; x++) for (let y = cy - 1; y <= cy + 1; y++) {
          for (const j of cells.get(x + ',' + y) ?? []) {
            if (j <= i) continue;
            const other = component[j];
            const dx = other.x - node.x || .001, dy = other.y - node.y;
            const distance = Math.hypot(dx, dy);
            const force = Math.max(0, node.radius + other.radius + 55 - distance) * .25 / distance;
            forces[i].x -= dx * force; forces[i].y -= dy * force;
            forces[j].x += dx * force; forces[j].y += dy * force;
          }
        }
      });
      if (pass < 130) for (const edge of links) {
        const i = index.get(edge.source)!, j = index.get(edge.target)!;
        const a = component[i], b = component[j], dx = b.x - a.x, dy = b.y - a.y;
        const distance = Math.hypot(dx, dy) || 1;
        const force = (distance - a.radius - b.radius - 105) * .025 / distance;
        forces[i].x += dx * force; forces[i].y += dy * force;
        forces[j].x -= dx * force; forces[j].y -= dy * force;
      }
      component.forEach((node, i) => {
        node.x += Math.max(-15, Math.min(15, forces[i].x));
        node.y += Math.max(-15, Math.min(15, forces[i].y));
      });
    }
    const left = Math.min(...component.map(node => node.x - node.radius)) - 70;
    const top = Math.min(...component.map(node => node.y - node.radius)) - 90;
    const width = Math.max(...component.map(node => node.x + node.radius)) - left + 70;
    const height = Math.max(...component.map(node => node.y + node.radius)) - top + 90;
    return { component, left, top, width, height };
  });
  const rowWidth = Math.max(1, Math.sqrt(boxes.reduce((sum, box) => sum + box.width * box.height, 0)) * 1.6);
  let x = 0, y = 0, rowHeight = 0;
  for (const box of boxes) {
    if (x && x + box.width > rowWidth) { x = 0; y += rowHeight; rowHeight = 0; }
    for (const node of box.component) { node.x += x - box.left; node.y += y - box.top; }
    x += box.width; rowHeight = Math.max(rowHeight, box.height);
  }
  return { nodes: allNodes, edges: allEdges, routes, starts: allNodes.filter(node => node.kind === 'start') };
}

export function graphEdgePath(edge: GraphEdge, nodes: Map<string, GraphNode>, reverse: boolean) {
  const a = nodes.get(edge.source)!, b = nodes.get(edge.target)!;
  if (a === b) {
    const r = a.radius;
    return 'M ' + (a.x - r * .65) + ' ' + (a.y - r * .8) + ' C ' + (a.x - r * 1.8) + ' ' + (a.y - r * 2.3) + ', ' + (a.x + r * 1.8) + ' ' + (a.y - r * 2.3) + ', ' + (a.x + r * .65) + ' ' + (a.y - r * .8);
  }
  const dx = b.x - a.x, dy = b.y - a.y, distance = Math.hypot(dx, dy) || 1;
  const bend = reverse ? 45 : 0;
  const cx = (a.x + b.x) / 2 - dy / distance * bend, cy = (a.y + b.y) / 2 + dx / distance * bend;
  const fromLength = Math.hypot(cx - a.x, cy - a.y) || 1, toLength = Math.hypot(cx - b.x, cy - b.y) || 1;
  return 'M ' + (a.x + (cx - a.x) * (a.radius + 3) / fromLength) + ' ' + (a.y + (cy - a.y) * (a.radius + 3) / fromLength)
    + ' Q ' + cx + ' ' + cy + ', ' + (b.x + (cx - b.x) * (b.radius + 6) / toLength) + ' ' + (b.y + (cy - b.y) * (b.radius + 6) / toLength);
}
