import { convertFileSrc } from "@tauri-apps/api/core";
import { diffLines } from "diff";
import { h, Node, parseMarkdown, type StyleProps } from "../../lib/canvas-dom";
import { isExpanded, state } from "../../store";
import type { Item, PromptImage, ToolContent, ToolItem, UserItem } from "../../types";
import { displayToolTitle, stripAnsi } from "../../utils";
import { relPath } from "../EditedFilesCard";
import { fmtDuration, fmtTokens, type Group } from "../TurnGroup";
import type { TranscriptTheme } from "./theme";

export type CanvasAction =
  | { type: "toggle-fold"; key: string }
  | { type: "toggle-thought"; key: string }
  | { type: "toggle-tool"; key: string }
  | { type: "toggle-tool-raw"; key: string }
  | { type: "edit-user"; itemId: number }
  | { type: "open-file"; path: string; line?: number }
  | { type: "copy-text"; text: string };

export interface BuildContext {
  theme: TranscriptTheme;
  isRunning: boolean;
  activeGroupIndex: number;
  lastGroupIndex: number;
  scrollTop: number;
  viewportHeight: number;
  /** Cached measured heights for placeholder virtualization */
  heightCache: Map<number, number>;
  /** Prefer expanding these placeholders even if slightly outside buffer */
  forceMount?: Set<number>;
}

const KIND_LABEL: Record<string, string> = {
  read: "读取",
  edit: "编辑",
  delete: "删除",
  move: "移动",
  search: "搜索",
  execute: "执行",
  think: "思考",
  fetch: "抓取",
};

function virtualBuffer(viewportHeight: number): number {
  return Math.max(1200, viewportHeight * 2);
}

function isBusyItem(item: Item): boolean {
  return (
    (item.type === "tool" && (item.status === "pending" || item.status === "in_progress")) ||
    (item.type === "thought" && item.text === "思考中…")
  );
}

function splitBody(group: Group, active: boolean): { process: Item[]; conclusion: Item[] } {
  const body = group.body;
  if (!group.turn) return { process: body, conclusion: [] };
  const lastConclusion = body.findLastIndex(
    (item) => item.type === "assistant" || item.type === "system",
  );
  if (lastConclusion < 0) return { process: body, conclusion: [] };
  let firstConclusion = lastConclusion;
  while (
    firstConclusion > 0 &&
    (body[firstConclusion - 1].type === "assistant" || body[firstConclusion - 1].type === "system")
  ) {
    firstConclusion--;
  }
  return {
    process: [...body.slice(0, firstConclusion), ...body.slice(lastConclusion + 1)],
    conclusion: body.slice(firstConclusion, lastConclusion + 1),
  };
}

function foldKey(group: Group): string {
  return `turn-${group.turn?.id ?? group.user?.id ?? group.body[0]?.id ?? 0}`;
}

function foldLabel(group: Group): string {
  const t = group.turn;
  const dur = t ? fmtDuration(t.durationMs) : "";
  const tok = t?.totalTokens ? `${fmtTokens(t.totalTokens)} tokens` : "";
  return ["已处理", dur, tok ? `· ${tok}` : ""].filter(Boolean).join(" ");
}

function attachmentSrc(img: PromptImage): string {
  if (img.data) return `data:${img.mimeType};base64,${img.data}`;
  const path = img.uri ? decodeURI(img.uri.replace(/^file:\/+/, "")) : "";
  return convertFileSrc(path);
}

function normalizeThoughtMarkdown(text: string): string {
  return state.agentKind === "opencode"
    ? text.replace(/(\S)\*{4}(?=\S)/g, "$1**\n\n**")
    : text;
}

function isCodexModelResumeWarning(item: Item): boolean {
  if (item.type !== "system" || item.level !== "error") return false;
  return (
    item.text.startsWith("This session was recorded with model `") &&
    item.text.includes("` but is resuming with `") &&
    item.text.includes("`. Consider switching back to `") &&
    item.text.endsWith("` as it may affect Codex performance.")
  );
}

