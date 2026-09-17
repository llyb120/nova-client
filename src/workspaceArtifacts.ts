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
  // 图片生成等工具把产物路径放在结果 JSON（{path, markdown}）里：
  // kind 是 other、没有 locations，只能从工具输出文本中识别。
  const collectGenerated = (value: unknown, depth = 0) => {
    if (depth > 6 || value == null) return;
    if (typeof value === "string") {
      const text = value.trim();
      if (!text.startsWith("{") && !text.startsWith("[")) return;
      try { collectGenerated(JSON.parse(text), depth + 1); } catch { /* 普通文本输出。 */ }
      return;
    }
    if (typeof value !== "object") return;
    if (Array.isArray(value)) {
      for (const entry of value) collectGenerated(entry, depth + 1);
      return;
    }
    const record = value as Record<string, unknown>;
    if (typeof record.markdown === "string" && record.markdown.includes("![")) add(record.path);
    for (const key of Object.keys(record)) collectGenerated(record[key], depth + 1);
  };
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i];
    if (item.type === "tool") {
      if (item.status && item.status !== "completed") continue;
      for (const content of item.content) if (content.type === "diff") add(content.path);
      collectGenerated(item.content);
      collectGenerated(item.rawOutput);
      if (["edit", "write", "create", "file_change"].includes(item.kind)) {
        for (const location of item.locations ?? []) add(location.path);
        const input = item.rawInput as Record<string, unknown> | undefined;
        if (input && typeof input === "object") {
          add(input.path ?? input.file_path ?? input.filePath);
          if (Array.isArray(input.files)) for (const file of input.files) add(file?.path ?? file?.file_path);
        }
      }
    } else if (item.type === "assistant") {
      for (const match of item.text.matchAll(/!?\[[^\]]*\]\((?:<([^>]+)>|([^\s()]*(?:\([^()]*\)[^\s()]*)*))\)/g)) add(match[1] ?? match[2]);
    }
  }
  return [...paths];
}
