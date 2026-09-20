import type { TimeMachineCheckpoint, TimeMachinePrompt } from './types';

export interface TimelineNode {
  id: string; checkpoint: TimeMachineCheckpoint | null; previewCheckpoint: TimeMachineCheckpoint | null;
  promptCount: number; currentPromptIndex: number | null; branchPrompts: TimeMachinePrompt[];
  title: string; x: number; y: number; current: boolean; onCurrentPath: boolean;
}
export interface TimelineGraph {
  nodes: TimelineNode[]; edges: { from: TimelineNode; to: TimelineNode; current: boolean }[];
  laneCount: number; width: number; height: number;
}
export const EMPTY_TIMELINE: TimelineGraph = { nodes: [], edges: [], laneCount: 1, width: 36, height: 80 };
type TreeNode = TimelineNode & { children: TreeNode[]; childrenById: Map<number, TreeNode[]>; prompt?: TimeMachinePrompt };
/** Fixed-length path fingerprint, not a concatenation of every ancestor's text.
 * Identity matching also checks the exact local prompt, so hashes never authorize edits.
 */
function fingerprint(parent: string, prompt: TimeMachinePrompt): string {
  const text = `${parent}\0${prompt.id}\0${prompt.text}`;
  let a = 2166136261, b = 5381;
  for (let i = 0; i < text.length; i++) { const c = text.charCodeAt(i); a = Math.imul(a ^ c, 16777619); b = Math.imul(b, 33) ^ c; }
  return `time-${(a >>> 0).toString(36)}-${(b >>> 0).toString(36)}`;
}
export function buildTimelineGraph(checkpoints: TimeMachineCheckpoint[], currentPrompts: TimeMachinePrompt[]): TimelineGraph {
  const root: TreeNode = { id: '__time_root__', checkpoint: null, previewCheckpoint: null, promptCount: 0,
    currentPromptIndex: null, branchPrompts: [], title: '会话开始', current: false, onCurrentPath: true,
    children: [], childrenById: new Map(), x: 0, y: 0 };
  const usedIds = new Set([root.id]);
  function insert(prompts: TimeMachinePrompt[], checkpoint: TimeMachineCheckpoint | null, current: boolean) {
    let parent = root;
    for (let i = 0; i < prompts.length; i++) {
      const prompt = prompts[i];
      const siblings = parent.childrenById.get(prompt.id) ?? [];
      let node = siblings.find(child => child.prompt?.text === prompt.text);
      if (!node) {
        const base = fingerprint(parent.id, prompt);
        let id = base, collision = 0;
        while (usedIds.has(id)) id = `${base}-${++collision}`;
        usedIds.add(id);
        node = { id, checkpoint: null, previewCheckpoint: checkpoint, promptCount: i + 1,
          currentPromptIndex: null, branchPrompts: prompts, title: prompt.text.trim().slice(0, 500) || `第 ${i + 1} 条提示词`,
          current: false, onCurrentPath: false, children: [], childrenById: new Map(), prompt, x: 0, y: 0 };
        siblings.push(node); parent.childrenById.set(prompt.id, siblings); parent.children.push(node);
      }
      if (!node.previewCheckpoint && checkpoint) node.previewCheckpoint = checkpoint;
      if (checkpoint && !current) node.branchPrompts = prompts;
      if (current) { node.onCurrentPath = true; node.currentPromptIndex = i; node.branchPrompts = prompts; }
      parent = node;
    }
    if (checkpoint) parent.checkpoint = checkpoint;
    return parent;
  }
  for (const checkpoint of checkpoints) insert(checkpoint.prompts, checkpoint, false);
  insert(currentPrompts, null, true).current = true;
  const children = (node: TreeNode) => [...node.children].sort((a, b) => Number(b.onCurrentPath) - Number(a.onCurrentPath));
  const lanes: [number, number][][] = [[]];
  const ends = new WeakMap<TreeNode, number>();
  function endOfSpine(node: TreeNode): number {
    const path: TreeNode[] = [];
    let cursor = node;
    while (!ends.has(cursor)) {
      path.push(cursor);
      const next = children(cursor).find(child => !child.onCurrentPath);
      if (!next) break;
      cursor = next;
    }
    const end = ends.get(cursor) ?? cursor.promptCount;
    for (const item of path) ends.set(item, end);
    return end;
  }
  function forkLane(node: TreeNode) {
    const start = node.promptCount, end = endOfSpine(node);
    for (let i = 1; i < lanes.length; i++) {
      if (lanes[i].every(([s, e]) => start > e + 1 || end < s - 1)) { lanes[i].push([start, end]); return i; }
    }
    lanes.push([[start, end]]); return lanes.length - 1;
  }
  const nodes: TimelineNode[] = [], links: [string, string][] = [];
  const stack = children(root).map(node => ({ node, lane: node.onCurrentPath ? 0 : forkLane(node) })).reverse();
  let maxPromptCount = currentPrompts.length;
  while (stack.length) {
    const { node, lane } = stack.pop()!;
    const nodeLane = node.onCurrentPath ? 0 : lane;
    const { children: _children, childrenById: _index, prompt: _prompt, ...view } = node;
    nodes.push({ ...view, x: 18 + nodeLane * 26, y: 20 + (node.promptCount - 1) * 32 });
    maxPromptCount = Math.max(maxPromptCount, node.promptCount);
    let continued = false;
    const next = children(node).map(child => {
      links.push([node.id, child.id]);
      let lane: number;
      if (child.onCurrentPath) lane = 0;
      else if (!node.onCurrentPath && !continued) { continued = true; lane = nodeLane; }
      else lane = forkLane(child);
      return { node: child, lane };
    });
    stack.push(...next.reverse());
  }
  const byId = new Map(nodes.map(node => [node.id, node]));
  return { nodes, edges: links.flatMap(([a, b]) => {
    const from = byId.get(a), to = byId.get(b);
    return from && to ? [{ from, to, current: from.onCurrentPath && to.onCurrentPath }] : [];
  }), laneCount: lanes.length, width: 36 + (lanes.length - 1) * 26, height: 48 + Math.max(1, maxPromptCount) * 32 };
}
export function visibleTimeline(graph: TimelineGraph, top: number, height: number) {
  const low = Math.max(0, top - 160), high = top + Math.max(300, height) + 160;
  return { nodes: graph.nodes.filter(n => n.y >= low && n.y <= high),
    edges: graph.edges.filter(e => e.to.y >= low && e.from.y <= high) };
}
