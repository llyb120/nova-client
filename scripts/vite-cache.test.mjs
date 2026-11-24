import assert from 'node:assert/strict';
import { test } from 'node:test';
import { resolveConfig } from 'vite';

test('the Nova dev server does not share its dependency cache with test servers', async () => {
  const { cacheDir, optimizeDeps } = await resolveConfig({}, 'serve');
  assert.deepEqual(optimizeDeps.entries, ['index.html']);
  for (const options of [
    { configFile: false },
    { server: { port: 0 } },
    { optimizeDeps: { entries: ['fixture.html'] } },
  ]) {
    const entries = options.optimizeDeps?.entries;
    const fixture = await resolveConfig(options, 'serve');
    assert.notEqual(fixture.cacheDir, cacheDir, JSON.stringify(options));
    if (entries) assert.deepEqual(fixture.optimizeDeps.entries, entries);
  }
});
