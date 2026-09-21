import assert from 'node:assert/strict';
import { writeFile, rm, access } from 'node:fs/promises';
import { createServer } from 'vite';
import { chromium } from 'playwright-core';
import { build } from 'esbuild';

const name = 'knowledge-check-' + process.pid;
const route = (id, task, conditions, steps) => ({ id, task, conditions, steps, checks: ['核对结果'], pitfalls: [], confidence: 'initial', successes: 1 });
const data = [
  { tool: 'chrome', scope: 'https://example.com', graph: { routes: [
    route('a', '完成归档', ['已登录项目'], ['打开菜单', '查看列表', '核对数据', '导出文件', '保存文件']),
    route('b', '完成归档', ['从搜索进入'], ['搜索记录', '查看列表', '导出文件', '保存文件']),
    route('c', '修订资料', ['已登录项目'], ['打开菜单', '查看列表', '编辑资料', '查看列表']),
    route('d', '连续检查', ['已登录项目'], ['核对数据', '核对数据']),
  ] } },
  { tool: 'jianlai', scope: '财务工作台', graph: { routes: [route('a', '完成归档', ['已打开报表'], ['打开菜单', '选择年度', '生成汇总', '核对金额', '导出年度汇总'])] } },
  { tool: 'webview', scope: 'https://other.example.com', graph: { routes: [route('a', '提交审批', ['已进入审批'], ['打开菜单', '填写说明', '提交审批'])] } },
];
const bundle = await build({ entryPoints: ['src/knowledgeGraph.ts'], bundle: true, write: false, format: 'esm', platform: 'node' });
const { buildKnowledgeGraph, graphEdgePath } = await import('data:text/javascript;base64,' + Buffer.from(bundle.outputFiles[0].text).toString('base64'));
const graph = buildKnowledgeGraph(data);
assert.equal(graph.starts.length, 4);
assert.equal(graph.routes.length, 6);
assert.equal(graph.nodes.filter(node => node.title === '打开菜单').length, 3, 'Different sources must keep distinct action nodes');
const shared = graph.nodes.find(node => node.title === '查看列表');
assert.equal(graph.nodes.filter(node => node.title === '查看列表').length, 1);
assert.equal(graph.edges.filter(edge => edge.target === shared.id).length, 3, 'Different starts and return paths converge');
assert.equal(graph.nodes.filter(node => node.kind === 'goal' && node.title === '完成归档').length, 1);
assert.equal(graph.nodes.find(node => node.kind === 'goal' && node.title === '完成归档').routeKeys.length, 3);
const byId = new Map(graph.nodes.map(node => [node.id, node]));
for (const edge of graph.edges) {
  assert.ok(byId.has(edge.source) && byId.has(edge.target));
  for (const occurrence of edge.occurrences) {
    const route = graph.routes.find(route => route.key === occurrence.routeKey);
    assert.equal(route.nodeIds[occurrence.step - 1], edge.source);
    assert.equal(route.nodeIds[occurrence.step], edge.target);
  }
  assert.ok(!/NaN|Infinity/.test(graphEdgePath(edge, byId, true)));
}
const self = graph.edges.find(edge => edge.source === edge.target);
assert.ok(self && graphEdgePath(self, byId, true).includes(' C '), 'Repeated operations draw a self-loop');
const edit = graph.nodes.find(node => node.title === '编辑资料');
assert.ok(graph.edges.some(edge => edge.source === shared.id && edge.target === edit.id));
assert.ok(graph.edges.some(edge => edge.source === edit.id && edge.target === shared.id));
for (let i = 0; i < graph.nodes.length; i++) for (let j = i + 1; j < graph.nodes.length; j++) {
  const a = graph.nodes[i], b = graph.nodes[j];
  assert.ok(Math.hypot(a.x - b.x, a.y - b.y) > a.radius + b.radius, 'Nodes must not overlap');
}
const large = buildKnowledgeGraph([{ tool: 'chrome', scope: 'https://large.example.com', graph: { routes: Array.from({ length: 300 }, (_, i) => route(String(i), '任务' + i, ['入口' + i], Array.from({ length: 12 }, (_, j) => '步骤' + i + '-' + j))) } }]);
assert.equal(large.starts.length, 300);
assert.equal(large.nodes.length, 4200, 'All 300 routes and 12 steps must survive without a depth or node cap');
assert.equal(large.edges.length, 3900);
assert.ok(large.nodes.every(node => Number.isFinite(node.x) && Number.isFinite(node.y)));
assert.deepEqual(buildKnowledgeGraph([]), { nodes: [], edges: [], routes: [], starts: [] });

