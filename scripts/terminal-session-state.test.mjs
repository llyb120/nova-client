import assert from 'node:assert/strict';
import { test } from 'node:test';
import { readFileSync } from 'node:fs';
import { transformSync } from 'esbuild';
import { createSignal, flush } from '@solidjs/signals';

// Execute the shared close action with real batched signals; no PTY or process is started.
const source = readFileSync(new URL('../src/terminalSessions.ts', import.meta.url), 'utf8');
const action = source.slice(source.indexOf('export async function closeTerminalTab('), source.indexOf('if (typeof window'));
const close = new Function(transformSync(action.replace('export ', ''), { loader: 'ts' }).code + '; return closeTerminalTab;')();
function group(ids, selected) {
  const [tabs, setTabs] = createSignal(ids.map(id => ({ id, disposed: false, terminal: { dispose() {} }, host: { remove() {} } })));
  const [activeId, setActiveId] = createSignal(selected);
  return { tabs, setTabs, activeId, setActiveId };
}

test('closing an active terminal chooses a surviving neighbour before the batch commits', async () => {
  const g = group(['a', 'b', 'c'], 'c');
  await close(g, g.tabs()[2]);
  flush();
  assert.deepEqual(g.tabs().map(tab => tab.id), ['a', 'b']);
  assert.equal(g.activeId(), 'b');
  await close(g, g.tabs()[1]);
  flush();
  assert.equal(g.activeId(), 'a');
  await close(g, g.tabs()[0]);
  flush();
  assert.equal(g.activeId(), '');
});

test('same-frame terminal closes never resurrect disposed tabs or keep a removed selection', async () => {
  for (const selected of ['a', 'b', 'c']) {
    const g = group(['a', 'b', 'c'], selected);
    const [a, b] = g.tabs();
    await Promise.all([close(g, a), close(g, b)]);
    flush();
    assert.deepEqual(g.tabs().map(tab => tab.id), ['c']);
    assert.equal(g.activeId(), 'c');
  }
});
