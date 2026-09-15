export type DiffRow = { text: string; kind: "add" | "del" | "context" | "meta"; old?: number; next?: number };

export function workspaceDiffRows(patch: string): DiffRow[][] {
  const groups: DiffRow[][] = [];
  let old = 0, next = 0, inHunk = false;
  for (const text of patch.replace(/\n$/, "").split("\n")) {
    const hunk = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(text);
    if (hunk) { old = Number(hunk[1]); next = Number(hunk[2]); inHunk = true; }
    else if (text.startsWith("diff --git")) inHunk = false;
    const kind = !hunk && inHunk ? text[0] === "+" ? "add" : text[0] === "-" ? "del" : text[0] === " " ? "context" : "meta" : "meta";
    const row: DiffRow = { text: kind === "meta" ? text : text.slice(1), kind };
    if (kind === "context" || kind === "del") row.old = old++;
    if (kind === "context" || kind === "add") row.next = next++;
    const previous = groups.at(-1);
    if (previous && previous[0].kind === kind) previous.push(row);
    else groups.push([row]);
  }
  return groups;
}
