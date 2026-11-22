import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { AjvJsonSchemaValidator } from '@modelcontextprotocol/sdk/validation/ajv';

const tools = JSON.parse(await readFile(new URL('./browser-tools.json', import.meta.url), 'utf8'));
const byName = Object.fromEntries(tools.map(t => [t.name, t]));
const validator = new AjvJsonSchemaValidator();
const validates = (name, args) => validator.getValidator(byName[name].parameters)(args).valid;

test('six clean browser_* schemas cover navigation, view, pointer act, eval, debug, and site experience', () => {
  const cases = {
    browser_tabs: { action: 'open', url: 'https://example.com' },
    browser_view: { scope: 'viewport', screenshot: true },
    browser_act: {
      steps: [
        { op: 'click', id: 1 },
        { op: 'type', id: 2, text: 'hello', clear: true, submit: true },
        { op: 'hover', x: 100, y: 120 },
        { op: 'drag', x1: 10, y1: 10, x2: 200, y2: 200 },
        { op: 'scroll', dy: 500 },
        { op: 'press', key: 'Ctrl+C' },
        { op: 'wait', ms: 100 },
      ],
    },
    browser_eval: { code: 'document.title', view: false },
    browser_debug: { kind: 'network', level: 'error' },
    browser_experience: { action: 'search', scope: 'https://example.com', task: '*' },
  };
  assert.deepEqual(Object.keys(byName).sort(), Object.keys(cases).sort());
  for (const [name, args] of Object.entries(cases)) {
    assert.equal(validates(name, args), true, name);
    assert.equal(validates(name, { ...args, extra: 1 }), false, `${name} rejects unknown fields`);
  }
});

test('browser_act and browser_view reject invalid arguments cleanly', () => {
  assert.equal(validates('browser_act', { steps: [] }), false);
  assert.equal(validates('browser_act', { steps: [{ op: 'invalid' }] }), false);
  assert.equal(validates('browser_act', { steps: [{ op: 'click', id: 0 }] }), false);
  assert.equal(validates('browser_view', { zoom: { x: 0, y: 0, width: 0, height: 10 } }), false);
  assert.equal(validates('browser_view', { zoom: { id: 5 } }), true);
  assert.equal(validates('browser_tabs', { action: 'list', query: 'github' }), true);
});

test('bundled schemas use only runtime automation_schema supported keywords', () => {
  const keys = new Set([
    'type', 'properties', 'required', 'additionalProperties', 'description', 'enum',
    'minimum', 'maximum', 'exclusiveMinimum', 'minLength', 'maxLength', 'anyOf', 'minItems', 'maxItems', 'items',
  ]);
  const visit = s => {
    for (const key of Object.keys(s)) assert(keys.has(key), `unsupported schema key: ${key}`);
    if (s.type) assert(['object', 'string', 'boolean', 'integer', 'number', 'array'].includes(s.type), s.type);
    for (const child of Object.values(s.properties || {})) visit(child);
    for (const child of s.anyOf || []) visit(child);
    if (s.items) visit(s.items);
  };
  for (const tool of tools) visit(tool.parameters);
});
