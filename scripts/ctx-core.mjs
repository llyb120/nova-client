// ctx-core.mjs — 上下文检索核心逻辑 (纯模块, 无协议)
//
// 被薄封装复用:
//   scripts/alkaid-core.mjs
//   scripts/cursor-filesystem-tools.mjs
//
// 设计目标: 一次 fast_context 就拿到"可直接动手"的完整上下文，不再需要补读。
//   1. 符号闭包 + 完整单元: 命中落到最内层符号单元并输出完整单元体；自动沿
//      import/use 精确解析依赖定义并完整打包（向下），同时把调用方/引用列成
//      IMPACT 清单（向上）。绝不输出半截代码——没有 partial 概念。
//   2. 预算放不下的定义降级为 ## SIG 签名清单，模型明确知道缺什么、在哪里，
//      只有真需要其函数体时才按 path:ln 精确补读。
//   3. 检索一轮 batched rg（不可用时回退 git grep / 有界进程内扫描），符号结构与
//      import 映射来自增量索引 (scripts/ctx-index.mjs)，热路径典型 <200ms。
//   4. task 自然语言描述提取标识符 token，既补充检索词，也参与单元排序。

import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import {
  EXCLUDE,
  LEGACY_PATH,
  NOISE_PATH,
  SRC_PATH,
  cacheLocationLabel,
  cleanupNovaCodemap,
  getIndex,
  isCodeFile,
  listCodeFiles,
  mapPool,
  normalizeWorkspaceRoot,
  novaDataRoot,
  resolveRef,
  run,
  scanSource,
} from './ctx-index.mjs';

const require = createRequire(import.meta.url);

export { cleanupNovaCodemap, normalizeWorkspaceRoot, novaDataRoot, scanSource };

export function repoRoot() {
  if (process.env.CTX_ROOT) return process.env.CTX_ROOT;
  try {
    return execFileSync('git', ['rev-parse', '--show-toplevel'], { encoding: 'utf8' }).trim();
  } catch {
    return process.cwd();
  }
}

// ---------------------------------------------------------------- 预算 / 阈值

const DEFAULT_BUDGET = 600;
const MIN_BUDGET = 100;
const MAX_BUDGET = 1200;
const DEFAULT_HARD_BYTES = 32 * 1024;
const MIN_HARD_BYTES = 8 * 1024;
const MAX_HARD_BYTES = 64 * 1024;
/** 正文只使用输出预算的一部分，给契约、IMPACT、SIG 和未展开清单留出空间。 */
const SOFT_BYTES_RATIO = 0.64;
/** 每个 `@@` 行段头的固定开销，计入预算，避免多块碎片把总量顶穿。 */
const RANGE_HEADER_BYTES = 48;
const MAX_FILES = 6;
const MAX_UNITS_PER_FILE = 4;
/** 命中所在单元至少要有这么多行才算"有分析价值"，否则上浮到父单元。 */
const MIN_UNIT_LINES = 12;
/** 超过这么多行的单元视为大单元：优先取更内层的紧凑单元（仍是完整单元）。 */
const BIG_UNIT_LINES = 80;
const MAX_DEP_FILES = 4;
/** 小文件直接整给，彻底消除"还要不要补读"的疑虑。 */
const FULL_FILE_MAX = 100;
/** files 参数点名的文件整给的上限。 */
const EXPLICIT_FULL_MAX = 300;
/** 文件名命中查询词的主题文件：≤此行数尝试整给，超出则按文件顺序通读打包单元。 */
const SUBJECT_FULL_MAX = 800;
const MAX_SUBJECT_UNITS = 30;
const SUBJECT_OUTLINE_CAP = 24;
const MAX_DEPS = 8;
const MAX_DEPS_PER_FILE = 3;
const MAX_IMPACT = 20;
const MAX_OUTLINE_TOP = 8;
const MAX_OUTLINE_OTHER = 0;
const MAX_CANDIDATES = 8;
const MAX_KEYWORDS = 5;
// task 最长 300 字；中文 2–5 gram 最多 1190 个，加少量 ASCII 标识符后仍有硬上限。
const MAX_TASK_TOKENS = 1250;
const CJK_NGRAM_MIN = 2;
const CJK_NGRAM_MAX = 5;
const MAX_GRAPH_TERMS = 12;
const MAX_HIT_LINES = 6000;
const MAX_HITS_PER_FILE = 60;
const MAX_LINE_CHARS = 240;
const SEARCH_DEF_RE = /^\s*(?:(?:pub(?:\([^)]*\))?|export|async|unsafe|default|static|const|move)\s+)*(?:fn|struct|enum|trait|impl|type|class|interface|function|def|mod)\b/;
const RG_GLOBS = [
  '!**/node_modules/**', '!**/dist/**', '!**/target/**', '!**/coverage/**',
  '!**/package-lock.json', '!*.png', '!*.jpg', '!*.jpeg', '!*.gif', '!*.webp',
  '!*.ico', '!*.woff', '!*.woff2', '!*.ttf', '!*.bin',
];

const KW_WEIGHT = (n) => (n <= 40 ? 1 : n <= 200 ? 0.6 : 0.25);
let rgBinCache;
let gitAvailableCache;

// ---------------------------------------------------------------- 检索

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

