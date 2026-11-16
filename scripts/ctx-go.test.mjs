import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { scanSource } from './ctx-index.mjs';
const files = JSON.parse(readFileSync(new URL('./fixtures/polaris-go.json', import.meta.url), 'utf8'));

test('Go scanner recognizes real declarations, not locals or raw SQL', () => {
  const entry = scanSource(files['services/entry.go'], 'services/entry.go');
  assert.deepEqual(entry.syms.map(s => s.name), ['Table','Select','Helper','GetMetricTableSqlFieldStringBySouce']);
  assert.deepEqual([entry.syms[1].ln, entry.syms[1].end], [6,12]);
  assert.deepEqual(entry.imports, [{name:'sqlstore',from:'example.test/shop/dao'}]);
  const dao = scanSource(files['dao/metrics.go'], 'dao/metrics.go');
  assert.deepEqual(dao.syms.map(s => s.name), ['First','Second','metricTableAlineaPconsoleSelectField','rawSQL','unused','Select','Generic']);
  assert.deepEqual(dao.syms.slice(0,4).map(s => [s.ln,s.end]), [[2,5],[2,5],[7,10],[11,13]]);
});
