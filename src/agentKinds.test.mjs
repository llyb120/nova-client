import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// Run with: node src/agentKinds.test.mjs
const { outputFiles } = await build({
  stdin: {
    contents: 'export * from "./types"; export { agentLabel, agentShort } from "./utils";',
    resolveDir: fileURLToPath(new URL('.', import.meta.url)),
    loader: 'ts',
  },
  bundle: true, platform: 'node', format: 'esm', write: false,
});
const { ALL_AGENT_KINDS, isSupportedAgentKind, scenesReady, SCENES, agentLabel, agentShort } =
  await import(`data:text/javascript;base64,${Buffer.from(outputFiles[0].contents).toString('base64')}`);
const scenesWith = (agentKind) => Object.fromEntries(
  SCENES.map(({ key }) => [key, [{ agentKind, model: 'saved-model' }]]),
);
assert.deepEqual(ALL_AGENT_KINDS, ['lyra', 'devin', 'kimi', 'claude', 'codex', 'codebuddy', 'workbuddy']);
for (const kind of ALL_AGENT_KINDS) {
  assert.equal(isSupportedAgentKind(kind), true);
  assert.equal(scenesReady(scenesWith(kind)), true);
  assert.notEqual(agentLabel(kind), '已移除的后端');
  assert.notEqual(agentShort(kind), '?');
}
for (const kind of ['cursor', 'cursorplus', 'unsupported', 'unknown', '', null, undefined]) {
  assert.equal(isSupportedAgentKind(kind), false);
  assert.equal(ALL_AGENT_KINDS.includes(kind), false);
  assert.equal(scenesReady(scenesWith(kind)), false);
  assert.equal(agentLabel(kind), '已移除的后端');
  assert.equal(agentShort(kind), '?');
}
assert.equal(scenesReady(), false);
assert.equal(scenesReady({}), false);
const mixed = scenesWith('unsupported');
for (const targets of Object.values(mixed)) targets.push({ agentKind: 'lyra', model: 'selected-model' });
assert.equal(scenesReady(mixed), true);
console.log('Supported backends and unavailable historical backend handling passed.');
