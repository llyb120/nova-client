// Nova Browser Page Runtime v7 (CDP Isolated World "nova", zero page globals).
// Provides:
//   - Sleek Obsidian Cyber-Blade virtual mouse cursor (appears as soon as a session begins
//     operating, stays continuously visible across clicks/waits/screenshots/navigations with zero blink,
//     subtly parks off the text baseline when typing so characters stay legible, and dissolves cleanly when turn ends)
//   - Human minimum-jerk cubic Bezier kinematics (0ms instant jump when headless w/o live viewer)
//   - Occlusion- & clip-aware single-pass viewport DOM indexer with <select> option preview,
//     occlusion/disabled/virtual-list-drift guards in resolve(), and controlled-input clear/readback helpers
(() => {
  if (globalThis.__novaBrowser?.v === 7) return;

  const IGNORE = '[data-nova-ignore]';
  const isTop = window.top === window;
  const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));
  const compact = (s, max = 120) => String(s ?? '').replace(/\s+/g, ' ').trim().slice(0, max);

  // -------------------- Persistent-During-Task Virtual Mouse Cursor (Top Frame Only) --------------------
  // Never vanishes during model thinking or waits; only unmounts cleanly when the task ends
  // (dismiss() called via turn-end / abort / detach) or after a 3-minute crash-orphan safety net.
  const ORPHAN_TTL_MS = 180000;
  let host = null, shadow = null, layerEl = null, ptrEl = null, ptrBodyEl = null, rippleEl = null, badgeEl = null;
  let cx = Math.round((window.innerWidth || 1280) * 0.5);
  let cy = Math.round((window.innerHeight || 800) * 0.5);
  let hasPos = false;
  let orphanTimer = 0, unmountTimer = 0, badgeTimer = 0, pressTimer = 0;

  function destroyHostNow() {
    clearTimeout(orphanTimer); orphanTimer = 0;
    clearTimeout(unmountTimer); unmountTimer = 0;
    clearTimeout(badgeTimer); badgeTimer = 0;
    clearTimeout(pressTimer); pressTimer = 0;
    if (host) {
      try { host.hidePopover?.(); } catch {}
      try { host.remove(); } catch {}
    }
    host = shadow = layerEl = ptrEl = ptrBodyEl = rippleEl = badgeEl = null;
  }

  function keepAlive() {
    if (!host?.isConnected) return;
    clearTimeout(orphanTimer);
    clearTimeout(unmountTimer); unmountTimer = 0;
    layerEl?.classList.remove('leaving');
    orphanTimer = setTimeout(() => dismissCursor(false), ORPHAN_TTL_MS);
  }

  function dismissCursor(immediate = false) {
    clearTimeout(orphanTimer); orphanTimer = 0;
    clearTimeout(unmountTimer); unmountTimer = 0;
    if (!host?.isConnected) { destroyHostNow(); return; }
    if (immediate || document.hidden) { destroyHostNow(); return; }
    layerEl?.classList.add('leaving');
    unmountTimer = setTimeout(destroyHostNow, 240);
  }

  function showCursor(x, y) {
    if (!isTop) return;
    if (Number.isFinite(x) && Number.isFinite(y)) {
      cx = clamp(x, 0, Math.max(1, innerWidth - 1));
      cy = clamp(y, 0, Math.max(1, innerHeight - 1));
      hasPos = true;
    } else if (!hasPos) {
      cx = Math.round((window.innerWidth || 1280) * 0.5);
      cy = Math.round((window.innerHeight || 800) * 0.45);
      hasPos = true;
    }
    placeCursor(cx, cy, 0);
  }

  function setShotHidden(_hidden) {
    // Keep cursor continuously visible during screenshots so live viewers never see it blink.
    keepAlive();
  }

  function ensureCursor() {
    if (!isTop) return false;
    clearTimeout(unmountTimer); unmountTimer = 0;
    if (host?.isConnected) {
      layerEl?.classList.remove('leaving');
      host.style.setProperty('visibility', 'visible', 'important');
      keepAlive();
      return true;
    }
    if (!document.documentElement) return false;
    try { document.getElementById('__nova-cursor-host')?.remove(); } catch {}

    host = document.createElement('div');
    host.id = '__nova-cursor-host';
    host.setAttribute('data-nova-ignore', '1');
    host.setAttribute('aria-hidden', 'true');
    host.setAttribute('popover', 'manual');
    host.style.cssText = [
      'all:initial', 'position:fixed', 'inset:0', 'width:100vw', 'height:100vh',
      'margin:0', 'padding:0', 'border:0', 'background:transparent', 'overflow:visible',
      'pointer-events:none', 'user-select:none', 'z-index:2147483647', 'contain:layout style',
    ].join('!important;') + '!important;';
    shadow = host.attachShadow({ mode: 'closed' });

    const style = document.createElement('style');
    style.textContent = `
      *{box-sizing:border-box;pointer-events:none!important;user-select:none!important}
      .layer{position:fixed;inset:0;width:100vw;height:100vh;overflow:visible;
             opacity:1;transition:opacity 220ms cubic-bezier(.4,0,.2,1)}
      .layer.leaving{opacity:0}
      .layer.leaving .ptr-body{transform:scale(.82)!important;transition:transform 220ms ease-in}
      .ripple{position:fixed;left:0;top:0;width:28px;height:28px;margin:-14px 0 0 -14px;border-radius:50%;
              border:1.5px solid rgba(56,189,248,.92);
              box-shadow:0 0 0 1px rgba(255,255,255,.55),0 0 10px rgba(56,189,248,.45);
              background:radial-gradient(circle,rgba(56,189,248,.22) 0%,rgba(56,189,248,0) 70%);
              opacity:0;transform:translate3d(-999px,-999px,0) scale(.2)}
      @keyframes nova-tap{
        0%{opacity:.95;transform:var(--pos) scale(.22)}
        100%{opacity:0;transform:var(--pos) scale(1.42)}
      }
      .ripple.pop{animation:nova-tap 310ms cubic-bezier(.16,1,.3,1) forwards}
      .ptr{position:fixed;left:0;top:0;width:24px;height:26px;opacity:0;will-change:transform;
           transition:opacity 140ms ease-out}
      .ptr.on{opacity:1}
      .ptr.on.resting{opacity:.72}
      .ptr-body{width:24px;height:26px;transform-origin:1.2px 1.0px;will-change:transform;
                filter:drop-shadow(0 3px 7px rgba(2,6,23,.44)) drop-shadow(0 0 5px rgba(56,189,248,.24))}
      .ptr.press .ptr-body{transform:scale(.84)!important;transition:transform 70ms cubic-bezier(.2,0,0,1)}
      .ptr svg{display:block;width:24px;height:26px;overflow:visible}
      .badge{position:fixed;left:0;top:0;margin:20px 0 0 15px;padding:2px 7px;border-radius:6px;
             background:rgba(9,11,18,.86);color:#e2e8f0;
             font:600 10.5px/1.4 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;
             letter-spacing:.02em;white-space:nowrap;
             border:1px solid rgba(148,163,184,.28);
             box-shadow:0 4px 12px rgba(2,6,23,.32);opacity:0;transition:opacity 160ms ease}
      .badge.on{opacity:1}
    `;

    const layer = document.createElement('div');
    layer.className = 'layer';
    layer.innerHTML = `
      <div class="ripple"></div>
      <div class="ptr">
        <div class="ptr-body">
          <svg viewBox="0 0 26 28" aria-hidden="true">
            <defs>
              <linearGradient id="nb-l" x1="0%" y1="0%" x2="40%" y2="100%">
                <stop offset="0%" stop-color="#1e2538"/>
                <stop offset="100%" stop-color="#06080f"/>
              </linearGradient>
              <linearGradient id="nb-r" x1="0%" y1="0%" x2="100%" y2="85%">
                <stop offset="0%" stop-color="#3c4b68"/>
                <stop offset="48%" stop-color="#172033"/>
                <stop offset="100%" stop-color="#0b101d"/>
              </linearGradient>
              <linearGradient id="nb-sheen" x1="0%" y1="0%" x2="85%" y2="75%">
                <stop offset="0%" stop-color="#38bdf8" stop-opacity="0.42"/>
                <stop offset="55%" stop-color="#818cf8" stop-opacity="0.14"/>
                <stop offset="100%" stop-color="#6366f1" stop-opacity="0"/>
              </linearGradient>
              <linearGradient id="nb-spine" x1="0%" y1="0%" x2="60%" y2="100%">
                <stop offset="0%" stop-color="#ffffff" stop-opacity="0.95"/>
                <stop offset="45%" stop-color="#38bdf8" stop-opacity="0.88"/>
                <stop offset="100%" stop-color="#6366f1" stop-opacity="0.22"/>
              </linearGradient>
            </defs>
            <g transform="translate(-1.2,-1.0)">
              <path d="M1.2 1.0 L3.1 21.8 L8.7 15.6 L18.2 14.1 Z"
                    fill="#ffffff" stroke="#ffffff" stroke-width="2.1" stroke-linejoin="round"/>
              <path d="M1.2 1.0 L3.1 21.8 L8.7 15.6 Z" fill="url(#nb-l)"/>
              <path d="M1.2 1.0 L8.7 15.6 L18.2 14.1 Z" fill="url(#nb-r)"/>
              <path d="M1.2 1.0 L8.7 15.6 L18.2 14.1 Z" fill="url(#nb-sheen)"/>
              <path d="M1.5 1.8 L8.6 15.2" stroke="url(#nb-spine)" stroke-width="1.05" stroke-linecap="round"/>
              <path d="M2.2 1.9 L17.0 13.4" stroke="#e0f2fe" stroke-width="0.6" stroke-linecap="round" opacity="0.45"/>
            </g>
          </svg>
        </div>
      </div>
      <div class="badge"></div>
    `;

    shadow.append(style, layer);
    document.documentElement.append(host);
    try { host.showPopover?.(); } catch {}
    layerEl = layer;
    rippleEl = shadow.querySelector('.ripple');
    ptrEl = shadow.querySelector('.ptr');
    ptrBodyEl = shadow.querySelector('.ptr-body');
    badgeEl = shadow.querySelector('.badge');
    keepAlive();
    return true;
  }

  function placeCursor(x, y, tiltDeg = 0, resting = false) {
    cx = x; cy = y; hasPos = true;
    if (!ensureCursor()) return;
    ptrEl.classList.add('on');
    ptrEl.classList.toggle('resting', Boolean(resting));
    ptrEl.style.transform = `translate3d(${x.toFixed(1)}px,${y.toFixed(1)}px,0)`;
    if (ptrBodyEl && !ptrEl.classList.contains('press')) {
      ptrBodyEl.style.transform = Math.abs(tiltDeg) > 0.08 ? `rotate(${tiltDeg.toFixed(2)}deg)` : '';
    }
    badgeEl.style.transform = `translate3d(${x.toFixed(1)}px,${y.toFixed(1)}px,0)`;
  }

  // Subtly parks the pointer tip near the lower-right inner edge of a narrow input while typing
  // so the 24x26px dark blade body never obscures freshly typed characters in screenshots or live view.
  function markInput() {
    keepAlive();
    if (!isTop || !host?.isConnected) return;
    const el = activeDeep();
    if (el && isEditable(el)) {
      const r = el.getBoundingClientRect();
      if (r.width > 12 && r.height > 8 && r.height <= 46) {
        const px = clamp(cx + 8, r.left + 4, Math.max(r.left + 4, Math.min(innerWidth - 4, r.right - 6)));
        const py = clamp(Math.max(cy + 6, r.bottom - 5), r.top + 4, Math.max(r.top + 4, Math.min(innerHeight - 4, r.bottom - 2)));
        placeCursor(px, py, 0, true);
        return;
      }
    }
    ptrEl?.classList.add('resting');
  }

  function showBadge(text, ttl = 520) {
    if (!text) { badgeEl?.classList.remove('on'); return; }
    if (!ensureCursor()) return;
    clearTimeout(badgeTimer);
    badgeEl.textContent = text;
    badgeEl.style.transform = `translate3d(${cx.toFixed(1)}px,${cy.toFixed(1)}px,0)`;
    badgeEl.classList.add('on');
    if (ttl > 0) badgeTimer = setTimeout(() => badgeEl?.classList.remove('on'), ttl);
  }

  const nextFrame = () => new Promise(resolve => {
    let done = false;
    const finish = () => { if (!done) { done = true; clearTimeout(t); resolve(); } };
    const t = setTimeout(finish, 22);
    requestAnimationFrame(finish);
  });

  const pause = ms => new Promise(r => setTimeout(r, ms));

  async function moveTo(tx, ty, durationHintMs) {
    tx = clamp(Number(tx) || 0, 0, Math.max(1, innerWidth - 1));
    ty = clamp(Number(ty) || 0, 0, Math.max(1, innerHeight - 1));
    if (!ensureCursor()) { cx = tx; cy = ty; hasPos = true; return [tx, ty]; }
    showBadge('', 0);

    const sx = hasPos ? cx : clamp(tx + (Math.random() < 0.5 ? -1 : 1) * (90 + Math.random() * 80), 24, Math.max(24, innerWidth - 24));
    const sy = hasPos ? cy : clamp(ty + (65 + Math.random() * 75), 24, Math.max(24, innerHeight - 24));
    const dx = tx - sx, dy = ty - sy;
    const dist = Math.hypot(dx, dy);

    if (document.hidden || dist < 3 || durationHintMs === 0) {
      placeCursor(tx, ty, 0, false);
      return [tx, ty];
    }

    const fastBatch = durationHintMs > 0 && durationHintMs <= 125;
    const fitts = 95 + 68 * Math.log2(1 + dist / 28) + (Math.random() * 24 - 12);
    const dur = fastBatch
      ? clamp(Math.round(fitts * 0.62), 65, 175)
      : clamp(Math.round(fitts * 0.86), 95, 310);

    const ux = dx / dist, uy = dy / dist;
    const nx = -uy, ny = ux;
    const side = Math.random() < 0.5 ? 1 : -1;
    const arc1 = side * clamp(dist * (0.035 + Math.random() * 0.08), 3, 38);
    const arc2 = side * clamp(dist * (Math.random() * 0.06 - 0.018), -16, 24);
    const f1 = 0.27 + Math.random() * 0.1;
    const f2 = 0.65 + Math.random() * 0.1;

    const doOvershoot = dist > 210 && Math.random() < 0.45;
    const overPx = doOvershoot ? 2.5 + Math.random() * 3.0 : 0;
    const ex = clamp(tx + ux * overPx, 2, Math.max(2, innerWidth - 2));
    const ey = clamp(ty + uy * overPx, 2, Math.max(2, innerHeight - 2));

    const p1x = clamp(sx + dx * f1 + nx * arc1, 4, Math.max(4, innerWidth - 4));
    const p1y = clamp(sy + dy * f1 + ny * arc1, 4, Math.max(4, innerHeight - 4));
    const p2x = clamp(sx + dx * f2 + nx * arc2, 4, Math.max(4, innerWidth - 4));
    const p2y = clamp(sy + dy * f2 + ny * arc2, 4, Math.max(4, innerHeight - 4));

    const driftPhase = Math.random() * Math.PI * 2;
    const driftAmp = clamp(dist * 0.0035, 0, 1.3);
    const split = doOvershoot ? 0.84 : 1.0;

    let prevX = sx, tilt = 0;
    const start = performance.now();
    for (;;) {
      const p = clamp((performance.now() - start) / dur, 0, 1);
      let x, y;
      if (p <= split) {
        const u = clamp(p / split, 0, 1);
        const w = Math.pow(u, 0.85);
        const t = w * w * w * (10 + w * (-15 + 6 * w));
        const inv = 1 - t;
        const bx = inv * inv * inv * sx + 3 * inv * inv * t * p1x + 3 * inv * t * t * p2x + t * t * t * ex;
        const by = inv * inv * inv * sy + 3 * inv * inv * t * p1y + 3 * inv * t * t * p2y + t * t * t * ey;
        const drift = Math.sin(u * Math.PI * 2.4 + driftPhase) * driftAmp * (1 - u) * u * 3.2;
        x = bx + nx * drift;
        y = by + ny * drift;
      } else {
        const u = clamp((p - split) / (1 - split), 0, 1);
        const s = u * u * (3 - 2 * u);
        x = ex + (tx - ex) * s;
        y = ey + (ty - ey) * s;
      }
      const vx = x - prevX;
      prevX = x;
      const targetTilt = p > 0.92 ? 0 : clamp(vx * 0.38, -5.5, 5.5);
      tilt = tilt * 0.72 + targetTilt * 0.28;
      placeCursor(x, y, tilt);
      if (p >= 1) break;
      await nextFrame();
    }
    placeCursor(tx, ty, 0, false);
    if (!fastBatch) await pause(12);
    return [tx, ty];
  }

  function clickFx(x, y) {
    placeCursor(x, y, 0);
    if (!ensureCursor()) return;
    clearTimeout(pressTimer);
    ptrEl.classList.add('press');
    pressTimer = setTimeout(() => {
      ptrEl?.classList.remove('press');
      if (ptrBodyEl) ptrBodyEl.style.transform = '';
    }, 105);
    rippleEl.style.setProperty('--pos', `translate3d(${x.toFixed(1)}px,${y.toFixed(1)}px,0)`);
    rippleEl.classList.remove('pop');
    void rippleEl.offsetWidth;
    rippleEl.classList.add('pop');
  }

  function dragFx(phase, x, y) {
    placeCursor(x, y, 0);
    if (!ensureCursor()) return;
    if (phase === 'start') {
      ptrEl.classList.add('press');
    } else if (phase === 'end') {
      ptrEl.classList.remove('press');
      if (ptrBodyEl) ptrBodyEl.style.transform = '';
    }
  }

  // -------------------- DOM Helpers & Occlusion-Aware Indexer --------------------
  const nodes = new Map();
  let nextId = 0;

  const isIgnored = el => !el || el.nodeType !== 1 || Boolean(el.closest?.(IGNORE));
  const parentOf = el => el?.assignedSlot || el?.parentElement || el?.getRootNode?.()?.host || null;
  const containsDeep = (root, child) => {
    for (let n = child; n; n = parentOf(n)) if (n === root) return true;
    return false;
  };

  function activeDeep() {
    let el = document.activeElement;
    while (el?.shadowRoot?.activeElement) el = el.shadowRoot.activeElement;
    return el && !isIgnored(el) ? el : null;
  }

  function hitDeep(x, y) {
    let el = document.elementFromPoint(x, y);
    while (el?.shadowRoot) {
      const next = el.shadowRoot.elementFromPoint(x, y);
      if (!next || next === el) break;
      el = next;
    }
    return el && !isIgnored(el) ? el : null;
  }

  function isEditable(el) {
    if (!el || el.nodeType !== 1) return false;
    if (el.disabled || el.readOnly) return false;
    if (el.isContentEditable) return true;
    const tag = el.tagName;
    if (tag === 'TEXTAREA') return true;
    if (tag === 'INPUT') {
      const t = (el.type || 'text').toLowerCase();
      return !['button', 'submit', 'reset', 'checkbox', 'radio', 'file', 'image', 'hidden', 'range', 'color'].includes(t);
    }
    const role = el.getAttribute('role');
    return role === 'textbox' || role === 'searchbox' || role === 'combobox';
  }

  const GENERIC_ICON_RE = /^(close|x|×|✕|✖|clear|remove|delete|del|cancel|icon|more|info|help|arrow|down|up|chevron|dropdown|toggle|edit|search|reset|sort|关闭|删除|移除|清除|清空|取消|更多|详情|帮助|展开|收起|排序|搜索|编辑|重置)?$/i;

  function rawElementName(el) {
    const labelledBy = el.getAttribute('aria-labelledby');
    if (labelledBy?.trim()) {
      const root = el.getRootNode?.() || document;
      const t = labelledBy.trim().split(/\s+/)
        .map(id => (root.getElementById?.(id) || document.getElementById(id))?.textContent || '')
        .join(' ');
      if (t.trim()) return compact(t, 100);
    }
    const aria = el.getAttribute('aria-label');
    if (aria?.trim()) return compact(aria, 100);
    if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.tagName === 'SELECT') {
      const ph = el.getAttribute('placeholder');
      const labels = el.labels;
      if (labels?.length) {
        const lt = Array.from(labels, l => l.textContent || '').join(' ');
        if (lt.trim()) return compact(lt, 100);
      }
      if (el.tagName === 'INPUT' && (el.type === 'submit' || el.type === 'button') && el.value) {
        return compact(el.value, 100);
      }
      if (ph?.trim()) return compact(ph, 100);
      if (el.name) return compact(el.name, 80);
    }
    const txt = el.innerText || el.textContent || '';
    if (txt.trim()) return compact(txt, 100);
    const alt = el.getAttribute('title') || el.getAttribute('alt')
      || el.querySelector?.('img[alt]')?.getAttribute('alt')
      || el.querySelector?.('svg title')?.textContent || '';
    if (alt.trim()) return compact(alt, 100);
    const hint = el.getAttribute('data-testid') || el.getAttribute('data-icon') || el.getAttribute('name') || '';
    return compact(hint, 60);
  }

  function elementName(el, rect) {
    const base = rawElementName(el);
    const r = rect || el.getBoundingClientRect();
    if (r.width <= 42 && r.height <= 42 && GENERIC_ICON_RE.test(base)) {
      let p = el.parentElement;
      for (let depth = 0; depth < 2 && p && p !== document.body; depth++, p = p.parentElement) {
        const pr = p.getBoundingClientRect();
        if (pr.width > 360 || pr.height > 58) break;
        const inp = p.querySelector('input:not([type=hidden]):not([type=checkbox]):not([type=radio]),textarea');
        if (inp && inp !== el) {
          const inpHint = compact(inp.getAttribute('placeholder') || inp.getAttribute('aria-label') || inp.value || 'input', 36);
          return `${base || 'clear'} (in input: ${inpHint})`;
        }
        const pt = compact(p.innerText || p.textContent || '', 44);
        if (pt && pt.toLowerCase() !== base.toLowerCase() && pt.length <= 40) {
          return `${base || 'icon'} (in: ${pt})`;
        }
      }
    }
    return base;
  }

  function focusedInfo() {
    const el = activeDeep();
    if (!el || el === document.body || el === document.documentElement) return null;
    const isPwd = el.tagName === 'INPUT' && el.type === 'password';
    const rawVal = el.isContentEditable ? el.innerText : ('value' in el ? el.value : undefined);
    return {
      tag: el.tagName.toLowerCase(),
      role: el.getAttribute('role') || undefined,
      name: elementName(el) || undefined,
      editable: isEditable(el),
      value: rawVal === undefined ? undefined : (isPwd ? '***' : compact(rawVal, 120)),
    };
  }

  const SEMANTIC = [
    'a[href]', 'button', 'input:not([type=hidden])', 'textarea', 'select',
    '[role=button]', '[role=link]', '[role=tab]', '[role=checkbox]', '[role=radio]',
    '[role=switch]', '[role=combobox]', '[role=menuitem]', '[role=menuitemcheckbox]',
    '[role=menuitemradio]', '[role=option]', '[role=textbox]', '[role=searchbox]',
    '[contenteditable]:not([contenteditable=false])', 'summary', 'canvas', 'iframe',
    '[onclick]', '[tabindex]:not([tabindex="-1"])',
  ].join(',');

  const OVERLAY_SELECTOR = [
    '[role=dialog]', '[role=alertdialog]', '[role=listbox]', '[role=menu]', '[role=tree]',
    'dialog[open]', '[aria-modal=true]',
  ].join(',');

  const CONFIRM_RE = /\b(confirm|apply|ok|okay|save|done|submit)\b|确定|确认|应用|保存|完成|提交/i;
  const PERSISTENT_WIDGET_RE = /\b(chat|support|intercom|crisp|zendesk|drift|helpdesk|cookie|consent|toast|snackbar|fab|dock|toolbar|floating[-_]?bar)\b|客服|反馈|悬浮|助手/i;

  function collectRoots() {
    const out = [document];
    for (let i = 0; i < out.length && out.length < 32; i++) {
      const walker = document.createTreeWalker(out[i], NodeFilter.SHOW_ELEMENT);
      let n, budget = 0;
      while ((n = walker.nextNode()) && ++budget < 4000) {
        if (n.shadowRoot && !n.hasAttribute?.('data-nova-ignore')) out.push(n.shadowRoot);
      }
    }
    return out;
  }

  function createVisHelper(vw, vh) {
    const styleCache = new WeakMap();
    const ancCache = new WeakMap();

    const styleOf = el => {
      let s = styleCache.get(el);
      if (!s) { s = getComputedStyle(el); styleCache.set(el, s); }
      return s;
    };

    const ancestorClip = el => {
      if (!el || el === document.documentElement) return [0, 0, vw, vh];
      if (ancCache.has(el)) return ancCache.get(el);
      const pClip = ancestorClip(parentOf(el));
      if (!pClip) { ancCache.set(el, null); return null; }
      const st = styleOf(el);
      if (st.display === 'none' || st.visibility === 'hidden' || st.visibility === 'collapse'
          || parseFloat(st.opacity) <= 0.01 || el.inert) {
        ancCache.set(el, null);
        return null;
      }
      if (st.clipPath && /inset\(\s*100%|inset\(\s*50%|rect\(\s*0(px)?\s*,\s*0(px)?/i.test(st.clipPath)) {
        ancCache.set(el, null);
        return null;
      }
      let out = pClip;
      if (el !== document.body && /hidden|clip|scroll|auto/.test(`${st.overflow} ${st.overflowX} ${st.overflowY}`)) {
        const r = el.getBoundingClientRect();
        const l = Math.max(pClip[0], r.left);
        const t = Math.max(pClip[1], r.top);
        const ri = Math.min(pClip[2], r.right);
        const b = Math.min(pClip[3], r.bottom);
        if (ri - l < 2 || b - t < 2) { ancCache.set(el, null); return null; }
        out = [l, t, ri, b];
      }
      ancCache.set(el, out);
      return out;
    };

    const visibleRect = (el, scope) => {
      const r = el.getBoundingClientRect();
      if (r.width < 2 || r.height < 2) return null;
      const st = styleOf(el);
      const checkable = el.tagName === 'INPUT' && (el.type === 'checkbox' || el.type === 'radio');
      const isDisabledSemantic = Boolean(el.disabled || el.getAttribute?.('aria-disabled') === 'true')
        && (el.tagName === 'BUTTON' || el.tagName === 'INPUT' || el.getAttribute?.('role') === 'button');
      if (st.display === 'none' || st.visibility === 'hidden' || st.visibility === 'collapse' || el.inert) return null;
      if (parseFloat(st.opacity) <= 0.01 && !checkable) return null;
      if (st.pointerEvents === 'none' && !checkable && !isDisabledSemantic) return null;
      if (scope === 'all') {
        if (!ancestorClip(parentOf(el))) return null;
        return [r.left, r.top, r.right, r.bottom];
      }
      const clip = ancestorClip(parentOf(el));
      if (!clip) return null;
      const l = Math.max(clip[0], r.left);
      const t = Math.max(clip[1], r.top);
      const ri = Math.min(clip[2], r.right);
      const b = Math.min(clip[3], r.bottom);
      if (ri - l < 2 || b - t < 2) return null;
      return [l, t, ri, b];
    };

    const isUnoccluded = (el, l, t, ri, b, checkable, isDisabledSemantic) => {
      const w = ri - l, h = b - t;
      const probes = w > 24 ? [[0.5, 0.5], [0.22, 0.5], [0.78, 0.5]] : [[0.5, 0.5]];
      for (const [u, v] of probes) {
        const hit = hitDeep(l + w * u, t + h * v);
        if (!hit || containsDeep(el, hit) || containsDeep(hit, el)) return true;
        if (checkable) {
          const lbl = el.closest('label') || el.parentElement;
          if (lbl && lbl.getBoundingClientRect().height <= 64 && containsDeep(lbl, hit)) return true;
        }
        if (isDisabledSemantic) {
          // Disabled buttons often have pointer-events:none so hit lands on their immediate parent bar
          if (containsDeep(hit, el)) return true;
        }
      }
      return false;
    };

    return { styleOf, visibleRect, isUnoccluded };
  }

  // -------------------- Universal Floating Overlay / Popover Detection --------------------
  // Works for ANY CSS framework (CSS Modules, Tailwind, Emotion, AntD, Element, Radix, custom portals)
  // regardless of DOM nesting depth or class naming!
  function effectiveZ(el, styleOf) {
    let best = 0;
    for (let n = el, d = 0; n && n !== document.documentElement && d < 8; n = parentOf(n), d++) {
      const st = styleOf(n);
      const z = parseInt(st.zIndex, 10);
      if (Number.isFinite(z) && z > best) best = z;
    }
    return best;
  }

  function detectOverlay(allRoots, vis, vw, vh, floatingCandidates = []) {
    const candidates = [];
    const seen = new Set();

    const tryAdd = (el, semantic) => {
      if (!el || isIgnored(el) || seen.has(el) || el === document.body || el === document.documentElement) return;
      seen.add(el);
      const tag = el.tagName;
      if (tag === 'HEADER' || tag === 'NAV' || tag === 'ASIDE' || tag === 'FOOTER' || tag === 'MAIN') return;
      const roleLow = (el.getAttribute('role') || '').toLowerCase();
      if (!semantic && (roleLow === 'complementary' || roleLow === 'region' || roleLow === 'status' || roleLow === 'banner' || roleLow === 'navigation')) return;
      const vr = vis.visibleRect(el, 'viewport');
      if (!vr) return;
      const [l, t, ri, b] = vr;
      const w = ri - l, h = b - t;
      if (w < 110 || h < 64) return;
      if (w * h > vw * vh * 0.92 && !semantic) return;
      const st = vis.styleOf(el);
      const pos = st.position;
      const zNum = effectiveZ(el, vis.styleOf);
      if (!semantic) {
        if (pos !== 'fixed' && pos !== 'absolute') return;
        // Skip fixed full-height sidebars, full-width top/bottom bars, and narrow floating icon toolbars
        if ((h > vh * 0.82 && (l <= 2 || ri >= vw - 2)) || (w > vw * 0.85 && (t <= 2 || b >= vh - 2))) return;
        if (w <= 76 || h <= 76) return;
        // Skip persistent corner-docked chat/support/cookie/assistant floating widgets
        const widgetHint = `${el.id || ''} ${el.getAttribute('aria-label') || ''} ${el.getAttribute('title') || ''} ${typeof el.className === 'string' ? el.className : ''}`;
        if (pos === 'fixed' && PERSISTENT_WIDGET_RE.test(widgetHint)) return;
        if (pos === 'fixed' && b >= vh - 36 && (ri >= vw - 36 || l <= 36) && w <= 380 && h <= 160) return;
        const hasShadow = Boolean(st.boxShadow && st.boxShadow !== 'none');
        const hasBg = Boolean(st.backgroundColor && st.backgroundColor !== 'rgba(0, 0, 0, 0)' && st.backgroundColor !== 'transparent');
        const hasBorder = parseFloat(st.borderTopWidth) > 0 || parseFloat(st.borderBottomWidth) > 0;
        if (!hasShadow && !(hasBg && hasBorder)) return;
        if (zNum < 5 && !hasShadow && el.parentElement !== document.body) return;
      }
      // Must actually be topmost at a probe point inside itself
      const probes = [[0.5, 0.5], [0.25, 0.25], [0.75, 0.25], [0.5, 0.75]];
      const onTop = probes.some(([u, v]) => {
        const hit = hitDeep(l + w * u, t + h * v);
        return hit && containsDeep(el, hit);
      });
      if (!onTop) return;
      if (!el.querySelector('input,button,select,textarea,a[href],[role=option],[role=checkbox],[role=radio],[role=menuitem],[role=button],[tabindex]')) {
        return;
      }
      candidates.push({ el, rect: [Math.round(l), Math.round(t), Math.round(w), Math.round(h)], z: zNum, semantic, area: w * h });
    };

    for (const root of allRoots) {
      for (const el of root.querySelectorAll(OVERLAY_SELECTOR)) tryAdd(el, true);
    }
    for (const el of floatingCandidates) tryAdd(el, false);

    if (!candidates.length) return null;

    // Keep tightest floating overlay root (drop outer full-screen wrappers containing a smaller panel)
    const pruned = candidates.filter(c => !candidates.some(o => o !== c && containsDeep(c.el, o.el) && o.area < c.area * 0.85));
    // Also drop inner child sections that sit strictly inside a parent overlay panel of similar z
    const roots = pruned.filter(c => !pruned.some(o => o !== c && containsDeep(o.el, c.el) && o.z >= c.z));
    const list = roots.length ? roots : pruned;
    list.sort((a, b) => (b.z - a.z) || (Number(b.semantic) - Number(a.semantic)) || (b.area - a.area));
    const top = list[0];
    if (!top) return null;

    const roleAttr = (top.el.getAttribute('role') || '').toLowerCase();
    const kind = (roleAttr === 'dialog' || roleAttr === 'alertdialog' || top.el.tagName === 'DIALOG' || top.el.getAttribute('aria-modal') === 'true')
      ? 'dialog'
      : (roleAttr === 'menu' || roleAttr === 'listbox' ? 'menu' : 'popover');

    let hasConfirm = false;
    for (const btn of top.el.querySelectorAll('button,[role=button],input[type=submit],input[type=button]')) {
      if (btn.disabled || btn.getAttribute('aria-disabled') === 'true') continue;
      if (!vis.visibleRect(btn, 'viewport')) continue;
      if (CONFIRM_RE.test(rawElementName(btn))) { hasConfirm = true; break; }
    }

    const outsidePoint = findOutsidePoint(top.el, top.rect, vw, vh);
    return { el: top.el, kind, rect: top.rect, hasConfirm, outsidePoint };
  }

  function isNeutralBlankTarget(el) {
    if (!el || isIgnored(el)) return false;
    if (el.closest?.(SEMANTIC)) return false;
    const tag = el.tagName;
    if (tag === 'CANVAS' || tag === 'IMG' || tag === 'VIDEO' || tag === 'IFRAME' || tag === 'SVG' || tag === 'PATH') return false;
    if (el.isContentEditable) return false;
    const st = getComputedStyle(el);
    if (st.cursor === 'pointer' || st.cursor === 'text') return false;
    return true;
  }

  function findOutsidePoint(overlayEl, rect, vw = innerWidth || 1, vh = innerHeight || 1) {
    const [ol, ot, ow, oh] = rect || [0, 0, 0, 0];
    const or = ol + ow, ob = ot + oh;
    const pad = 28;
    const pts = overlayEl ? [
      [ol - pad, ot + Math.min(40, oh * 0.25)],
      [ol - pad, ot + oh * 0.5],
      [ol - pad, Math.max(24, ot - pad)],
      [or + pad, ot + Math.min(40, oh * 0.25)],
      [ol + ow * 0.5, ot - pad],
      [ol + ow * 0.5, ob + pad],
    ] : [];
    for (let gy = 0.08; gy <= 0.92; gy += 0.14) {
      for (let gx = 0.06; gx <= 0.94; gx += 0.12) {
        pts.push([vw * gx, vh * gy]);
      }
    }

    let best = null;
    for (const [rx, ry] of pts) {
      const x = Math.round(rx), y = Math.round(ry);
      if (x < 12 || y < 12 || x > vw - 12 || y > vh - 12) continue;
      if (overlayEl && x >= ol - 6 && x <= or + 6 && y >= ot - 6 && y <= ob + 6) continue;
      const hit = hitDeep(x, y);
      if (!hit || (overlayEl && containsDeep(overlayEl, hit))) continue;

      if (overlayEl) {
        const hst = getComputedStyle(hit);
        const hr = hit.getBoundingClientRect();
        const isBackdrop = hit !== document.body && hit !== document.documentElement
          && hst.position === 'fixed' && hr.width > vw * 0.7 && hr.height > vh * 0.7
          && !(hit.innerText || '').trim();
        if (isBackdrop) return [x, y];
      }

      if (!isNeutralBlankTarget(hit)) continue;
      const safeMargin = [[14, 0], [-14, 0], [0, 14], [0, -14]].every(([dx, dy]) => {
        const nh = hitDeep(x + dx, y + dy);
        return nh && (!overlayEl || !containsDeep(overlayEl, nh)) && isNeutralBlankTarget(nh);
      });
      const dist = overlayEl ? Math.hypot(x - (ol + ow * 0.5), y - (ot + oh * 0.5)) : Math.hypot(x - vw * 0.5, y - vh * 0.2);
      const score = (safeMargin ? 60 : 20)
        + (/^(BODY|HTML|MAIN|SECTION|DIV|HEADER|H1|H2|H3)$/.test(hit.tagName) ? 20 : 0)
        - dist * 0.03;
      if (!best || score > best.score) best = { x, y, score };
    }
    return best ? [best.x, best.y] : null;
  }

  function scanFloatingCandidates(allRoots, vis, vw, vh) {
    const out = [];
    for (const root of allRoots) {
      let scanned = 0;
      const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
      for (let el; (el = walker.nextNode()) && scanned < 2400;) {
        if (el.hasAttribute?.('data-nova-ignore')) continue;
        const tag = el.tagName;
        if (tag === 'SCRIPT' || tag === 'STYLE' || tag === 'SVG' || tag === 'PATH' || tag === 'NOSCRIPT'
            || tag === 'SPAN' || tag === 'A' || tag === 'P' || tag === 'IMG' || tag === 'INPUT' || tag === 'BUTTON') continue;
        const r = el.getBoundingClientRect();
        if (r.width < 110 || r.height < 64 || r.bottom <= 0 || r.top >= vh || r.right <= 0 || r.left >= vw) continue;
        scanned++;
        const st = vis.styleOf(el);
        if (st.position === 'fixed' || st.position === 'absolute') {
          out.push(el);
        }
      }
    }
    return out;
  }

  function overlayState() {
    const vw = innerWidth || 1, vh = innerHeight || 1;
    const allRoots = collectRoots();
    const vis = createVisHelper(vw, vh);
    const floating = scanFloatingCandidates(allRoots, vis, vw, vh);
    const ov = detectOverlay(allRoots, vis, vw, vh, floating);
    if (!ov) {
      return { open: false, safeBlankPoint: findOutsidePoint(null, null, vw, vh) };
    }
    return { open: true, kind: ov.kind, rect: ov.rect, hasConfirm: ov.hasConfirm, outsidePoint: ov.outsidePoint };
  }

  // -------------------- SPA Primary Scroll Container Detection --------------------
  let primaryScrollerRef = null;

  function findPrimaryScroller(scrollables, vw, vh) {
    const se = document.scrollingElement || document.documentElement;
    if (se && se.scrollHeight > (se.clientHeight || vh) + 32) {
      return { el: se, rect: [0, 0, vw, vh], isDocument: true };
    }
    let best = null;
    for (const { el, rect } of scrollables) {
      const [l, t, ri, b] = rect;
      const w = ri - l, h = b - t;
      const maxY = el.scrollHeight - el.clientHeight;
      if (maxY <= 32) continue;
      if (w < vw * 0.35 || h < vh * 0.35) continue;
      const score = w * h + (el.tagName === 'MAIN' ? vw * vh * 0.5 : 0);
      if (!best || score > best.score) best = { el, rect, isDocument: false, score };
    }
    if (best) return best;
    return { el: se, rect: [0, 0, vw, vh], isDocument: true };
  }

  function scrollTarget(id) {
    const vw = innerWidth || 1, vh = innerHeight || 1;
    let el = id !== undefined && id !== null ? nodes.get(Number(id))?.ref?.deref() : primaryScrollerRef?.deref();
    if (!el || !el.isConnected) {
      const allRoots = collectRoots();
      const vis = createVisHelper(vw, vh);
      const scrollables = [];
      for (const root of allRoots) {
        const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
        let n, budget = 0;
        while ((n = walker.nextNode()) && ++budget < 1800) {
          if (isIgnored(n) || n === document.scrollingElement) continue;
          const vr = vis.visibleRect(n, 'viewport');
          if (!vr) continue;
          const st = vis.styleOf(n);
          if (n.scrollHeight > n.clientHeight + 32 && /auto|scroll|overlay/.test(st.overflowY)) {
            scrollables.push({ el: n, rect: vr });
          }
        }
      }
      el = findPrimaryScroller(scrollables, vw, vh).el;
    }
    if (!el || el === document.scrollingElement || el === document.documentElement || el === document.body) {
      return { x: Math.round(vw * 0.5), y: Math.round(vh * 0.5), top: Math.round(scrollY), left: Math.round(scrollX), maxY: Math.max(0, Math.round((document.scrollingElement?.scrollHeight || vh) - vh)) };
    }
    const r = el.getBoundingClientRect();
    const l = clamp(r.left, 0, vw), t = clamp(r.top, 0, vh);
    const ri = clamp(r.right, 0, vw), b = clamp(r.bottom, 0, vh);
    // Prefer a point inside the scroller that isn't directly on a <canvas> (which may intercept wheel events)
    let px = Math.round((l + ri) * 0.5), py = Math.round((t + b) * 0.5);
    for (const [u, v] of [[0.5, 0.25], [0.85, 0.5], [0.15, 0.5], [0.5, 0.85], [0.5, 0.5]]) {
      const tx = Math.round(l + (ri - l) * u), ty = Math.round(t + (b - t) * v);
      const hit = hitDeep(tx, ty);
      if (hit && containsDeep(el, hit) && hit.tagName !== 'CANVAS') { px = tx; py = ty; break; }
    }
    return { x: px, y: py, top: Math.round(el.scrollTop), left: Math.round(el.scrollLeft), maxY: Math.max(0, Math.round(el.scrollHeight - el.clientHeight)) };
  }

  function verifyOrFallbackScroll(id, dx, dy, beforeTop, beforeLeft) {
    const el = id !== undefined && id !== null ? nodes.get(Number(id))?.ref?.deref() : primaryScrollerRef?.deref();
    const isDoc = !el || !el.isConnected || el === document.scrollingElement || el === document.documentElement || el === document.body;
    const readPos = () => isDoc
      ? [Math.round(scrollY), Math.round(scrollX), Math.max(0, Math.round((document.scrollingElement?.scrollHeight || innerHeight) - innerHeight))]
      : [Math.round(el.scrollTop), Math.round(el.scrollLeft), Math.max(0, Math.round(el.scrollHeight - el.clientHeight))];
    let [curTop, curLeft, maxY] = readPos();
    // If wheel event was swallowed by a chart/canvas/nested layer, scroll the container directly!
    if (curTop === beforeTop && curLeft === beforeLeft && (dy !== 0 || dx !== 0)) {
      if (isDoc) window.scrollBy({ top: dy, left: dx, behavior: 'instant' });
      else el.scrollBy({ top: dy, left: dx, behavior: 'instant' });
      [curTop, curLeft, maxY] = readPos();
    }
    return { beforeTop, afterTop: curTop, beforeLeft, afterLeft: curLeft, maxY };
  }

  function observe(opts = {}) {
    keepAlive();
    const started = performance.now();
    const scope = opts.scope === 'all' ? 'all' : 'viewport';
    const query = String(opts.query || '').trim().toLowerCase();
    const maxItems = clamp(Number(opts.maxItems) || 100, 1, 300);
    const maxText = clamp(Number(opts.maxTextChars ?? 3000), 0, 20000);
    const vw = innerWidth || 1, vh = innerHeight || 1;

    nodes.clear();
    nextId = Number(opts.idOffset) || 0;
    const allRoots = collectRoots();
    const vis = createVisHelper(vw, vh);

    const seen = new Set();
    const raw = [];
    const floatingCandidates = [];
    const scrollables = [];
    let hasIframes = false;

    for (const root of allRoots) {
      for (const el of root.querySelectorAll(SEMANTIC)) {
        if (isIgnored(el) || seen.has(el)) continue;
        if (el.tagName === 'IFRAME') hasIframes = true;
        seen.add(el);
        raw.push(el);
      }
      // Single TreeWalker collects: clickable divs/spans/lis/svgs, scrollable containers, AND floating overlay panels!
      let scanned = 0;
      const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
      for (let el; (el = walker.nextNode()) && scanned < 2200;) {
        if (el.hasAttribute?.('data-nova-ignore')) continue;
        const tag = el.tagName;
        if (tag === 'SCRIPT' || tag === 'STYLE' || tag === 'PATH' || tag === 'NOSCRIPT') continue;
        const vr = vis.visibleRect(el, 'viewport');
        if (!vr) continue;
        const rw = vr[2] - vr[0], rh = vr[3] - vr[1];
        if (rw < 6 || rh < 6) continue;
        scanned++;
        const st = vis.styleOf(el);

        // Collect floating overlay candidates (independent of class names or DOM depth!)
        if ((st.position === 'fixed' || st.position === 'absolute') && rw >= 110 && rh >= 64
            && tag !== 'SPAN' && tag !== 'A' && tag !== 'P' && tag !== 'IMG' && tag !== 'SVG' && tag !== 'INPUT' && tag !== 'BUTTON') {
          floatingCandidates.push(el);
        }

        const isScroll = el !== document.scrollingElement
          && ((el.scrollHeight > el.clientHeight + 8 && /auto|scroll|overlay/.test(st.overflowY))
            || (el.scrollWidth > el.clientWidth + 8 && /auto|scroll|overlay/.test(st.overflowX)));
        if (isScroll) scrollables.push({ el, rect: vr });

        if (seen.has(el)) continue;
        const isSmallIcon = rw <= 36 && rh <= 36 && (tag === 'SVG' || tag === 'I' || Boolean(el.querySelector('svg,img')));
        const isPtr = st.cursor === 'pointer' && rw * rh < vw * vh * 0.25
          && !el.querySelector(SEMANTIC)
          && !el.closest(SEMANTIC);
        if (isScroll || (isPtr && (el.childElementCount <= 3 || tag === 'LI' || isSmallIcon))) {
          const up = parentOf(el);
          // Allow small clickable SVG/icon buttons even if an ancestor has cursor:pointer, unless parent is also tiny
          if (isPtr && !isScroll && up && up.nodeType === 1 && vis.styleOf(up).cursor === 'pointer' && tag !== 'LI') {
            const ur = up.getBoundingClientRect();
            if (!isSmallIcon || (ur.width <= 44 && ur.height <= 44)) continue;
          }
          seen.add(el);
          raw.push(el);
        }
      }
    }

    const meaningfulFrame = isTop || (vw >= 240 && vh >= 180);
    const overlay = meaningfulFrame ? detectOverlay(allRoots, vis, vw, vh, floatingCandidates) : null;
    const primary = meaningfulFrame ? findPrimaryScroller(scrollables, vw, vh) : null;
    if (meaningfulFrame) primaryScrollerRef = primary?.el ? new WeakRef(primary.el) : null;

    if (overlay) {
      const inside = [], outside = [];
      for (const el of raw) (containsDeep(overlay.el, el) ? inside : outside).push(el);
      raw.length = 0;
      raw.push(...inside, ...outside);
    }

    const items = [];
    const overlayItemIds = [];
    let primaryScrollItemId = null;

    for (const el of raw) {
      if (items.length >= maxItems) break;
      const vr = vis.visibleRect(el, scope);
      if (!vr) continue;
      const [cl, ct, cr, cb] = vr;
      const rw = cr - cl, rh = cb - ct;
      const inView = cb > 0 && ct < vh && cr > 0 && cl < vw;
      if (scope === 'viewport' && !inView) continue;

      const st = vis.styleOf(el);
      const checkable = el.tagName === 'INPUT' && (el.type === 'checkbox' || el.type === 'radio');
      const disabled = Boolean(el.disabled || el.getAttribute?.('aria-disabled') === 'true');
      const isDisabledSemantic = disabled && (el.tagName === 'BUTTON' || el.tagName === 'INPUT' || el.getAttribute?.('role') === 'button');

      if (inView && !vis.isUnoccluded(el, cl, ct, cr, cb, checkable, isDisabledSemantic)) continue;

      const isScroll = el !== document.scrollingElement
        && ((el.scrollHeight > el.clientHeight + 8 && /auto|scroll|overlay/.test(st.overflowY))
          || (el.scrollWidth > el.clientWidth + 8 && /auto|scroll|overlay/.test(st.overflowX)));
      if ((el.tagName === 'SPAN' || el.tagName === 'DIV') && !el.hasAttribute('role') && !el.isContentEditable && el.querySelector('button,a[href],input,textarea,select')) {
        if (!isScroll) continue;
      }

      const name = elementName(el, { width: rw, height: rh });
      if ((el.tagName === 'DIV' || el.tagName === 'SPAN') && !name && !isEditable(el) && !isScroll && !el.querySelector('svg,img,canvas')) {
        continue;
      }

      const role = el.getAttribute('role') || (checkable ? el.type : el.tagName.toLowerCase());
      const href = el.tagName === 'A' ? compact(el.getAttribute('href') || '', 140) : '';
      if (query) {
        const hay = `${name} ${role} ${href} ${el.value || ''}`.toLowerCase();
        if (!hay.includes(query)) continue;
      }

      const id = ++nextId;
      nodes.set(id, { ref: new WeakRef(el), role, sig: rawElementName(el) });
      if (primary && !primary.isDocument && el === primary.el) primaryScrollItemId = id;
      const cxp = Math.round((cl + cr) * 0.5);
      const cyp = Math.round((ct + cb) * 0.5);
      const isPwd = el.tagName === 'INPUT' && el.type === 'password';
      const val = el.tagName === 'SELECT'
        ? compact(el.options?.[el.selectedIndex]?.text || el.value || '', 80)
        : (el.isContentEditable ? compact(el.innerText, 80)
          : (el.tagName === 'INPUT' && !checkable && el.type !== 'button' && el.type !== 'submit'
            ? (isPwd ? '***' : compact(el.value, 80))
            : (el.tagName === 'TEXTAREA' ? compact(el.value, 80) : '')));
      const checked = checkable ? Boolean(el.checked)
        : (el.getAttribute('aria-checked') === 'true' ? true
          : (el.getAttribute('aria-selected') === 'true' ? true : undefined));
      const expanded = el.getAttribute('aria-expanded');
      const inOverlay = Boolean(overlay && containsDeep(overlay.el, el));
      if (inOverlay) overlayItemIds.push(id);

      const entry = {
        id,
        role,
        name,
        x: cxp,
        y: cyp,
        rect: [Math.round(cl), Math.round(ct), Math.round(Math.max(0, rw)), Math.round(Math.max(0, rh))],
        inView,
      };
      if (val) entry.val = val;
      if (el.tagName === 'SELECT') {
        const optsList = Array.from(el.options || []).filter(o => !o.disabled).slice(0, 8).map(o => compact(o.text || o.value, 32));
        if (optsList.length) entry.opts = optsList;
      }
      if (checked !== undefined) entry.checked = checked;
      if (expanded === 'true' || expanded === 'false') entry.expanded = expanded === 'true';
      if (disabled) entry.disabled = true;
      if (isEditable(el)) entry.editable = true;
      if (isScroll) {
        entry.scroll = true;
        const maxY = Math.max(0, Math.round(el.scrollHeight - el.clientHeight));
        if (maxY > 8) entry.scrollY = [Math.round(el.scrollTop), maxY];
      }
      if (href) entry.href = href;
      if (inOverlay) entry.inOverlay = true;
      if (el.tagName === 'CANVAS') entry.canvas = [el.width, el.height];
      items.push(entry);
    }

    let text = '';
    if (maxText > 0) {
      const chunks = [];
      let used = 0, scannedTxt = 0;
      for (const root of allRoots) {
        const startNode = root === document ? (document.body || document.documentElement) : root;
        if (!startNode) continue;
        const walker = document.createTreeWalker(startNode, NodeFilter.SHOW_TEXT);
        for (let t; (t = walker.nextNode()) && scannedTxt++ < 8000 && used < maxText;) {
          const p = t.parentElement;
          if (!p || isIgnored(p)) continue;
          const pt = p.tagName;
          if (pt === 'SCRIPT' || pt === 'STYLE' || pt === 'NOSCRIPT') continue;
          const s = compact(t.nodeValue, 400);
          if (!s) continue;
          if (scope === 'viewport' && !vis.visibleRect(p, 'viewport')) continue;
          chunks.push(s);
          used += s.length + 1;
        }
      }
      text = chunks.join('\n').slice(0, maxText);
    }

    const pEl = primary?.el || document.scrollingElement || document.documentElement;
    const isDoc = !primary || primary.isDocument;
    const sy = Math.round(isDoc ? scrollY : (pEl?.scrollTop || 0));
    const sx = Math.round(isDoc ? scrollX : (pEl?.scrollLeft || 0));
    const sw = Math.round(pEl?.scrollWidth || vw);
    const sh = Math.round(pEl?.scrollHeight || vh);
    const ch = Math.round(isDoc ? vh : (pEl?.clientHeight || vh));
    const maxY = Math.max(0, sh - ch);

    const out = {
      url: location.href,
      title: document.title || '',
      readyState: document.readyState,
      viewport: [vw, vh, window.devicePixelRatio || 1],
      scroll: [sx, sy, sw, sh],
      scrollInfo: {
        y: sy,
        maxY,
        viewH: ch,
        totalH: sh,
        moreBelow: Math.max(0, maxY - sy),
        containerId: primaryScrollItemId,
      },
      cursor: hasPos ? [Math.round(cx), Math.round(cy)] : null,
      focused: focusedInfo(),
      hasIframes,
      items,
      text,
      observeMs: Math.round(performance.now() - started),
    };
    if (overlay) {
      out.overlay = {
        kind: overlay.kind,
        rect: overlay.rect,
        hasConfirm: overlay.hasConfirm,
        itemCount: overlayItemIds.length,
        outsidePoint: overlay.outsidePoint,
      };
    }
    return out;
  }

  // -------------------- O(1) Target Resolver + Form Helpers --------------------
  async function resolve(id, actionKind = '') {
    keepAlive();
    const rec = nodes.get(Number(id));
    const el = rec?.ref?.deref();
    if (!el || !el.isConnected) throw Error(`元素 [${id}] 已消失或页面已变化，请重新查看视图`);
    if ((actionKind === 'click' || actionKind === 'type') && (el.disabled || el.getAttribute?.('aria-disabled') === 'true')) {
      throw Error(`元素 [${id}] 当前处于禁用状态 (disabled)，无法操作`);
    }
    if (actionKind && rec?.sig && rec.sig.length >= 2 && !isEditable(el)
        && el.tagName !== 'SELECT' && el.tagName !== 'INPUT' && el.tagName !== 'TEXTAREA') {
      const curSig = rawElementName(el);
      if (curSig && curSig !== rec.sig && !curSig.includes(rec.sig) && !rec.sig.includes(curSig)
          && el.closest?.('[role=row],tr,[data-index],[data-row-key]')) {
        throw Error(`列表元素 [${id}] 内容已由 "${rec.sig}" 变为 "${curSig}"，请根据最新视图确认后再操作`);
      }
    }
    let r = el.getBoundingClientRect();
    if (r.bottom <= 0 || r.top >= innerHeight || r.right <= 0 || r.left >= innerWidth) {
      el.scrollIntoView({ block: 'center', inline: 'nearest', behavior: 'instant' });
      await nextFrame();
      r = el.getBoundingClientRect();
    }
    if (r.width <= 0 || r.height <= 0) throw Error(`元素 [${id}] 当前不可见`);
    const checkable = el.tagName === 'INPUT' && (el.type === 'checkbox' || el.type === 'radio');
    const assocLabel = checkable ? (el.closest('label') || el.labels?.[0] || null) : null;
    const rects = Array.from(el.getClientRects()).filter(q => q.width > 0 && q.height > 0);
    const list = rects.length ? rects.sort((a, b) => b.width * b.height - a.width * a.height) : [r];
    let best = null;
    const probes = [[0.5, 0.5], [0.25, 0.5], [0.75, 0.5], [0.5, 0.25], [0.5, 0.75], [0.2, 0.25], [0.8, 0.25], [0.2, 0.75], [0.8, 0.75]];
    for (const q of list.slice(0, 6)) {
      const l = clamp(q.left, 0, innerWidth), t = clamp(q.top, 0, innerHeight);
      const ri = clamp(q.right, 0, innerWidth), b = clamp(q.bottom, 0, innerHeight);
      if (ri <= l || b <= t) continue;
      for (const [u, v] of probes) {
        const px = l + (ri - l) * u, py = t + (b - t) * v;
        const hit = hitDeep(px, py);
        if (!hit || containsDeep(el, hit) || containsDeep(hit, el) || (assocLabel && containsDeep(assocLabel, hit))) {
          best = { x: px, y: py };
          break;
        }
      }
      if (best) break;
    }
    const cl = clamp(r.left, 0, innerWidth), ct = clamp(r.top, 0, innerHeight);
    const cr = clamp(r.right, 0, innerWidth), cb = clamp(r.bottom, 0, innerHeight);
    if (!best) {
      const mx = (cl + cr) * 0.5, my = (ct + cb) * 0.5;
      const blocker = hitDeep(mx, my);
      if (actionKind && blocker && !containsDeep(el, blocker) && !containsDeep(blocker, el)) {
        throw Error(`元素 [${id}] 当前被上层弹层或其它元素遮挡，请先用 op="dismiss" 关闭浮层或重新查看视图`);
      }
      best = { x: mx, y: my };
    }
    return {
      x: best.x,
      y: best.y,
      rect: [cl, ct, Math.max(0, cr - cl), Math.max(0, cb - ct)],
      tag: el.tagName.toLowerCase(),
      role: el.getAttribute('role') || el.tagName.toLowerCase(),
      name: elementName(el, r),
      editable: isEditable(el),
      password: el.tagName === 'INPUT' && el.type === 'password',
    };
  }

  // Native <select> option matcher (triggered when op="type" targets a <select> element).
  function selectOption(id, rawText) {
    keepAlive();
    const el = (id !== undefined && id !== null ? nodes.get(Number(id))?.ref?.deref() : null) || activeDeep();
    if (!el || el.tagName !== 'SELECT') return null;
    const needle = String(rawText ?? '').trim();
    const nLow = needle.toLowerCase();
    const opts = Array.from(el.options || []).filter(o => !o.disabled);
    const match = opts.find(o => o.value === needle)
      || opts.find(o => compact(o.text, 120) === needle)
      || opts.find(o => compact(o.text, 120).toLowerCase() === nLow || o.value.toLowerCase() === nLow)
      || (nLow ? opts.find(o => compact(o.text, 120).toLowerCase().includes(nLow)) : null);
    if (!match) {
      const avail = opts.slice(0, 10).map(o => `"${compact(o.text || o.value, 32)}"`).join(', ');
      throw Error(`下拉框无匹配选项 "${needle}"；可选值：${avail}`);
    }
    el.value = match.value;
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
    return { selected: compact(match.text || match.value, 60), value: compact(el.value, 60) };
  }

  // Fallback clear for controlled React/Vue/AntD inputs that intercept Ctrl+A or mask Backspace.
  function ensureCleared() {
    const el = activeDeep();
    if (!el || el.disabled || el.readOnly) return false;
    if ((el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') && typeof el.value === 'string' && el.value.length > 0) {
      const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      const setter = Object.getOwnPropertyDescriptor(proto, 'value')?.set;
      if (setter) setter.call(el, '');
      else el.value = '';
      el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'deleteContentBackward' }));
      return true;
    }
    return false;
  }

  function readTypedValue() {
    const el = activeDeep();
    if (!el) return null;
    if (el.tagName === 'INPUT' && el.type === 'password') return '***';
    if (el.isContentEditable) return compact(el.innerText, 80);
    if ('value' in el && typeof el.value === 'string') return compact(el.value, 80);
    return null;
  }

  // -------------------- Smart Cursor-Ignoring DOM Quiet --------------------
  function waitQuiet(quietMs = 120, maxMs = 900) {
    return new Promise(resolve => {
      if (!document.documentElement) { resolve({ quiet: true, ms: 0 }); return; }
      const started = performance.now();
      let lastChange = started, sawChange = false;
      const obs = new MutationObserver(records => {
        for (const rec of records) {
          const target = rec.target?.nodeType === 1 ? rec.target : rec.target?.parentElement;
          if (target && !isIgnored(target)) {
            sawChange = true;
            lastChange = performance.now();
            break;
          }
        }
      });
      obs.observe(document.documentElement, { subtree: true, childList: true, attributes: true, characterData: true });
      const check = () => {
        const now = performance.now();
        const elapsed = now - started;
        if (elapsed >= maxMs || (now - lastChange) >= quietMs) {
          obs.disconnect();
          resolve({ quiet: (now - lastChange) >= quietMs, changed: sawChange, ms: Math.round(elapsed) });
        } else {
          setTimeout(check, 35);
        }
      };
      setTimeout(check, Math.min(quietMs, 45));
    });
  }

  async function evalCode(code) {
    keepAlive();
    try {
      const value = await (0, eval)(code);
      const encoded = JSON.stringify(value ?? null);
      if (encoded === undefined) return { ok: true, value: null };
      if (encoded.length > 65536) return { ok: false, error: '脚本返回值超过 64K 字符，请精简返回 JSON' };
      return { ok: true, value: JSON.parse(encoded) };
    } catch (err) {
      return { ok: false, error: String(err?.stack || err).slice(0, 3000) };
    }
  }

  const api = {
    v: 7,
    observe,
    resolve,
    selectOption,
    ensureCleared,
    readTypedValue,
    focused: focusedInfo,
    overlayState,
    scrollTarget,
    verifyOrFallbackScroll,
    waitQuiet,
    evalCode,
    cursor: {
      show: showCursor,
      moveTo,
      click: clickFx,
      drag: dragFx,
      badge: showBadge,
      dismiss: dismissCursor,
      keepAlive,
      setShotHidden,
      markInput,
      settleForShot: keepAlive,
      pos: () => (hasPos ? [Math.round(cx), Math.round(cy)] : null),
      sync: (x, y) => { if (Number.isFinite(x) && Number.isFinite(y)) { cx = x; cy = y; hasPos = true; } },
    },
  };
  Object.defineProperty(globalThis, '__novaBrowser', { value: api, configurable: true });
})();
