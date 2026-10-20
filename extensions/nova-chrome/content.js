// Floating launchers for published Nova miniapps whose browser.match covers this page.
// Matching and the page URL come from the worker (sender.tab), never from page script.
(() => {
  if (window.top !== window || globalThis.__novaMiniapps) return;
  globalThis.__novaMiniapps = true;
  const KEY = 'miniappLauncherPosition';
  let host, generation = 0;
  const clamp = (value, max) => Math.max(0, Math.min(max, value));
  async function render() {
    const current = ++generation;
    let apps = [];
    try { apps = (await chrome.runtime.sendMessage({type: 'miniapp-match'}))?.apps || []; } catch { /* Worker restarting; next navigation retries. */ }
    const position = (await chrome.storage.local.get(KEY).catch(() => ({})))[KEY] || {right: 20, bottom: 120};
    if (current !== generation) return;
    host?.remove();
    host = undefined;
    if (!apps.length) return;
    host = document.createElement('nova-miniapps');
    host.style.cssText = 'all:initial;position:fixed;z-index:2147483647;';
    const place = () => { host.style.right = `${clamp(position.right, innerWidth - 56)}px`; host.style.bottom = `${clamp(position.bottom, innerHeight - 56)}px`; };
    place();
    const root = host.attachShadow({mode: 'closed'});
    const style = document.createElement('style');
    style.textContent = `
      .dock{display:flex;flex-direction:column;gap:8px;align-items:flex-end;font:13px/1.4 system-ui,sans-serif}
      button{width:44px;height:44px;border-radius:50%;border:2px solid #fff;background:linear-gradient(135deg,#6d74e8,#4a50c4);color:#fff;font:600 17px system-ui,sans-serif;box-shadow:0 4px 14px #0003;cursor:grab;touch-action:none;user-select:none}
      button:hover{filter:brightness(1.08)}button:focus-visible{outline:3px solid #9aa0ff;outline-offset:2px}
      button[aria-busy=true]{opacity:.6;cursor:progress}
      .tip{max-width:260px;padding:6px 10px;border-radius:8px;background:#242936;color:#fff;box-shadow:0 4px 14px #0003}`;
    const dock = document.createElement('div');
    dock.className = 'dock';
    const tip = document.createElement('div');
    tip.className = 'tip';
    tip.setAttribute('role', 'status');
    tip.hidden = true;
    let tipTimer;
    const say = text => { tip.textContent = text; tip.hidden = false; clearTimeout(tipTimer); tipTimer = setTimeout(() => { tip.hidden = true; }, 3500); };
    dock.append(tip);
    for (const app of apps) {
      const button = document.createElement('button');
      button.type = 'button';
      button.textContent = Array.from(app.name.trim() || '✦')[0];
      button.title = `Nova：${app.name}（可拖动）`;
      button.setAttribute('aria-label', `打开 Nova 应用：${app.name}`);
      let start, dragged = false;
      button.addEventListener('pointerdown', event => {
        start = {x: event.clientX, y: event.clientY, right: position.right, bottom: position.bottom};
        dragged = false;
        button.setPointerCapture(event.pointerId);
      });
      button.addEventListener('pointermove', event => {
        if (!start) return;
        const dx = event.clientX - start.x, dy = event.clientY - start.y;
        if (!dragged && Math.hypot(dx, dy) < 5) return;
        dragged = true;
        position.right = clamp(start.right - dx, innerWidth - 56);
        position.bottom = clamp(start.bottom - dy, innerHeight - 56);
        place();
      });
      button.addEventListener('pointerup', () => {
        start = undefined;
        if (dragged) void chrome.storage.local.set({[KEY]: {right: position.right, bottom: position.bottom}}).catch(() => {});
      });
      button.addEventListener('click', async () => {
        if (dragged) { dragged = false; return; }
        if (button.getAttribute('aria-busy') === 'true') return;
        button.setAttribute('aria-busy', 'true');
        try {
          const reply = await chrome.runtime.sendMessage({type: 'miniapp-launch', origin: app.origin, appId: app.appId});
          if (reply?.error) say(`未能打开：${reply.error}`);
        } catch (error) { say(`未能打开：${error?.message || error}`); }
        finally { button.removeAttribute('aria-busy'); }
      });
      dock.append(button);
    }
    root.append(style, dock);
    document.documentElement.append(host);
  }
  chrome.runtime.onMessage.addListener(message => { if (message?.type === 'miniapp-refresh') void render(); });
  void render();
})();
