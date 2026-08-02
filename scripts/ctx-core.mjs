// ctx-core.mjs — 上下文检索核心逻辑 (纯模块, 无协议)
//
// 被两个薄封装复用:
//   scripts/alkaid-core.mjs              Vega / Alkaid 内置工具
//   scripts/cursor-filesystem-tools.mjs  Cursor MCP 工具
//
// Pipeline: Discover → Rank → Expand → Pack → Cover
// 检索引擎: git grep + 内存符号窗（子进程并行），全程无 LLM、无预建索引。

import { execFile, execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { readFile } from 'node:fs/promises';

export function repoRoot() {
  if (process.env.CTX_ROOT) return process.env.CTX_ROOT;
  try {
    return execFileSync('git', ['rev-parse', '--show-toplevel'], {
      encoding: 'utf8',
      maxBuffer: 1024 * 1024,
    }).trim() || process.cwd();
  } catch {
    return process.cwd();
  }
}

const EXCLUDE = /(?:^|\/)(?:node_modules|dist|target|src-tauri\/target)(?:\/|$)|package-lock|\.png$|\.jpg$|\.jpeg$|\.gif$|\.webp$|\.ico$|\.woff2?$|\.ttf$|\.bin$/i;
const NEIGHBOR_EXCLUDE = /\.md$|\.github\/|\.yml$|\.yaml$|\.json$|\.toml$|docs\/|scripts\/legacy-context/;
const TEST_PATH = /(?:^|\/)(?:tests?|__tests__|spec)(?:\/|$)|(?:\.|\b)(?:test|spec)\.[^.\/]+$/i;
const CONFIG_PATH = /\.(?:toml|json|ya?ml|ini|env|lock)$/i;
const SOURCE_EXT = /\.(?:rs|ts|tsx|js|jsx|mjs|cjs|go|py|java|kt|swift|c|cc|cpp|h|hpp)$/i;

const DEF_LINE_RE = /^\s*(?:(?:pub(?:\([^)]*\))?|export|async|unsafe|default|static|const|move)\s+)*(?:fn|struct|enum|trait|impl|type|class|interface|function|def|mod)\b/;
const SYM_START_RE = /^(\s*)(?:(?:pub(?:\([^)]*\))?|export|async|unsafe|default|static|const|move)\s+)*(?:(?:fn|struct|enum|trait|type|class|interface|function|def|mod)\s+([A-Za-z_][\w]*)|(impl)(?:\s*<[^>]+>)?\s+(?:(?:[\w:]+)\s+for\s+)?([A-Za-z_][\w:]*))/;

const INTENT_CHARS = { edit: 48_000, explain: 36_000, locate: 16_000 };
const HARD_MAX_CHARS = 80_000;
const FULL_ALWAYS = 120;
const FULL_HIGH_SCORE = 220;
const HIGH_SCORE = 10;
const MAX_WINDOW_LINES = 180;
const MERGE_GAP = 15;
const CORE_SOFT_CAP = 12;
const NEIGHBOR_CAP = 12;
const NEIGHBOR_THIN_CAP = 4;
const WALL_MS = 4500;
const PROC_TIMEOUT_MS = 1800;

function runAsync(root, cmd, args, timeoutMs = PROC_TIMEOUT_MS) {
  return new Promise((resolve) => {
    execFile(
      cmd,
      args,
      { cwd: root, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, timeout: timeoutMs, killSignal: 'SIGKILL' },
      (err, stdout) => {
        if (stdout) return resolve(stdout);
        if (err?.stdout) return resolve(String(err.stdout));
        resolve('');
      },
    );
  });
}

async function mapPool(items, concurrency, fn) {
  const list = [...items];
  if (list.length === 0) return [];
  const results = new Array(list.length);
  let next = 0;
  const workers = Array.from(
    { length: Math.min(Math.max(1, concurrency), list.length) },
    async () => {
      while (true) {
        const i = next++;
        if (i >= list.length) return;
        results[i] = await fn(list[i], i);
      }
    },
  );
  await Promise.all(workers);
  return results;
}