let server, browser;
try {
  await writeFile(name + '.html', '<div id="root"></div><script type="module" src="/' + name + '.tsx"></script>');
  await writeFile(name + '.tsx', "import { render } from 'solid-js/web'; import View from './src/components/KnowledgeGraphView'; import './src/app.css'; render(() => <div style={{display:'flex',height:'100vh'}}><View /></div>, document.getElementById('root'));");
  server = await createServer({ server: { host: '127.0.0.1', port: 5189, strictPort: false } });
  await server.listen();
  let executablePath;
  for (const path of [process.env.TEST_BROWSER, 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', '/usr/bin/chromium'].filter(Boolean)) {
    try { await access(path); executablePath = path; break; } catch {}
  }
  browser = await chromium.launch({ executablePath, headless: true });
  const page = await browser.newPage({ viewport: { width: 1440, height: 960 } });
  page.setDefaultTimeout(10000); page.setDefaultNavigationTimeout(60000);
  const errors = []; page.on('pageerror', error => errors.push(error.message));
  await page.addInitScript(data => {
    window.__TAURI_INTERNALS__ = { invoke: async command => {
      if (command !== 'knowledge_graph') throw Error('Unexpected command: ' + command);
      if (window.failRead) throw Error('经验库忙');
      return window.empty ? [] : data;
    } };
  }, data);
  await page.goto('http://127.0.0.1:' + server.httpServer.address().port + '/' + name + '.html');
  await page.locator('.knowledge-node').first().waitFor();
  assert.equal(await page.locator('.knowledge-node').count(), graph.nodes.length);
  assert.equal(await page.locator('.knowledge-node[data-kind=start]').count(), graph.starts.length);
  assert.equal(await page.locator('.knowledge-edges > path').count(), graph.edges.length);
  assert.equal(await page.getByLabel('展开层数').count(), 0);
  await page.screenshot({ path: (process.env.TEMP || '/tmp') + '/nova-knowledge-graph.png' });
  await page.getByLabel('搜索全部图谱').fill('导出年度汇总');
  await page.getByRole('region', { name: '搜索结果' }).getByRole('button').click();
  await page.locator('.knowledge-node.selected').filter({ hasText: '导出年度汇总' }).waitFor();
  assert.equal(await page.locator('.knowledge-node').count(), graph.nodes.length, 'Search must not hide any graph');
  const position = await page.evaluate(() => {
    const target = document.querySelector('.knowledge-node.selected').getBoundingClientRect();
    const canvas = document.querySelector('.knowledge-canvas').getBoundingClientRect();
    return Math.hypot(target.x + target.width / 2 - canvas.x - canvas.width / 2, target.y + target.height / 2 - canvas.y - canvas.height / 2);
  });
  assert.ok(position < 2);
  await page.getByLabel('搜索全部图谱').fill('');
  await page.locator('.knowledge-detail .knowledge-next').filter({ hasText: '完成归档' }).click();
  await page.getByText('已到记录终点，请核对下方成功检查点。').waitFor();
  assert.equal(await page.getByRole('button', { name: '从此处展开' }).count(), 0);
  await page.locator('.knowledge-route summary').first().click();
  await page.getByRole('button', { name: '高亮这条路径' }).first().click();
  assert.ok(await page.locator('.knowledge-node.route-member').count() > 3);
  assert.equal(await page.locator('.knowledge-node').count(), graph.nodes.length);
  await page.getByRole('button', { name: '全图总览' }).click();
  const edgeButton = page.locator('.knowledge-edges > path').filter({ has: page.locator('title') }).first();
  await edgeButton.focus(); await edgeButton.press('Enter');
  await page.getByText('连接来源', { exact: true }).waitFor();
  await page.getByRole('button', { name: '全图总览' }).click();
  const before = await page.locator('.knowledge-world').getAttribute('style');
  await page.getByLabel('放大', { exact: true }).click();
  assert.notEqual(await page.locator('.knowledge-world').getAttribute('style'), before);
  await page.getByRole('button', { name: '适应画布' }).click();
  await page.getByLabel('搜索全部图谱').fill('不存在的步骤');
  await page.getByText('没有找到匹配的步骤或目标，请换个关键词。').waitFor();
  await page.getByLabel('搜索全部图谱').fill('');
  await page.evaluate(() => document.documentElement.dataset.theme = 'ink-light');
  await page.setViewportSize({ width: 700, height: 900 });
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await page.evaluate(() => { window.empty = true; });
  await page.getByRole('button', { name: '刷新记录' }).click();
  await page.getByText('还没有操作路径', { exact: true }).waitFor();
  await page.evaluate(() => { window.failRead = true; });
  await page.getByRole('button', { name: '刷新记录' }).click();
  await page.getByRole('alert').waitFor();
  await page.evaluate(() => { window.failRead = false; window.empty = false; });
  await page.getByRole('button', { name: '刷新记录' }).click();
  await page.locator('.knowledge-node').first().waitFor();
  assert.deepEqual(errors, []);
  console.log('Knowledge graph passed: all sources/starts/depths, convergence, cycles, self-loops, evidence, 4200 nodes, search, full-graph navigation, errors/retry.');
} finally {
  await browser?.close(); await server?.close();
  await Promise.all([rm(name + '.html', { force: true }), rm(name + '.tsx', { force: true })]);
}
