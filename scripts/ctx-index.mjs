// ctx-index.mjs — 仓库符号索引 (纯 JS 扫描, 无 rg 子进程) + 增量缓存
//
// 设计目标: fast_context / code_map / find_symbols 共用一份索引。
//   - 符号边界用括号配对算真实 end 行 (而非"下一个符号起始 -1")，并保留嵌套层级，
//     使命中可落到最内层单元 (方法/闭包) 而不是整个 impl/class。
//   - 全程无子进程 (仅列文件用一次 git ls-files)，扫描在进程内完成。
//   - 缓存按 size+mtime 逐文件失效，冷启动全量扫描亦在百毫秒级。

import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  statSync,
  unlinkSync,
  writeFileSync,
} from 'node:fs';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { promisify } from 'node:util';

const execFileAsync = promisify(execFile);

/** 花括号系语言用同一个块扫描器；Python 走缩进扫描；其余扩展名仍可被检索，只是没有符号边界。 */
const BRACE_EXT = 'rs|ts|tsx|mts|cts|js|jsx|mjs|cjs|go|java|kt|kts|swift|c|cc|cpp|cxx|h|hh|hpp|hxx|cs|php|scala|dart|m|mm|zig|vue|svelte';
const INDENT_EXT = 'py|pyi';
export const CODE_EXT_RE = new RegExp(`\\.(?:${BRACE_EXT}|${INDENT_EXT})$`, 'i');
export const CODE_GLOBS = [
  ...`${BRACE_EXT}|${INDENT_EXT}`.split('|').map((e) => `*.${e}`),
];
export const EXCLUDE = /src-tauri\/target|node_modules|package-lock|\.png|(^|\/)dist\/|(^|\/)coverage\/|\.min\.js$|\.lock$|\.generated\./;
export const NOISE_PATH = /\.(test|spec)\.[^.]+$|\/__tests__\/|\/tests?\//i;
export const SRC_PATH = /^(src\/|src-tauri\/src\/)/;
export const LEGACY_PATH = /scripts\/legacy-context/;

const RUN_TIMEOUT_MS = 10_000;
const INDEX_CONCURRENCY = 8;
const MAX_INDEX_FILE_BYTES = 2 * 1024 * 1024;
const MAX_WALK_FILES = 8000;
const MAX_SCAN_PER_CALL = 1500;
const SIG_MAX_CHARS = 120;

// Keep in lockstep with native context.rs CACHE_VERSION. Shared versioning prevents one backend
// from ranking stale symbols after scanner/query changes while the other rebuilt its own cache.
export const INDEX_CACHE_VERSION = 14;
const INDEX_CACHE_NAME = 'cache.json';
const INDEX_DIR_NAME = 'codemap';
const INDEX_MAX_BYTES = 20 * 1024 * 1024;
const INDEX_MAX_WORKSPACE_BUCKETS = 40;

export async function run(root, cmd, args, timeout = RUN_TIMEOUT_MS) {
  try {
    const { stdout } = await execFileAsync(cmd, args, {
      cwd: root,
      encoding: 'utf8',
      maxBuffer: 64 * 1024 * 1024,
      timeout,
    });
    return stdout ?? '';
  } catch (e) {
    return e.stdout?.toString?.() ?? e.stdout ?? '';
  }
}

export async function mapPool(items, concurrency, fn) {
  const list = [...items];
  const out = new Array(list.length);
  let cursor = 0;
  const workers = Array.from({ length: Math.min(concurrency, Math.max(list.length, 1)) }, async () => {
    while (cursor < list.length) {
      const i = cursor++;
      out[i] = await fn(list[i], i);
    }
  });
  await Promise.all(workers);
  return out;
}

// ---------------------------------------------------------------- 源码扫描

/**
 * 去掉字符串/注释后逐行留下"代码骨架"，同时记录每行起止的花括号深度。
 * 处理 JS 模板串 `${}` 嵌套、Rust 生命周期 `'a`、Rust 原始串 r#".."#。
 */
