// Matched production-mode Polaris A/B + Command Code GLM evidence check.
// node bench/polaris-latency-ab.mjs [--model] [--cold-check] [--index-update] [--baseline <git-ref>] [--corpus-ref <git-ref>] [--out <report.json>]
// Builds the unchanged native module from each arm in one optimized standalone binary.
// No model calls unless --model; credentials stay in the existing local config.
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, writeFile, mkdir, rm } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import { resolve, join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createInterface } from 'node:readline';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const option = (key, fallback) => args.includes(key) ? args[args.indexOf(key) + 1] : fallback;
const baseline = option('--baseline', 'HEAD');
const corpusRef = option('--corpus-ref', baseline);
const out = resolve(option('--out', join(repo, 'bench/polaris-latency-ab.report.json')));
const model = 'z-ai/glm-5.3-flash';
const cases = [
  { id: 'index', keywords: ['load_cache', 'store_cache'], task: '说明符号索引的缓存读取与发布流程', expect: ['load_cache', 'store_cache'], facts: ['MEMO', 'Arc', 'persist'] },
  { id: 'tools', keywords: ['govern', 'execute_inner'], task: '说明 Lyra 工具结果预算治理和执行入口', expect: ['govern', 'execute_inner'], facts: ['polaris', 'archive', 'ToolOutcome'] },
  { id: 'prompt', keywords: ['build_system_prompt'], task: '说明系统提示词中的工具选择规则和只读模式', expect: ['build_system_prompt'], facts: ['read_only', 'polaris', 'dynamic'] },
];
const sha = text => createHash('sha256').update(text).digest('hex');
const median = values => { const sorted = [...values].sort((a, b) => a - b); return sorted[Math.floor(sorted.length / 2)]; };
const work = await mkdtemp(join(tmpdir(), 'nova-polaris-ab-'));
const children = [];
const corpus = join(work, 'corpus');
const report = { ranAt: new Date().toISOString(), baseline, corpusRef, model: args.includes('--model') ? model : null,
  methodology: 'Same optimized binary, production cfg/deadlines, fixed baseline source corpus (src-tauri/src, src, scripts), separate per-arm caches. Corpus has one synthetic commit, not original git history; no new benchmark/answer files. Alternating A/B order. First observation is cold process/index, not cold OS cache. Model receives one forced Polaris tool result; no agent loop. Fact hits are smoke checks, not semantic correctness scores.',
  queries: [], modelRuns: [] };

function command(program, argv, timeout = 180000) {
  return new Promise((done, fail) => {
    const child = spawn(program, argv, { cwd: repo, stdio: ['ignore', 'ignore', 'pipe'] });
    let stderr = '';
    const timer = setTimeout(() => { child.kill('SIGKILL'); fail(new Error(`${program} timed out`)); }, timeout);
    child.stderr.on('data', chunk => { stderr = (stderr + chunk).slice(-6000); });
    child.on('error', error => { clearTimeout(timer); fail(error); });
    child.on('exit', code => { clearTimeout(timer); code === 0 ? done() : fail(new Error(`${program} exited ${code}: ${stderr}`)); });
  });
}

function worker(exe, arm, cacheName = arm) {
  const child = spawn(exe, [arm, join(work, `cache-${cacheName}`)], { cwd: repo, stdio: ['pipe', 'pipe', 'pipe'] });
  children.push(child);
  const pending = new Map();
  let seq = 0;
  let profile = [];
  createInterface({ input: child.stderr }).on('line', line => {
    const match = line.match(/^\[nova-tools-profile\] ([^:]+): (.+)$/);
    if (match && profile.length < 200) profile.push({ stage: match[1], value: match[2] });
  });
  createInterface({ input: child.stdout }).on('line', line => {
    const value = JSON.parse(line);
    const job = pending.get(value.id);
    if (!job) return;
    pending.delete(value.id); clearTimeout(job.timer);
    value.profile = profile;
    profile = [];
    value.error ? job.fail(new Error(value.error)) : job.done(value);
  });
  child.on('exit', code => {
    for (const job of pending.values()) { clearTimeout(job.timer); job.fail(new Error(`worker ${arm} exited ${code}`)); }
    pending.clear();
  });
  return params => new Promise((done, fail) => {
    const id = ++seq;
    const timer = setTimeout(() => { pending.delete(id); child.kill('SIGKILL'); fail(new Error(`worker ${arm} timed out`)); }, 30000);
    pending.set(id, { done, fail, timer });
    child.stdin.write(JSON.stringify({ id, root: corpus, params }) + '\n');
  });
}