function markdownNode(text: string, theme: TranscriptTheme, base?: StyleProps): Node {
  return parseMarkdown(
    text,
    {
      width: "100%",
      color: theme.text,
      fontSize: 14,
      fontFamily: theme.sans,
      lineHeight: 1.7,
      ...base,
    },
    theme.markdown,
  );
}

function buildUser(item: UserItem, ctx: BuildContext, canEdit: boolean): Node {
  const t = ctx.theme;
  const bubbleKids: Node[] = [];
  if (item.images?.length) {
    const row = h("div", {
      display: "flex",
      flexDirection: "row",
      flexWrap: "wrap",
      gap: 6,
      margin: [0, 0, 8, 0],
    });
    for (const img of item.images) {
      if (img.mimeType.startsWith("image/")) {
        row.appendChild(
          h("img", {
            src: attachmentSrc(img),
            width: 72,
            height: 72,
            borderRadius: 6,
          }),
        );
      } else {
        row.appendChild(
          h(
            "span",
            {
              display: "inline-block",
              padding: [4, 8],
              background: t.panel,
              borderRadius: 6,
              color: t.textDim,
              fontSize: 12,
              fontFamily: t.sans,
            },
            img.name,
          ),
        );
      }
    }
    bubbleKids.push(row);
  }
  bubbleKids.push(
    h(
      "div",
      {
        color: t.text,
        fontSize: 14,
        fontFamily: t.sans,
        lineHeight: 1.6,
        whiteSpace: "pre",
      },
      item.text,
    ),
  );

  const bubble = h(
    "div",
    {
      background: t.accentDim,
      borderRadius: t.radiusLg,
      padding: [10, 16],
      // approximate max-width 85% via flex end alignment of parent
    },
    "",
    bubbleKids,
  );

  const rowKids: Node[] = [];
  if (canEdit) {
    const editBtn = h(
      "button",
      {
        background: "transparent",
        color: t.textFaint,
        fontSize: 12,
        padding: [4, 6],
        borderRadius: 6,
        margin: [0, 6, 0, 0],
        cursor: "pointer",
        userSelect: "none",
      },
      "编辑",
    );
    editBtn.meta = { onClick: `edit-user:${item.id}`, kind: "edit-user", itemId: item.id };
    rowKids.push(editBtn);
  }
  rowKids.push(bubble);

  const row = h(
    "div",
    {
      display: "flex",
      flexDirection: "row",
      justifyContent: "flex-end",
      alignItems: "flex-start",
      margin: [20, 0, 16, 0],
      width: "100%",
    },
    "",
    rowKids,
  );
  row.meta = { kind: "user", itemId: item.id };
  return row;
}

function buildAssistant(item: Extract<Item, { type: "assistant" }>, ctx: BuildContext): Node | null {
  if (item.text.trim() === "None") return null;
  const wrap = h("div", { margin: [14, 0], width: "100%" });
  wrap.appendChild(markdownNode(item.text, ctx.theme));
  wrap.meta = { kind: "assistant", itemId: item.id };
  return wrap;
}

