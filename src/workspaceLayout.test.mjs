import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// Run with: node src/workspaceLayout.test.mjs
const { outputFiles } = await build({
  entryPoints: [fileURLToPath(new URL('./workspaceLayout.ts', import.meta.url))],
  bundle: true, platform: 'node', format: 'esm', write: false,
});
const moduleUrl = `data:text/javascript;base64,${Buffer.from(outputFiles[0].contents).toString('base64')}`;
const storage = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
try {
  for (const mode of ['browser', 'files', 'artifacts', 'git', 'terminal']) {
    const saved = { open: true, widthRatio: .5, minimap: false, softWrap: false, mode };
    const values = new Map([['fd:workspaceLayout', JSON.stringify(saved)]]);
    Object.defineProperty(globalThis, 'localStorage', { configurable: true, value: {
      getItem: key => values.get(key) ?? null,
      setItem: (key, value) => values.set(key, value),
    } });
    const { workspaceLayout, setWorkspaceLayout } = await import(`${moduleUrl}#${mode}`);
    const expected = { ...saved, mode: mode === 'browser' ? 'files' : mode };
    assert.deepEqual({ ...workspaceLayout }, expected);
    setWorkspaceLayout({ open: false });
    assert.deepEqual(JSON.parse(values.get('fd:workspaceLayout')), { ...expected, open: false });
  }
} finally {
  if (storage) Object.defineProperty(globalThis, 'localStorage', storage);
  else delete globalThis.localStorage;
}
console.log('Workspace modes and legacy browser-mode fallback passed.');
