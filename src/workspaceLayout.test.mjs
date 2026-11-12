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
    const { workspaceLayout, setWorkspaceLayout, syncWorkspaceOpen } = await import(`${moduleUrl}#${mode}`);
    // 保存过的 open 不再跨会话/重启恢复：侧栏开合按会话记。
    assert.deepEqual({ ...workspaceLayout }, { ...saved, open: false });
    setWorkspaceLayout({ open: true });
    assert.deepEqual(JSON.parse(values.get('fd:workspaceLayout')), { ...saved, open: false });
    // 一个会话打开不带着别的会话一起开；切回来仍记得。
    syncWorkspaceOpen('a');
    setWorkspaceLayout({ open: true });
    syncWorkspaceOpen('b');
    assert.equal(workspaceLayout.open, false);
    syncWorkspaceOpen('a');
    assert.equal(workspaceLayout.open, true);
    // 首页没有侧栏：只保存当前会话的开合，不改它。
    syncWorkspaceOpen(null);
    assert.equal(workspaceLayout.open, true);
    syncWorkspaceOpen('a');
    assert.equal(workspaceLayout.open, true);
  }
  // 未知模式与越界宽度回落到默认值。
  const values = new Map([['fd:workspaceLayout', JSON.stringify({ open: true, widthRatio: 2, minimap: false, softWrap: false, mode: 'bogus' })]]);
  Object.defineProperty(globalThis, 'localStorage', { configurable: true, value: {
    getItem: key => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
  } });
  const { workspaceLayout } = await import(`${moduleUrl}#invalid`);
  assert.deepEqual({ ...workspaceLayout }, { open: false, widthRatio: null, minimap: false, softWrap: false, mode: 'files' });
} finally {
  if (storage) Object.defineProperty(globalThis, 'localStorage', storage);
  else delete globalThis.localStorage;
}
console.log('Workspace modes, session-scoped open state and unknown-mode fallback passed.');