function buildThought(
  item: Extract<Item, { type: "thought" }>,
  ctx: BuildContext,
  active: boolean,
): Node {
  const t = ctx.theme;
  if (item.text === "思考中…") {
    return h(
      "div",
      {
        margin: [6, 0],
        color: t.textFaint,
        fontSize: 13,
        fontFamily: t.sans,
      },
      "思考中…",
    );
  }
  const key = `thought-${item.id}`;
  const open = isExpanded(key, active);
  const toggle = h(
    "button",
    {
      background: "transparent",
      color: t.textFaint,
      fontSize: 12,
      fontFamily: t.sans,
      padding: [3, 6],
      borderRadius: 6,
      cursor: "pointer",
      userSelect: "none",
    },
    `${open ? "▾" : "▸"} 思考过程`,
  );
  toggle.meta = { onClick: `toggle-thought:${key}`, kind: "thought-toggle" };

  const wrap = h("div", { margin: [6, 0], width: "100%" }, "", [toggle]);
  if (open) {
    const body = h(
      "div",
      {
        margin: [6, 0, 0, 0],
        padding: [8, 14],
        border: [0, 0, 0, 2],
        borderColor: t.borderLight,
        color: t.textDim,
      },
    );
    body.appendChild(
      markdownNode(normalizeThoughtMarkdown(item.text), ctx.theme, {
        fontSize: 13,
        color: t.textDim,
        lineHeight: 1.6,
      }),
    );
    wrap.appendChild(body);
  }
  return wrap;
}

function toolLabel(item: ToolItem): string {
  const title = (item.title || "").trim();
  if (title) return displayToolTitle(stripAnsi(title));
  return KIND_LABEL[item.kind] ?? item.kind;
}

function toolDiffStats(item: ToolItem): { add: number; del: number } {
  let add = 0;
  let del = 0;
  for (const b of item.content) {
    if (b.type !== "diff") continue;
    const d = b as Extract<ToolContent, { type: "diff" }>;
    for (const part of diffLines(d.oldText ?? "", d.newText ?? "")) {
      const n = part.count ?? 0;
      if (part.added) add += n;
      else if (part.removed) del += n;
    }
  }
  return { add, del };
}

function compactValue(value: unknown): string {
  if (value == null) return "";
  if (typeof value === "string") return stripAnsi(value).trim();
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  try {
    const text = stripAnsi(JSON.stringify(value, null, 2)).replace(/\s+/g, " ").trim();
    return text.length > 180 ? `${text.slice(0, 180)}...` : text;
  } catch {
    return stripAnsi(String(value));
  }
}

function toolSummary(item: ToolItem): string {
  const raw = item.rawInput ?? item.rawOutput;
  if (raw && typeof raw === "object" && !Array.isArray(raw)) {
    const preferred = ["command", "cmd", "query", "q", "path", "file_path", "url", "symbol", "task", "prompt"];
    for (const key of preferred) {
      const text = compactValue((raw as Record<string, unknown>)[key]);
      if (text) return `${key}: ${text}`;
    }
  }
  return compactValue(raw);
}

function buildToolOutputText(item: ToolItem): string {
  const parts: string[] = [];
  for (const block of item.content) {
    if (block.type === "content") {
      const inner = (block as { content?: { type?: string; text?: string } }).content;
      if (inner?.type === "text" && inner.text) {
        const clean = stripAnsi(inner.text).trim();
        if (clean && clean !== "null" && clean !== "undefined") parts.push(clean);
      }
    } else if (block.type === "diff") {
      const d = block as Extract<ToolContent, { type: "diff" }>;
      const lines: string[] = [`@@ ${relPath(d.path)}`];
      const diffs = diffLines(d.oldText ?? "", d.newText ?? "");
      for (const part of diffs) {
        const sign = part.added ? "+" : part.removed ? "-" : " ";
        const ls = part.value.replace(/\n$/, "").split("\n");
        if (!part.added && !part.removed && ls.length > 8) {
          for (const l of ls.slice(0, 3)) lines.push(`${sign}${l}`);
          lines.push(` … 省略 ${ls.length - 6} 行 …`);
          for (const l of ls.slice(-3)) lines.push(`${sign}${l}`);
        } else {
          for (const l of ls) lines.push(`${sign}${l}`);
        }
      }
      parts.push(lines.join("\n"));
    }
  }
  if (parts.length === 0) {
    const s = toolSummary(item);
    if (s) parts.push(s);
  }
  return parts.join("\n\n");
}

