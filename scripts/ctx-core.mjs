// ctx-core.mjs — 上下文检索核心逻辑 (纯模块, 无协议)
//
// 被两个薄封装复用:
//   scripts/ctx        CLI, 给 Vega 原生工具 / 终端
//   scripts/ctx-mcp.mjs MCP server, 仅给 Cursor
//
// 检索引擎: git grep + rg (子进程), 全程无 LLM。

import { execFileSync } from 'node:child_process';
import { readFileSync, existsSync } from 'node:fs';

export function repoRoot() {
  if (process.env.CTX_ROOT) return process.env.CTX_ROOT;
  try {
    return execFileSync('git', ['rev-parse', '--show-toplevel'], { encoding: 'utf8' }).trim();
  } catch {
    return process.cwd();
  }
}

const EXCLUDE = /src-tauri\/target|node_modules|package-lock|\.png|dist\//;
const NEIGHBOR_EXCLUDE = /\.md$|\.github\/|\.yml$|\.yaml$|\.json$|\.toml$|docs\/|scripts\/legacy-context/;
const SYM_RE = '^\\s*(pub(?:\\(.*\\))?\\s+)?(async\\s+)?(fn|struct|enum|trait|impl|type|export)\\b';

function run(root, cmd, args) {
  try {
    return execFileSync(cmd, args, { cwd: root, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
  } catch (e) {
    return e.stdout?.toString() ?? '';
  }
}
const gitGrep = (root, pattern) => run(root, 'git', ['grep', '-nI', '--', pattern]);
const rgSym = (root, file) => run(root, 'rg', ['-nN', SYM_RE, file]);
function rgExtractSym(root, file) {
  return run(root, 'rg', ['-oN',
    '^\\s*(?:pub(?:\\(.*\\))?\\s+)?(?:async\\s+)?(?:fn|struct|enum|trait|type)\\s+([A-Za-z_][A-Za-z0-9_]*)',
    '-r', '$1', file]);
}
const cleanSym = (line) => line.replace(/\s*\{.*$/, '').replace(/\s*=.*$/, '').replace(/\s*$/, '');
const head = (text, n) => text.split('\n').slice(0, n).join('\n');
const shortHead = (root) => run(root, 'git', ['rev-parse', '--short', 'HEAD']).trim();
const branch = (root) => run(root, 'git', ['rev-parse', '--abbrev-ref', 'HEAD']).trim();
const fileLines = (root, f) => readFileSync(`${root}/${f}`, 'utf8').split('\n');

// ---------- context_bundle ----------
export function contextBundle({ keywords, budget = 700, ctx = 12, maxFiles = 12 }, root = repoRoot()) {
  if (!Array.isArray(keywords) || keywords.length === 0) return '错误: keywords 不能为空';

  const hits = new Map();
  for (const kw of keywords) {
    for (const line of gitGrep(root, kw).split('\n')) {
      const f = line.split(':')[0];
      if (!f || EXCLUDE.test(f)) continue;
      hits.set(f, (hits.get(f) ?? 0) + 1);
    }
  }
  if (hits.size === 0) return `无命中: ${keywords.join(' ')}`;
  const ranked = [...hits.entries()].sort((a, b) => b[1] - a[1]);

  const extra = new Set();
  for (const [f] of ranked.slice(0, 5)) {
    for (const s of [...new Set(rgExtractSym(root, f).split('\n').map(x => x.trim()).filter(Boolean))].slice(0, 20)) {
      if (s.length < 4) continue;
      for (const line of gitGrep(root, `\\b${s}\\b`).split('\n').slice(0, 5)) {
        const cf = line.split(':')[0];
        if (!cf || cf === f || EXCLUDE.test(cf) || NEIGHBOR_EXCLUDE.test(cf)) continue;
        extra.add(cf);
      }
    }
  }

  const out = [];
  out.push('# Context Bundle', '');
  out.push(`- 查询: ${keywords.join(' ')}`);
  out.push(`- 仓库: ${root}`);
  out.push(`- commit: ${shortHead(root)}  branch: ${branch(root)}`);
  out.push(`- 预算: ${budget} 行  命中文件: ${hits.size}  扩展文件: ${extra.size}`);
  out.push('', '## 命中排名 (命中数 文件)');
  for (const [f, n] of ranked) out.push(`    ${n} ${f}`);
  out.push('');

  let used = 0;
  const assemble = (f, tag) => {
    if (!existsSync(`${root}/${f}`)) return;
    const lines = fileLines(root, f);
    const total = lines.length;
    out.push(`----- [${tag}] ${f}  (${total} 行) -----`);
    if (total <= 200) {
      lines.forEach((l, i) => out.push(`${String(i + 1).padStart(6)}  ${l}`));
      used += total;
    } else {
      out.push('  ## 符号大纲');
      rgSym(root, f).split('\n').slice(0, 60).forEach(l => out.push('  ' + cleanSym(l)));
      out.push('  ## 命中上下文');
      for (const kw of keywords) {
        out.push(head(run(root, 'git', ['grep', '-nI', '-C', String(ctx), '--', kw, f]), 120));
      }
      used += 120;
    }
    out.push('');
  };

  out.push('# ===== 核心命中文件 =====');
  for (const [f] of ranked.slice(0, maxFiles)) {
    if (used >= budget) { out.push('(预算耗尽, 其余命中文件仅列名于末尾)'); break; }
    assemble(f, 'HIT');
  }

  out.push('# ===== 1 跳扩展文件(仅大纲) =====');
  for (const f of extra) {
    if (!existsSync(`${root}/${f}`)) continue;
    if (used >= budget) { out.push('(预算耗尽, 剩余邻居仅列名于末尾)'); break; }
    out.push(`----- [NEIGHBOR] ${f} -----`);
    rgSym(root, f).split('\n').slice(0, 30).forEach(l => out.push('  ' + cleanSym(l)));
    out.push('');
    used += 30;
  }

  out.push('# ===== 未展开文件(可追加) =====');
  for (const [f] of ranked.slice(maxFiles)) out.push(`    ${f}`);
  return out.join('\n');
}

// ---------- code_map ----------
export function codeMap(_args = {}, root = repoRoot()) {
  const files = run(root, 'git', ['ls-files', '*.rs', '*.ts', '*.tsx', '*.mjs'])
    .split('\n').filter(f => f && !/src-tauri\/target|node_modules|package-lock/.test(f));
  const out = [];
  out.push(`# CODEMAP — ${root.split('/').pop()} @ ${branch(root)}`, '');
  out.push('## 文件 (行数)');
  for (const f of files) {
    if (!existsSync(`${root}/${f}`)) continue;
    out.push(`${String(fileLines(root, f).length).padStart(6)}  ${f}`);
  }
  out.push('', '## 符号大纲');
  for (const f of files) {
    const s = rgSym(root, f).split('\n').map(cleanSym).filter(Boolean);
    if (s.length) out.push('', `### ${f}`, ...s);
  }
  return out.join('\n');
}

// ---------- find_symbol ----------
export function findSymbol({ name }, root = repoRoot()) {
  if (!name) return '错误: name 不能为空';
  const lines = gitGrep(root, `\\b${name}\\b`).split('\n')
    .filter(l => l && !EXCLUDE.test(l.split(':')[0]));
  return `# 符号定位: ${name}  [${shortHead(root)}]\n` + lines.slice(0, 40).join('\n');
}

// ---------- 工具 schema + 分发 (供两个封装共用) ----------
export const TOOLS = [
  {
    name: 'context_bundle',
    description: '按关键词/符号一次性打包相关代码上下文(命中文件分层装配 + 1跳调用邻居)。用于在分析/修改前快速获取代码全貌, 减少逐步读取。',
    inputSchema: {
      type: 'object',
      properties: {
        keywords: { type: 'array', items: { type: 'string' }, description: '关键词或符号名列表' },
        budget: { type: 'number', description: '总行数预算, 默认700; 想一次看全建议1500' },
        ctx: { type: 'number', description: '命中行上下文半径, 默认12' },
        maxFiles: { type: 'number', description: '核心命中文件上限, 默认12' },
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
    name: 'find_symbol',
    description: '快速定位某个符号在仓库中的所有出现位置(文件:行号)。',
    inputSchema: {
      type: 'object',
      properties: { name: { type: 'string', description: '符号名' } },
      required: ['name'],
    },
  },
];

export function callTool(name, args) {
  switch (name) {
    case 'context_bundle': return contextBundle(args ?? {});
    case 'code_map': return codeMap(args ?? {});
    case 'find_symbol': return findSymbol(args ?? {});
    default: throw new Error(`未知工具: ${name}`);
  }
}
