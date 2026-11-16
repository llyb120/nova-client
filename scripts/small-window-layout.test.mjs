// 小窗口布局回归：侧栏内容保持可读、首页不被裁、底部工具条留在视口内。
// 用真实 app.css 渲染与 HomeView/Sidebar 同构的骨架，按视口断言计算样式。
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { chromium } from 'playwright-core';

const css = readFileSync(new URL('../src/app.css', import.meta.url), 'utf8');
const browser = await chromium.launch({ headless: true, channel: process.env.PLAYWRIGHT_CHANNEL ?? 'msedge' });
try {
  const page = await browser.newPage({ viewport: { width: 945, height: 591 } });
  await page.setContent(`<style>${css}</style>
    <div class="app">
    <div class="window-titlebar"><div class="titlebar-side">
      <div class="brand"><span class="brand-icon">N</span><span class="brand-name">Nova</span></div>
    </div><div class="titlebar-main"></div></div>
    <div class="app-body">
      <div class="sidebar-shell"><aside class="sidebar">
        <div class="sidebar-head">
          <div class="brand"><span class="brand-name">Nova</span><span class="brand-version">v0.1.708</span></div>
          <button class="new-thread-btn">+ 新对话</button>
        </div>
        <div class="thread-list"><div class="thread-group">
          <div class="group-label">
            <button class="group-project-toggle"><svg width="12" height="12"></svg>
              <span class="group-name">nova-client-master</span></button>
            <button class="group-delete">x</button>
          </div>
          <button class="thread-item"><span class="thread-content">
            <span class="thread-title">会话标题</span></span>
            <span class="thread-history-meta">01/01</span></button>
        </div></div>
        <div class="sidebar-foot"><button class="settings-btn">设置</button></div>
      </aside></div>
      <main class="home"><div class="home-center">
        <header class="home-intro">
          <div class="home-eyebrow">NOVA WORKSPACE</div>
          <h1 class="home-title">让想法，从这里开始。</h1>
          <p class="home-description">选择项目，描述任务，和 Nova 一起把想法变成现实。</p>
        </header>
        <div class="home-composer">
          <textarea class="composer-input" style="height:192px"></textarea>
          <div class="composer-bar"><span class="pill">自动</span>
            <div class="composer-actions"><button class="composer-btn send">↑</button></div></div>
        </div>
        <div class="recent"><div class="recent-label">最近会话</div>
          <button class="recent-item"><span class="recent-title">会话</span></button></div>
      </div></main>
    </div></div>`);

  const probe = () => page.evaluate(() => {
    const box = sel => {
      const el = document.querySelector(sel);
      if (!el) return null;
      const r = el.getBoundingClientRect();
      return { top: r.top, bottom: r.bottom, right: r.right, width: r.width, height: r.height };
    };
    const style = sel => getComputedStyle(document.querySelector(sel));
    const home = document.querySelector('.home');
    return {
      viewport: [innerWidth, innerHeight],
      sidebarShell: box('.sidebar-shell'),
      sidebarPanel: box('.sidebar'),
      titlebarSide: box('.titlebar-side'),
      footDisplay: style('.sidebar-foot').display,
      groupNameDisplay: style('.group-name').display,
      threadTitleDisplay: style('.thread-title').display,
      inputHeight: box('.composer-input').height,
      send: box('.composer-btn.send'),
      recent: box('.recent'),
      homeScroll: home.scrollHeight,
      homeClient: home.clientHeight,
    };
  });

  for (const [w, h] of [[945, 591], [760, 520], [640, 470]]) {
    await page.setViewportSize({ width: w, height: h });
    const p = await probe();
    assert.deepEqual(p.viewport, [w, h]);
    // 矮/窄屏不能把侧栏内容隐藏成一条空白壳（旧版就是这样：组名和会话标题一起消失）。
    assert.notEqual(p.groupNameDisplay, 'none', `${w}x${h}: 目录分组名保持可见`);
    assert.notEqual(p.threadTitleDisplay, 'none', `${w}x${h}: 会话标题保持可见`);
    // 首页不能纵向溢出（.home 是 overflow:hidden，一溢出就把工具条和最近会话裁掉）。
    assert.ok(p.homeScroll <= p.homeClient + 1, `${w}x${h}: 首页内容 ${p.homeScroll} 超出可视高 ${p.homeClient}`);
    assert.ok(p.send.bottom <= h, `${w}x${h}: 发送按钮 ${p.send.bottom} 超出视口 ${h}`);
    // 输入框 inline 高度固定 192px，矮屏必须被 max-height 压回。
    assert.ok(p.inputHeight <= 140, `${w}x${h}: 输入框仍高 ${p.inputHeight}px`);
    assert.ok(p.recent.height > 0, `${w}x${h}: 最近会话被压缩到不可见`);
    // 侧栏内层不能比外壳宽（否则盖住主区域），标题栏左列也必须与侧栏同宽。
    assert.ok(p.sidebarPanel.width <= p.sidebarShell.width + 1, `${w}x${h}: 侧栏面板 ${p.sidebarPanel.width} 宽于外壳 ${p.sidebarShell.width}`);
    assert.ok(Math.abs(p.titlebarSide.width - p.sidebarShell.width) < 1, `${w}x${h}: 标题栏左列 ${p.titlebarSide.width} 与侧栏 ${p.sidebarShell.width} 错位`);
    // 设置/成就入口不能被紧凑化隐藏。
    assert.notEqual(p.footDisplay, 'none', `${w}x${h}: 侧栏底部入口被隐藏`);
  }
  assert.ok((await probe()).sidebarShell.width <= 216, '窄窗侧栏让出宽度');

  // 狭窄的聊天区（如并排打开侧栏）仍要容纳新增的缓存命中率，不能横向溢出。
  await page.setContent(`<style>${css}</style><div class="composer"><div class="composer-bar">
    <div class="sel wide"><span class="pill"><span class="pill-text">Example long model</span></span></div>
    <span class="composer-run-stats"><span class="composer-run-dot"></span><span>1:23</span>
      <span class="composer-run-sep">·</span><span>上下文 128K/1M</span><span class="composer-run-sep">·</span>
      <span>≈35 tok/s</span><span class="composer-run-sep">·</span><span>50.0%</span></span>
    <div class="composer-actions"><button class="composer-btn send">↑</button></div></div></div>`);
  for (const width of [220, 260, 300, 380]) {
    const box = await page.evaluate(width => {
      const composer = document.querySelector('.composer');
      composer.style.width = `${width}px`;
      const bar = document.querySelector('.composer-bar').getBoundingClientRect();
      const stats = document.querySelector('.composer-run-stats');
      return { barRight: bar.right, statsRight: stats.getBoundingClientRect().right, scroll: stats.scrollWidth, client: stats.clientWidth };
    }, width);
    assert.ok(box.statsRight <= box.barRight + 1 && box.scroll <= box.client + 1, `${width}px: 统计栏横向溢出`);
  }
  // 许多后台任务时，标题保持可见，列表而非整个聊天区域滚动。
  await page.setContent(`<style>${css}</style><section class="background-tasks" style="width:260px">
    <div class="background-tasks-head"><span class="background-tasks-title">后台任务</span><span class="background-tasks-count">12 运行中</span><button class="background-tasks-toggle">查看已结束 3</button></div>
    <div class="background-tasks-list">${'<div class="background-tasks-row running"><span class="background-tasks-command">npm run very-long-task-command-with-output</span><span class="background-tasks-meta">运行 1m 15s · #9</span><button class="background-tasks-stop">停止</button></div>'.repeat(12)}</div>
  </section>`);
  for (const width of [220, 260, 380]) {
    const box = await page.evaluate(width => {
      const panel = document.querySelector('.background-tasks');
      panel.style.width = `${width}px`;
      const list = panel.querySelector('.background-tasks-list');
      return { height: panel.getBoundingClientRect().height, scrollHeight: list.scrollHeight, clientHeight: list.clientHeight,
        panelScrollWidth: panel.scrollWidth, panelClientWidth: panel.clientWidth };
    }, width);
    assert.ok(box.height <= Math.min(192, 0.28 * 470) + 1, `${width}px: 后台面板超过最大高度`);
    assert.ok(box.scrollHeight > box.clientHeight, `${width}px: 后台任务列表应可滚动`);
    assert.ok(box.panelScrollWidth <= box.panelClientWidth + 1, `${width}px: 后台面板横向溢出`);
  }
  console.log('small window layout checks passed (945x591, 760x520, 640x470; narrow composer stats; bounded background tasks)');
} finally {
  await browser.close();
}
