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
    const { workspaceLayout, setWorkspaceLayout, syncWorkspaceOpen, openWorkspaceBrowser, clearPendingWorkspaceBrowsers } = await import(`${moduleUrl}#${mode}`);
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
    // 后台会话打开网页不抢画面；切回来时打开浏览器，之后允许手动关闭。
    setWorkspaceLayout({ open: false, mode: 'files' });
    openWorkspaceBrowser('b', 'a');
    assert.equal(workspaceLayout.open, false);
    assert.equal(workspaceLayout.mode, 'files');
    syncWorkspaceOpen('b');
    assert.equal(workspaceLayout.open, true);
    assert.equal(workspaceLayout.mode, 'browser');
    setWorkspaceLayout({ open: false });
    syncWorkspaceOpen('a');
    syncWorkspaceOpen('b');
    assert.equal(workspaceLayout.open, false);
    openWorkspaceBrowser('b', 'b');
    assert.equal(workspaceLayout.open, true);
    // 关闭无头模式后，不再兑现后台尚未显示的打开请求。
    setWorkspaceLayout({ open: false });
    syncWorkspaceOpen('a');
    setWorkspaceLayout({ mode: 'files' });
    openWorkspaceBrowser('b', 'a');
    clearPendingWorkspaceBrowsers();
    syncWorkspaceOpen('b');
    assert.equal(workspaceLayout.open, false);
    assert.equal(workspaceLayout.mode, 'files');
    // 会话刚切换但布局 effect 尚未执行时，前台通知也不能被旧状态覆盖。
    openWorkspaceBrowser('a', 'a');
    syncWorkspaceOpen('a');
    assert.equal(workspaceLayout.open, true);
    assert.equal(workspaceLayout.mode, 'browser');
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
