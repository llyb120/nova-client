// Runs in a CDP isolated world. Page scripts cannot replace the element-reference map.
(() => {
  const compact = (text, max = 100) => String(text || '').replace(/\s+/g, ' ').trim().slice(0, max);
  const label = e => compact(e.getAttribute('aria-label') || e.labels?.[0]?.innerText || e.innerText || e.getAttribute('placeholder') || e.getAttribute('title'));
  const fingerprint = e => [e.tagName, label(e), e.getAttribute('type'), e.getAttribute('href')].join('|');
  const roots = root => [root, ...Array.from(root.querySelectorAll('*')).flatMap(e => e.shadowRoot ? roots(e.shadowRoot) : [])];
  let entries = new Map();
  let version = '';
  let captureGutter, captureTimer;
  const hitAt = (x, y) => {
    let hit = document.elementFromPoint(x, y);
    while (hit?.shadowRoot) { const next = hit.shadowRoot.elementFromPoint(x, y); if (!next || next === hit) break; hit = next; }
    return hit;
  };
  const points = e => {
    const r = e.getBoundingClientRect();
    const left = Math.max(0, r.left), top = Math.max(0, r.top), right = Math.min(innerWidth, r.right), bottom = Math.min(innerHeight, r.bottom);
    if (right <= left || bottom <= top) return [];
    return [[.5,.5],[.15,.5],[.85,.5],[.5,.15],[.5,.85],[.15,.15],[.85,.85]].map(([u,v]) => ({ x:left+(right-left)*u, y:top+(bottom-top)*v }));
  };
  const clickable = e => points(e).find(p => { const h = hitAt(p.x,p.y); return h === e || e.contains(h); });
  const stamp = () => JSON.stringify([location.href, innerWidth, innerHeight, scrollX, scrollY]);
  const api = {
    captureLayout(begin) {
      const root=document.documentElement;
      clearTimeout(captureTimer);
      if (!begin) {
        if (captureGutter) {
          const [value,priority]=captureGutter;
          if (value) root.style.setProperty('scrollbar-gutter',value,priority);
          else root.style.removeProperty('scrollbar-gutter');
          captureGutter=undefined;
        }
        return true;
      }
      // WebView2 full-page capture can temporarily remove classic scrollbar space. Keep the original layout.
      if (root.clientWidth < innerWidth && getComputedStyle(root).scrollbarGutter === 'auto') {
        captureGutter=[root.style.getPropertyValue('scrollbar-gutter'),root.style.getPropertyPriority('scrollbar-gutter')];
        root.style.setProperty('scrollbar-gutter','stable','important');
      }
      captureTimer=setTimeout(()=>api.captureLayout(false),20000);
      return true;
    },
    observe(nonce, limit = 20000) {
      entries = new Map(); version = nonce;
      const items = [];
      let total = 0;
      const candidates = roots(document).flatMap(root => Array.from(root.querySelectorAll('button,a[href],input:not([type=hidden]),textarea,select,[role=button],[role=tab],[role=checkbox],[role=combobox],[contenteditable=true],summary,[role=region],[role=radio],[role=menuitem],[role=option],[aria-haspopup],[aria-expanded],[tabindex],[onclick]')));
      const inView = e => { const r = e.getBoundingClientRect(); return r.bottom > 0 && r.top < innerHeight && r.right > 0 && r.left < innerWidth; };
      const containers = roots(document).flatMap(root => Array.from(root.querySelectorAll('div,section,main,ul,ol,table,textarea'))).filter(e => e.scrollHeight > e.clientHeight + 1 && /auto|scroll/.test(getComputedStyle(e).overflowY));
      const candidateSet = new Set(candidates), containerSet = new Set(containers);
      candidates.push(...containers.filter(e => !candidateSet.has(e)));
        for (const e of candidates) {
          const r = e.getBoundingClientRect();
          if (!r.width || !r.height || getComputedStyle(e).visibility === 'hidden') continue;
          total++;
          if (items.length >= limit) continue;
          const ref = `${nonce}:${items.length}`;
          entries.set(ref, { e, fingerprint: fingerprint(e) });
          const p = clickable(e), center = points(e)[0], blocker = center && !p ? hitAt(center.x,center.y) : null;
          items.push({ ref, role: e.getAttribute('role') || e.tagName.toLowerCase(), name: label(e),
            href: e.href || undefined, expanded: e.getAttribute('aria-expanded') ?? undefined,
            haspopup: e.getAttribute('aria-haspopup') ?? undefined, selected: e.getAttribute('aria-selected') ?? undefined,
            region: compact(e.closest('form,section,[role=dialog],tr,nav')?.innerText, 160),
            value: e.type === 'password' ? undefined : compact(e.value, 100), disabled: !!e.disabled,
            inView: inView(e), point: p, documentRect: { x:r.left+scrollX,y:r.top+scrollY,width:r.width,height:r.height },
            scroll: containerSet.has(e) ? { top:e.scrollTop,height:e.scrollHeight,viewportHeight:e.clientHeight } : undefined, blockedBy: blocker ? compact(label(blocker) || blocker.outerHTML, 180) : undefined });
        }
      const fullText = [document.body?.innerText || '', ...roots(document).slice(1).map(root => Array.from(root.children).filter(e => !['STYLE','SCRIPT'].includes(e.tagName)).map(e=>e.innerText || '').join('\n'))].join('\n');
      const headings = Array.from(document.querySelectorAll('h1,h2,h3,h4,h5,h6,[role=heading]')).map(e => ({level:e.getAttribute('aria-level') || e.tagName, text:compact(e.innerText,300)}));
      const declaredRows = Array.from(document.querySelectorAll('[aria-rowcount],[aria-setsize]')).map(e => ({total:Number(e.getAttribute('aria-rowcount') || e.getAttribute('aria-setsize')),rendered:e.querySelectorAll('[role=row],[role=listitem]').length}));
      // ponytail: cap pathological documents at 1M characters/20K targets per frame; report overflow explicitly.
      return { url:location.href,title:document.title,version,stamp:stamp(),text:fullText.slice(0,1000000),textLength:fullText.length,
        items,totalItems:total,headings:headings.slice(0,1000),truncated:total>items.length || fullText.length>1000000 || headings.length>1000,
        coverage:{scope:'loaded DOM including offscreen and nested scroll containers',lazyImages:Array.from(document.querySelectorAll('img[loading=lazy]')).filter(e=>!e.complete||!e.naturalWidth).length,declaredRows,note:'Unloaded lazy/virtualized content may require targeted scroll; no DOM API can return data the site has not loaded.'},
        documentSize:{width:Math.max(document.documentElement.scrollWidth,document.body?.scrollWidth||0),height:Math.max(document.documentElement.scrollHeight,document.body?.scrollHeight||0)},
        viewport:{width:innerWidth,height:innerHeight,scrollX,scrollY} };

    },
    async prepare(ref) {
      const entry = entries.get(ref);
      if (!entry || !entry.e.isConnected || fingerprint(entry.e) !== entry.fingerprint) throw Error('目标引用已失效，请重新观察');
      const e = entry.e;
      if (e.disabled || e.getAttribute('aria-disabled') === 'true') throw Error('目标不可用');
      // Already-visible targets must not move: sticky headers and nested scroll containers made center-only clicks fail.
      if (!clickable(e)) {
        e.scrollIntoView({ block: 'nearest', inline: 'nearest', behavior: 'instant' });
        await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));
      }
      if (!e.isConnected || fingerprint(e) !== entry.fingerprint || e.disabled) throw Error('目标在准备期间已变化，请重新观察');
      const p = clickable(e);
      if (!p) {
        const center = points(e)[0], blocker = center && hitAt(center.x,center.y);
        throw Error('目标被遮挡：' + compact(blocker ? label(blocker) || blocker.outerHTML : '在视口之外', 180) + '；请关闭遮挡层、滚动或换用截图中的可见目标');
      }
      return { ...p, editable: e.matches('input,textarea,[contenteditable=true]'), password: e.type === 'password' };
    },
    async coordinate(x, y, expectedStamp, fullPage = false) {
      if (stamp() !== expectedStamp) throw Error('截图视口已变化，请重新截图');
      if (![x,y].every(Number.isFinite) || x < 0 || y < 0) throw Error('坐标超出视口');
      if (fullPage) {
        if (x >= document.documentElement.scrollWidth || y >= document.documentElement.scrollHeight) throw Error('坐标超出文档');
        if (x < scrollX || y < scrollY || x >= scrollX+innerWidth || y >= scrollY+innerHeight) {
          scrollTo({left:Math.max(0,x-innerWidth/2),top:Math.max(0,y-innerHeight/2),behavior:'instant'});
          await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));
        }
        x-=scrollX; y-=scrollY;
      }
      if (x>=innerWidth || y>=innerHeight) throw Error('坐标超出视口');
      const e = hitAt(x,y);
      return { x, y, editable: !!e?.matches('input,textarea,[contenteditable=true]'), password: e?.type === 'password' };
    },
  };
  Object.defineProperty(globalThis, '__novaWebview', { value: api, configurable: true });
})()
