// CDP isolated world: no page-provided selectors, scripts, or reference maps are trusted.
(() => {
  if (globalThis.__novaWebview?.apiVersion === 2) return;
  const compact = (text, max = 100) => String(text || '').replace(/\s+/g, ' ').trim().slice(0, max);
  const selector = 'button,a[href],input:not([type=hidden]),textarea,select,[role],summary,[contenteditable],[aria-haspopup],[aria-expanded],[tabindex],[onclick],canvas';
  const interactive = 'button,a[href],input,textarea,select,summary,[role=button],[role=tab],[role=checkbox],[role=radio],[role=menuitem],[role=option],[role=combobox]';
  const epoch = Array.from(crypto.getRandomValues(new Uint32Array(4))).join('-');
  const parentOf = e => e.parentElement || e.getRootNode()?.host;
  const inside = (parent, child) => {
    for (let e = child; e; e = parentOf(e)) if (e === parent) return true;
    return false;
  };
  const ancestor = (e, test) => { for (; e; e = parentOf(e)) if (test(e)) return e; };
  const label = e => {
    const refs = compact(e.getAttribute('aria-labelledby'), 512).split(' ').filter(Boolean).slice(0, 16);
    const labelled = refs.map(id => (e.getRootNode().getElementById?.(id) || document.getElementById(id))?.textContent || '').join(' ').trim();
    return compact(labelled || e.getAttribute('aria-label') || Array.from(e.labels || []).map(l => l.innerText).join(' ')
      || (e.matches('input[type=submit],input[type=button]') ? e.value : '') || e.innerText
      || e.getAttribute('alt') || e.getAttribute('placeholder') || e.getAttribute('title'));
  };
  const rowIdentity = e => {
    const row = ancestor(parentOf(e), e => e.matches('tr,[role=row],li,[role=listitem],[data-row-key]'));
    return row ? [row.id, row.getAttribute('data-row-key'), compact(row.innerText, 300)].join('|') : '';
  };
  const fingerprint = (e, name = label(e)) => JSON.stringify([e.tagName, name, e.id, e.getAttribute('name'), e.getAttribute('type'),
    e.getAttribute('href'), e.getAttribute('role'), rowIdentity(e)]);
  const unavailable = e => !!ancestor(e, n => n.matches(':disabled,[aria-disabled=true],[inert]'));
  const editable = e => !e.readOnly && (e.isContentEditable || e.tagName==='TEXTAREA'
    || (e.tagName==='INPUT' && ['text','search','tel','url','email','number','password'].includes(e.type)));
  const perceptible = e => !ancestor(e,n=>{const s=getComputedStyle(n);return s.display==='none' || s.visibility==='hidden' || s.visibility==='collapse' || Number(s.opacity)===0;});
  const hitAt = (x, y) => {
    let hit = document.elementFromPoint(x, y);
    while (hit?.shadowRoot) {
      const next = hit.shadowRoot.elementFromPoint(x, y);
      if (!next || next === hit) break;
      hit = next;
    }
    return hit;
  };
  const accepts = (e, hit) => {
    if (!hit || !inside(e, hit)) return false;
    // A row/region is not permission to click a nested Delete/Submit control.
    for (let n = hit; n && n !== e; n = parentOf(n)) if (n.matches(interactive)) return false;
    return true;
  };
  const geometry = e => {
    const r = e.getBoundingClientRect();
    return { x:r.x, y:r.y, width:r.width, height:r.height };
  };
  const sameRect = (a, b) => ['x','y','width','height'].every(k => Math.abs(a[k] - b[k]) <= .5);
  const points = e => {
    const output = [];
    // Inline/wrapped links may have a bounding-box centre in empty space.
    for (const r of Array.from(e.getClientRects()).slice(0, 8)) {
      const left = Math.max(0, r.left), top = Math.max(0, r.top), right = Math.min(innerWidth, r.right), bottom = Math.min(innerHeight, r.bottom);
      if (right <= left || bottom <= top) continue;
      for (const [u,v] of [[.5,.5],[.15,.5],[.85,.5],[.5,.15],[.5,.85],[.15,.15],[.85,.85]])
        output.push({ x:left+(right-left)*u, y:top+(bottom-top)*v });
    }
    return output;
  };
  const clickable = e => points(e).find(p => accepts(e, hitAt(p.x, p.y)));
  const stamp = () => JSON.stringify([epoch, location.href, innerWidth, innerHeight, scrollX, scrollY,
    visualViewport?.offsetLeft, visualViewport?.offsetTop, visualViewport?.width, visualViewport?.height, visualViewport?.scale]);
  // rAF is throttled in background tabs; never await it without a bounded fallback.
  const frame = () => new Promise(resolve => {
    const timer = setTimeout(done, 40); let raf = requestAnimationFrame(done);
    function done() { clearTimeout(timer); cancelAnimationFrame(raf); resolve(); }
  });
  let entries = new Map(), version = '', captureGutter, captureTimer;
  const entryFor = ref => {
    const entry = entries.get(ref);
    if (!entry || !entry.e.isConnected || fingerprint(entry.e) !== entry.fingerprint)
      throw Error('目标引用已失效（元素或所在行身份变化），请重新观察');
    return entry;
  };
  const readyTarget = ref => {
    const entry = entryFor(ref);
    if (unavailable(entry.e) || !perceptible(entry.e)) throw Error('目标不可用或不可见');
    return entry.e;
  };
  const api = {
    apiVersion: 2,
    stamp,
    captureLayout(begin) {
      const root = document.documentElement;
      clearTimeout(captureTimer);
      if (!begin) {
        if (captureGutter) {
          const [value, priority] = captureGutter;
          if (value) root.style.setProperty('scrollbar-gutter', value, priority);
          else root.style.removeProperty('scrollbar-gutter');
          captureGutter = undefined;
        }
        return true;
      }
      if (root.clientWidth < innerWidth && getComputedStyle(root).scrollbarGutter === 'auto') {
        captureGutter = [root.style.getPropertyValue('scrollbar-gutter'), root.style.getPropertyPriority('scrollbar-gutter')];
        root.style.setProperty('scrollbar-gutter', 'stable', 'important');
      }
      captureTimer = setTimeout(() => api.captureLayout(false), 20000);
      return true;
    },
    observe(nonce, limit = 20000, query = '') {
      const started = performance.now();
      entries = new Map(); version = nonce;
      const search=String(query).trim().toLocaleLowerCase();
      const regionCache=new WeakMap();
      const regionOf=e=>{const root=ancestor(e,e=>e.matches('form,section,[role=dialog],tr,nav'));if(!root)return '';if(!regionCache.has(root))regionCache.set(root,compact(root.innerText,160));return regionCache.get(root);};
      const items = [], headings = [], declaredRows = [], shadowText = [], candidates = [];
      const containers = new Set(); let total = 0, scanned = 0, lazyImages = 0, scanTruncated = false;
      const queue = [document];
      // One composed-tree pass replaces repeated querySelectorAll('*') / shadow-root scans.
      // Bound traversal itself, not only the returned array, on pathological pages.
      for (let index = 0; index < queue.length && !scanTruncated; index++) {
        const root = queue[index];
        if (index) shadowText.push(Array.from(root.children).filter(e => !e.matches('style,script')).map(e => e.innerText || '').join('\n'));
        const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
        for (let e; (e = walker.nextNode());) {
          if (++scanned > 100000) { scanTruncated = true; break; }
          if (e.shadowRoot) queue.push(e.shadowRoot);
          if (e.matches('h1,h2,h3,h4,h5,h6,[role=heading]')) headings.push({level:e.getAttribute('aria-level') || e.tagName,text:compact(e.innerText,300)});
          if (e.matches('[aria-rowcount],[aria-setsize]')) declaredRows.push({total:Number(e.getAttribute('aria-rowcount') || e.getAttribute('aria-setsize')),rendered:e.querySelectorAll('[role=row],[role=listitem]').length});
          if (e.matches('img[loading=lazy]') && (!e.complete || !e.naturalWidth)) lazyImages++;
          const overflow = e.matches('div,section,main,ul,ol,table,textarea') && (e.scrollHeight > e.clientHeight+1 || e.scrollWidth > e.clientWidth+1);
          if (overflow && /auto|scroll/.test(getComputedStyle(e).overflow)) containers.add(e);
          if (e.matches(selector) || containers.has(e)) candidates.push(e);
        }
      }
      for (const e of candidates) {
        const name=label(e),region=regionOf(e);
        if(search && !`${name} ${region} ${e.href || ''}`.toLocaleLowerCase().includes(search)) continue;
        const r = e.getBoundingClientRect(), style = getComputedStyle(e);
        if (!r.width || !r.height || style.visibility === 'hidden' || style.visibility === 'collapse' || style.display === 'none' || Number(style.opacity)===0) continue;
        total++; if (items.length >= limit) continue;
        const ref = `${nonce}:${items.length}`, disabled=unavailable(e);
        const inView = r.bottom>0 && r.top<innerHeight && r.right>0 && r.left<innerWidth;
        const p = inView && !disabled ? clickable(e) : undefined;
        const center = inView && !p ? points(e)[0] : undefined, blocker = center ? hitAt(center.x, center.y) : null;
        const visual = e.tagName === 'CANVAS';
        const scrollOnly=containers.has(e) && !e.matches(interactive) && !e.hasAttribute('onclick') && !e.hasAttribute('role');
        entries.set(ref, {e, fingerprint:fingerprint(e,name),scrollOnly});
        items.push({ref, role:e.getAttribute('role') || (scrollOnly?'scroll-container':e.tagName.toLowerCase()), name,actions:scrollOnly?['scroll']:undefined,
          href:e.href || undefined, expanded:e.getAttribute('aria-expanded') ?? undefined,
          haspopup:e.getAttribute('aria-haspopup') ?? undefined, selected:e.getAttribute('aria-selected') ?? undefined,
          region,
          value:e.type === 'password' ? undefined : compact(e.value,100), disabled, readOnly:!!e.readOnly,
          inView, point:p || undefined, viewportRect:{x:r.x,y:r.y,width:r.width,height:r.height}, documentRect:{x:r.left+scrollX,y:r.top+scrollY,width:r.width,height:r.height},
          visual:visual ? {kind:'canvas',bitmapWidth:e.width,bitmapHeight:e.height,coordinateSpace:'CSS viewport; screenshot imageId maps image pixels automatically',semanticCoverage:'DOM label/fallback only; drawn objects require the screenshot'} : undefined,
          scroll:containers.has(e) ? {top:e.scrollTop,left:e.scrollLeft,height:e.scrollHeight,width:e.scrollWidth,viewportHeight:e.clientHeight,viewportWidth:e.clientWidth} : undefined,
          blockedBy:blocker ? compact(label(blocker) || blocker.tagName,180) : undefined});
      }
      const fullText = [document.body?.innerText || '', ...shadowText].join('\n');
      const visualRequired = items.some(i => i.visual && i.inView && i.viewportRect.width>=96 && i.viewportRect.height>=48);
      return {url:location.href,title:document.title,version,stamp:stamp(),text:fullText.slice(0,1000000),textLength:fullText.length,
        items,totalItems:total,headings:headings.slice(0,1000),visualRequired,
        truncated:scanTruncated || total>items.length || fullText.length>1000000 || headings.length>1000,
        coverage:{query:search || undefined,scope:search?'query-matching loaded DOM, open shadow roots and nested scroll containers':'loaded DOM, open shadow roots and nested scroll containers',lazyImages,declaredRows:declaredRows.slice(0,1000),scanTruncated,
          note:'Canvas/WebGL pixels, closed shadow roots and unloaded virtual content are not DOM targets. Use returned screenshots and fresh imageId; do not invent refs.'},
        documentSize:{width:Math.max(document.documentElement.scrollWidth,document.body?.scrollWidth||0),height:Math.max(document.documentElement.scrollHeight,document.body?.scrollHeight||0)},
        viewport:{width:innerWidth,height:innerHeight,scrollX,scrollY,devicePixelRatio,visualScale:visualViewport?.scale || 1},
        timings:{observeMs:performance.now()-started,scannedElements:scanned}};
    },
    async prepare(ref, forScroll = false) {
      if (entryFor(ref).scrollOnly && !forScroll) throw Error('目标是滚动容器，请选择具体控件，或使用 scroll');
      let e = readyTarget(ref);
      if (!clickable(e)) { e.scrollIntoView({block:'nearest',inline:'nearest',behavior:'instant'}); await frame(); }
      let previous = geometry(e), quiet = 0;
      const deadline = performance.now()+500;
      do {
        await frame(); e = readyTarget(ref);
        const current = geometry(e);
        quiet = sameRect(previous, current) ? quiet+1 : 0; previous = current;
        if (quiet >= 2) {
          const p = clickable(e);
          if (p) return {...p,rect:current,editable:editable(e),password:e.type==='password'};
        }
      } while (performance.now()<deadline);
      const center = points(e)[0], blocker = center && hitAt(center.x,center.y);
      throw Error('目标被遮挡或持续移动：'+compact(blocker ? label(blocker) || blocker.tagName : '视口之外',180)+'；请重新观察');
    },
    validate(ref, expected) {
      const e = readyTarget(ref);
      if (!sameRect(expected.rect, geometry(e)) || !accepts(e, hitAt(expected.x, expected.y)))
        throw Error('目标在准备期间已变化或被遮挡，请重新观察');
      return true;
    },
    focused(ref) {
      const e = readyTarget(ref);
      let focused = document.activeElement;
      while (focused?.shadowRoot?.activeElement) focused = focused.shadowRoot.activeElement;
      if (!(focused === e || (e.isContentEditable && inside(e, focused))) || !editable(e) || e.type === 'password')
        throw Error('输入焦点不再是已确认字段，停止填写');
      return true;
    },
    value(ref) { const e=entries.get(ref)?.e;if(!e?.isConnected)throw Error('填写后的字段已替换，请观察确认');return e.type==='password'?null:(e.isContentEditable?e.innerText:e.value); },
    async waitFor(ref, state, text, ms = 1500) {
      if (!['visible','hidden','enabled','text'].includes(state) || !Number.isFinite(ms) || ms<0 || ms>2000)
        throw Error('无效的等待条件');
      if (!entries.has(ref)) throw Error('等待目标引用不存在，请重新观察');
      const deadline = performance.now()+ms;
      do {
        const entry = entries.get(ref), e = entry?.e;
        const exists = !!e?.isConnected;
        const visible = exists && perceptible(e) && !!clickable(e);
        if (state==='hidden' && !visible) return {matched:true,state};
        if (exists) {
          if (state==='text') { if (String(e.innerText || e.value || '').includes(String(text))) return {matched:true,state}; }
          else { entryFor(ref); if (visible && (state==='visible' || !unavailable(e))) return {matched:true,state}; }
        }
        await frame();
      } while (performance.now()<deadline);
      throw Error('等待条件未满足；输入未重放，请观察当前页面');
    },
    async coordinate(x, y, expectedStamp, fullPage = false) {
      if (stamp() !== expectedStamp) throw Error('截图视口已变化，请重新截图');
      if (![x,y].every(Number.isFinite) || x<0 || y<0) throw Error('坐标超出视口');
      // Browser zoom/DPR is supported via image metadata. Pinch-zoom has a separate visual origin.
      if (Math.abs((visualViewport?.scale || 1)-1)>.001) throw Error('截图处于手势缩放状态，请恢复页面缩放后重新截图');
      const original = hitAt(x-(fullPage?scrollX:0),y-(fullPage?scrollY:0));
      if (fullPage) {
        if (x>=Math.max(document.documentElement.scrollWidth,document.body?.scrollWidth||0) || y>=Math.max(document.documentElement.scrollHeight,document.body?.scrollHeight||0)) throw Error('坐标超出文档');
        if (x<scrollX || y<scrollY || x>=scrollX+innerWidth || y>=scrollY+innerHeight) {
          scrollTo({left:Math.max(0,x-innerWidth/2),top:Math.max(0,y-innerHeight/2),behavior:'instant'});
          await frame(); await frame();
        }
        x-=scrollX; y-=scrollY;
      }
      if (x>=innerWidth || y>=innerHeight || x<0 || y<0) throw Error('坐标超出视口');
      const e = hitAt(x,y);
      if (!e || unavailable(e) || !perceptible(e)) throw Error('目标不可用或不可见');
      // A full-page document coordinate may be covered by a sticky header after scrolling.
      if (fullPage && !original && ancestor(e,n=>['fixed','sticky'].includes(getComputedStyle(n).position)))
        throw Error('目标被遮挡：整页坐标落入固定/粘性元素，请用视口截图');
      api.coordinateTarget = {e,fingerprint:fingerprint(e),x,y,rect:geometry(e)};
      return {x,y,editable:editable(e),password:e.type==='password'};
    },
    validateCoordinate() {
      const t = api.coordinateTarget;
      if (!t || !t.e.isConnected || unavailable(t.e) || !perceptible(t.e) || hitAt(t.x,t.y)!==t.e || fingerprint(t.e)!==t.fingerprint || !sameRect(t.rect,geometry(t.e)))
        throw Error('目标在准备期间已变化，请重新截图');
      return true;
    },
    async frameOwner(e, point) {
      if (!e.isConnected) throw Error('框架已变化');
      // Axis-aligned CSS scales and borders map correctly; reject rotation/skew rather than misclick.
      for (let n=e;n;n=parentOf(n)) {
        const transform=getComputedStyle(n).transform;
        if (transform!=='none') {
          const m=new DOMMatrixReadOnly(transform);
          if (!m.is2D || Math.abs(m.b)>.0001 || Math.abs(m.c)>.0001 || m.a<=0 || m.d<=0) throw Error('框架旋转/倾斜不支持安全坐标映射');
        }
      }
      const r=e.getBoundingClientRect(), sx=r.width/e.offsetWidth, sy=r.height/e.offsetHeight;
      if (!Number.isFinite(sx) || !Number.isFinite(sy)) throw Error('框架尺寸无效');
      const x=r.x+(e.clientLeft+point.x)*sx,y=r.y+(e.clientTop+point.y)*sy;
      if (hitAt(x,y)!==e) throw Error('框架被遮挡');
      return {x,y};
    },
  };
  Object.defineProperty(globalThis,'__novaWebview',{value:api,configurable:true});
})()