export function stripAndDepth(lines, multilineStrings = false) {
  /** @type {string[]} */
  const code = [];
  /** @type {number[]} */
  const depthStart = [];
  /** @type {number[]} */
  const depthAfter = [];
  let depth = 0;
  let state = 'code';
  let rawHashes = 0;
  /** @type {number[]} 模板串 `${` 进入时的深度栈 */
  const tmplStack = [];
  /** 正则字面量只能出现在这些字符之后（否则 `/` 是除号）。 */
  // 不含 `<`/`>`：JSX 的 `</div>` 会被误判成正则起始。
  const REGEX_PREV = new Set(['', '(', ',', '=', ':', '[', '!', '&', '|', '?', '{', ';', '+', '*', '%', '~', '^']);
  const regexAllowed = (out) => {
    const t = out.replace(/\s+$/, '');
    if (!t) return true;
    const last = t[t.length - 1];
    if (REGEX_PREV.has(last)) return true;
    return /\b(?:return|case|typeof|instanceof|in|of|do|else|yield|await|new)$/.test(t);
  };

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    depthStart.push(depth);
    let out = '';
    for (let j = 0; j < line.length; j++) {
      const c = line[j];
      const n = line[j + 1];
      if (state === 'block') {
        if (c === '*' && n === '/') {
          state = 'code';
          j++;
        }
        continue;
      }
      if (state === 'dq' || state === 'sq') {
        if (c === '\\') j++;
        else if ((state === 'dq' && c === '"') || (state === 'sq' && c === "'")) state = 'code';
        continue;
      }
      if (state === 'raw') {
        if (c === '"') {
          let k = 0;
          while (k < rawHashes && line[j + 1 + k] === '#') k++;
          if (k === rawHashes) {
            j += rawHashes;
            state = 'code';
          }
        }
        continue;
      }
      if (state === 'tmpl') {
        if (c === '\\') j++;
        else if (c === '`') state = 'code';
        else if (c === '$' && n === '{') {
          tmplStack.push(depth);
          depth++;
          state = 'code';
          j++;
        }
        continue;
      }
      // state === 'code'
      if (c === '/' && n === '/') break;
      if (c === '/' && n === '*') {
        state = 'block';
        j++;
        continue;
      }
      if (c === '/' && regexAllowed(out)) {
        // 正则字面量：整体丢弃，避免其中的 `/` `{` `"` 破坏状态与深度
        let k = j + 1;
        let cls = false;
        let closed = false;
        for (; k < line.length; k++) {
          const rc = line[k];
          if (rc === '\\') {
            k++;
            continue;
          }
          if (cls) {
            if (rc === ']') cls = false;
            continue;
          }
          if (rc === '[') cls = true;
          else if (rc === '/') {
            closed = true;
            break;
          }
        }
        if (closed) {
          while (k + 1 < line.length && /[a-z]/.test(line[k + 1])) k++;
          j = k;
          continue;
        }
      }
      if (c === '"') {
        state = 'dq';
        continue;
      }
      if (c === '`') {
        state = 'tmpl';
        continue;
      }
      if (c === 'r' && (n === '"' || n === '#') && !/[A-Za-z0-9_]/.test(line[j - 1] ?? ' ')) {
        let k = 0;
        while (line[j + 1 + k] === '#') k++;
        if (line[j + 1 + k] === '"') {
          rawHashes = k;
          state = 'raw';
          j += k + 1;
          continue;
        }
      }
      if (c === "'") {
        // Rust 生命周期 (`'a`) 不是字符串; 只有 'x' / '\x' 形式才当字符字面量吞掉
        const m = /^'(\\.|[^\\'])'/.exec(line.slice(j));
        if (m) {
          j += m[0].length - 1;
          continue;
        }
        out += c;
        continue;
      }
      if (c === '{') {
        depth++;
        out += c;
        continue;
      }
      if (c === '}') {
        depth = Math.max(0, depth - 1);
        if (tmplStack.length && depth === tmplStack[tmplStack.length - 1]) {
          tmplStack.pop();
          state = 'tmpl';
          continue;
        }
        out += c;
        continue;
      }
      out += c;
    }
    if (state === 'dq' || state === 'sq') {
      // Rust 普通串可含裸换行；JS 串不能跨行，未闭合即视为扫描失误并复位。
      // 行尾奇数个反斜杠 = 显式续行，两种语言都保留状态。
      const m = /\\+$/.exec(line);
      const cont = multilineStrings || (m && m[0].length % 2 === 1);
      if (!cont) state = 'code';
    }
    code.push(out);
    depthAfter.push(depth);
  }
  return { code, depthStart, depthAfter };
}

const DECLS = [
  // Rust
  [/^(?:pub(?:\([^)]*\))?\s+)?(?:default\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?(?:extern\s+(?:"[^"]*"\s+)?)?fn\s+([A-Za-z_]\w*)/, 'fn'],
  [/^(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|trait|union)\s+([A-Za-z_]\w*)/, 'type'],
  [/^(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_]\w*)/, 'mod'],
  [/^impl(?:\s*<[^>]*>)?\s+(?:[\w:]+\s+for\s+)?([A-Za-z_][\w:]*)/, 'impl'],
  [/^macro_rules!\s+([A-Za-z_]\w*)/, 'macro'],
  // TS / JS
  [/^(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][\w$]*)/, 'class'],
  [/^(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:async\s+)?function\s*\*?\s*([A-Za-z_$][\w$]*)/, 'fn'],
  [/^(?:export\s+)?(?:declare\s+)?(?:interface|namespace)\s+([A-Za-z_$][\w$]*)/, 'type'],
  [/^(?:export\s+)?(?:declare\s+)?(?:const\s+)?enum\s+([A-Za-z_$][\w$]*)/, 'type'],
  [/^(?:export\s+)?(?:declare\s+)?type\s+([A-Za-z_$][\w$]*)/, 'type'],
  [/^(?:export\s+)?(?:pub(?:\([^)]*\))?\s+)?(?:declare\s+)?(?:const|let|var|static)\s+([A-Za-z_$][\w$]*)/, 'const'],
];