try {
  const path = 'src-tauri/src/nova_tools_native/context.rs';
  const a = execFileSync('git', ['show', `${baseline}:${path}`], { cwd: repo, maxBuffer: 2 * 1024 * 1024 }).toString();
  const b = await readFile(join(repo, path), 'utf8');
  report.sourceHashes = { A: sha(a), B: sha(b) };
  report.workspaceRevision = execFileSync('git', ['rev-parse', `${corpusRef}^{commit}`], { cwd: repo }).toString().trim();
  await mkdir(corpus);
  const archive = join(work, 'corpus.tar');
  await command('git', ['archive', '--format=tar', '-o', archive, corpusRef, 'src-tauri/src', 'src', 'scripts']);
  await command('tar', ['-xf', archive, '-C', corpus]);
  await command('git', ['-C', corpus, 'init', '--quiet']);
  await command('git', ['-C', corpus, 'add', 'src-tauri/src', 'src', 'scripts']);
  await command('git', ['-C', corpus, '-c', 'user.name=Polaris benchmark', '-c', 'user.email=bench@localhost', '-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'Fixed source corpus']);
  await mkdir(join(work, 'src'));
  await writeFile(join(work, 'src/a.rs'), a);
  await writeFile(join(work, 'src/b.rs'), b);
  await writeFile(join(work, 'Cargo.toml'), `[package]
name = "polaris-ab"
version = "0.0.0"
edition = "2021"
[dependencies]
regex = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"
wait-timeout = "0.2"
walkdir = "2"
libc = "0.2"
bincode = "1.3"
[profile.release]
opt-level = 3
`);
  const updateProbe = args.includes('--index-update') ? `
    pub fn index_update_probe() -> serde_json::Value {
        let mut postings = HashMap::<String, Vec<String>>::new();
        let mut contents = HashMap::new();
        let mut file_tokens = HashMap::new();
        for i in 0..1500 {
            let file = format!("src/file_{i}.rs");
            let text = format!("pub fn symbol_{i}() {}", "shared field value;".repeat(256));
            let tokens = tokens_for_search_text(&text);
            for token in &tokens { postings.entry(token.clone()).or_default().push(file.clone()); }
            file_tokens.insert(file.clone(), tokens);
            contents.insert(file, text);
        }
        let mut index = index_from_snapshot(SearchSnapshot { version: SEARCH_SNAPSHOT_VERSION,
            root: "synthetic".into(), head: "base".into(), git_signature: "base".into(),
            dirty_files: Vec::new(), postings, contents, file_tokens });
        let mut delta = SearchDelta { version: SEARCH_SNAPSHOT_VERSION, root: "synthetic".into(),
            base_head: "base".into(), base_signature: "base".into(), head: "base".into(),
            git_signature: "dirty".into(), dirty_files: vec!["src/file_0.rs".into()], files: HashMap::new() };
        let mut updates = Vec::new();
        let mut replays = Vec::new();
        for i in 0..8 {
            let body = format!("pub fn changed_{i}() {{}}");
            delta.files.insert("src/file_0.rs".into(), Some(body.clone()));
            let start = Instant::now();
            let updated = apply_search_delta(index.clone(), &delta).unwrap();
            updates.push(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(updated.contents["src/file_0.rs"].as_str(), body.as_str());
            assert!(index.contents["src/file_1499.rs"].contains("symbol_1499"));
            let start = Instant::now();
            let replay = apply_search_delta(updated.clone(), &delta).unwrap();
            replays.push(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(replay.contents["src/file_0.rs"].as_str(), body.as_str());
            index = replay;
        }
        serde_json::json!({"files":1500,"updatesMs":updates,"replaysMs":replays})
    }
  ` : '';
  await writeFile(join(work, 'src/main.rs'), `#![allow(dead_code)]
mod a { include!("a.rs"); ${updateProbe} }
mod b { include!("b.rs"); ${updateProbe} }
use std::{io::{BufRead, Write}, path::Path, time::Instant};
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args[1] == "A" { a::set_data_root(args[2].clone().into()); } else { b::set_data_root(args[2].clone().into()); }
    for line in std::io::stdin().lock().lines() {
        let req: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        let root = Path::new(req["root"].as_str().unwrap());
        let start = Instant::now();
        ${args.includes('--index-update') ? `if req["params"]["_indexUpdateProbe"] == true {
            let value = if args[1] == "A" { a::index_update_probe() } else { b::index_update_probe() };
            println!("{}", serde_json::json!({"id":req["id"],"probe":value}));
            std::io::stdout().flush().unwrap();
            continue;
        }` : ''}
        let result = if args[1] == "A" { a::polaris(root, req["params"].clone()) } else { b::polaris(root, req["params"].clone()) };
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        let response = match result {
            Ok(text) => serde_json::json!({"id":req["id"],"ms":ms,"text":text}),
            Err(error) => serde_json::json!({"id":req["id"],"ms":ms,"error":error}),
        };
        println!("{response}");
        std::io::stdout().flush().unwrap();
    }
}
`);
  console.log('Building both native arms (same Rust compiler/options)...');
  await command('cargo', ['build', '--release', '--offline', '-j', '1', '--manifest-path', join(work, 'Cargo.toml')]);
  const exe = join(work, 'target/release/polaris-ab');
  if (args.includes('--cold-check')) {
    report.coldChecks = [];
    for (let repeat = 0; repeat < 3; repeat++) for (const arm of repeat % 2 ? ['B', 'A'] : ['A', 'B']) {
      const test = cases[0];
      const invoke = worker(exe, arm, `${arm}-cold-${repeat}`);
      const result = await invoke({ keywords: test.keywords, task: test.task });
      report.coldChecks.push({ arm, repeat, ms: result.ms, text: result.text, hash: sha(result.text), profile: result.profile });
      children.at(-1).kill();
    }
  }
  const workers = { A: worker(exe, 'A'), B: worker(exe, 'B') };
  const contexts = {};
  for (let run = 0; run < 6; run++) {
    for (const test of cases) {
      for (const arm of run % 2 ? ['B', 'A'] : ['A', 'B']) {
        const result = await workers[arm]({ keywords: test.keywords, task: test.task });
        assert(!result.text.startsWith('# CTX MISS'), `${test.id}/${arm} missed`);
        assert(Buffer.byteLength(result.text) <= 32768);
        const expanded = [...result.text.matchAll(/^### (\S+)/gm)].map(m => m[1]);
        const row = { id: test.id, arm, run, ms: result.ms, bytes: Buffer.byteLength(result.text), hash: sha(result.text), expanded, profile: result.profile };
        report.queries.push(row);
        if (run === 5) contexts[`${test.id}/${arm}`] = result.text;
        console.log(`${test.id}/${arm} #${run}: ${result.ms.toFixed(1)}ms ${row.bytes}B`);
      }
    }
    // Give async snapshot construction a bounded settling window before warm measurements.
    if (run === 0) await new Promise(done => setTimeout(done, 2000));
  }
  report.toolSummary = cases.map(test => {
    const times = arm => report.queries.filter(r => r.id === test.id && r.arm === arm && r.run >= 1).map(r => r.ms);
    const A = median(times('A')), B = median(times('B'));
    const pairs = report.queries.filter(r => r.id === test.id && r.arm === 'A').map(a => {
      const b = report.queries.find(r => r.id === test.id && r.arm === 'B' && r.run === a.run);
      return { run: a.run, identicalOutput: a.hash === b.hash };
    });
    return { id: test.id, medianA: A, medianB: B, improvementPercent: 100 * (A - B) / A, pairs,
      identicalWarmOutput: pairs.filter(p => p.run >= 1).every(p => p.identicalOutput) };
  });
  if (args.includes('--index-update')) {
    report.indexUpdateProbe = {};
    for (const arm of ['A', 'B']) report.indexUpdateProbe[arm] = (await workers[arm]({ _indexUpdateProbe: true })).probe;
  }
  await writeFile(out, JSON.stringify(report, null, 2));
  assert(report.toolSummary.every(row => row.identicalWarmOutput), 'warm evidence changed; inspect paired hashes before model evaluation');

  if (args.includes('--model')) {
    const config = JSON.parse(await readFile(join(homedir(), '.nova/alkaid/config.jsonc'), 'utf8'));
    const provider = config.provider?.commandcode;
    assert(provider?.options?.apiKey && provider.models?.[model], 'Command Code GLM config missing');
    // Two crossed-order pairs per case, fresh conversation each time. No automatic retries.
    for (let repeat = 0; repeat < 2; repeat++) for (const test of cases) {
      for (const arm of repeat % 2 ? ['B', 'A'] : ['A', 'B']) {
        const start = performance.now();
        const response = await fetch(`${provider.options.baseURL.replace(/\/$/, '')}/chat/completions`, {
          method: 'POST',
          headers: { Authorization: `Bearer ${provider.options.apiKey}`, 'Content-Type': 'application/json' },
          body: JSON.stringify({ model, stream: false, max_tokens: 3000, temperature: 1, top_p: 0.95,
            tools: [{ type: 'function', function: { name: 'polaris', description: 'Retrieve code context', parameters: { type: 'object', properties: { keywords: { type: 'array', items: { type: 'string' } }, task: { type: 'string' } } } } }],
            tool_choice: 'none',
            messages: [
              { role: 'system', content: '你是代码分析助手。只根据工具证据回答，引用函数和文件，不把签名当完整函数体。证据不足时明确指出，不猜测。最多300字。' },
              { role: 'user', content: test.task },
              { role: 'assistant', content: null, tool_calls: [{ id: 'polaris_evidence', type: 'function', function: { name: 'polaris', arguments: JSON.stringify({ keywords: test.keywords, task: test.task }) } }] },
              { role: 'tool', tool_call_id: 'polaris_evidence', content: contexts[`${test.id}/${arm}`] },
            ] }),
          signal: AbortSignal.timeout(90000),
        });
        const json = await response.json();
        assert(response.ok, `Command Code HTTP ${response.status}`);
        const choice = json.choices?.[0];
        const text = choice?.message?.content ?? '';
        const row = { id: test.id, arm, repeat, model: json.model, ms: performance.now() - start,
          finishReason: choice?.finish_reason, usage: json.usage, text,
          anchorHits: test.expect.filter(x => text.includes(x)), factHits: test.facts.filter(x => text.toLowerCase().includes(x.toLowerCase())) };
        report.modelRuns.push(row);
        await writeFile(out, JSON.stringify(report, null, 2));
        assert.equal(json.model, model, 'provider returned a different model');
        assert.equal(row.finishReason, 'stop', 'truncated/incomplete model answer');
        assert(text.trim(), 'empty model answer');
        console.log(`GLM ${test.id}/${arm} #${repeat}: ${(row.ms / 1000).toFixed(1)}s anchors=${row.anchorHits.length}/${test.expect.length}`);
      }
    }
  }
  console.log(JSON.stringify(report.toolSummary, null, 2));
  console.log(`Report: ${out}`);
} catch (error) {
  report.error = error.message;
  await writeFile(out, JSON.stringify(report, null, 2));
  throw error;
} finally {
  for (const child of children) child.kill();
  await rm(work, { recursive: true, force: true });
}