function buildTool(item: ToolItem, ctx: BuildContext, active: boolean): Node {
  const t = ctx.theme;
  const key = `tool-${item.id}`;
  const rawKey = `tool-raw-${item.id}`;
  const defaultOpen =
    !!active || item.status === "pending" || item.status === "in_progress";
  const open = isExpanded(key, defaultOpen);
  const showRaw = isExpanded(rawKey);
  const stats = toolDiffStats(item);
  const hasBody =
    item.content.length > 0 ||
    item.locations.length > 0 ||
    item.rawInput !== undefined ||
    item.rawOutput !== undefined;

  const statusSuffix =
    item.status === "failed"
      ? " ●"
      : item.status === "pending" || item.status === "in_progress"
        ? " …"
        : "";
  const statsText =
    stats.add + stats.del > 0 ? `  +${stats.add} -${stats.del}` : "";
  const title = `${toolLabel(item)}${statsText}${statusSuffix}${hasBody ? (open ? "  ▾" : "  ▸") : ""}`;

  const line = h(
    "button",
    {
      display: "flex",
      width: "100%",
      background: "transparent",
      color: item.status === "failed" ? t.red : t.textDim,
      fontSize: 13,
      fontFamily: t.sans,
      textAlign: "left",
      padding: [4, 2],
      borderRadius: 6,
      cursor: hasBody ? "pointer" : "default",
      userSelect: "none",
    },
    title,
  );
  if (hasBody) line.meta = { onClick: `toggle-tool:${key}`, kind: "tool-toggle", itemId: item.id };

  const wrap = h("div", { margin: [4, 0], width: "100%" }, "", [line]);

  if (open && hasBody) {
    const body = h("div", {
      margin: [4, 0, 4, 12],
      padding: [4, 0, 4, 10],
      border: [0, 0, 0, 2],
      borderColor: t.border,
      width: "100%",
    });

    if (item.locations.length > 0) {
      const locs = h("div", {
        display: "flex",
        flexWrap: "wrap",
        gap: 6,
        margin: [0, 0, 6, 0],
      });
      for (const loc of item.locations) {
        if (!loc.path) continue;
        const chip = h(
          "button",
          {
            background: t.panel,
            color: t.accent,
            fontSize: 11,
            fontFamily: t.mono,
            padding: [2, 8],
            borderRadius: 999,
            cursor: "pointer",
            userSelect: "none",
          },
          `${loc.path.split(/[\\/]/).pop()}${loc.line != null ? `:${loc.line}` : ""}`,
        );
        chip.meta = {
          onClick: `open-file:${loc.path}${loc.line != null ? `:${loc.line}` : ""}`,
          kind: "open-file",
          path: loc.path,
          line: loc.line,
        };
        locs.appendChild(chip);
      }
      body.appendChild(locs);
    }

    const out = buildToolOutputText(item);
    if (out) {
      const pre = h(
        "pre",
        {
          background: t.panel,
          color: t.textDim,
          fontFamily: t.mono,
          fontSize: 12,
          lineHeight: 1.45,
          padding: 10,
          borderRadius: 8,
          whiteSpace: "pre",
          overflow: "auto",
          height: Math.min(280, Math.max(64, out.split("\n").length * 17 + 20)),
          margin: [0, 0, 6, 0],
        },
        out,
      );
      pre.meta = { kind: "tool-output", itemId: item.id, nestedScroll: true };
      body.appendChild(pre);
    }

    if (item.rawInput !== undefined || item.rawOutput !== undefined) {
      const rawToggle = h(
        "button",
        {
          background: "transparent",
          color: t.textFaint,
          fontSize: 11,
          fontFamily: t.sans,
          padding: [2, 4],
          cursor: "pointer",
          userSelect: "none",
        },
        showRaw ? "隐藏原始数据" : "原始数据",
      );
      rawToggle.meta = { onClick: `toggle-tool-raw:${rawKey}`, kind: "tool-raw" };
      body.appendChild(rawToggle);
      if (showRaw) {
        const rawText = [
          item.rawInput !== undefined ? `input:\n${compactValue(item.rawInput)}` : "",
          item.rawOutput !== undefined ? `output:\n${compactValue(item.rawOutput)}` : "",
        ]
          .filter(Boolean)
          .join("\n\n");
        body.appendChild(
          h(
            "pre",
            {
              background: t.panel,
              color: t.textMuted,
              fontFamily: t.mono,
              fontSize: 11,
              padding: 8,
              borderRadius: 6,
              whiteSpace: "pre",
              overflow: "auto",
              height: Math.min(200, rawText.split("\n").length * 15 + 16),
              margin: [4, 0, 0, 0],
            },
            rawText,
          ),
        );
      }
    }

    wrap.appendChild(body);
  }

  return wrap;
}

