import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';

// Static route-boundary check when the desktop Rust build lacks system libraries.
// Run: node scripts/lyra-only-backends.test.mjs
const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');
const lib = read('src-tauri/src/lib.rs');
const relay = read('src-tauri/src/relay.rs');
const remote = read('src-tauri/src/remote.rs');
const sdk = read('src-tauri/src/lyra/runtime.rs');
assert.doesNotMatch(sdk, /SdkAdapter|LyraAdapter|self\.adapter|new_with_env|borrowed_root/);
assert.doesNotMatch(lib, /\bprewarm\b|\bmod sdk_(?:adapters|runtime)\b/);
for (const path of ['src-tauri/src/sdk_runtime.rs', 'src-tauri/src/sdk_adapters']) {
  assert.equal(existsSync(new URL(`../${path}`, import.meta.url)), false, `${path} must stay removed`);
}
for (const source of [lib, relay, remote, sdk]) {
  assert.doesNotMatch(source, /\bacp_for\b|\b(?:state|self)\s*\.\s*(?:claude|codexplus|codex)\b/);
  assert.doesNotMatch(source, /\.state::<AppState>\(\)\s*\.\s*(?:claude|codexplus|codex)\b/);
}
assert.doesNotMatch(lib, /AcpManager::new|CodexManager::new|CodexAdapter|pub (?:claude|codex|codexplus):/);
assert.doesNotMatch(lib, /\b(?:cli_manager|codex_app_server|codex_radar)\b/);
assert.match(lib, /\*kind == AgentKind::Lyra && self\.settings\.lock\(\)\.unwrap\(\)\.lyra_enabled/);
assert.match(lib, /if t\.agent_kind != AgentKind::Lyra/);
assert.match(lib, /if thread\.agent_kind != AgentKind::Lyra[\s\S]*?编辑重发/);
assert.doesNotMatch(sdk, /codex_launch|codex_app_server::(?:run|spawn_prompt)/);
assert.match(sdk, /if thread\.agent_kind != AgentKind::Lyra[\s\S]*?return;[\s\S]*?thread\.take_prompt_context/);
assert.doesNotMatch(read('src-tauri/src/acp.rs'), /\bAcpManager\b|\bspawn_conn\b/);
const settings = read('src-tauri/src/settings.rs');
assert.doesNotMatch(settings, /pub (?:claude_enabled|codex_enabled|codexplus_enabled|codex_integration|codex_args):/);
for (const file of ['cli_manager.rs', 'codex_app_server.rs', 'codex_app_server', 'codex_radar.rs']) {
  assert.equal(existsSync(new URL(`../src-tauri/src/${file}`, import.meta.url)), false, `${file} must stay removed`);
}
const pkg = JSON.parse(read('package.json'));
const lock = JSON.parse(read('package-lock.json'));
assert.ok(pkg.dependencies['@modelcontextprotocol/sdk']);
assert.equal(pkg.scripts['build:nova-tools-mcp'], undefined);
assert.equal(existsSync(new URL('../scripts/nova-tools-mcp.mjs', import.meta.url)), false, 'Node MCP bridge must stay removed');
assert.equal(pkg.scripts['test:codex-app-server'], undefined);
for (const name of ['@tencent-ai/agent-sdk', '@agentclientprotocol/sdk']) {
  assert.equal(pkg.dependencies[name], undefined);
  assert.equal(lock.packages[`node_modules/${name}`], undefined);
}
const threads = read('src-tauri/src/threads.rs');
// Persist retired provenance for migration, but never accept it as an executable backend.
const legacyArm = threads.match(/((?:"[a-z]+"\s*\|\s*)*"[a-z]+")\s*=> AgentKind::Legacy\(raw\)/)?.[1];
assert.ok(legacyArm, 'retired sources must deserialize as Legacy, not Unsupported or Lyra');
assert.doesNotMatch(legacyArm, /"(?:kimi|cursor)"/);
for (const source of ['claude', 'claudecode', 'codex', 'codexplus', 'devin', 'codebuddy', 'codebuddyplus', 'workbuddy', 'opencode', 'opencodeplus']) {
  assert.ok(legacyArm.includes(`"${source}"`), `${source} must retain its migration source`);
}
assert.match(threads, /AgentKind::Legacy\(source\) => source/);
const kinds = threads.split('pub fn from_str(s: &str)')[1].split('/// 展示用名称')[0];
assert.deepEqual([...kinds.matchAll(/"([a-z]+)"/g)].map(match => match[1]), ['lyra', 'alkaid']);
assert.match(kinds, /"lyra"\s*\|\s*"alkaid" => Some\(AgentKind::Lyra\)/);
assert.match(kinds, /_ => None/);
assert.doesNotMatch(kinds, /AgentKind::Legacy/);
assert.match(relay, /AgentKind::from_str\(agent\.trim\(\)\)/);
assert.match(remote, /AgentKind::from_str\(&cmd\.agent_kind\)/);
const ui = read('src/components/SettingsModal.tsx');
assert.doesNotMatch(ui, /setClaudeEnabled|setCodexEnabled|<CliManager|getCliStatuses/);
const providerUi = read('src/components/LyraConfigPanel.tsx');
assert.match(providerUi, /local-codex.*local-claude-code/);
assert.match(providerUi, /优先使用 CLI 自定义 API，否则使用内置订阅反代/);
console.log('Lyra-only routes, non-executable Legacy provenance, removed modules, dependencies and UI checks passed.');
