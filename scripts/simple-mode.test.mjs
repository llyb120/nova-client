import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { build } from 'esbuild';
const compiled = await build({ entryPoints: ['src/simpleMode.ts'], bundle: true, write: false, format: 'esm', platform: 'node' });
const { ensureServerSupportProvider, selectServerSceneModels, resolveSimpleModeTarget, mergeSimpleModeServerScenes } = await import(`data:text/javascript;base64,${Buffer.from(compiled.outputFiles[0].text).toString('base64')}`);

test('relay provider is added or reused without altering other providers', () => {
  const config = { provider: { local: { preset: 'deepseek' } } };
  const added = ensureServerSupportProvider(config);
  assert.deepEqual(added.provider, {preset:'server-support'});
  assert.deepEqual(added.config.provider.local, config.provider.local);
  assert.equal(ensureServerSupportProvider({provider:{team:added.provider}}).providerId, 'team');
});
test('one server selection for all scenes including effort, never selects a local or old per-scene override', () => {
  const fetched = { m:{serverModel:true, variants:{high:{},medium:{}}}, other:{serverComputer:true, modalities:{input:['image']}} };
  const target = {agentKind:'lyra', model:'team/m/variant/medium'};
  assert.deepEqual(selectServerSceneModels('team', fetched), {lightweight:target,coding:target,computer:target,displayName:'m'});
  fetched.m.serverModelVariant = 'high';
  assert.equal(selectServerSceneModels('team', fetched).computer.model,'team/m/variant/high');
  assert.throws(() => selectServerSceneModels('team', {other:{}}), /未指定/);
  assert.throws(() => resolveSimpleModeTarget(null), /尚未就绪/);
  const scenes = {lightweight:target.model,coding:'obsolete',computer:'obsolete'};
  assert.deepEqual(resolveSimpleModeTarget(scenes), target);
  const base = {simpleMode:true, sceneModels:{}};
  const next = mergeSimpleModeServerScenes(base, scenes);
  assert.deepEqual(next.sceneModels, {lightweight:[target],coding:[target],computer:[target]});
  assert.equal(mergeSimpleModeServerScenes(next, scenes), null);
  assert.equal(mergeSimpleModeServerScenes({...base,simpleMode:false}, scenes), null);
});
test('OnboardingWizard does not call onCleanup inside onSettled', () => {
  const source = readFileSync(new URL('../src/components/OnboardingWizard.tsx', import.meta.url), 'utf8');
  assert.doesNotMatch(source, /onCleanup\(/);
});