function buildSystem(item: Extract<Item, { type: "system" }>, ctx: BuildContext): Node | null {
  if (isCodexModelResumeWarning(item)) return null;
  const t = ctx.theme;
  const isCompaction = item.level === "compacting" || item.level === "compacted";
  if (isCompaction) {
    return h(
      "div",
      {
        margin: [14, 0],
        color: t.textFaint,
        fontSize: 12,
        fontFamily: t.sans,
        textAlign: "center",
      },
      `── ${item.text} ──`,
    );
  }
  const color =
    item.level === "error" ? t.red : item.level === "warn" ? t.yellow : t.textFaint;
  const bg =
    item.level === "error"
      ? "rgba(224,125,118,0.08)"
      : item.level === "warn"
        ? "rgba(212,178,110,0.07)"
        : "transparent";
  return h(
    "div",
    {
      margin: [10, 0],
      padding: [8, 13],
      borderRadius: 8,
      color,
      background: bg,
      fontSize: 12.5,
      fontFamily: t.sans,
      textAlign: item.level === "info" || item.level === "error" || item.level === "warn" ? "left" : "center",
    },
    item.text,
  );
}

function buildItem(item: Item, ctx: BuildContext, activeItem: boolean): Node | null {
  switch (item.type) {
    case "user":
      return buildUser(item, ctx, !ctx.isRunning);
    case "assistant":
      return buildAssistant(item, ctx);
    case "thought":
      return buildThought(item, ctx, activeItem);
    case "tool":
      return buildTool(item, ctx, activeItem);
    case "system":
      return buildSystem(item, ctx);
    default:
      return null;
  }
}

function collectEdits(body: Item[]): Array<{ path: string; add: number; del: number }> {
  const map = new Map<string, { first: string | null; last: string }>();
  for (const item of body) {
    if (item.type !== "tool") continue;
    for (const b of item.content) {
      if (b.type !== "diff") continue;
      const d = b as Extract<ToolContent, { type: "diff" }>;
      if (!d.path) continue;
      const prev = map.get(d.path);
      if (prev) prev.last = d.newText ?? "";
      else map.set(d.path, { first: d.oldText ?? null, last: d.newText ?? "" });
    }
  }
  return [...map.entries()]
    .map(([path, { first, last }]) => {
      let add = 0;
      let del = 0;
      for (const part of diffLines(first ?? "", last)) {
        if (part.added) add += part.count ?? 0;
        else if (part.removed) del += part.count ?? 0;
      }
      return { path, add, del };
    })
    .filter((e) => e.add + e.del > 0);
}

