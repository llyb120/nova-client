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
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i];
    if (item.type === "tool") {
      if (item.status && item.status !== "completed") continue;
      for (const content of item.content) if (content.type === "diff") add(content.path);
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
