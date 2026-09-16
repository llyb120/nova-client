// This probe is intentionally restricted to one generated local page; it never enumerates business-page contents.
let running = false;
async function upload(report, config) {
  let uploaded = false, uploadError;
  try {
    const response = await fetch(`${config.origin}/result`, { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${config.token}` }, body: JSON.stringify(report), signal: AbortSignal.timeout(10000) });
    if (!response.ok) throw Error(`本地服务 HTTP ${response.status}`);
    uploaded = true;
  } catch(error) { uploadError = String(error?.message || error); }
  const {screenshot, ...summary} = report;
  await chrome.storage.local.set({probeStatus:{...summary, uploaded, uploadError}});
  if (uploaded) await chrome.storage.local.remove('pendingReport');
  await chrome.action.setBadgeText({ text: report.passed ? (uploaded ? 'PASS' : 'LOCAL') : 'FAIL' });
}
async function run() {
  if (running) return;
  running = true;
  let config;
  const report = { passed: false, checks: {}, timingsMs: {}, userAgent: navigator.userAgent };
  let target, attached = false, popup;
  const assert = (condition, message) => { if (!condition) throw Error(message); };
  try {
    await chrome.storage.local.set({probeStatus:{running:true}});
    config = await (await fetch(chrome.runtime.getURL('config.json'))).json();
    const tabs = await chrome.tabs.query({ url: `${config.origin}/*` });
    const existing = tabs.filter(t => t.url === config.fixtureUrl);
    assert(existing.length === 1, '请只保留一个本次生成的测试页，再点击“运行验证”。');
    target = { tabId: existing[0].id };
    const start = performance.now();
    await chrome.debugger.attach(target, '1.3'); attached = true;
    report.timingsMs.attach = Math.round(performance.now() - start);
    const cdp = async (method, params = {}) => {
      assert((await chrome.tabs.get(target.tabId)).url === config.fixtureUrl, 'Test tab navigated; refusing to operate on another page.');
      return chrome.debugger.sendCommand(target, method, params);
    };
    const evaluate = async (expression, contextId) => {
      const result = await cdp('Runtime.evaluate', { expression, ...(contextId ? { contextId } : {}), returnByValue: true, awaitPromise: true });
      assert(!result.exceptionDetails, JSON.stringify(result.exceptionDetails));
      return result.result.value;
    };
    const initial = await evaluate(`({before:window.beforeAttach,form:document.querySelector('input').value,session:localStorage.getItem(${JSON.stringify('nova-probe-' + config.nonce)})})`);
    assert(initial.before === 'already-open' && ['existing-form','Nova原生输入-美国'].includes(initial.form) && initial.session === 'existing-session', 'Existing page/session not preserved');
    report.checks.attachExistingTabWithoutRestart = true;
    const tree = await cdp('Page.getFrameTree');
    const world = await cdp('Page.createIsolatedWorld', { frameId: tree.frameTree.frame.id, worldName: 'nova-chrome-probe' });
    const script = await (await fetch(chrome.runtime.getURL('native_browser_page.js'))).text();
    await evaluate(script, world.executionContextId);
    let tick = performance.now();
    const dom = await evaluate('__novaWebview.observe("probe")', world.executionContextId);
    assert(dom.text.includes(`整页底部标记 ${config.nonce}`) && dom.documentSize.height > dom.viewport.height, 'Offscreen DOM missing');
    report.timingsMs.wholeDom = Math.round(performance.now() - tick);
    report.checks.reusedNovaWholeDomObserver = true;
    tick = performance.now();
    const input = dom.items.find(i => i.role === 'input');
    const point = await evaluate(`__novaWebview.prepare(${JSON.stringify(input.ref)})`, world.executionContextId);
    const click = async p => {
      await cdp('Input.dispatchMouseEvent', { type: 'mousePressed', x: p.x, y: p.y, button: 'left', clickCount: 1 });
      await cdp('Input.dispatchMouseEvent', { type: 'mouseReleased', x: p.x, y: p.y, button: 'left', clickCount: 1 });
    };
    await click(point);
    await cdp('Input.dispatchKeyEvent', { type: 'keyDown', key: 'a', code: 'KeyA', windowsVirtualKeyCode: 65, modifiers: 2 });
    await cdp('Input.dispatchKeyEvent', { type: 'keyUp', key: 'a', code: 'KeyA', windowsVirtualKeyCode: 65, modifiers: 2 });
    await cdp('Input.insertText', { text: 'Nova原生输入-美国' });
    const button = dom.items.find(i => i.name === '验证输入');
    await click(await evaluate(`__novaWebview.prepare(${JSON.stringify(button.ref)})`, world.executionContextId));
    const state = await evaluate('({text:document.querySelector("output").textContent,inputTrusted:window.inputTrusted,clickTrusted:window.clickTrusted})');
    assert(state.text === 'Nova原生输入-美国' && state.inputTrusted && state.clickTrusted, 'Native trusted input/click failed');
    report.timingsMs.inputAndFeedback = Math.round(performance.now() - tick);
    report.checks.trustedChineseInputAndClick = true;
    tick = performance.now();
    report.screenshot = (await cdp('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })).data;
    report.timingsMs.screenshot = Math.round(performance.now() - tick);
    report.checks.screenshot = !!report.screenshot;
    const link = dom.items.find(i => i.name === '新标签测试');
    await click(await evaluate(`__novaWebview.prepare(${JSON.stringify(link.ref)})`, world.executionContextId));
    for (let i = 0; i < 30; i++) {
      popup = (await chrome.tabs.query({ url: `${config.origin}/popup/${config.nonce}` })).find(t => t.openerTabId === target.tabId);
      if (popup) break;
      await new Promise(resolve => setTimeout(resolve, 100));
    }
    assert(popup, 'Popup tab/opener relationship missing');
    await chrome.tabs.update(target.tabId, { active: true });
    assert(await evaluate('document.querySelector("input").value') === 'Nova原生输入-美国', 'Switching tabs lost form state');
    report.checks.popupAndTabState = true;
    await chrome.debugger.detach(target); attached = false;
    await chrome.debugger.attach(target, '1.3'); attached = true;
    assert(await evaluate('document.querySelector("input").value') === 'Nova原生输入-美国', 'Reattachment lost page state');
    report.checks.detachAndReattach = true;
    report.passed = true;
  } catch (error) { report.error = String(error?.message || error); }
  finally {
    if (attached) await chrome.debugger.detach(target).catch(() => {});
    if (popup) await chrome.tabs.remove(popup.id).catch(() => {});
    // Persist before network I/O so a failed transfer never requires replaying browser actions.
    await chrome.storage.local.set({pendingReport:report});
    await upload(report, config);
    running = false;
  }
}
chrome.runtime.onMessage.addListener((message,sender,reply)=>{
  if (sender.id !== chrome.runtime.id || !['run-probe','retry-report'].includes(message?.type)) return;
  const task = async () => {
    if (running) throw Error('正在执行验证，请稍后');
    if (message.type === 'run-probe') {
      try { return await run(); } finally { running = false; }
    }
    running = true;
    try {
      const {pendingReport,probeStatus} = await chrome.storage.local.get(['pendingReport','probeStatus']);
      // Older versions retained only the summary. Upload that evidence without inventing a lost screenshot.
      const report = pendingReport ?? (typeof probeStatus?.passed === 'boolean' ? {...probeStatus,screenshotUnavailable:true} : null);
      if (!report) throw Error('没有待回传报告，请先运行验证');
      const {uploaded,uploadError,...evidence} = report;
      await upload(evidence, await (await fetch(chrome.runtime.getURL('config.json'))).json());
    } finally { running = false; }
  };
  task().then(()=>reply({done:true}),error=>reply({error:String(error)}));
  return true;
});