function buildEditedFiles(body: Item[], undoneKey: string, ctx: BuildContext): Node | null {
  const edits = collectEdits(body);
  if (edits.length === 0) return null;
  const t = ctx.theme;
  const undone = isExpanded(undoneKey);
  let add = 0;
  let del = 0;
  for (const e of edits) {
    add += e.add;
    del += e.del;
  }
  const head = h(
    "div",
    {
      margin: [10, 0, 4, 0],
      color: t.textDim,
      fontSize: 12.5,
      fontFamily: t.sans,
    },
    undone
      ? `已撤销 ${edits.length} 个文件的改动`
      : `已编辑 ${edits.length} 个文件  +${add} -${del}`,
  );
  const wrap = h("div", { width: "100%" }, "", [head]);
  if (!undone) {
    for (const e of edits) {
      const row = h(
        "button",
        {
          background: "transparent",
          color: t.accent,
          fontSize: 12,
          fontFamily: t.mono,
          textAlign: "left",
          padding: [2, 0],
          cursor: "pointer",
          userSelect: "none",
        },
        `${relPath(e.path)}  +${e.add} -${e.del}`,
      );
      row.meta = { onClick: `open-file:${e.path}`, kind: "open-file", path: e.path };
      wrap.appendChild(row);
    }
  }
  return wrap;
}

function activeBodyId(group: Group, active: boolean): number {
  if (!active) return -1;
  const b = group.body;
  for (let i = b.length - 1; i >= 0; i--) {
    if (isBusyItem(b[i])) return b[i].id;
  }
  return b.length ? b[b.length - 1].id : -1;
}

function showLiveTail(group: Group, active: boolean): boolean {
  if (!active) return false;
  const b = group.body;
  if (b.length === 0 || !b.some(isBusyItem)) return false;
  const last = b[b.length - 1];
  return last.type === "assistant" || last.type === "system";
}

function buildGroupFull(group: Group, index: number, ctx: BuildContext, active: boolean): Node {
  const t = ctx.theme;
  const kids: Node[] = [];
  if (group.user) kids.push(buildUser(group.user, ctx, !ctx.isRunning));
  if (group.turn?.actualModel) {
    kids.push(
      h(
        "div",
        {
          margin: [-8, 0, 8, 0],
          padding: [2, 7],
          borderRadius: 999,
          border: 1,
          borderColor: t.borderLight,
          color: t.textFaint,
          background: t.panel,
          fontFamily: t.mono,
          fontSize: 11,
          // right-ish: use align via parent flex end — approximate with marginLeft auto via flex
        },
        `实际模型：${group.turn.actualModel}`,
      ),
    );
  }

  const { process, conclusion } = splitBody(group, active);
  const foldable = !!group.turn && !active;
  const bodyExpanded = group.body.some((it) => state.expanded[String(it.id)]);
  const open = state.expanded[foldKey(group)] ?? bodyExpanded;
  const activeId = activeBodyId(group, active);

  if (process.length > 0) {
    if (foldable) {
      const fold = h(
        "button",
        {
          background: "transparent",
          color: t.textDim,
          fontSize: 13,
          fontFamily: t.sans,
          padding: [4, 8],
          margin: [12, 0, 2, -8],
          borderRadius: 7,
          cursor: "pointer",
          userSelect: "none",
        },
        `${foldLabel(group)} ${open ? "▾" : "▸"}`,
      );
      fold.meta = { onClick: `toggle-fold:${foldKey(group)}`, kind: "turn-fold" };
      kids.push(fold);
      if (open) {
        const processWrap = h("div", {
          margin: [4, 0, 6, 0],
          padding: [2, 0, 2, 12],
          border: [0, 0, 0, 2],
          borderColor: t.border,
        });
        for (const item of process) {
          const n = buildItem(item, ctx, false);
          if (n) processWrap.appendChild(n);
        }
        kids.push(processWrap);
      }
    } else {
      for (const item of process) {
        const n = buildItem(item, ctx, active && item.id === activeId);
        if (n) kids.push(n);
      }
    }
  }

  if (showLiveTail(group, active)) {
    kids.push(
      h(
        "div",
        {
          margin: [4, 0, 10, 0],
          color: t.textFaint,
          fontSize: 12.5,
          fontFamily: t.sans,
        },
        "● 继续处理中…",
      ),
    );
  }

  for (const item of conclusion) {
    const n = buildItem(item, ctx, false);
    if (n) kids.push(n);
  }

  if (foldable) {
    const edited = buildEditedFiles(group.body, `undone-${foldKey(group)}`, ctx);
    if (edited) kids.push(edited);
  }

  const root = h("div", { width: "100%", margin: [0, 0, 8, 0] }, "", kids);
  root.meta = {
    kind: "group",
    groupIndex: index,
    cacheLayer: foldable && !open,
  };
  return root;
}