function shortHead(root) {
  try {
    return execFileSync('git', ['rev-parse', '--short', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
  } catch {
    return 'unknown';
  }
}

function resolveMaxChars({ intent = 'edit', maxChars, budget, ctx, maxFiles } = {}) {
  const base = INTENT_CHARS[intent] ?? INTENT_CHARS.edit;
  let chars = Number.isFinite(maxChars) ? maxChars : base;
  if (!Number.isFinite(maxChars) && Number.isFinite(budget)) {
    const radius = Number.isFinite(ctx) ? ctx : 12;
    chars = Math.min(HARD_MAX_CHARS, Math.max(8_000, Math.floor(budget * Math.max(40, radius * 3))));
  }
  if (Number.isFinite(maxFiles) && maxFiles > 0 && maxFiles < CORE_SOFT_CAP) {
    chars = Math.min(chars, Math.max(12_000, maxFiles * 4_000));
  }
  return Math.max(4_000, Math.min(HARD_MAX_CHARS, Math.floor(chars)));
}

function classifyPath(path) {
  if (TEST_PATH.test(path)) return 'test';
  if (CONFIG_PATH.test(path) || /(^|\/)(?:settings|config|Cargo\.toml|package\.json)/i.test(path)) return 'config';
  return 'other';
}

function pathHintBonus(path, pathHints) {
  if (!pathHints?.length) return 0;
  let bonus = 0;
  for (const hint of pathHints) {
    if (!hint) continue;
    if (path === hint || path.startsWith(hint.replace(/\/?$/, '/'))) bonus += 6;
    else if (path.includes(hint)) bonus += 3;
  }
  return bonus;
}

function parseGrepLines(text) {
  const out = [];
  for (const line of text.split('\n')) {
    if (!line) continue;
    const i1 = line.indexOf(':');
    if (i1 <= 0) continue;
    const i2 = line.indexOf(':', i1 + 1);
    if (i2 <= i1) continue;
    const file = line.slice(0, i1);
    const lineNo = Number(line.slice(i1 + 1, i2));
    if (!file || !Number.isFinite(lineNo)) continue;
    out.push({ file, line: lineNo, text: line.slice(i2 + 1) });
  }
  return out;
}

async function discoverKeyword(root, kw) {
  const stdout = await runAsync(root, 'git', ['grep', '-nI', '--', kw]);
  return parseGrepLines(stdout).filter((h) => !EXCLUDE.test(h.file));
}

function scoreFile(rec, pathHints) {
  const kind = classifyPath(rec.path);
  let score = Math.min(12, rec.hitLines.size) * 1.5;
  score += Math.min(8, rec.keywords.size) * 2;
  if (rec.defHits > 0) score += 8 + Math.min(4, rec.defHits);
  score += pathHintBonus(rec.path, pathHints);
  if (SOURCE_EXT.test(rec.path)) score += 3;
  if (kind === 'test') score -= 4;
  if (kind === 'config') score -= 2;
  if (rec.mentionOnly && rec.defHits === 0) score -= 3;
  rec.kind = rec.defHits > 0 ? 'def' : kind === 'other' ? 'use' : kind;
  rec.score = score;
  return rec;
}

function extractSymbolStarts(lines) {
  const starts = [];
  for (let i = 0; i < lines.length; i++) {
    const m = lines[i].match(SYM_START_RE);
    if (!m) continue;
    const name = m[2] || m[4] || m[3] || 'symbol';
    starts.push({ line: i + 1, name: String(name).replace(/:.*/, ''), indent: m[1].length });
  }
  return starts;
}

function windowEnd(lines, startLine, nextStartLine) {
  // Next-symbol boundary is the reliable closer. Brace-balancing is unsafe in JS/TS
  // because regex literals, templates, and `= {}` defaults routinely confuse scanners.
  if (nextStartLine) return nextStartLine - 1;
  return lines.length;
}

function hitToWindow(hitLine, starts, lines) {
  let idx = -1;
  for (let i = 0; i < starts.length; i++) {
    if (starts[i].line <= hitLine) idx = i;
    else break;
  }
  if (idx < 0) {
    return {
      start: Math.max(1, hitLine - 20),
      end: Math.min(lines.length, hitLine + 20),
      name: null,
      partial: false,
    };
  }
  const start = starts[idx].line;
  const next = starts[idx + 1]?.line;
  const end = windowEnd(lines, start, next);
  if (end - start + 1 > MAX_WINDOW_LINES) {
    const localStart = Math.max(start, hitLine - 20);
    const localEnd = Math.min(end, hitLine + 20);
    return {
      start,
      end,
      name: starts[idx].name,
      partial: true,
      slices: [
        { start, end: Math.min(end, start + 15) },
        { start: localStart, end: localEnd },
      ],
    };
  }
  return { start, end, name: starts[idx].name, partial: false };
}

function mergeSlices(slices) {
  const sorted = [...slices].sort((a, b) => a.start - b.start);
  const out = [];
  for (const s of sorted) {
    const last = out[out.length - 1];
    if (!last || s.start > last.end + MERGE_GAP) out.push({ ...s });
    else last.end = Math.max(last.end, s.end);
  }
  return out;
}

function mergeWindows(windows) {
  if (!windows.length) return [];
  const sorted = [...windows].sort((a, b) => a.start - b.start);
  const out = [];
  for (const w of sorted) {
    const last = out[out.length - 1];
    if (!last) {
      out.push({ ...w, slices: w.slices ? [...w.slices] : null });
      continue;
    }
    if (w.start <= last.end + MERGE_GAP) {
      last.end = Math.max(last.end, w.end);
      last.partial = last.partial || w.partial;
      if (w.name && !last.name) last.name = w.name;
      if (w.slices || last.slices) {
        last.slices = mergeSlices([
          ...(last.slices || [{ start: last.start, end: last.end }]),
          ...(w.slices || [{ start: w.start, end: w.end }]),
        ]);
        last.partial = true;
      }
    } else {
      out.push({ ...w, slices: w.slices ? [...w.slices] : null });
    }
  }
  return out;
}

function coveredRanges(windows, total) {
  const ranges = [];
  for (const w of windows) {
    if (w.partial && w.slices) {
      for (const s of w.slices) ranges.push([s.start, s.end]);
    } else {
      ranges.push([w.start, w.end]);
    }
  }
  ranges.sort((a, b) => a[0] - b[0]);
  const merged = [];
  for (const r of ranges) {
    const last = merged[merged.length - 1];
    if (!last || r[0] > last[1] + 1) merged.push([...r]);
    else last[1] = Math.max(last[1], r[1]);
  }
  const gaps = [];
  let cursor = 1;
  for (const [a, b] of merged) {
    if (cursor < a) gaps.push([cursor, a - 1]);
    cursor = Math.max(cursor, b + 1);
  }
  if (cursor <= total) gaps.push([cursor, total]);
  return { covered: merged, gaps };
}

function formatRangeList(ranges) {
  return ranges.map(([a, b]) => (a === b ? `${a}` : `${a}-${b}`)).join(',');
}

function clipText(text, maxChars) {
  if (text.length <= maxChars) return { text, clipped: false };
  return { text: `${text.slice(0, Math.max(0, maxChars - 20))}\n…[clipped]`, clipped: true };
}

function renderLines(lines, start, end) {
  const out = [];
  const lo = Math.max(1, start);
  const hi = Math.min(lines.length, end);
  for (let i = lo; i <= hi; i++) out.push(`${String(i).padStart(6)}  ${lines[i - 1]}`);
  return out.join('\n');
}

function outlineFromStarts(starts, limit = 40) {
  return starts.slice(0, limit).map((s) => `  L${s.line}  ${s.name}`).join('\n');
}

function hitRelevantToKeywords(hitLine, window, lines, keywords) {
  const lineText = lines[hitLine - 1] ?? '';
  const onDef = DEF_LINE_RE.test(lineText) && keywords.some((k) => lineText.includes(k));
  if (onDef) return 'def';
  if (window.name && keywords.some((k) => window.name === k || window.name.includes(k) || k.includes(window.name))) {
    return 'named';
  }
  return 'mention';
}

async function expandFile(root, rec, keywords = []) {
  const abs = `${root}/${rec.path}`;
  if (!existsSync(abs)) return null;
  let content;
  try {
    content = await readFile(abs, 'utf8');
  } catch {
    return null;
  }
  const lines = content.split('\n');
  const total = lines.length;
  const starts = extractSymbolStarts(lines);
  const hitLines = [...rec.hitLines].sort((a, b) => a - b);
  const windows = [];
  for (const hl of hitLines) {
    const full = hitToWindow(hl, starts, lines);
    const rel = hitRelevantToKeywords(hl, full, lines, keywords);
    if (rel === 'mention') {
      // Keep local evidence only; do not inflate unrelated enclosing symbols.
      windows.push({
        start: Math.max(1, hl - 12),
        end: Math.min(total, hl + 12),
        name: full.name,
        partial: false,
      });
    } else {
      windows.push(full);
    }
  }
  const merged = mergeWindows(windows);
  const high = rec.score >= HIGH_SCORE;
  const mode = total <= FULL_ALWAYS || (high && total <= FULL_HIGH_SCORE)
    ? 'full'
    : merged.length
      ? 'body'
      : 'outline';
  // Prefer symbol names that match query keywords when seeding neighbors.
  const preferred = starts
    .map((s) => s.name)
    .filter((name) => name && keywords.some((k) => name === k || name.includes(k) || k.includes(name)));
  const symbols = [...new Set([...preferred, ...starts.map((s) => s.name).filter(Boolean)])];
  return {
    path: rec.path,
    score: rec.score,
    kind: rec.kind,
    lines,
    total,
    starts,
    windows: merged,
    mode,
    symbols,
  };
}

async function discoverNeighbors(root, coreFiles, corePaths, keywords = []) {
  const symbolNames = [];
  for (const kw of keywords) {
    if (/^[A-Za-z_][\w]{3,}$/.test(kw) && !symbolNames.includes(kw)) symbolNames.push(kw);
  }
  for (const f of coreFiles.slice(0, 5)) {
    for (const name of f.symbols.slice(0, 12)) {
      if (name.length < 4) continue;
      if (!symbolNames.includes(name)) symbolNames.push(name);
      if (symbolNames.length >= 16) break;
    }
    if (symbolNames.length >= 16) break;
  }
  const neighHits = new Map();
  await mapPool(symbolNames, 8, async (name) => {
    const stdout = await runAsync(root, 'git', ['grep', '-nIw', '--', name]);
    for (const h of parseGrepLines(stdout).slice(0, 8)) {
      if (
        EXCLUDE.test(h.file)
        || NEIGHBOR_EXCLUDE.test(h.file)
        || TEST_PATH.test(h.file)
        || corePaths.has(h.file)
      ) continue;
      let rec = neighHits.get(h.file);
      if (!rec) {
        rec = { path: h.file, hitLines: new Set(), symbols: new Set() };
        neighHits.set(h.file, rec);
      }
      rec.hitLines.add(h.line);
      rec.symbols.add(name);
    }
  });
  const ranked = [...neighHits.values()]
    .map((r) => ({ ...r, score: r.hitLines.size + r.symbols.size * 2 }))
    .sort((a, b) => b.score - a.score)
    .slice(0, NEIGHBOR_CAP);

  const expanded = await mapPool(ranked, 8, async (rec) => {
    const abs = `${root}/${rec.path}`;
    if (!existsSync(abs)) return null;
    let content;
    try {
      content = await readFile(abs, 'utf8');
    } catch {
      return null;
    }
    const lines = content.split('\n');
    const starts = extractSymbolStarts(lines);
    const windows = mergeWindows([...rec.hitLines].map((hl) => hitToWindow(hl, starts, lines))).slice(0, 2);
    return {
      path: rec.path,
      score: rec.score,
      lines,
      total: lines.length,
      starts,
      windows,
      symbols: [...rec.symbols],
    };
  });
  return expanded.filter(Boolean);
}

function packBundle({
  keywords,
  intent,
  maxChars,
  commit,
  rankedMeta,
  core,
  neighbors,
  omitted,
}) {
  const coverBudget = Math.floor(maxChars * 0.15);
  const headerBudget = Math.floor(maxChars * 0.05);
  const neighborBudget = intent === 'locate' ? 0 : Math.floor(maxChars * 0.25);
  const coreBudget = Math.max(2000, maxChars - coverBudget - headerBudget - neighborBudget);

  const header = [];
  header.push('# fast_context');
  header.push(`query: ${keywords.join(' ')} | intent: ${intent} | commit: ${commit} | chars: 0/${maxChars}`);
  header.push('');
  header.push('## rank');
  for (const r of rankedMeta.slice(0, 40)) {
    header.push(`  ${String(Math.round(r.score)).padStart(4)}  ${r.path}           ${r.kind}`);
  }
  header.push('');

  const coreOut = ['## core'];
  const coverage = { full: [], body: [], outline: [], omitted: omitted.slice(0, 30) };
  const nextReads = [];
  let coreUsed = 0;

  for (const f of core) {
    if (coreUsed >= coreBudget) {
      coverage.omitted.push(`${f.path} (budget)`);
      continue;
    }
    const remain = coreBudget - coreUsed;

    if (f.mode === 'full') {
      const block = [`----- [FULL] ${f.path} (${f.total} lines) -----`, renderLines(f.lines, 1, f.total), ''].join('\n');
      const { text, clipped } = clipText(block, remain);
      coreOut.push(text);
      coreUsed += text.length;
      coverage.full.push(f.path);
      if (clipped) {
        nextReads.push({
          path: f.path,
          offset: Math.min(f.total, Math.max(1, Math.floor(remain / 40))),
          limit: 200,
          note: 'FULL 被字符预算截断',
        });
      }
      continue;
    }

    if (f.mode === 'outline' || !f.lines?.length) {
      const hitLines = (f.windows || []).flatMap((w) => (w.slices || [{ start: w.start, end: w.end }]));
      const parts = [`----- [OUTLINE] ${f.path} -----`, outlineFromStarts(f.starts, 40) || '  (none)'];
      if (hitLines.length && f.lines?.length) {
        parts.push('  # anchors');
        for (const s of hitLines.slice(0, 8)) parts.push(renderLines(f.lines, s.start, s.end));
      } else if (hitLines.length) {
        parts.push('  # anchors');
        for (const s of hitLines.slice(0, 8)) parts.push(`  L${s.start}`);
      }
      parts.push('');
      const { text } = clipText(parts.join('\n'), remain);
      coreOut.push(text);
      coreUsed += text.length;
      coverage.outline.push(f.path);
      nextReads.push({ path: f.path, offset: 1, limit: 80, note: '仅大纲/锚点' });
      continue;
    }

    const parts = [
      `----- [BODY] ${f.path} -----`,
      '  # outline (top symbols)',
      outlineFromStarts(f.starts, 30) || '  (none)',
      '  # windows',
    ];
    const { covered, gaps } = coveredRanges(f.windows, f.total);
    for (const w of f.windows) {
      if (w.partial && w.slices) {
        parts.push(`  L${w.start}-L${w.end}  (${w.name || 'symbol'}, partial)`);
        for (const s of w.slices) {
          parts.push(renderLines(f.lines, s.start, s.end));
          parts.push('  ...');
        }
      } else {
        parts.push(`  L${w.start}-L${w.end}${w.name ? `  (${w.name})` : ''}`);
        parts.push(renderLines(f.lines, w.start, w.end));
      }
      parts.push('');
    }
    const block = `${parts.join('\n')}\n`;
    const { text, clipped } = clipText(block, remain);
    coreOut.push(text);
    coreUsed += text.length;
    coverage.body.push({
      path: f.path,
      covered: formatRangeList(covered),
      gaps: formatRangeList(gaps),
      partial: f.windows.some((w) => w.partial) || clipped,
    });
    for (const [a, b] of gaps.slice(0, 3)) {
      if (b - a + 1 < 3) continue;
      nextReads.push({
        path: f.path,
        offset: a,
        limit: Math.min(200, b - a + 1),
        note: clipped ? '正文截断后的缺口' : '窗间缺口',
      });
    }
  }

  const neighOut = ['## neighbors'];
  let neighUsed = 0;
  let thinCount = 0;
  for (const n of neighbors) {
    if (neighUsed >= neighborBudget) {
      coverage.outline.push(n.path);
      continue;
    }
    const remain = neighborBudget - neighUsed;
    const wantThin = thinCount < NEIGHBOR_THIN_CAP && n.windows.length > 0;
    if (wantThin) {
      const sym = n.symbols[0] || n.windows[0]?.name || 'symbol';
      const parts = [`----- [THIN] ${n.path} :: ${sym} -----`];
      for (const w of n.windows.slice(0, 2)) {
        const slices = w.partial && w.slices ? w.slices : [{ start: w.start, end: w.end }];
        for (const s of slices) parts.push(renderLines(n.lines, s.start, Math.min(s.end, s.start + 40)));
      }
      parts.push('');
      const { text } = clipText(parts.join('\n'), remain);
      neighOut.push(text);
      neighUsed += text.length;
      thinCount++;
      const { covered, gaps } = coveredRanges(n.windows, n.total);
      coverage.body.push({
        path: n.path,
        covered: formatRangeList(covered),
        gaps: formatRangeList(gaps),
        partial: true,
      });
      for (const [a, b] of gaps.slice(0, 1)) {
        nextReads.push({ path: n.path, offset: a, limit: Math.min(80, b - a + 1), note: '邻居实现' });
      }
    } else {
      const block = [`----- [OUTLINE] ${n.path} -----`, outlineFromStarts(n.starts, 25) || '  (none)', ''].join('\n');
      const { text } = clipText(block, remain);
      neighOut.push(text);
      neighUsed += text.length;
      coverage.outline.push(n.path);
      if (intent === 'edit' || intent === 'explain') {
        nextReads.push({ path: n.path, offset: 1, limit: 80, note: '邻居仅大纲' });
      }
    }
  }
  if (neighOut.length === 1) neighOut.push('(none)');

  const coverOut = ['## coverage'];
  coverOut.push('FULL:');
  for (const p of coverage.full) coverOut.push(`  - ${p}`);
  if (!coverage.full.length) coverOut.push('  (none)');
  coverOut.push('BODY:');
  for (const b of coverage.body) {
    coverOut.push(`  - ${b.path}  covered: ${b.covered || '-'}  gaps: ${b.gaps || '-'}${b.partial ? '  (partial)' : ''}`);
  }
  if (!coverage.body.length) coverOut.push('  (none)');
  coverOut.push('OUTLINE:');
  for (const p of coverage.outline) coverOut.push(`  - ${p}`);
  if (!coverage.outline.length) coverOut.push('  (none)');
  coverOut.push('OMITTED:');
  for (const p of coverage.omitted) coverOut.push(`  - ${p}`);
  if (!coverage.omitted.length) coverOut.push('  (none)');
  coverOut.push('');
  coverOut.push('## next_reads');
  const uniqReads = [];
  const seenRead = new Set();
  for (const r of nextReads) {
    const key = `${r.path}@${r.offset}:${r.limit}`;
    if (seenRead.has(key)) continue;
    seenRead.add(key);
    uniqReads.push(r);
  }
  if (!uniqReads.length) coverOut.push('  (none)');
  else {
    for (const r of uniqReads.slice(0, 12)) {
      coverOut.push(`  - {path: ${r.path}, offset: ${r.offset}, limit: ${r.limit}}  # ${r.note}`);
    }
  }
  coverOut.push('');
  coverOut.push('## rules');
  coverOut.push('已覆盖路径/行段视为已读，禁止再 read。');
  coverOut.push('仅允许按 next_reads 或 coverage.gaps 补读；多缺口合并一次 read_files。');
  coverOut.push('仍不足时增大 maxChars 或收窄 keywords/pathHints 重调 fast_context，禁止无范围全文读。');

  let body = [...header, ...coreOut, '', ...neighOut, '', ...coverOut].join('\n');
  body = body.replace(/chars: 0\//, `chars: ${body.length}/`);
  if (body.length > maxChars) {
    const keepRules = coverOut.join('\n');
    const room = Math.max(0, maxChars - keepRules.length - 80);
    body = `${body.slice(0, room)}\n\n…[pack clipped to maxChars]\n\n${keepRules}`;
    body = body.replace(/chars: \d+\//, `chars: ${body.length}/`);
  }
  return body;
}

/**
 * One-shot context pack: ranked defs + neighbor thin/outlines + coverage table.
 * @returns {Promise<string>}
 */
export async function contextBundle(params = {}, root = repoRoot()) {
  const keywords = params.keywords;
  if (!Array.isArray(keywords) || keywords.length === 0) return '错误: keywords 不能为空';
  const intent = ['edit', 'explain', 'locate'].includes(params.intent) ? params.intent : 'edit';
  const pathHints = Array.isArray(params.pathHints) ? params.pathHints : [];
  const maxChars = resolveMaxChars({ ...params, intent });
  const softCoreCap = Number.isFinite(params.maxFiles)
    ? Math.max(1, Math.min(CORE_SOFT_CAP, params.maxFiles))
    : CORE_SOFT_CAP;

  const t0 = Date.now();
  const commit = shortHead(root);

  const perKw = await mapPool(keywords, Math.min(6, keywords.length), (kw) => discoverKeyword(root, kw));
  const files = new Map();
  keywords.forEach((kw, i) => {
    for (const h of perKw[i] || []) {
      let rec = files.get(h.file);
      if (!rec) {
        rec = {
          path: h.file,
          hitLines: new Set(),
          keywords: new Set(),
          defHits: 0,
          mentionOnly: true,
        };
        files.set(h.file, rec);
      }
      rec.hitLines.add(h.line);
      rec.keywords.add(kw);
      if (DEF_LINE_RE.test(h.text) && h.text.includes(kw)) {
        rec.defHits++;
        rec.mentionOnly = false;
      } else if (!/^\s*(?:\/\/|#|\/\*|\*|["'])/.test(h.text)) {
        rec.mentionOnly = false;
      }
    }
  });

  if (files.size === 0) return `无命中: ${keywords.join(' ')}`;

  const ranked = [...files.values()]
    .map((r) => scoreFile(r, pathHints))
    .sort((a, b) => b.score - a.score || a.path.localeCompare(b.path));

  const timedOut = () => Date.now() - t0 > WALL_MS;
  const locateOnly = intent === 'locate' || timedOut();

  const coreCandidates = ranked.slice(0, softCoreCap);
  const omitted = ranked.slice(softCoreCap).map((r) => `${r.path} (low score)`);

  let core = [];
  let neighbors = [];

  if (locateOnly) {
    core = (await mapPool(coreCandidates.slice(0, 8), 8, async (rec) => {
      const abs = `${root}/${rec.path}`;
      let lines = [];
      try {
        if (existsSync(abs)) lines = (await readFile(abs, 'utf8')).split('\n');
      } catch { /* ignore */ }
      const hitLines = [...rec.hitLines].sort((a, b) => a - b);
      const starts = lines.length ? extractSymbolStarts(lines) : hitLines.slice(0, 20).map((line) => ({ line, name: `hit@${line}`, indent: 0 }));
      return {
        path: rec.path,
        score: rec.score,
        kind: rec.kind,
        lines,
        total: lines.length,
        starts: starts.slice(0, 40),
        windows: hitLines.slice(0, 8).map((line) => ({
          start: line,
          end: line,
          name: null,
          partial: true,
          slices: [{ start: line, end: line }],
        })),
        mode: 'outline',
        symbols: [],
      };
    })).filter(Boolean);
  } else {
    core = (await mapPool(coreCandidates, 12, (rec) => expandFile(root, rec, keywords))).filter(Boolean);
    core.sort((a, b) => b.score - a.score);
    for (const c of core) {
      if (c.mode === 'outline' && c.windows.length) c.mode = 'body';
    }
    if (!timedOut()) {
      const corePaths = new Set(core.map((c) => c.path));
      neighbors = await discoverNeighbors(root, core, corePaths, keywords);
    }
  }

  return packBundle({
    keywords,
    intent: locateOnly && intent !== 'locate' ? 'locate' : intent,
    maxChars: intent === 'locate' || locateOnly ? Math.min(maxChars, INTENT_CHARS.locate) : maxChars,
    commit,
    rankedMeta: ranked,
    core,
    neighbors: intent === 'locate' || locateOnly ? [] : neighbors,
    omitted,
  });
}

// ---------- code_map ----------
export function codeMap(_args = {}, root = repoRoot()) {
  let files = '';
  try {
    files = execFileSync('git', ['ls-files', '*.rs', '*.ts', '*.tsx', '*.mjs'], {
      cwd: root,
      encoding: 'utf8',
      maxBuffer: 64 * 1024 * 1024,
    });
  } catch {
    files = '';
  }
  const list = files.split('\n').filter((f) => f && !EXCLUDE.test(f));
  const out = [];
  let branch = 'HEAD';
  try {
    branch = execFileSync('git', ['rev-parse', '--abbrev-ref', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
  } catch { /* ignore */ }
  out.push(`# CODEMAP — ${root.split('/').pop()} @ ${branch}`, '');
  out.push('## 文件 (行数)');
  for (const f of list) {
    if (!existsSync(`${root}/${f}`)) continue;
    const n = readFileSync(`${root}/${f}`, 'utf8').split('\n').length;
    out.push(`${String(n).padStart(6)}  ${f}`);
  }
  out.push('', '## 符号大纲');
  for (const f of list) {
    if (!existsSync(`${root}/${f}`)) continue;
    const lines = readFileSync(`${root}/${f}`, 'utf8').split('\n');
    const starts = extractSymbolStarts(lines);
    if (!starts.length) continue;
    out.push('', `### ${f}`, ...starts.map((s) => `L${s.line}  ${s.name}`));
  }
  return out.join('\n');
}

// ---------- find_symbols ----------
export async function findSymbols({ names }, root = repoRoot()) {
  if (!Array.isArray(names) || names.length === 0) return '错误: names 不能为空';
  const rev = shortHead(root);
  const sections = await mapPool(names, Math.min(8, names.length), async (name) => {
    const stdout = await runAsync(root, 'git', ['grep', '-nI', '--', `\\b${name}\\b`]);
    const lines = stdout.split('\n').filter((l) => l && !EXCLUDE.test(l.split(':')[0]));
    return `## ${name}\n` + (lines.slice(0, 40).join('\n') || '(无命中)');
  });
  return `# 符号定位 [${rev}]\n` + sections.join('\n\n');
}

export const FAST_CONTEXT_DESCRIPTION =
  '分析/修改前未知分布时必须先调用：一次打包定义体+1跳邻居+覆盖表。coverage 中 FULL/BODY.covered 禁止再读；禁止对已打包关键词再用 rg/Grep/git grep 重搜；仅按 gaps/next_reads 补读（多缺口合并 read_files）。intent: edit|explain|locate。';

export const TOOLS = [
  {
    name: 'fast_context',
    description: FAST_CONTEXT_DESCRIPTION,
    inputSchema: {
      type: 'object',
      properties: {
        keywords: { type: 'array', items: { type: 'string' }, minItems: 1, description: '关键词或符号名，建议 2–6 个' },
        intent: { type: 'string', enum: ['edit', 'explain', 'locate'], description: '默认 edit：定义体优先；explain 更宽；locate 只定位' },
        pathHints: { type: 'array', items: { type: 'string' }, description: '可选目录/文件前缀加权' },
        maxChars: { type: 'number', description: '字符预算；默认 edit=48000 explain=36000 locate=16000，硬顶 80000' },
        budget: { type: 'number', description: '兼容旧参数：行预算，近似映射为 maxChars' },
        ctx: { type: 'number', description: '兼容旧参数：命中上下文半径' },
        maxFiles: { type: 'number', description: '兼容旧参数：核心文件软顶' },
      },
      required: ['keywords'],
    },
  },
  {
    name: 'code_map',
    description: '生成整个仓库的符号地图(文件行数 + 各文件符号大纲)。用于建立整体架构认知。',
    inputSchema: { type: 'object', properties: {} },
  },
  {
    name: 'find_symbols',
    description: '并行定位多个符号在仓库中的所有出现位置(文件:行号)。只要行号不要正文时用；需要上下文用 fast_context。',
    inputSchema: {
      type: 'object',
      properties: { names: { type: 'array', items: { type: 'string' }, minItems: 1, description: '符号名列表' } },
      required: ['names'],
    },
  },
];

export async function callTool(name, args) {
  switch (name) {
    case 'fast_context': return contextBundle(args ?? {});
    case 'code_map': return codeMap(args ?? {});
    case 'find_symbols': return findSymbols(args ?? {});
    default: throw new Error(`未知工具: ${name}`);
  }
}
