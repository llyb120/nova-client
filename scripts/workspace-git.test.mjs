import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { createServer } from 'vite';
import { chromium } from 'playwright-core';

const name = `workspace-git-check-${process.pid}`;
let server, browser;
try {
  await fs.writeFile(`${name}.html`, `<div id="root"></div><script type="module" src="/${name}.tsx"></script>`);
  await fs.writeFile(`${name}.tsx`, `
    import { render } from 'solid-js/web';
    import WorkspaceGit from './src/components/WorkspaceGit';
    import { api } from './src/ipc';
    window.calls = [];
    api.workspaceGitStatus = async () => ({repo: 'test', files: [{path: 'large.txt', oldPath: null, index: ' ', worktree: 'M'}]});
    api.workspaceGitDiff = async (_id, _path, _staged, fullContext) => {
      window.calls.push(fullContext);
      return fullContext
        ? '@@ -1,11 +1,11 @@\\n' + Array.from({length: 10}, (_, i) => ' context ' + i + '\\n').join('') + '-old\\n+new\\n'
        : '@@ -8,4 +8,4 @@\\n context 7\\n context 8\\n context 9\\n-old\\n+new\\n';
    };
    render(() => <WorkspaceGit threadId="test" onOpen={() => {}} />, document.getElementById('root'));
  `);
  server = await createServer({ server: { host: '127.0.0.1', port: 5201, strictPort: false } });
  await server.listen();
  browser = await chromium.launch({ channel: 'msedge', headless: true });
  const page = await browser.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.goto(`${server.resolvedUrls.local[0]}${name}.html`);
  await page.locator('button.workspace-file[title="large.txt"]').click();
  await page.getByText('new', { exact: true }).waitFor();
  assert.deepEqual(await page.evaluate(() => window.calls), [false]);
  assert.equal(await page.getByText('context 0', { exact: true }).count(), 0);
  await page.getByRole('button', { name: '展开全部', exact: true }).click();
  await page.getByText('context 0', { exact: true }).waitFor();
  await page.getByText('context 5', { exact: true }).waitFor();
  assert.deepEqual(await page.evaluate(() => window.calls), [false, true]);
  await page.getByRole('button', { name: '折叠未变动', exact: true }).click();
  await page.getByText('new', { exact: true }).waitFor();
  assert.deepEqual(await page.evaluate(() => window.calls), [false, true, false]);
  assert.equal(await page.getByText('context 0', { exact: true }).count(), 0);
  assert.deepEqual(errors, []);
  console.log('Git preview loads compact diffs first and full context on demand');
} finally {
  await browser?.close();
  await server?.close();
  await Promise.all([`${name}.html`, `${name}.tsx`].map(path => fs.rm(path, { force: true })));
}