function buildPlaceholder(index: number, height: number): Node {
  const node = h("div", {
    width: "100%",
    height: Math.max(48, height),
    margin: [0, 0, 8, 0],
  });
  node.meta = { kind: "placeholder", groupIndex: index };
  return node;
}

/**
 * Estimate cumulative group tops from height cache (for virtualization without layout).
 * Unknown heights fall back to 160px.
 */
function estimateOffsets(groups: Group[], heightCache: Map<number, number>): number[] {
  const tops: number[] = [];
  let y = 0;
  for (let i = 0; i < groups.length; i++) {
    tops.push(y);
    y += heightCache.get(i) ?? 160;
  }
  return tops;
}

/** Build the full transcript content tree with far-viewport placeholders. */
export function buildTranscriptNodes(groups: Group[], ctx: BuildContext): Node {
  const content = h("div", {
    width: "100%",
    padding: [24, 0, 16, 0],
  });

  // Empty state is rendered as a DOM overlay in CanvasTranscript (includes cwd).
  if (groups.length === 0) return content;

  const buffer = virtualBuffer(ctx.viewportHeight);
  const tops = estimateOffsets(groups, ctx.heightCache);
  const viewTop = ctx.scrollTop - buffer;
  const viewBottom = ctx.scrollTop + ctx.viewportHeight + buffer;

  for (let i = 0; i < groups.length; i++) {
    const group = groups[i];
    const active = ctx.isRunning && !group.turn;
    const keepMounted = i === ctx.lastGroupIndex || active || ctx.forceMount?.has(i);
    const top = tops[i];
    const hGuess = ctx.heightCache.get(i) ?? 160;
    const near = top + hGuess >= viewTop && top <= viewBottom;

    if (!keepMounted && !near) {
      content.appendChild(buildPlaceholder(i, hGuess));
    } else {
      content.appendChild(buildGroupFull(group, i, ctx, active));
    }
  }

  return content;
}

/** After layout, refresh height cache from real group / placeholder nodes. */
export function updateHeightCache(root: Node, heightCache: Map<number, number>): void {
  const walk = (n: Node) => {
    const kind = n.meta?.kind;
    const idx = n.meta?.groupIndex;
    if ((kind === "group" || kind === "placeholder") && typeof idx === "number" && n._height > 0) {
      heightCache.set(idx, n._height);
    }
    for (const c of n.children) walk(c);
  };
  walk(root);
}

export function parseCanvasAction(raw: string): CanvasAction | null {
  if (raw.startsWith("toggle-fold:")) return { type: "toggle-fold", key: raw.slice(12) };
  if (raw.startsWith("toggle-thought:")) return { type: "toggle-thought", key: raw.slice(15) };
  if (raw.startsWith("toggle-tool-raw:")) return { type: "toggle-tool-raw", key: raw.slice(16) };
  if (raw.startsWith("toggle-tool:")) return { type: "toggle-tool", key: raw.slice(12) };
  if (raw.startsWith("edit-user:")) return { type: "edit-user", itemId: Number(raw.slice(10)) };
  if (raw.startsWith("open-file:")) {
    const rest = raw.slice(10);
    const m = rest.match(/^(.*):(\d+)$/);
    if (m) return { type: "open-file", path: m[1], line: Number(m[2]) };
    return { type: "open-file", path: rest };
  }
  if (raw.startsWith("copy-text:")) return { type: "copy-text", text: raw.slice(10) };
  return null;
}
