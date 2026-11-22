import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { AjvJsonSchemaValidator } from '@modelcontextprotocol/sdk/validation/ajv';

const tools = JSON.parse(await readFile(new URL('./jianlai-tools.json', import.meta.url), 'utf8'));
const byName = Object.fromEntries(tools.map((t) => [t.name, t]));
const validator = new AjvJsonSchemaValidator();
const valid = (name, args) => validator.getValidator(byName[name].parameters)(args).valid;

test('five compact desktop tools cover windows, view, act, recall, and experience', () => {
  const cases = {
    jianlai_windows: { action: 'list', query: 'Editor', observe: true, background: false, maxEdge: 0 },
    jianlai_view: { target: 'desktop', controls: true, maxItems: 60 },
    jianlai_act: {
      snapshotId: 's',
      imageId: 'i',
      background: false,
      actions: [
        { action: 'click', id: 3 },
        { action: 'type', id: 4, text: 'hello', clear: true, submit: true },
        { action: 'scroll', delta: 6 },
        { action: 'dismiss' },
        { action: 'press', key: 'Ctrl+C' },
      ],
    },
    jianlai_recall: { imagePath: 'old.jpg' },
    jianlai_experience: {
      action: 'save',
      recordingId: 'demo',
      scope: 'Editor',
      task: 'Open',
      conditions: ['Home'],
      steps: ['Open'],
      checks: ['Visible'],
      evidence: 'Visible',
      redacted: true,
    },
  };
  assert.deepEqual(Object.keys(byName).sort(), Object.keys(cases).sort());
  for (const [name, args] of Object.entries(cases)) {
    assert.equal(valid(name, args), true, name);
    assert.equal(valid(name, { ...args, operation: 'act' }), false, `${name} rejects stray operation`);
  }
  assert.equal(valid('jianlai_windows', { action: 'activate', windowId: 42 }), true);
  assert.equal(valid('jianlai_act', { actions: [{ action: 'wait', ms: 10000 }], feedback: 'none' }), true);
  assert.equal(valid('jianlai_act', { actions: [{ action: 'press', key: 'Enter' }] }), true);
});

test('jianlai_view unifies full screenshot, zoom, and screenshot=false UIA control queries', () => {
  for (const args of [
    {},
    { target: 'desktop', background: false, maxEdge: 1200 },
    { target: 'window', windowId: 42, filter: '保存' },
    { target: 'monitor', monitorId: 0, controls: false },
    { target: 'zoom', zoom: { id: 5 } },
    { snapshotId: 's', imageId: 'i', zoom: { x: 0, y: 1, width: 20, height: 30, space: 'image' } },
    { windowId: 42, zoom: { x: 0, y: 1, width: 20, height: 30, space: 'source' } },
    { screenshot: false, windowId: 42, depth: 5, maxItems: 100, filter: '确定' },
    { screenshot: false, snapshotId: 's', imageId: 'i', x: 10, y: 20 },
  ]) {
    assert.equal(valid('jianlai_view', args), true, JSON.stringify(args));
  }
  for (const bad of [
    { depth: 0 },
    { depth: 7 },
    { maxItems: 0 },
    { maxItems: 121 },
    { x: -1 },
    { filter: 'x'.repeat(61) },
    { zoom: { id: 0 } },
    { zoom: { x: 0, y: 0, width: 0, height: 10 } },
    { target: 'invalid' },
  ]) {
    assert.equal(valid('jianlai_view', bad), false, JSON.stringify(bad));
  }
});

test('jianlai_act validates bounds, screen coordinateSpace, and wait cap up to 10000ms', () => {
  assert.deepEqual(byName.jianlai_act.parameters.properties.coordinateSpace.enum, ['image', 'screen']);
  assert.equal(valid('jianlai_act', { coordinateSpace: 'screen', actions: [{ action: 'click', x: -5, y: 7 }] }), true);
  assert.equal(valid('jianlai_act', { coordinateSpace: 'px', actions: [{ action: 'click', x: 0, y: 0 }] }), false);
  for (const bad of [
    { actions: [] },
    { actions: [{ action: 'wait', ms: 10001 }] },
    { actions: [{ action: 'wait', ms: -1 }] },
    { actions: [{ action: 'click', id: 0 }] },
    { actions: [{ action: 'scroll', delta: 101 }] },
    { actions: [{ action: 'wait' }], feedback: 'invalid' },
    { actions: [{ action: 'wait' }], unexpected: true },
    { actions: Array(17).fill({ action: 'wait' }) },
  ]) {
    assert.equal(valid('jianlai_act', bad), false, JSON.stringify(bad));
  }
  for (const maxEdge of [0, 640, 1200, 3840]) assert.equal(valid('jianlai_windows', { maxEdge }), true);
  for (const maxEdge of [-1, 1, 639, 3841]) assert.equal(valid('jianlai_windows', { maxEdge }), false);
});

test('schemas stay within the shared runtime validator vocabulary', () => {
  const keys = new Set([
    'type', 'properties', 'required', 'additionalProperties', 'description',
    'enum', 'minimum', 'maximum', 'exclusiveMinimum', 'minLength', 'maxLength',
    'anyOf', 'minItems', 'maxItems', 'items',
  ]);
  const visit = (s) => {
    for (const key of Object.keys(s)) assert(keys.has(key), key);
    for (const v of Object.values(s.properties || {})) visit(v);
    for (const v of s.anyOf || []) visit(v);
    if (s.items) visit(s.items);
  };
  for (const tool of tools) visit(tool.parameters);
});