async function searchInProcess(root, terms, ignoreCase, word) {
  const files = await listCodeFiles(root);
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
export async function searchText(root, terms, { ignoreCase = false, word = false } = {}) {
  const list = [...new Set(terms.map((term) => String(term ?? '')).filter(Boolean))];
  if (!list.length) return [];
  const rg = resolveRgBin();
  if (rg) {
    const args = ['-n', '--no-heading', '--color', 'never', '-F', '--max-count', String(MAX_HITS_PER_FILE)];
    if (ignoreCase) args.push('-i');
    if (word) args.push('-w');
    for (const term of list) args.push('-e', term);
    for (const glob of RG_GLOBS) args.push('--glob', glob);
    args.push('.');
    return rowsFromText(await run(root, rg, args));
  }
  if (gitSearchAvailable(root)) {
    const args = ['grep', '-nI', '--untracked'];
    if (ignoreCase) args.push('-i');
    if (word) args.push('-w');
    args.push('-F');
    for (const term of list) args.push('-e', term);
    args.push('--');
    return rowsFromText(await run(root, 'git', args));
  }
  return searchInProcess(root, list, ignoreCase, word);
}

function resolveOutputBudget(args) {
  const requested = Number(args?.maxBytes ?? args?.maxChars);
  const hardBytes = Number.isFinite(requested)
    ? Math.max(MIN_HARD_BYTES, Math.min(MAX_HARD_BYTES, Math.floor(requested)))
    : DEFAULT_HARD_BYTES;
  return { hardBytes, softBytes: Math.floor(hardBytes * SOFT_BYTES_RATIO) };
}

function scoreFilePrior(file) {
  let s = 0;
  if (!isCodeFile(file)) s -= 90;
  if (NOISE_PATH.test(file)) s -= 55;
  if (LEGACY_PATH.test(file)) s -= 60;
  if (SRC_PATH.test(file)) s += 14;
  if (/^scripts\//.test(file)) s += 10;
  if (/\.(md|json|ya?ml|toml|txt)$/i.test(file)) s -= 40;
  return s;
}

/** task 里的 ASCII 标识符与中文字符 n-gram。中文不依赖词典、停用词或固定句式。 */
function taskTokens(task) {
  const out = [];
  const seen = new Set();
  const add = (token) => {
    const t = String(token ?? '').trim();
    const low = t.toLowerCase();
    if (!t || seen.has(low) || STOP.has(low) || out.length >= MAX_TASK_TOKENS) return;
    seen.add(low);
    out.push(t);
  };
  const text = String(task ?? '');
  for (const m of text.matchAll(/[A-Za-z_$][\w$]{3,}/g)) add(m[0]);
  for (const m of text.matchAll(/[\p{Script=Han}]{2,}/gu)) {
    const chars = [...m[0]];
    for (let size = Math.min(CJK_NGRAM_MAX, chars.length); size >= CJK_NGRAM_MIN; size--) {
      for (let start = 0; start + size <= chars.length; start++) add(chars.slice(start, start + size).join(''));
    }
  }
  return out;
}

/** 先从任务预测会改到哪类关系，再从目标单元反向提取需要检索的符号。 */
function retrievalPlan(task) {
  const text = String(task ?? '').toLowerCase();
  const has = (...words) => words.some((word) => text.includes(word));
  return {
    active: Boolean(String(task ?? '').trim()),
    errors: has('错误', '失败', '异常', 'error', 'exception', 'fail', 'throw', 'catch'),
    callers: has('调用方', '兼容', 'api', '返回', 'return', 'caller', '签名'),
    tests: has('测试', '回归', 'test', 'spec'),
    config: has('配置', '设置', 'config', 'setting', 'option'),
    state: has('状态', '会话', '缓存', '并发', '锁', 'state', 'session', 'cache', 'concurr', 'lock', 'mutex'),
  };
}

function planTermsFromBodies(plan, index, bodies, existing) {
  if (!plan.active) return [];
  const blocked = new Set([...existing].map((term) => term.toLowerCase()));
  const candidates = new Map();
  for (const { file, text } of bodies) {
    const locals = new Set([...text.matchAll(LOCAL_DECL_RE)].map((match) => match[1]));
    for (const match of text.matchAll(IDENT_RE)) {
      const name = match[0];
      const low = name.toLowerCase();
      if (name.length < 3 || blocked.has(low) || STOP.has(low) || locals.has(name)) continue;
      if (match.index > 0 && text[match.index - 1] === '.') continue;
      const def = resolveRef(index, name, file);
      if (!def) continue; // 只沿可解析定义边扩图，不把普通文本 token 当关系
      const prefix = text.slice(Math.max(0, match.index - 40), match.index);
      const suffix = text.slice(match.index + name.length, match.index + name.length + 8);
      const call = /^\s*(?:<[^>]*>)?\s*\(/.test(suffix);
      let score = 12 + (call ? 12 : 0) + (/^[A-Z]/.test(name) ? 5 : 0);
      // 任务类型只调整边权，不再决定哪些符号允许进入关系图。
      if (plan.errors && /(?:throw|catch|instanceof|reject|fail|new)\s*$/i.test(prefix)) score += 24;
      if (plan.callers && call) score += 8;
      if (plan.config && /(?:Config|Settings|Options?|Policy)$/i.test(name)) score += 6;
      if (plan.state && /(?:State|Store|Session|Cache|Lock|Mutex|Queue|Manager)$/i.test(name)) score += 6;
      const current = candidates.get(name);
      if (current) {
        current.score += score;
        current.refs += 1;
      } else candidates.set(name, { name, score, refs: 1 });
    }
  }
  return [...candidates.values()]
    .sort((a, b) => b.score - a.score || b.refs - a.refs || a.name.localeCompare(b.name))
    .slice(0, MAX_GRAPH_TERMS)
    .map((entry) => entry.name);
}

// ---------------------------------------------------------------- 单元定位

/**
 * 命中行 → 输出单元。优先最内层且有分析价值的单元；大单元则回落到内层紧凑单元。
 * @returns {{ unit: any | null, chain: any[] }}
 */
function unitForHit(syms, ln) {
  const chain = syms.filter((s) => s.ln <= ln && s.end >= ln).sort((a, b) => a.depth - b.depth || a.ln - b.ln);
  const span = (s) => s.end - s.ln + 1;
  let pick = null;
  for (let i = chain.length - 1; i >= 0; i--) {
    if (span(chain[i]) >= MIN_UNIT_LINES) {
      pick = chain[i];
      break;
    }
  }
  if (!pick) pick = chain[chain.length - 1] ?? null;
  if (pick && span(pick) > BIG_UNIT_LINES) {
    for (let i = chain.length - 1; i >= 0; i--) {
      const n = span(chain[i]);
      if (n >= 4 && n <= BIG_UNIT_LINES) {
        pick = chain[i];
        break;
      }
    }
  }
  return { unit: pick, chain };
}

function unitLabel(chain, unit) {
  if (!unit) return '';
  const path = chain.filter((s) => s.depth < unit.depth).slice(-2).map((s) => s.name);
  const own = unit.kind === 'prop' || unit.kind === 'method' ? unit.name : `${unit.kind} ${unit.name}`;
  return [...path, own].join(' > ');
}

/** @param {[number, number][]} ranges */
function mergeRanges(ranges) {
  if (!ranges.length) return [];
  const sorted = [...ranges].sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  /** @type {[number, number][]} */
  const out = [];
  let [s, e] = sorted[0];
  for (let i = 1; i < sorted.length; i++) {
    const [a, b] = sorted[i];
    if (a <= e + 1) e = Math.max(e, b);
    else {
      out.push([s, e]);
      s = a;
      e = b;
    }
  }
  out.push([s, e]);
  return out;
}

const fmtRanges = (rs) => rs.map(([a, b]) => (a === b ? `${a}` : `${a}-${b}`)).join(',');

/** 大纲条目：`行号 名字`；impl 带 kind 以区分同名 struct。 */
const outlineEntry = (s) => `${s.ln} ${s.kind === 'impl' ? 'impl ' : ''}${s.name}`;

/** 读源码 + 拿符号表；索引与磁盘不一致时（刚被改过）就地重扫，保证行段永不错位。 */
function readSource(root, index, file) {
  const abs = join(root, file);
  if (!existsSync(abs)) return null;
  let text;
  try {
    text = readFileSync(abs, 'utf8');
  } catch {
    return null;
  }
  const raw = text.split('\n');
  const lines = raw.length > 1 && raw[raw.length - 1] === '' ? raw.slice(0, -1) : raw;
  const entry = index.files[file];
  const syms = entry && entry.total === lines.length ? entry.syms : scanSource(text, file).syms;
  return { lines, total: lines.length, syms };
}

// ---------------------------------------------------------------- 依赖符号

const STOP = new Set([
  'self', 'this', 'true', 'false', 'null', 'none', 'some', 'void', 'undefined', 'async', 'await',
  'const', 'let', 'var', 'function', 'return', 'export', 'import', 'from', 'default', 'class',
  'extends', 'implements', 'interface', 'type', 'enum', 'struct', 'trait', 'impl', 'pub', 'crate',
  'super', 'match', 'while', 'break', 'continue', 'else', 'catch', 'throw', 'typeof', 'instanceof',
  'string', 'number', 'boolean', 'object', 'symbol', 'bigint', 'never', 'unknown', 'any', 'array',
  'promise', 'record', 'partial', 'readonly', 'static', 'public', 'private', 'protected', 'delete',
  'error', 'result', 'option', 'vec', 'hashmap', 'value', 'data', 'text', 'name', 'path', 'file',
  'line', 'lines', 'args', 'options', 'opts', 'params', 'props', 'state', 'index', 'item', 'items',
  'json', 'utf8', 'length', 'push', 'slice', 'split', 'join', 'test', 'exec', 'clone', 'unwrap',
  'expect', 'into', 'iter', 'collect', 'format', 'println', 'console', 'process', 'require',
  'with', 'then', 'else', 'when', 'that', 'this', 'true', 'false',
]);

const IDENT_RE = /[A-Za-z_$][A-Za-z0-9_$]*/g;
const LOCAL_DECL_RE = /\b(?:const|let|var|function|fn|struct|enum|class|type|interface)\s+([A-Za-z_$][\w$]*)/g;

/**
 * 从已选单元文本里找依赖符号，用索引的 import 映射精确解析定义位置（resolveRef：
 * 同文件 > import 来源文件 > 全局唯一定义）。零额外检索开销。
 */
function collectDeps(index, unitTexts, ownedKeys, keywordSet) {
  /** @type {Map<string, { n: number, call: boolean, from: Set<string> }>} */
  const seen = new Map();
  const locals = new Set();
  for (const { file, text } of unitTexts) {
    for (const m of text.matchAll(LOCAL_DECL_RE)) locals.add(m[1]);
    for (const m of text.matchAll(IDENT_RE)) {
      const name = m[0];
      if (name.length < 4 || STOP.has(name.toLowerCase())) continue;
      const before = m.index > 0 ? text[m.index - 1] : '';
      if (before === '.') continue;
      const after = text[m.index + name.length];
      const call = after === '(' || after === '<';
      const e = seen.get(name);
      if (e) {
        e.n += 1;
        e.call = e.call || call;
        e.from.add(file);
      } else seen.set(name, { n: 1, call, from: new Set([file]) });
    }
  }
  const out = [];
  for (const [name, info] of seen) {
    if (keywordSet.has(name) || locals.has(name)) continue;
    let def = null;
    for (const f of info.from) {
      def = resolveRef(index, name, f);
      if (def) break;
    }
    if (!def || ownedKeys.has(`${def.file}:${def.ln}`)) continue;
    if (def.kind === 'mod' || def.kind === 'impl') continue;
    const size = def.end - def.ln + 1;
    let score = info.n * 3 + (info.call ? 8 : 0);
    if (size <= 40) score += 6;
    if (SRC_PATH.test(def.file) || /^scripts\//.test(def.file)) score += 3;
    out.push({ name, def, score, size });
  }
  out.sort((a, b) => b.score - a.score);
  const picked = [];
  /** @type {Map<string, number>} */
  const perFile = new Map();
  for (const d of out) {
    if (picked.length >= MAX_DEPS) break;
    const n = perFile.get(d.def.file) ?? 0;
    if (n >= MAX_DEPS_PER_FILE) continue;
    perFile.set(d.def.file, n + 1);
    picked.push(d);
  }
  return picked;
}

// ---------------------------------------------------------------- 种子符号

/**
 * keywords 与索引 defs 的名字匹配：精确(3) > 忽略大小写(2) > 名字包含关键词(1, 词长≥5)。
 * @returns {{ file: string, ln: number, end: number, kind: string, name: string, sig: string, w: number }[]}
 */
function seedDefs(index, keywords) {
  /** @type {Map<string, any>} */
  const out = new Map();
  const add = (d, name, w) => {
    const key = `${d.file}:${d.ln}`;
    const cur = out.get(key);
    if (!cur || cur.w < w) out.set(key, { ...d, name, w });
  };
  for (const kw of keywords) {
    for (const d of index.defs.get(kw) ?? []) add(d, kw, 3);
    const low = kw.toLowerCase();
    if (low === kw && kw.length < 3) continue;
    for (const [name, arr] of index.defs) {
      const nl = name.toLowerCase();
      if (nl === low) {
        if (name !== kw) for (const d of arr) add(d, name, 2);
      } else if (kw.length >= 5 && nl.includes(low)) {
        for (const d of arr) add(d, name, 1);
      }
    }
  }
  return [...out.values()];
}

// ---------------------------------------------------------------- 主流程

/**
 * fast_context: 一次调用产出可直接动手的完整多文件上下文。
 * 输出契约：EDIT/DEPS 全是完整单元或完整文件；放不下的定义进 SIG（仅签名）；
 * 调用方/引用进 IMPACT（仅行）。不存在 partial。
 * @param {{ keywords?: string[], task?: string, files?: string[], budget?: number, maxBytes?: number, maxChars?: number }} args
 */
export async function contextBundle(args = {}, root = repoRoot()) {
  const keywords = [...new Set((Array.isArray(args?.keywords) ? args.keywords : [])
    .map((k) => String(k ?? '').trim())
    .filter(Boolean))].slice(0, MAX_KEYWORDS);
  const task = String(args?.task ?? '').trim().slice(0, 300);
  const tTokens = taskTokens(task).filter((t) => !keywords.some((k) => k.toLowerCase() === t.toLowerCase()));
  const planIntent = retrievalPlan(task);
  // 编辑任务默认把调用方和测试当候选闭包；只有任务明确要求时才升为 required。
  const inferRelations = planIntent.active;
  const wantFiles = [...new Set((Array.isArray(args?.files) ? args.files : [])
    .map((f) => String(f ?? '').trim().replace(/\\/g, '/').replace(/^\.\//, ''))
    .filter(Boolean))].slice(0, 6);
  const terms = [...keywords, ...tTokens];
  if (!terms.length && !wantFiles.length) return '错误: 需要 keywords / task / files 至少其一';
  const budget = Math.max(MIN_BUDGET, Math.min(Number(args?.budget) || DEFAULT_BUDGET, MAX_BUDGET));
  const { hardBytes, softBytes } = resolveOutputBudget(args);

  const [rows0, rev0] = await Promise.all([
    terms.length ? searchText(root, terms) : Promise.resolve([]),
    run(root, 'git', ['rev-parse', '--short', 'HEAD']).then((s) => s.trim()),
  ]);
  const rev = rev0 || 'unknown';

  // 只有部分检索词零命中时，补一轮大小写不敏感检索（一次进程）
  let rows = rows0;
  const hitTerm = new Set();
  for (const r of rows) for (const k of terms) if (r.text.includes(k)) hitTerm.add(k);
  const missing = terms.filter((k) => !hitTerm.has(k));
  let looseKw = [];
  if (missing.length) {
    const extra = await searchText(root, missing, { ignoreCase: true });
    if (extra.length) {
      rows = rows.concat(extra);
      looseKw = missing.filter((k) => extra.some((r) => r.text.toLowerCase().includes(k.toLowerCase())));
    }
  }

  /** @type {Map<string, { lns: Map<number, { kws: Set<string>, text: string }>, kws: Set<string> }>} */
  const hits = new Map();
  /** @type {Map<string, number>} */
  const kwCount = new Map();
  const ingestRows = (newRows, matchTerms = terms) => {
    for (const r of newRows) {
      let e = hits.get(r.file);
      if (!e) {
        e = { lns: new Map(), kws: new Set() };
        hits.set(r.file, e);
      }
      if (!e.lns.has(r.ln) && e.lns.size >= MAX_HITS_PER_FILE) continue;
      let cell = e.lns.get(r.ln);
      if (!cell) {
        cell = { kws: new Set(), text: r.text };
        e.lns.set(r.ln, cell);
      }
      const low = r.text.toLowerCase();
      for (const k of matchTerms) {
        if (r.text.includes(k) || low.includes(k.toLowerCase())) {
          cell.kws.add(k);
          e.kws.add(k);
          kwCount.set(k, (kwCount.get(k) ?? 0) + 1);
        }
      }
    }
  };
  ingestRows(rows);

  const subjectTerms = [...keywords, ...tTokens].map((t) => t.toLowerCase()).filter((t) => t.length >= 4);
  const isSubject = (file) => {
    if (!subjectTerms.length) return false;
    const base = file.split('/').pop().toLowerCase();
    return subjectTerms.some((t) => base.includes(t));
  };

  // 先用纯搜索结果做轻量排名，只索引最可能进入 EDIT 的文件；IMPACT 仍保留全量命中行。
  const preliminary = [...hits.entries()].map(([file, e]) => {
    let score = scoreFilePrior(file) + Math.min(e.lns.size, 8) * 4;
    for (const k of e.kws) score += 30 * KW_WEIGHT(kwCount.get(k) ?? 1) * (keywords.includes(k) ? 1 : 0.5);
    if ([...e.lns.values()].some((cell) => SEARCH_DEF_RE.test(cell.text))) score += 120;
    if (isSubject(file)) score += 600;
    if (wantFiles.includes(file)) score += 500;
    return { file, score };
  }).sort((a, b) => b.score - a.score);
  const priorityFiles = [...new Set([
    ...wantFiles,
    ...preliminary.slice(0, MAX_CANDIDATES).map((r) => r.file),
  ])];
  const index = await getIndex(root, {
    priorityFiles,
    matchTerms: terms,
    mode: 'focused',
    dependencyDepth: 3,
  });

  const missedAll = keywords.filter((k) => !kwCount.has(k));
  // task-only 调用也应建立目标定义。只纳入有限数量的 ASCII 标识符，避免中文
  // n-gram 和自然语言词把 defs 全表匹配放大成二次扫描。
  const seedTerms = [...new Set([
    ...keywords,
    ...tTokens.filter((term) => /^[A-Za-z_$][\w$]{2,}$/.test(term)).slice(0, MAX_GRAPH_TERMS),
  ])];
  const seeds = seedDefs(index, seedTerms);

  // 计划驱动二次检索：先看目标定义体，再搜索错误/配置/状态符号的处理方。
  // 这样可找到“不引用目标函数、只处理其 Error/State/Config”的关键上下文。
  const seedBodies = [];
  for (const d of seeds) {
    const src = readSource(root, index, d.file);
    if (src) seedBodies.push({ file: d.file, text: src.lines.slice(d.ln - 1, d.end).join('\n') });
  }
  const plannedTerms = planTermsFromBodies(planIntent, index, seedBodies, terms);
  if (plannedTerms.length) {
    const plannedRows = await searchText(root, plannedTerms);
    terms.push(...plannedTerms);
    ingestRows(plannedRows, plannedTerms);
  }

  if (!hits.size && !wantFiles.length && !seeds.length) {
    return `# CTX @${rev}\n无命中: ${terms.join(' ')}\n提示: 换更短的符号名/字符串片段，或改用 find_symbols / grep 定位后用 read。`;
  }

  // ---- 文件排名
  const seedFiles = new Set(seeds.map((d) => d.file));
  const seedNames = [...new Set(seeds.map((d) => d.name))];
  const relationBonus = (file, e) => {
    const referencesSeed = [...e.lns.values()].some((cell) => seedNames.some((name) => cell.text.includes(name)));
    const callsSeed = [...e.lns.values()].some((cell) => seedNames.some((name) => cell.text.includes(`${name}(`)));
    let bonus = 0;
    if ((planIntent.callers || inferRelations) && callsSeed) bonus += 180;
    if ((planIntent.tests || inferRelations) && referencesSeed && NOISE_PATH.test(file)) bonus += 220;
    if (plannedTerms.some((term) => [...e.lns.values()].some((cell) => cell.text.includes(term)))) bonus += 140;
    return bonus;
  };
  // 查询词命中文件名（不含扩展名之外的 basename 子串）→ 该文件就是查询主体
  const ranked = [...hits.entries()].map(([file, e]) => {
    let s = scoreFilePrior(file);
    for (const k of e.kws) s += 30 * KW_WEIGHT(kwCount.get(k) ?? 1) * (keywords.includes(k) ? 1 : 0.5);
    s += Math.min(e.lns.size, 8) * 4;
    if (seedFiles.has(file)) s += 120;
    if (isSubject(file)) s += 600;
    if (wantFiles.includes(file)) s += 500;
    s += relationBonus(file, e);
    return { file, e, score: s };
  }).sort((a, b) => b.score - a.score);

  for (const f of wantFiles) {
    if (!hits.has(f)) {
      ranked.unshift({ file: f, e: { lns: new Map(), kws: new Set() }, score: 1000 });
    }
  }
  // 种子定义所在文件即使没进命中（名字匹配但正文命中在别处）也要成为候选
  for (const f of seedFiles) {
    if (!ranked.some((r) => r.file === f)) {
      ranked.push({ file: f, e: { lns: new Map(), kws: new Set() }, score: 100 });
    }
  }
  // 主题文件可能正文零命中（查询词只在文件名里），必须成为候选
  for (const f of Object.keys(index.files).sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)))) {
    if (!isSubject(f) || ranked.some((r) => r.file === f)) continue;
    ranked.push({ file: f, e: { lns: new Map(), kws: new Set() }, score: 550 });
  }
  // 追加后重排，保证 wantFiles > 主题文件 > 其余命中
  ranked.sort((a, b) => b.score - a.score);

  const baseCandidateLimit = MAX_CANDIDATES + Math.floor(Math.max(0, hardBytes - DEFAULT_HARD_BYTES) / 8192) * 2;
  const candidateLimit = Math.min(20, Math.max(16, baseCandidateLimit));
  const baseFileLimit = MAX_FILES + Math.floor(Math.max(0, hardBytes - DEFAULT_HARD_BYTES) / 16384) * 2;
  const fileLimit = Math.min(12, Math.max(8, baseFileLimit));
  const unitsPerFile = Math.min(8, MAX_UNITS_PER_FILE + Math.floor(Math.max(0, hardBytes - DEFAULT_HARD_BYTES) / 16384));
  let candidates = ranked.filter((r) => isCodeFile(r.file) || wantFiles.includes(r.file)).slice(0, candidateLimit);
  // 全是非代码文件时（配置/文档命中）也要展开，否则模型只拿到一堆位置还得自己去读
  if (!candidates.length) candidates = ranked.slice(0, 3);

  // ---- 读取候选文件
  /** @type {Map<string, { lines: string[], total: number, syms: any[] }>} */
  const srcs = new Map();
  await mapPool(candidates, 8, async ({ file }) => {
    const src = readSource(root, index, file);
    if (src) srcs.set(file, src);
  });

  // ---- 候选单元
  /** @type {{ file: string, ln: number, end: number, label: string, tag: string, score: number, hits: number[], kws: Set<string>, unit: any }[]} */
  const units = [];
  const fileRank = new Map(candidates.map((c, i) => [c.file, i]));
  const tLow = tTokens.map((t) => t.toLowerCase());
  for (const { file, e, score } of candidates) {
    const src = srcs.get(file);
    if (!src) continue;
    /** @type {Map<string, any>} */
    const grouped = new Map();
    for (const [ln, cell] of e.lns) {
      if (ln > src.total) continue;
      const { unit, chain } = unitForHit(src.syms, ln);
      const key = unit ? `${unit.ln}-${unit.end}` : `w${Math.floor(ln / 20)}`;
      let g = grouped.get(key);
      if (!g) {
        // 无符号边界的文件（配置/文档等）：以命中行为中心的完整窗口即"单元"
        const lo = Math.max(1, ln - 8);
        const hi = Math.min(src.total, ln + 8);
        g = {
          file,
          ln: unit ? unit.ln : lo,
          end: unit ? unit.end : hi,
          label: unit ? unitLabel(chain, unit) : '',
          unit,
          tag: 'hit',
          hits: [],
          kws: new Set(),
        };
        grouped.set(key, g);
      }
      g.hits.push(ln);
      for (const k of cell.kws) g.kws.add(k);
    }
    // 种子定义直接成为候选单元（即使该定义行没有正文命中）
    for (const d of seeds) {
      if (d.file !== file) continue;
      const key = `${d.ln}-${d.end}`;
      let g = grouped.get(key);
      if (!g) {
        const chain = src.syms
          .filter((s) => s.ln <= d.ln && s.end >= d.end)
          .sort((a, b) => a.depth - b.depth || a.ln - b.ln);
        g = {
          file,
          ln: d.ln,
          end: d.end,
          label: unitLabel(chain, chain[chain.length - 1]) || `${d.kind} ${d.name}`,
          unit: chain[chain.length - 1] ?? null,
          tag: 'hit',
          hits: [],
          kws: new Set(),
        };
        grouped.set(key, g);
      }
      g.tag = 'def';
      g.seedW = Math.max(g.seedW ?? 0, d.w);
    }
    for (const g of grouped.values()) {
      const size = g.end - g.ln + 1;
      const body = src.lines.slice(g.ln - 1, g.end).join('\n');
      const referencesSeed = seedNames.some((name) => body.includes(name));
      const callsSeed = seedNames.some((name) => body.includes(`${name}(`));
      const plannedRelation = plannedTerms.some((name) => body.includes(name));
      g.role = g.tag === 'def' ? 'target'
        : (planIntent.tests || inferRelations) && NOISE_PATH.test(file) && referencesSeed ? 'test'
          : planIntent.errors && plannedRelation ? 'handler'
            : (planIntent.callers || inferRelations) && callsSeed ? 'caller'
              : 'related';
      // caller/test 的 required 是行为类别义务，不是“所有引用都必须展开”；
      // 子模覆盖会保留每类代表，其余留在 IMPACT。
      g.required = g.role === 'target' || (g.role === 'handler' && planIntent.errors);
      let s = score * 0.35;
      for (const k of g.kws) s += 45 * KW_WEIGHT(kwCount.get(k) ?? 1) * (keywords.includes(k) ? 1 : 0.5);
      s += Math.min(g.hits.length, 6) * 8;
      if (g.tag === 'def') s += (g.seedW ?? 1) >= 3 ? 200 : (g.seedW ?? 1) === 2 ? 120 : 30;
      if (g.unit && /^(fn|method|class|type)$/.test(g.unit.kind)) s += 20;
      // task token 与单元正文的重叠度
      if (tLow.length) {
        const bodyLow = body.toLowerCase();
        for (const t of tLow) if (bodyLow.includes(t)) s += 15;
      }
      if (g.required) s += 260;
      if (size > 150) s -= (size - 150) / 8;
      if ((fileRank.get(file) ?? 9) < 2) s += 12;
      g.score = s;
      g.body = body;
      const estimatedBytes = Math.max(96, Buffer.byteLength(body, 'utf8'));
      g.estimatedBytes = estimatedBytes;
      g.utility = s / Math.max(1, estimatedBytes / 1024);
      units.push(g);
    }
  }
  // 子模覆盖：按新增闭包义务/字节选择，不让同构调用方和重复文本命中垄断预算。
  {
    const remaining = [...units];
    const ordered = [];
    const covered = new Set();
    const behavior = (unit) => {
      if (unit.role !== 'caller') return unit.role;
      if (/\b(?:try|catch)\b/.test(unit.body)) return 'caller:error';
      if (seedNames.some((name) => new RegExp(`\\breturn\\s+await\\s+${name}\\s*\\(`).test(unit.body))) return 'caller:await';
      if (seedNames.some((name) => new RegExp(`\\breturn\\s+${name}\\s*\\(`).test(unit.body))) return 'caller:return';
      if (/\bawait\b/.test(unit.body)) return 'caller:await-consume';
      return 'caller:invoke';
    };
    const features = (unit) => {
      const out = new Map();
      if (unit.role === 'target') out.set(`target:${unit.file}:${unit.ln}`, 120);
      else if (unit.role === 'handler') out.set('handler', 85);
      else if (unit.role === 'test') out.set('test', 70);
      else if (unit.role === 'caller') out.set(behavior(unit), 65);
      for (const keyword of unit.kws) out.set(`term:${keyword.toLowerCase()}`, 12);
      if (unit.role === 'related') out.set(`related:${unit.file}`, 6);
      return out;
    };
    while (remaining.length) {
      let bestIndex = 0;
      let bestValue = -Infinity;
      for (let index = 0; index < remaining.length; index += 1) {
        const unit = remaining[index];
        let gain = unit.required ? 1000 : 0;
        for (const [feature, weight] of features(unit)) if (!covered.has(feature)) gain += weight;
        if (NOISE_PATH.test(unit.file) && unit.role !== 'test') gain -= 50;
        const value = gain / Math.max(1, unit.estimatedBytes / 1024) + unit.score / 1000;
        if (value > bestValue) {
          bestValue = value;
          bestIndex = index;
        }
      }
      const [picked] = remaining.splice(bestIndex, 1);
      const pickedFeatures = features(picked);
      picked.behaviorNovel = picked.role !== 'caller'
        || [...pickedFeatures.keys()].some((feature) => feature.startsWith('caller:') && !covered.has(feature));
      ordered.push(picked);
      for (const feature of pickedFeatures.keys()) covered.add(feature);
    }
    units.splice(0, units.length, ...ordered);
  }

  // ---- 装配
  // planned 只累计代码正文的估算字节（渲染时的头/尾开销另算），避免重复计数。
  let planned = 0;
  let usedLines = 0;
  const clip = (line) => (line.length > MAX_LINE_CHARS
    ? `${line.slice(0, MAX_LINE_CHARS)}…(+${line.length - MAX_LINE_CHARS}c)`
    : line);
  const costOf = (file, ranges) => {
    const src = srcs.get(file);
    let n = 0;
    for (const [a, b] of ranges) {
      n += RANGE_HEADER_BYTES;
      for (let i = a; i <= b; i++) n += Buffer.byteLength(clip(src.lines[i - 1] ?? ''), 'utf8') + 1;
    }
    return n;
  };

  /** @type {Map<string, { full: boolean, section: string, blocks: { ranges: [number,number][], label: string, tag: string }[] }>} */
  const plan = new Map();
  /** @type {{ file: string, ln: number, sig: string }[]} 预算内放不下的定义，仅签名 */
  const sigList = [];
  const planFile = (file, section) => {
    let p = plan.get(file);
    if (!p) {
      p = { full: false, section, blocks: [] };
      plan.set(file, p);
    }
    return p;
  };
  const shownRanges = (file) => {
    const p = plan.get(file);
    if (!p) return [];
    if (p.full) return [[1, srcs.get(file).total]];
    return mergeRanges(p.blocks.flatMap((b) => b.ranges));
  };
  const covers = (file, ln) => shownRanges(file).some(([a, b]) => ln >= a && ln <= b);

  // required 闭包单元不能仅因兼容行预算被降成 SIG；仍受更高的正文字节上限和
  // 最终 hardBytes 回退保护。普通相关单元继续遵守 softBytes + 行预算。
  const requiredBytes = Math.max(softBytes, Math.floor(hardBytes * 0.86));
  const fitsBudget = (cost, lines, required = false) => (
    planned + cost <= (required ? requiredBytes : softBytes)
    && (required || usedLines + lines <= budget)
  );
  const take = (cost, lines) => {
    planned += cost;
    usedLines += lines;
  };

  const countLines = (ranges) => ranges.reduce((n, [a, b]) => n + (b - a + 1), 0);
  const pushSig = (file, ln, sig) => {
    if (!sig || sig.length < 3) return;
    if (!sigList.some((x) => x.file === file && x.ln === ln)) sigList.push({ file, ln, sig });
  };

  // 显式指定的文件优先整读，保证"点名要的东西一次到手"
  for (const f of wantFiles) {
    const src = srcs.get(f);
    if (!src || src.total > EXPLICIT_FULL_MAX) continue;
    const cost = costOf(f, [[1, src.total]]);
    if (!fitsBudget(cost, src.total)) continue;
    planFile(f, 'edit').full = true;
    take(cost, src.total);
  }

  // 主题文件（文件名命中查询词）：查询主体就是文件本身，预算优先给它。
  // 分两相：先给所有 ≤SUBJECT_FULL_MAX 的主题文件整给（小主题不被大主题挤掉），
  // 装不下的大主题再按「含命中单元优先、其余按文件顺序」通读打包（主流程一次拿全），
  // 放不下的顶层单元进 SIG，未打包的在渲染时用大纲行兜底。测试文件不当主题。
  // 已有明确 seed 时，先保证目标闭包；文件名主题扩展不能抢走目标、调用方和依赖预算。
  // 仅文件名命中、没有可解析 seed 时，保留原来的主题文件通读行为。
  const subjectList = candidates.map((c) => c.file).filter((f) => !seeds.length && isSubject(f) && !NOISE_PATH.test(f) && srcs.has(f));
  for (const f of subjectList) {
    const src = srcs.get(f);
    if (src.total > SUBJECT_FULL_MAX || plan.get(f)?.full) continue;
    const cost = costOf(f, [[1, src.total]]);
    if (fitsBudget(cost, src.total)) {
      planFile(f, 'edit').full = true;
      take(cost, src.total);
    }
  }
  for (const f of subjectList) {
    const src = srcs.get(f);
    if (plan.get(f)?.full) continue;
    const p = planFile(f, 'edit');
    const packed = (s) => p.blocks.some((b) => b.ranges.some(([a, bb]) => s.ln >= a && s.end <= bb));
    const hitEntry = hits.get(f);
    const hitLns = hitEntry ? [...hitEntry.lns.keys()] : [];
    const eligible = src.syms.filter((s) => {
      if (s.depth > 1 || (s.kind === 'const' && s.end === s.ln)) return false;
      if (/^(tests?|spec)$/i.test(s.name)) return false;
      const chain = src.syms.filter((x) => x.ln <= s.ln && x.end >= s.end && x.depth < s.depth);
      return !chain.some((x) => /^(tests?|spec)$/i.test(x.name));
    });
    const hasHit = (s) => hitLns.some((ln) => ln >= s.ln && ln <= s.end);
    eligible.sort((a, b) => (Number(hasHit(b)) - Number(hasHit(a))) || (a.ln - b.ln));
    for (const s of eligible) {
      if (p.blocks.length >= MAX_SUBJECT_UNITS) break;
      if (packed(s)) continue;
      const chain = src.syms
        .filter((x) => x.ln <= s.ln && x.end >= s.end && x.depth < s.depth)
        .sort((a, b) => a.depth - b.depth || a.ln - b.ln);
      const ranges = [[s.ln, Math.min(s.end, src.total)]];
      const cost = costOf(f, ranges);
      const lines = countLines(ranges);
      if (!fitsBudget(cost, lines)) {
        if (s.depth === 0) pushSig(f, s.ln, s.sig);
        continue;
      }
      p.blocks.push({ ranges, label: unitLabel(chain, s) || `${s.kind} ${s.name}`, tag: 'hit' });
      take(cost, lines);
    }
  }

  for (const u of units) {
    if (u.role === 'caller' && !u.required && u.behaviorNovel === false) continue;
    const src = srcs.get(u.file);
    if (!src) continue;
    const p = plan.get(u.file);
    if (p?.full) continue;
    if (plan.size >= fileLimit && !p) continue;
    if ((p?.blocks.length ?? 0) >= unitsPerFile) continue;
    if (u.hits.length && u.hits.every((ln) => covers(u.file, ln))) continue;

    // 小文件直接整给
    if (!p && src.total <= FULL_FILE_MAX && ((fileRank.get(u.file) ?? 9) < 3 || u.tag === 'def')) {
      const cost = costOf(u.file, [[1, src.total]]);
      if (fitsBudget(cost, src.total)) {
        planFile(u.file, 'edit').full = true;
        take(cost, src.total);
        continue;
      }
    }

    // 只给完整单元；放不进预算就降级到 SIG（仅真实符号），绝不截断代码
    const ranges = [[u.ln, Math.min(u.end, src.total)]];
    const cost = costOf(u.file, ranges);
    const lines = countLines(ranges);
    if (!fitsBudget(cost, lines, u.required)) {
      if (u.unit) pushSig(u.file, u.ln, u.unit.sig);
      continue;
    }
    const outputTag = u.role === 'target' ? 'def' : u.role === 'related' ? u.tag : u.role;
    planFile(u.file, 'edit').blocks.push({ ranges, label: u.label, tag: outputTag, score: u.score, required: u.required });
    take(cost, lines);
  }

  // ---- 依赖闭包（向下）：沿 import 映射精确解析，完整打包定义体
  const hitFileCount = plan.size;
  const ownedKeys = new Set();
  const unitTexts = [];
  for (const file of plan.keys()) {
    const src = srcs.get(file);
    for (const [a, b] of shownRanges(file)) unitTexts.push({ file, text: src.lines.slice(a - 1, b).join('\n') });
    for (const s of src.syms) if (covers(file, s.ln)) ownedKeys.add(`${file}:${s.ln}`);
  }
  const depQueue = collectDeps(index, unitTexts, ownedKeys, new Set([...keywords, ...tTokens]));
  const deps = [];
  const depSeen = new Set();
  const dependencyWaves = 3;
  for (let depth = 0; depth < dependencyWaves && depQueue.length; depth++) {
    const wave = depQueue.splice(0);
    const nextTexts = [];
    for (const d of wave) {
      const key = `${d.def.file}:${d.def.ln}`;
      if (depSeen.has(key) || (depth > 0 && d.def.exp === false)) continue;
      depSeen.add(key);
      deps.push({ ...d, depth });
      const src = readSource(root, index, d.def.file);
      if (src) nextTexts.push({ file: d.def.file, text: src.lines.slice(d.def.ln - 1, d.def.end).join('\n') });
    }
    if (depth + 1 < dependencyWaves && nextTexts.length) {
      for (const d of collectDeps(index, nextTexts, new Set([...ownedKeys, ...depSeen]), new Set([...keywords, ...tTokens]))) depQueue.push(d);
    }
  }
  for (const d of deps) {
    const file = d.def.file;
    if (plan.get(file)?.full) continue;
    if (!plan.has(file) && plan.size >= hitFileCount + MAX_DEP_FILES) {
      pushSig(file, d.def.ln, d.def.sig);
      continue;
    }
    let src = srcs.get(file);
    if (!src) {
      src = readSource(root, index, file);
      if (!src) continue;
      srcs.set(file, src);
    }
    if (covers(file, d.def.ln)) continue;
    const ranges = [[d.def.ln, Math.min(d.def.end, src.total)]];
    const cost = costOf(file, ranges);
    const lines = countLines(ranges);
    if (!fitsBudget(cost, lines, d.depth === 0)) {
      pushSig(file, d.def.ln, d.def.sig);
      continue;
    }
    planFile(file, 'dep').blocks.push({ ranges, label: `${d.def.kind} ${d.name}`, tag: d.depth ? 'dep2' : 'dep', score: 120 - d.depth * 30, required: d.depth === 0 });
    take(cost, lines);
  }

  // 最终硬预算也只移除完整 block/file，并把被移除定义降级到 SIG。
  const downgradeRangesToSig = (file, ranges) => {
    const src = srcs.get(file);
    if (!src) return;
    let found = false;
    for (const s of src.syms) {
      if (!s.sig || s.depth > 1) continue;
      if (!ranges.some(([a, b]) => s.ln >= a && s.end <= b)) continue;
      pushSig(file, s.ln, s.sig);
      found = true;
    }
    if (!found) {
      const line = ranges[0]?.[0];
      const sig = line ? String(src.lines[line - 1] ?? '').trim().slice(0, 120) : '';
      pushSig(file, line, sig);
    }
  };
  const dropLowestPriorityBlock = () => {
    const order = [...plan.keys()].sort((a, b) => {
      const sa = plan.get(a).section === 'dep' ? 1 : 0;
      const sb = plan.get(b).section === 'dep' ? 1 : 0;
      return sb - sa || (fileRank.get(b) ?? 99) - (fileRank.get(a) ?? 99);
    });
    for (const file of order) {
      const p = plan.get(file);
      const src = srcs.get(file);
      if (!p || !src) continue;
      if (p.full) {
        downgradeRangesToSig(file, [[1, src.total]]);
        plan.delete(file);
        return true;
      }
      const removable = p.blocks
        .map((block, index) => ({ block, index }))
        .filter(({ block }) => !block.required)
        .sort((a, b) => (a.block.score ?? 0) - (b.block.score ?? 0))[0]
        ?? p.blocks.map((block, index) => ({ block, index })).sort((a, b) => (a.block.score ?? 0) - (b.block.score ?? 0))[0];
      if (!removable) continue;
      const [block] = p.blocks.splice(removable.index, 1);
      downgradeRangesToSig(file, block.ranges);
      if (!p.blocks.length) plan.delete(file);
      return true;
    }
    return false;
  };

  let impactLimit = MAX_IMPACT;
  let compactIndex = false;
  const renderOutput = () => {
    const out = [];
    const push = (line) => out.push(line);
    const order = [...plan.keys()].sort((a, b) => (fileRank.get(a) ?? 99) - (fileRank.get(b) ?? 99));
    const head = [];
    head.push(`# CTX ${keywords.length ? `q=${keywords.join(',')}` : ''}${task ? ` task="${task.slice(0, 80)}"` : ''}${wantFiles.length ? ` files=${wantFiles.join(',')}` : ''} @${rev}`);
    let blockCount = 0;
    const renderFile = (file) => {
    const p = plan.get(file);
    const src = srcs.get(file);
    const shown = shownRanges(file);
    if (p.full) {
      push(`### ${file} (${src.total}L) FULL`);
      for (const l of src.lines) push(clip(l));
      blockCount += 1;
    } else {
      push(`### ${file} (${src.total}L) shown=${fmtRanges(shown)}`);
      p.blocks.sort((x, y) => x.ranges[0][0] - y.ranges[0][0]);
      for (const b of p.blocks) {
        blockCount += 1;
        for (const [a, bb] of b.ranges) {
          push(`@@ ${a}-${bb} ${b.label}${b.tag !== 'hit' ? ` [${b.tag}]` : ''}`);
          for (let i = a; i <= bb; i++) push(clip(src.lines[i - 1] ?? ''));
        }
      }
      // 同文件未展示的顶层符号：给一行索引，避免模型为"看看还有什么"而整读；主题文件给全大纲
      const rest = src.syms
        .filter((s) => s.depth === 0 && !covers(file, s.ln) && !(s.kind === 'const' && s.end === s.ln))
        .map(outlineEntry);
      const cap = isSubject(file) ? SUBJECT_OUTLINE_CAP : (fileRank.get(file) ?? 9) < 1 ? MAX_OUTLINE_TOP : MAX_OUTLINE_OTHER;
      if (rest.length && cap > 0) {
        const shownRest = rest.slice(0, cap);
        push(`~ ${shownRest.join(' | ')}${rest.length > cap ? ` | +${rest.length - cap}` : ''}`);
      }
    }
    push('');
  };

  const editFiles = order.filter((f) => plan.get(f).section === 'edit');
  const depFiles = order.filter((f) => plan.get(f).section === 'dep');
  if (editFiles.length) {
    push('## EDIT');
    for (const f of editFiles) renderFile(f);
  }
  if (depFiles.length) {
    push('## DEPS (依赖定义, 完整单元)');
    for (const f of depFiles) renderFile(f);
  }

  // ---- IMPACT（向上）：未展示的引用/调用行，单行清单
  const kwSet = new Set(keywords);
  const refs = [];
  let totalRefs = 0;
  for (const { file, e } of ranked) {
    for (const [ln, cell] of e.lns) {
      if (plan.has(file) && covers(file, ln)) continue;
      const isSeedRef = seedNames.length
        ? seedNames.some((n) => cell.text.includes(n))
        : [...kwSet].some((k) => cell.text.includes(k));
      if (!isSeedRef) continue;
      totalRefs += 1;
      if (refs.length < impactLimit) refs.push(`${file}:${ln} ${cell.text.trim().slice(0, 120)}`);
    }
  }
  if (refs.length) {
    push(`## IMPACT (调用方/引用清单 ${refs.length}/${totalRefs}, 仅行; 确需函数体按 path:ln 补读)`);
    for (const r of refs) push(r);
    push('');
  }
  const proof = [];
  const targetCount = units.filter((u) => u.role === 'target' && covers(u.file, u.ln)).length;
  const coveredRoleCount = (role) => new Set(units
    .filter((u) => u.role === role && covers(u.file, u.ln))
    .map((u) => u.file)).size;
  const callerCount = coveredRoleCount('caller');
  const handlerCount = coveredRoleCount('handler');
  const testCount = coveredRoleCount('test');
  const depCount = [...plan.values()].reduce((n, p) => n + (p.section === 'dep' ? p.blocks.length : 0), 0);
  proof.push(`符号关系: ${plannedTerms.length ? `已解析 ${plannedTerms.length}` : '无可解析扩展边'}`);
  proof.push(`目标定义: ${targetCount ? `已闭合 ${targetCount}` : '缺口'}`);
  proof.push(`依赖定义: ${depCount ? `已闭合 ${depCount}` : '未发现'}`);
  if (planIntent.errors) proof.push(`错误处理: ${handlerCount ? `已闭合 ${handlerCount}` : '缺口'}`);
  if (planIntent.callers) proof.push(`关键调用方: ${callerCount ? `已闭合 ${callerCount}` : '缺口'}`);
  if (planIntent.tests) proof.push(`相关测试: ${testCount ? `已闭合 ${testCount}` : '缺口'}`);
  push('## PROOF (任务闭包检查)');
  for (const item of proof) push(item);
  push('');

  if (sigList.length) {
    push('## SIG (预算内放不下或最终回退的定义, 仅签名)');
    for (const d of sigList) push(`${d.file}:${d.ln} ${d.sig}`);
    push('');
  }

  const notes = [];
  if (!compactIndex) {
    if (missedAll.length) notes.push(`未命中关键词: ${missedAll.join(' ')}`);
    if (looseKw.length) notes.push(`忽略大小写才命中: ${looseKw.join(' ')}`);
    const unexpanded = ranked.filter((r) => !plan.has(r.file)).map((r) => r.file);
    if (unexpanded.length) notes.push(`其它命中文件(未展开): ${unexpanded.slice(0, 10).join(' ')}${unexpanded.length > 10 ? ` +${unexpanded.length - 10}` : ''}`);
  }

  const body = out.join('\n');
  const shownLines = order.reduce((sum, file) => sum + shownRanges(file).reduce((n, [a, b]) => n + b - a + 1, 0), 0);
  head[0] += `  ${order.length}文件/${blockCount}块 ${shownLines}行 ${(Buffer.byteLength(body, 'utf8') / 1024).toFixed(1)}KB`;
  head.push('# 契约: 已按修改计划构建符号关系、计算任务闭包并做缺口证明；正文均为完整单元/完整文件。已展示行段禁止重读。');
  for (const n of notes) head.push(`# ${n}`);
  head.push('');

    return `${head.join('\n')}\n${body}`;
  };

  let text = renderOutput();
  while (Buffer.byteLength(text, 'utf8') > hardBytes && dropLowestPriorityBlock()) {
    text = renderOutput();
  }
  // 所有回退定义必须保留在 SIG；若索引开销仍超限，只收敛 IMPACT 行数。
  while (Buffer.byteLength(text, 'utf8') > hardBytes && impactLimit > 0) {
    impactLimit = Math.max(0, impactLimit - 5);
    text = renderOutput();
  }
  if (Buffer.byteLength(text, 'utf8') > hardBytes) {
    compactIndex = true;
    text = renderOutput();
  }
  return text;
}

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

// ---------------------------------------------------------------- 工具 schema

export const FAST_CONTEXT_DESCRIPTION =
  '未知修改分布时调用一次：按 keywords+task+files 打包完整编辑单元、import/use 依赖定义与 IMPACT 调用方清单。输出无 partial；已展示范围视为已读，SIG/IMPACT 仅在确需函数体时按 path:line 精确补读。路径和行段已知时直接 read/read_files，不要调用本工具。';

export const TOOLS = [
  {
    name: 'fast_context',
    description: FAST_CONTEXT_DESCRIPTION,
    inputSchema: {
      type: 'object',
      properties: {
        keywords: { type: 'array', items: { type: 'string' }, description: '1-5 个符号/关键词' },
        task: { type: 'string', description: '一句话任务描述（用于提词与排序），如 "给设置面板加一个开关"' },
        files: { type: 'array', items: { type: 'string' }, description: '已知必看路径（可与 keywords 同用）' },
        budget: { type: 'number', description: '代码行预算，默认 600，范围 100-1200' },
        maxBytes: { type: 'number', description: '输出硬预算，默认 32768，范围 8192-65536；仅完整文件/单元边界收敛' },
      },
    },
  },
  {
    name: 'code_map',
    description: '仓库文件清单；带 scope 给该目录顶层符号大纲。仅需全局结构时用。',
    inputSchema: {
      type: 'object',
      properties: { scope: { type: 'string', description: '目录前缀' } },
    },
  },
  {
    name: 'find_symbols',
    description: '只要定义/引用位置、不要代码正文时用（比 fast_context 轻）。',
    inputSchema: {
      type: 'object',
      properties: { names: { type: 'array', items: { type: 'string' }, minItems: 1, description: '符号名' } },
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