/** 方法 / 对象字面量属性: 只在缩进层 (depth>=1) 识别，且必须排除控制流。 */
const METHOD_RE = /^(?:(?:public|private|protected|readonly|static|async|get|set|override|abstract)\s+)*\*?\s*([A-Za-z_$][\w$]*)\s*(?:<[^>]*>)?\s*\(/;
const PROP_BLOCK_RE = /^([A-Za-z_$][\w$]*)\s*:\s*(?:async\s*)?(?:function\b|\(|\{|$)/;
const CTRL = new Set([
  'if', 'else', 'for', 'while', 'switch', 'case', 'catch', 'try', 'do', 'return', 'match', 'loop',
  'function', 'new', 'typeof', 'await', 'yield', 'throw', 'with', 'in', 'of', 'as', 'is', 'let',
  'const', 'var', 'import', 'export', 'require', 'super', 'this', 'self', 'and', 'or', 'not',
]);

function blockEnd(i, d, code, depthAfter, limit) {
  const self = code[i].trimEnd();
  if (depthAfter[i] <= d && /[;,]\s*$/.test(self)) return i;
  let open = -1;
  const lookahead = Math.min(limit, i + 14);
  for (let j = i; j < lookahead; j++) {
    if (depthAfter[j] > d) {
      open = j;
      break;
    }
    if (j > i) {
      const t = code[j].trimEnd();
      if (/[;,]\s*$/.test(t)) return j;
      if (t.trim() === '') return j - 1;
    }
  }
  if (open < 0) return i;
  for (let j = open; j < limit; j++) if (depthAfter[j] <= d) return j;
  return limit - 1;
}

const PY_DECL_RE = /^([ \t]*)(?:async[ \t]+)?(def|class)[ \t]+([A-Za-z_]\w*)/;

// ---------------------------------------------------------------- import/use 提取

/**
 * 从原始文本提取具名导入（import/use），供跨文件依赖精确解析。
 * 输入是未剥离的源码：import 语句的模块指定符是字符串字面量，stripped code 里拿不到。
 * 行首锚定规避注释里的假命中；解析不出的（bare specifier、外部 crate）由解析阶段跳过。
 * @returns {{ name: string, from: string }[]} name=本地名, from=原始指定符（Rust 为含符号名的完整 :: 路径）
 */
function extractImports(text, file) {
  /** @type {{ name: string, from: string }[]} */
  const out = [];
  if (/\.pyi?$/i.test(file)) return out;
  if (/\.rs$/.test(file)) {
    const USE_RE = /^\s*(?:pub\s+)?use\s+([^;]+);/gm;
    for (const m of text.matchAll(USE_RE)) {
      let base = m[1].trim();
      /** @type {string[]} */
      let names = [];
      const g = /^(.*)::\{([^}]*)\}$/.exec(base);
      if (g) {
        base = g[1];
        names = g[2].split(',').map((s) => s.trim()).filter(Boolean);
      } else {
        const segs = base.split('::');
        names = [segs.pop()];
        base = segs.join('::');
      }
      for (const n of names) {
        const as = /^(\w+)\s+as\s+(\w+)$/.exec(n);
        const orig = as ? as[1] : n;
        if (orig === 'self' || orig === 'crate' || orig === 'super') continue;
        out.push({ name: as ? as[2] : orig, from: base ? `${base}::${orig}` : orig });
      }
    }
    return out;
  }
  // JS/TS/Vue/Svelte: import Default, { a, b as c, type T } from '...'；含 re-export。
  const IMPORT_RE = /^\s*import\s+(?:type\s+)?(?!\()([\s\S]*?)\s+from\s*['"]([^'"]+)['"]/gm;
  const REEXPORT_RE = /^\s*export\s+(?:type\s+)?\{([^}]*)\}\s*from\s*['"]([^'"]+)['"]/gm;
  const pushNamed = (body, spec) => {
    for (let part of body.split(',')) {
      part = part.trim().replace(/^type\s+/, '');
      if (!part) continue;
      const as = /^([A-Za-z_$][\w$]*)\s+as\s+([A-Za-z_$][\w$]*)$/.exec(part);
      const name = as ? as[2] : part;
      if (/^[A-Za-z_$][\w$]*$/.test(name)) out.push({ name, from: spec });
    }
  };
  for (const m of text.matchAll(IMPORT_RE)) {
    let rest = m[1];
    const spec = m[2];
    const ns = /\*\s+as\s+([A-Za-z_$][\w$]*)/.exec(rest);
    if (ns) {
      out.push({ name: ns[1], from: spec });
      rest = rest.replace(ns[0], '');
    }
    const braces = /\{([^}]*)\}/.exec(rest);
    if (braces) {
      pushNamed(braces[1], spec);
      rest = rest.replace(braces[0], '');
    }
    const def = /([A-Za-z_$][\w$]*)/.exec(rest.replace(/,/g, ' '));
    if (def) out.push({ name: def[1], from: spec });
  }
  for (const m of text.matchAll(REEXPORT_RE)) pushNamed(m[1], m[2]);
  return out;
}

/** Python 按缩进定界：符号体延伸到下一条缩进不大于自身的非空非注释行之前。 */
function scanPython(lines) {
  const indentOf = (s) => s.length - s.replace(/^[ \t]*/, '').length;
  const blank = lines.map((l) => l.trim() === '' || l.trim().startsWith('#'));
  const syms = [];
  for (let i = 0; i < lines.length; i++) {
    const m = PY_DECL_RE.exec(lines[i]);
    if (!m) continue;
    const ind = m[1].length;
    let end = i;
    for (let j = i + 1; j < lines.length; j++) {
      if (blank[j]) continue;
      if (indentOf(lines[j]) <= ind) break;
      end = j;
    }
    syms.push({
      ln: i + 1,
      end: end + 1,
      depth: ind === 0 ? 0 : 1,
      kind: m[2] === 'class' ? 'class' : ind === 0 ? 'fn' : 'method',
      name: m[3],
      sig: lines[i].trim().replace(/\s+/g, ' ').slice(0, SIG_MAX_CHARS),
      exp: !m[3].startsWith('_'),
    });
  }
  return syms;
}

/**
 * 扫描单个源文件。
 * @returns {{ total: number, syms: { ln: number, end: number, depth: number, kind: string, name: string, sig: string, exp: boolean }[], imports: { name: string, from: string }[] }}
 */
export function scanSource(text, file = '') {
  const raw = text.split('\n');
  const lines = raw.length > 1 && raw[raw.length - 1] === '' ? raw.slice(0, -1) : raw;
  const total = lines.length;
  if (/\.pyi?$/i.test(file)) return { total, syms: scanPython(lines), imports: [] };
  const { code, depthStart, depthAfter } = stripAndDepth(lines, /\.rs$/.test(file));
  const syms = [];
  for (let i = 0; i < total; i++) {
    const d = depthStart[i];
    if (d > 2) continue;
    const t = code[i].replace(/^\s+/, '');
    if (!t || t.length < 3) continue;
    let name = '';
    let kind = '';
    for (const [re, k] of DECLS) {
      const m = re.exec(t);
      if (m) {
        name = m[1];
        kind = k;
        break;
      }
    }
    if (!name && d >= 1) {
      const mm = METHOD_RE.exec(t);
      if (mm && !CTRL.has(mm[1])) {
        name = mm[1];
        kind = 'method';
      } else {
        const mp = PROP_BLOCK_RE.exec(t);
        if (mp && !CTRL.has(mp[1])) {
          name = mp[1];
          kind = 'prop';
        }
      }
    }
    if (!name) continue;
    const end = blockEnd(i, d, code, depthAfter, total);
    if (end < i) continue;
    // prop/method 单行 (无块体) 无分析价值
    if ((kind === 'prop' || kind === 'method') && end === i && !/[({]\s*$/.test(code[i].trimEnd())) continue;
    const sig = lines[i].trim().replace(/\s+/g, ' ').slice(0, SIG_MAX_CHARS);
    syms.push({
      ln: i + 1,
      end: end + 1,
      depth: d,
      kind,
      name,
      sig,
      // 是否对外可见：跨文件依赖解析只认导出符号，避免把别处的局部同名量当成定义
      exp: /^(?:export\b|pub\b)/.test(t),
    });
  }
  return { total, syms, imports: extractImports(text, file) };
}

/** 命中行所在的最内层单元。 */
export function innermostUnit(syms, ln) {
  let best = null;
  for (const s of syms) {
    if (s.ln > ln) break;
    if (s.end < ln) continue;
    if (!best || s.depth > best.depth || (s.depth === best.depth && s.ln > best.ln)) best = s;
  }
  return best;
}

// ---------------------------------------------------------------- 缓存位置

/** 用户数据目录：NOVA_DATA_DIR，缺省 ~/.nova（开发构建由宿主注入为 ~/.novadev）。 */
export function novaDataRoot(home = homedir(), env = process.env) {
  return env.NOVA_DATA_DIR || join(home, '.nova');
}

/** 规范化工作区根路径，保证多 worktree / 大小写差异下 key 稳定。 */
export function normalizeWorkspaceRoot(root) {
  let p = resolve(root || '.').replace(/\\/g, '/');
  if (process.platform === 'win32') p = p.toLowerCase();
  if (p.length > 1 && p.endsWith('/')) p = p.slice(0, -1);
  return p;
}

function workspaceCacheKey(root) {
  return createHash('sha256').update(normalizeWorkspaceRoot(root)).digest('hex').slice(0, 16);
}

function indexRootDir(home = homedir(), env = process.env) {
  return join(novaDataRoot(home, env), INDEX_DIR_NAME);
}

function indexCacheDir(root, home = homedir(), env = process.env) {
  return join(indexRootDir(home, env), workspaceCacheKey(root));
}

function indexCachePath(root, home = homedir(), env = process.env) {
  return join(indexCacheDir(root, home, env), INDEX_CACHE_NAME);
}

export function cacheLocationLabel(root) {
  return `$NOVA_DATA_DIR/${INDEX_DIR_NAME}/${workspaceCacheKey(root)}/${INDEX_CACHE_NAME}`;
}

function safeRm(path, removed, label) {
  try {
    rmSync(path, { recursive: true, force: true });
    removed.push(label || path);
  } catch {
    /* ignore */
  }
}

/** 清理仓库内旧版 .nova/codemap* 遗留（缓存已迁到用户数据目录）。 */
function cleanupLegacyRepoNova(root, removed) {
  const legacy = join(root, '.nova');
  if (!existsSync(legacy)) return;
  let entries = [];
  try {
    entries = readdirSync(legacy);
  } catch {
    return;
  }
  for (const name of entries) {
    if (!/^codemap/i.test(name)) continue;
    safeRm(join(legacy, name), removed, `legacy:${name}`);
  }
  try {
    if (readdirSync(legacy).length === 0) safeRm(legacy, removed, 'legacy:.nova');
  } catch {
    /* ignore */
  }
}

/** 清理当前工作区桶内的 tmp/损坏/超大缓存，以及失效的其它 worktree 桶。 */
export function cleanupNovaCodemap(root) {
  const removed = [];
  const ws = normalizeWorkspaceRoot(root);
  cleanupLegacyRepoNova(root, removed);

  const base = indexRootDir();
  if (!existsSync(base)) return { removed };

  const currentDir = indexCacheDir(root);
  const currentKey = workspaceCacheKey(root);

  if (existsSync(currentDir)) {
    let entries = [];
    try {
      entries = readdirSync(currentDir);
    } catch {
      entries = [];
    }
    for (const name of entries) {
      const p = join(currentDir, name);
      const isPrimary = name === INDEX_CACHE_NAME;
      const isTmp = /\.tmp$/i.test(name) || /\.tmp\./i.test(name);
      if (!isPrimary && !isTmp && !/\.json$/i.test(name)) continue;
      try {
        const st = statSync(p);
        if (!isPrimary || st.size > INDEX_MAX_BYTES || isTmp) safeRm(p, removed, `${currentKey}/${name}`);
      } catch {
        /* ignore */
      }
    }
    const primary = join(currentDir, INDEX_CACHE_NAME);
    if (existsSync(primary)) {
      try {
        const st = statSync(primary);
        if (st.size > INDEX_MAX_BYTES) throw new Error('huge');
        const data = JSON.parse(readFileSync(primary, 'utf8'));
        if (data?.version !== INDEX_CACHE_VERSION || !data.files || typeof data.files !== 'object') throw new Error('bad');
        if (data.workspaceRoot && data.workspaceRoot !== ws) throw new Error('ws-mismatch');
      } catch {
        safeRm(primary, removed, `${currentKey}/${INDEX_CACHE_NAME}`);
      }
    }
  }

  let buckets = [];
  try {
    buckets = readdirSync(base);
  } catch {
    return { removed };
  }
  /** @type {{ key: string, path: string, mtimeMs: number }[]} */
  const live = [];
  for (const key of buckets) {
    const dir = join(base, key);
    let st;
    try {
      st = statSync(dir);
      if (!st.isDirectory()) {
        safeRm(dir, removed, key);
        continue;
      }
    } catch {
      continue;
    }
    if (key === currentKey) {
      live.push({ key, path: dir, mtimeMs: st.mtimeMs });
      continue;
    }
    const primary = join(dir, INDEX_CACHE_NAME);
    let keep = false;
    let mtimeMs = st.mtimeMs;
    if (existsSync(primary)) {
      try {
        const pst = statSync(primary);
        mtimeMs = pst.mtimeMs;
        if (pst.size > INDEX_MAX_BYTES) throw new Error('huge');
        const data = JSON.parse(readFileSync(primary, 'utf8'));
        const cachedRoot = data?.workspaceRoot;
        if (!cachedRoot || typeof cachedRoot !== 'string') throw new Error('no-root');
        if (!existsSync(cachedRoot)) throw new Error('gone');
        if (data.version !== INDEX_CACHE_VERSION || !data.files) throw new Error('bad');
        keep = true;
      } catch {
        keep = false;
      }
    }
    if (!keep) {
      safeRm(dir, removed, key);
      continue;
    }
    try {
      for (const name of readdirSync(dir)) {
        if (name === INDEX_CACHE_NAME) continue;
        if (/\.tmp$/i.test(name) || /\.json$/i.test(name)) safeRm(join(dir, name), removed, `${key}/${name}`);
      }
    } catch {
      /* ignore */
    }
    live.push({ key, path: dir, mtimeMs });
  }

  if (live.length > INDEX_MAX_WORKSPACE_BUCKETS) {
    live.sort((a, b) => a.mtimeMs - b.mtimeMs);
    const overflow = live.length - INDEX_MAX_WORKSPACE_BUCKETS;
    for (let i = 0; i < overflow; i++) {
      if (live[i].key === currentKey) continue;
      safeRm(live[i].path, removed, `evict:${live[i].key}`);
    }
  }
  return { removed };
}

function loadCache(root) {
  const p = indexCachePath(root);
  if (!existsSync(p)) return null;
  try {
    const data = JSON.parse(readFileSync(p, 'utf8'));
    if (data?.version !== INDEX_CACHE_VERSION || !data.files || typeof data.files !== 'object') return null;
    if (data.workspaceRoot && data.workspaceRoot !== normalizeWorkspaceRoot(root)) return null;
    return data;
  } catch {
    return null;
  }
}

function saveCache(root, cache) {
  const dir = indexCacheDir(root);
  mkdirSync(dir, { recursive: true });
  const p = indexCachePath(root);
  const tmp = join(dir, `${INDEX_CACHE_NAME}.${process.pid}.tmp`);
  try {
    writeFileSync(tmp, JSON.stringify(cache));
    if (existsSync(p)) unlinkSync(p);
    renameSync(tmp, p);
  } catch {
    try {
      writeFileSync(p, JSON.stringify(cache));
    } catch {
      /* ignore */
    }
    try {
      if (existsSync(tmp)) unlinkSync(tmp);
    } catch {
      /* ignore */
    }
  }
}

// ---------------------------------------------------------------- 文件清单

export function isCodeFile(file) {
  return CODE_EXT_RE.test(file) && !EXCLUDE.test(file);
}

const WALK_SKIP_DIR = /^(node_modules|target|dist|coverage|\.git|\.venv|venv|__pycache__|build|out|vendor)$/;

function walkFiles(root) {
  const out = [];
  /** @type {string[]} */
  const stack = [''];
  while (stack.length && out.length < MAX_WALK_FILES) {
    const rel = stack.pop();
    let entries;
    try {
      entries = readdirSync(join(root, rel), { withFileTypes: true });
    } catch {
      continue;
    }
    for (const e of entries) {
      const child = rel ? `${rel}/${e.name}` : e.name;
      if (e.isDirectory()) {
        if (e.name.startsWith('.') || WALK_SKIP_DIR.test(e.name)) continue;
        stack.push(child);
      } else if (isCodeFile(child)) out.push(child);
    }
  }
  return out;
}

/** 已跟踪 + 未忽略的未跟踪代码文件（-z 避免非 ASCII 路径被 git 转义）。 */
export async function listCodeFiles(root) {
  const out = await run(root, 'git', ['ls-files', '-c', '-o', '--exclude-standard', '-z', '--', ...CODE_GLOBS]);
  const files = out.split('\0').filter((f) => f && isCodeFile(f));
  if (files.length) return [...new Set(files)];
  return walkFiles(root);
}

// ---------------------------------------------------------------- 索引

/** 进程内热缓存：同一次会话多次调用不重复解析 JSON。 */
const memo = new Map();

// ---------------------------------------------------------------- 模块指定符解析

const RESOLVE_EXT = ['.ts', '.tsx', '.mts', '.cts', '.js', '.jsx', '.mjs', '.cjs', '.vue', '.svelte', '.rs', '.py'];

/** 纯词法 posix 路径拼接（不碰磁盘），处理 . 与 ..。 */
function posixJoin(...parts) {
  const out = [];
  for (const part of parts) {
    for (const seg of String(part).split('/')) {
      if (!seg || seg === '.') continue;
      if (seg === '..') out.pop();
      else out.push(seg);
    }
  }
  return out.join('/');
}

function tryModuleFile(base, fileSet) {
  if (!base) return null;
  if (fileSet.has(base)) return base;
  for (const e of RESOLVE_EXT) if (fileSet.has(base + e)) return base + e;
  for (const e of RESOLVE_EXT) if (fileSet.has(`${base}/index${e}`)) return `${base}/index${e}`;
  for (const e of RESOLVE_EXT) if (fileSet.has(`${base}/mod${e}`)) return `${base}/mod${e}`;
  return null;
}

/**
 * 把 import/use 的原始指定符解析为仓库相对路径；解析不了返回 null。
 * Rust 的 from 形如 crate::a::b::Sym（含符号名），模块路径 = 去掉末段，
 * 再按 a/b.rs 或 a/b/mod.rs 查找；末段本身也可能是模块（use crate::a::b;），做兜底尝试。
 */
function resolveSpecifier(spec, fromFile, fileSet) {
  if (spec.includes('::')) {
    let segs = spec.split('::').filter(Boolean);
    let baseDir = '';
    if (segs[0] === 'crate') {
      segs.shift();
      // crate 根 = fromFile 路径中名为 src 的最近祖先目录
      const dirs = fromFile.split('/').slice(0, -1);
      const i = dirs.lastIndexOf('src');
      if (i < 0) return null;
      baseDir = dirs.slice(0, i + 1).join('/');
    } else if (segs[0] === 'super' || segs[0] === 'self') {
      const dirs = fromFile.split('/').slice(0, -1);
      while (segs[0] === 'super') {
        segs.shift();
        dirs.pop();
      }
      if (segs[0] === 'self') segs.shift();
      baseDir = dirs.join('/');
    } else return null; // 外部 crate
    const modBase = posixJoin(baseDir, segs.slice(0, -1).join('/'));
    return tryModuleFile(modBase, fileSet) ?? tryModuleFile(posixJoin(baseDir, segs.join('/')), fileSet);
  }
  if (!spec.startsWith('.')) return null; // bare specifier（npm 包等）
  const dir = fromFile.includes('/') ? fromFile.slice(0, fromFile.lastIndexOf('/')) : '';
  return tryModuleFile(posixJoin(dir, spec), fileSet);
}

/**
 * 跨文件引用解析：同文件顶层符号 > import 来源文件中的定义 > 全局唯一定义（名字足够长才可信）。
 * @returns {{ file: string, ln: number, end: number, depth: number, kind: string, sig: string, exp: boolean } | null}
 */
export function resolveRef(index, name, fromFile) {
  const local = index.files[fromFile]?.syms?.find((s) => s.name === name && s.depth === 0 && s.kind !== 'prop');
  if (local) {
    return { file: fromFile, ln: local.ln, end: local.end, depth: local.depth, kind: local.kind, sig: local.sig, exp: local.exp === true };
  }
  const from = index.imports?.get(fromFile)?.get(name);
  if (from) {
    const d = (index.defs.get(name) ?? []).find((x) => x.file === from);
    if (d) return d;
  }
  // 全局唯一回退只在全仓索引完整时可信；focused 冷索引不能把“当前唯一”误判为“全仓唯一”。
  const defs = index.defs.get(name);
  if (index.complete && defs?.length === 1 && name.length >= 6 && defs[0].kind !== 'method') return defs[0];
  return null;
}

function normalizeFile(file) {
  return String(file ?? '').replace(/\\/g, '/').replace(/^\.\//, '');
}

function cacheEntryFresh(root, cache, file) {
  try {
    const st = statSync(join(root, file));
    if (st.size > MAX_INDEX_FILE_BYTES) return false;
    const prev = cache.files[file];
    return Boolean(prev && prev.size === st.size && prev.mtimeMs === Math.trunc(st.mtimeMs));
  } catch {
    return false;
  }
}

async function scanIntoCache(root, cache, files) {
  const unique = [...new Set(files)].filter(Boolean);
  await mapPool(unique, INDEX_CONCURRENCY, async (file) => {
    try {
      const st = statSync(join(root, file));
      if (st.size > MAX_INDEX_FILE_BYTES) return;
      const text = readFileSync(join(root, file), 'utf8');
      const { total, syms, imports } = scanSource(text, file);
      cache.files[file] = { size: st.size, mtimeMs: Math.trunc(st.mtimeMs), total, syms, imports };
    } catch {
      delete cache.files[file];
    }
  });
  return unique.length;
}

function buildIndexView(cache, allFiles, complete, stats) {
  /** @type {Map<string, any[]>} */
  const defs = new Map();
  for (const [file, entry] of Object.entries(cache.files).sort(([a], [b]) => Buffer.compare(Buffer.from(a), Buffer.from(b)))) {
    for (const s of entry.syms) {
      if (s.kind === 'prop' || s.depth > 1) continue;
      if (s.depth === 1 && !/^(fn|method|type|class)$/.test(s.kind)) continue;
      let arr = defs.get(s.name);
      if (!arr) {
        arr = [];
        defs.set(s.name, arr);
      }
      arr.push({ file, ln: s.ln, end: s.end, depth: s.depth, kind: s.kind, sig: s.sig, exp: s.exp === true });
    }
  }

  const fileSet = new Set(allFiles);
  /** @type {Map<string, Map<string, string>>} */
  const imports = new Map();
  for (const [file, entry] of Object.entries(cache.files).sort(([a], [b]) => Buffer.compare(Buffer.from(a), Buffer.from(b)))) {
    if (!entry.imports?.length) continue;
    const m = new Map();
    for (const imp of entry.imports) {
      const target = resolveSpecifier(imp.from, file, fileSet);
      if (target) m.set(imp.name, target);
    }
    if (m.size) imports.set(file, m);
  }
  return { files: cache.files, allFiles, defs, imports, complete, stats };
}

/**
 * 取仓库符号索引。focused 模式只同步扫描命中/点名/主题文件及其直接 import/use；
 * full 模式供 code_map 使用，渐进扫描全仓。缓存按 size+mtime 逐文件失效。
 */
export async function getIndex(root, {
  files: only = null,
  priorityFiles = [],
  matchTerms = [],
  mode = 'full',
  dependencyDepth = 1,
} = {}) {
  const ws = normalizeWorkspaceRoot(root);
  let cache = memo.get(ws);
  if (!cache) {
    cleanupNovaCodemap(root);
    cache = loadCache(root) ?? {
      version: INDEX_CACHE_VERSION,
      workspaceRoot: ws,
      updatedAt: '',
      files: {},
    };
    memo.set(ws, cache);
  }

  const list = only ?? (await listCodeFiles(root));
  const fileSet = new Set(list);
  const cold = Object.keys(cache.files).length === 0;
  const requested = new Set((priorityFiles || []).map(normalizeFile).filter((f) => fileSet.has(f)));
  const subjectTerms = (matchTerms || []).map((t) => String(t).toLowerCase()).filter((t) => t.length >= 4);
  if (subjectTerms.length) {
    for (const file of list) {
      const base = file.split('/').pop().toLowerCase();
      if (subjectTerms.some((term) => base.includes(term))) requested.add(file);
    }
  }

  let targets;
  if (only) targets = list;
  else if (mode === 'focused') targets = [...requested];
  else {
    const stale = list.filter((file) => !cacheEntryFresh(root, cache, file));
    targets = [...stale.filter((file) => requested.has(file)), ...stale.filter((file) => !requested.has(file))]
      .slice(0, MAX_SCAN_PER_CALL);
  }

  let scanned = 0;
  const initialStale = targets.filter((file) => !cacheEntryFresh(root, cache, file));
  scanned += await scanIntoCache(root, cache, initialStale);

  // focused 冷路径只扩一跳直接模块依赖；解析路径依赖完整文件清单，不要求全仓先建符号索引。
  let frontier = [...requested];
  for (let depth = 0; mode === 'focused' && depth < dependencyDepth && frontier.length; depth++) {
    const deps = new Set();
    for (const file of frontier) {
      for (const imp of cache.files[file]?.imports ?? []) {
        const target = resolveSpecifier(imp.from, file, fileSet);
        if (target && !requested.has(target)) deps.add(target);
      }
    }
    const depList = [...deps];
    const staleDeps = depList.filter((file) => !cacheEntryFresh(root, cache, file));
    scanned += await scanIntoCache(root, cache, staleDeps);
    for (const file of depList) requested.add(file);
    frontier = depList;
  }

  if (mode !== 'focused') {
    for (const file of Object.keys(cache.files)) if (!fileSet.has(file)) delete cache.files[file];
  }
  if (scanned) {
    cache.updatedAt = new Date().toISOString();
    saveCache(root, cache);
  }

  // focused 模式不为判断“完整”而 stat 全仓；只有 full 模式且文件清单均已有条目才开放全局唯一回退。
  const complete = mode !== 'focused' && list.every((file) => Boolean(cache.files[file]));
  // Focused results must depend on this request, not whichever files an older process happened to
  // leave in the shared disk cache. Keep the full cache for reuse, but expose only requested/deps.
  const viewCache = mode === 'focused'
    ? { ...cache, files: Object.fromEntries(Object.entries(cache.files).filter(([file]) => requested.has(file))) }
    : cache;
  return buildIndexView(viewCache, list, complete, {
    scanned,
    pending: Math.max(0, list.length - Object.keys(cache.files).filter((file) => fileSet.has(file)).length),
    total: list.length,
    cold,
    mode,
  });
}
