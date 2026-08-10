// ctx-core.mjs — 上下文检索共享基础设施（纯模块，无协议）
//
// fast_context 的唯一实现是线上的 Rust native（nova-tools-napi, src-tauri/src/
// nova_tools_native/context.rs），不再维护 JS 镜像。本文件只保留：
//   1. 检索基础设施 searchText（batched rg → git grep → 有界进程内扫描），供
//      find_symbols 与评测脚本复用；
//   2. find_symbols / code_map 的 JS 实现（native 不可用时的降级路径）；
//   3. FAST_CONTEXT_DESCRIPTION 工具描述与 repoRoot 等公共导出。

import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import {
  EXCLUDE,
  cacheLocationLabel,
  getIndex,
  listCodeFiles,
  mapPool,
  run,
} from './ctx-index.mjs';

// esbuild's CJS output replaces import.meta with an empty shim. Nova's CodeBuddy bridge is
// bundled as CJS, so prefer Node's real CJS filename there while retaining import.meta.url in ESM.
const moduleLocation = typeof __filename === "string" ? __filename : import.meta.url;
const require = createRequire(moduleLocation);

export function repoRoot() {
  if (process.env.CTX_ROOT) return process.env.CTX_ROOT;
  try {
    return execFileSync('git', ['rev-parse', '--show-toplevel'], { encoding: 'utf8' }).trim();
  } catch {
    return process.cwd();
  }
}

// ---------------------------------------------------------------- 检索

const MAX_HIT_LINES = 6000;
const MAX_HITS_PER_FILE = 60;
const RG_GLOBS = [
  '!**/node_modules/**', '!**/dist/**', '!**/target/**', '!**/coverage/**',
  '!**/package-lock.json', '!*.png', '!*.jpg', '!*.jpeg', '!*.gif', '!*.webp',
  '!*.ico', '!*.woff', '!*.woff2', '!*.ttf', '!*.bin',
];

let rgBinCache;
let gitAvailableCache;

