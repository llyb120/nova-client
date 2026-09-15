import type { Item } from "./types";

export function collectWorkspaceArtifacts(items: readonly Item[]): string[] {
  const paths = new Set<string>();
  const add = (value: unknown) => {
    if (typeof value !== "string" || !value || /^(?:https?:|data:|#)/i.test(value)) return;
    let path = value;
    try { path = decodeURIComponent(path); } catch { /* Literal percent in a filename. */ }
    path = path.replace(/^file:\/\/(?=\/|[a-z]:)/i, "").replace(/^\/([a-z]:[\\/])/i, "$1")
      .replace(/(?::\d+(?::\d+)?|#L\d+(?:-L?\d+)?)$/i, "");
    paths.add(path);
  };
  // ponytail: 最近 2000 条记录、200 个产物；完整历史仍通过会话内文件菜单打开。
  for (let i = items.length - 1; i >= Math.max(0, items.length - 2000) && paths.size < 200; i--) {
    const item = items[i];
    if (item.type === "tool") {
      for (const content of item.content) if (content.type === "diff") add(content.path);
    } else if (item.type === "assistant") {
      for (const match of item.text.matchAll(/!?\[[^\]]*\]\((?:<([^>]+)>|([^\s()]*(?:\([^()]*\)[^\s()]*)*))\)/g)) add(match[1] ?? match[2]);
    }
  }
  return [...paths].slice(0, 200);
}