function normalizeRepoPath(file) {
  return String(file ?? '').replace(/\\/g, '/').replace(/^\.\//, '');
}

function parseGrepLine(line) {
  const i1 = line.indexOf(':');
  if (i1 < 0) return null;
  const i2 = line.indexOf(':', i1 + 1);
  if (i2 < 0) return null;
  const file = normalizeRepoPath(line.slice(0, i1));
  const ln = Number(line.slice(i1 + 1, i2));
  if (!file || !Number.isFinite(ln)) return null;
  return { file, ln, text: line.slice(i2 + 1) };
}

/** PATH 中的 rg 优先；桌面包可回退到可选的 @vscode/ripgrep。 */
export function resolveRgBin() {
  if (rgBinCache !== undefined) return rgBinCache;
  try {
    execFileSync('rg', ['--version'], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'], timeout: 1500 });
    return (rgBinCache = 'rg');
  } catch { /* try packaged rg */ }
  try {
    const rgPath = require('@vscode/ripgrep')?.rgPath;
    if (rgPath && existsSync(rgPath)) return (rgBinCache = rgPath);
  } catch { /* optional dependency */ }
  return (rgBinCache = null);
}

function gitSearchAvailable(root) {
  if (gitAvailableCache === false) return false;
  try {
    const inside = execFileSync('git', ['rev-parse', '--is-inside-work-tree'], {
      cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'], timeout: 1500,
    }).trim();
    if (inside === 'true') return (gitAvailableCache = true);
  } catch { /* non-git workspace */ }
  return false;
}

function rowsFromText(text) {
  const rows = [];
  for (const line of String(text ?? '').split('\n')) {
    if (!line || rows.length >= MAX_HIT_LINES) continue;
    const p = parseGrepLine(line);
    if (p && !EXCLUDE.test(p.file)) rows.push(p);
  }
  return rows.sort((a, b) => Buffer.compare(Buffer.from(a.file), Buffer.from(b.file)) || a.ln - b.ln || Buffer.compare(Buffer.from(a.text), Buffer.from(b.text)));
}

async function searchInProcess(root, terms, ignoreCase, word, scopedFiles = []) {
  const files = scopedFiles.length ? scopedFiles : await listCodeFiles(root);
  const needles = terms.map((term) => ignoreCase ? term.toLowerCase() : term);
  const wordRes = word
    ? terms.map((term) => new RegExp(`\\b${term.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b`, ignoreCase ? 'i' : ''))
    : null;
  const parts = await mapPool(files, 8, async (file) => {
    let text;
    try { text = readFileSync(join(root, file), 'utf8'); } catch { return []; }
    const out = [];
    const lines = text.split('\n');
    for (let i = 0; i < lines.length && out.length < MAX_HITS_PER_FILE; i++) {
      const hay = ignoreCase ? lines[i].toLowerCase() : lines[i];
      const matched = word ? wordRes.some((re) => re.test(lines[i])) : needles.some((n) => hay.includes(n));
      if (matched) out.push({ file, ln: i + 1, text: lines[i] });
    }
    return out;
  });
  return parts.flat()
    .sort((a, b) => Buffer.compare(Buffer.from(a.file), Buffer.from(b.file)) || a.ln - b.ln || Buffer.compare(Buffer.from(a.text), Buffer.from(b.text)))
    .slice(0, MAX_HIT_LINES);
}

/** 统一仓库文本检索：单次批量 rg → git grep → 有界进程内扫描。 */
export async function searchText(root, terms, { ignoreCase = false, word = false, files = [] } = {}) {
  const seen = new Set();
  const list = terms
    .map((term) => String(term ?? ''))
    .filter((term) => {
      if (!term) return false;
      const key = ignoreCase ? term.toLowerCase() : term;
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    });
  if (!list.length) return [];
  const scopedFiles = [...new Set(files
    .map((file) => String(file ?? '').replace(/\\/g, '/').replace(/^\.\//, ''))
    .filter(Boolean))];
  // 分块限定路径，避免 Windows 命令行上限；不因候选多而退化回全仓搜索。
  if (scopedFiles.length > 128) {
    const chunks = [];
    for (let start = 0; start < scopedFiles.length; start += 128) chunks.push(scopedFiles.slice(start, start + 128));
    const parts = await mapPool(chunks, 4, (chunk) => searchText(root, list, { ignoreCase, word, files: chunk }));
    return parts.flat()
      .sort((a, b) => Buffer.compare(Buffer.from(a.file), Buffer.from(b.file)) || a.ln - b.ln || Buffer.compare(Buffer.from(a.text), Buffer.from(b.text)))
      .slice(0, MAX_HIT_LINES);
  }
  const rg = resolveRgBin();
  if (rg) {
    const args = ['-n', '--with-filename', '--no-heading', '--color', 'never', '-F', '--max-count', String(MAX_HITS_PER_FILE)];
    if (ignoreCase) args.push('-i');
    if (word) args.push('-w');
    for (const term of list) args.push('-e', term);
    for (const glob of RG_GLOBS) args.push('--glob', glob);
    args.push(...(scopedFiles.length ? scopedFiles : ['.']));
    return rowsFromText(await run(root, rg, args));
  }
  if (gitSearchAvailable(root)) {
    const args = ['grep', '-nI', '--untracked'];
    if (ignoreCase) args.push('-i');
    if (word) args.push('-w');
    args.push('-F');
    for (const term of list) args.push('-e', term);
    args.push('--', ...scopedFiles);
    return rowsFromText(await run(root, 'git', args));
  }
  return searchInProcess(root, list, ignoreCase, word, scopedFiles);
}

/** 大纲条目：`行号 名字`；impl 带 kind 以区分同名 struct。 */
const outlineEntry = (s) => `${s.ln} ${s.kind === 'impl' ? 'impl ' : ''}${s.name}`;

// ---------------------------------------------------------------- code_map

export async function codeMap(args = {}, root = repoRoot()) {
  const index = await getIndex(root);
  const scope = String(args?.scope ?? '').trim().replace(/\\/g, '/').replace(/^\.\//, '');
  const files = Object.keys(index.files).filter((f) => !scope || f.startsWith(scope)).sort();
  if (!files.length) return `# CODEMAP\n无匹配文件${scope ? `: ${scope}` : ''}`;
  const [rev, br] = await Promise.all([
    run(root, 'git', ['rev-parse', '--short', 'HEAD']).then((s) => s.trim()),
    run(root, 'git', ['rev-parse', '--abbrev-ref', 'HEAD']).then((s) => s.trim()),
  ]);
  const out = [];
  const repoName = root.replace(/\\/g, '/').split('/').pop();
  out.push(`# CODEMAP ${repoName} @${br} ${rev}  ${files.length} files  cache: ${cacheLocationLabel(root)}`);
  if (scope) {
    out.push(`# scope=${scope}  (每个文件: 行数 + 顶层符号)`);
    for (const f of files) {
      const e = index.files[f];
      const syms = e.syms.filter((s) => s.depth === 0).map(outlineEntry);
      out.push(`## ${f} (${e.total}L)`);
      if (syms.length) out.push(syms.join(' | '));
    }
  } else {
    out.push('# 行数 符号数 文件  (要看符号大纲请带 scope=<目录前缀>)');
    for (const f of files) {
      const e = index.files[f];
      const n = e.syms.filter((s) => s.depth === 0).length;
      out.push(`${String(e.total).padStart(6)} ${String(n).padStart(4)}  ${f}`);
    }
  }
  return out.join('\n');
}

// ---------------------------------------------------------------- find_symbols

export async function findSymbols({ names } = {}, root = repoRoot()) {
  const list = [...new Set((Array.isArray(names) ? names : []).map((n) => String(n ?? '').trim()).filter(Boolean))].slice(0, 12);
  if (!list.length) return '错误: names 不能为空';
  const rows = await searchText(root, list, { word: true });
  const index = await getIndex(root, {
    priorityFiles: [...new Set(rows.map((r) => r.file))],
    matchTerms: list,
    mode: 'focused',
    dependencyDepth: 0,
  });
  /** @type {Map<string, { file: string, ln: number, text: string }[]>} */
  const refs = new Map();
  for (const p of rows) {
    for (const n of list) {
      if (!new RegExp(`\\b${n.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b`).test(p.text)) continue;
      const arr = refs.get(n) ?? [];
      if (arr.length < 400) arr.push({ file: p.file, ln: p.ln, text: p.text });
      refs.set(n, arr);
    }
  }
  const out = [`# 符号定位 @${(await run(root, 'git', ['rev-parse', '--short', 'HEAD'])).trim()}`];
  for (const n of list) {
    const defs = index.defs.get(n) ?? [];
    const hits = refs.get(n) ?? [];
    out.push('', `## ${n}  defs=${defs.length} refs=${hits.length}`);
    for (const d of defs.slice(0, 6)) out.push(`DEF ${d.file}:${d.ln}-${d.end} ${d.sig}`);
    const rest = hits.filter((h) => !defs.some((d) => d.file === h.file && d.ln === h.ln));
    for (const h of rest.slice(0, 24)) out.push(`    ${h.file}:${h.ln} ${h.text.trim().slice(0, 110)}`);
    if (rest.length > 24) out.push(`    … +${rest.length - 24}`);
    if (!defs.length && !hits.length) out.push('(无命中)');
  }
  return out.join('\n');
}

// ---------------------------------------------------------------- 工具描述

export const FAST_CONTEXT_DESCRIPTION =
  '任务涉及跨文件查找或修改（含分析要改哪里）、或需要阅读多个文件正文来理解/规划改动时，先调用一次：按 keywords+task+files 打包完整编辑单元、import/use 依赖定义与 IMPACT 调用方清单，一次调用通常替代 5–10 轮 rg+read 往返，比自行 rg/grep 往返更省 token。目标路径和行段都已明确且只需少量行段时直接 read；只需符号的定义/引用行号时用 find_symbols；已定位但仍需阅读正文的多个文件，通过 files 传入一次打包，不要逐个 read。默认只传 keywords/task/files；任务里点名了某个工具/符号（如要改某个函数）时也不要用 find_symbols 代替本工具——find_symbols 只给行号不给正文；调用后不要再用 rg/git grep 重复检索同一批关键词，已展示范围视为已读。返回 CTX MISS 时按输出中的 next 提示修正符号名或用 files 指定入口文件重试一次，不要直接退回 rg/grep 逐个搜索。';
