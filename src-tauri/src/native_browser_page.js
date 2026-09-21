// CDP isolated world: references are never recovered by text or a page-supplied selector.
(() => {
  if (globalThis.__novaWebview?.apiVersion === 3) return;
  const compact = (text, max = 160) => String(text ?? '').replace(/\s+/g, ' ').trim().slice(0, max);
  const parent = e => e?.assignedSlot || e?.parentElement || e?.getRootNode()?.host;
  const contains = (e, child) => { for (; child; child = parent(child)) if (child === e) return true; return false; };
  const ancestors = e => { const result = []; for (; e; e = parent(e)) result.push(e); return result; };
  const label = (e, labelCache) => {
    const labelledBy=e.getAttribute('aria-labelledby');
    if (labelledBy?.trim()) {
      const root=e.getRootNode();
      const text=labelledBy.trim().split(/\s+/).map(id=>(root.getElementById?.(id)||document.getElementById(id))?.textContent||'').join(' ').trim();
      if (text) return compact(text);
    }
    const aria=e.getAttribute('aria-label');if(aria) return compact(aria);
    // Reverse-index labels once per observation. Repeated e.labels enumeration
    // is surprisingly expensive on forms with thousands of controls.
    let labelText=labelCache?.get(e)||'';
    if (!labelCache) {
      const labels=e.labels;
      if(labels?.length===1) labelText=labels[0].innerText;
      else if(labels?.length) labelText=Array.from(labels,l=>l.innerText).join(' ');
    }
    if(labelText) return compact(labelText);
    return compact((e.matches('input[type=submit],input[type=button]') ? e.value : '') || e.innerText
      || e.getAttribute('placeholder') || e.getAttribute('alt') || e.getAttribute('title'));
  };
  const contextInfo = (e, cache) => {
    for (let scope=parent(e); scope; scope=parent(scope)) {
      if (!scope.matches('tr,[role=row],[role=listitem],li,form,[role=dialog],section,nav')) continue;
      if (cache?.has(scope)) return cache.get(scope);
      const key=['id','data-id','data-key','data-row-key','aria-rowindex','aria-label'].map(a=>scope.getAttribute(a)||'').join('|');
      const text=compact(scope.innerText,300), row=scope.matches('tr,[role=row],[role=listitem],li');
      // A row's text identifies its record. A form's transient validation/status
      // text must not invalidate every other field in a safe fill batch.
      const heading=compact(scope.querySelector('legend,h1,h2,h3,[role=heading]')?.textContent,200);
      const info={region:key.replace(/\|/g,'') ? key+' '+text : text,identity:[scope.tagName,key,row?text:heading].join('|')};
      cache?.set(scope,info);return info;
    }
    return {region:'',identity:''};
  };
  const fingerprint = (e, name=label(e), context=contextInfo(e).identity) => JSON.stringify([e.tagName, name, ...['id','name','type','role','href','data-testid','aria-controls'].map(a => e.getAttribute(a)), context]);
  const disabled = e => !!e.disabled || e.matches(':disabled') || ancestors(e).some(p => p.inert || p.getAttribute('aria-disabled') === 'true');
  const editable = (e, isDisabled=disabled(e)) => (e.matches('textarea,input:not([type]),input[type=text],input[type=search],input[type=email],input[type=url],input[type=tel],input[type=number],input[type=password]') || e.isContentEditable)
    && !e.readOnly && !ancestors(e).some(p=>p.getAttribute('aria-readonly')==='true') && !isDisabled;
  const roots = () => {
    const result = [document];
    for (let i = 0; i < result.length; i++) for (const e of result[i].querySelectorAll('*')) if (e.shadowRoot) result.push(e.shadowRoot);
    return result;
  };
  let entries = new Map(), version = '', captureGutter, captureTimer;
  const semanticCache=new WeakMap(), rootObservers=new Map();
  let revision=0, layoutKey='';
  const trackRoots = allRoots => {
    const live=new Set(allRoots);
    for(const [root,observer] of rootObservers) if(!live.has(root)) {observer.disconnect();rootObservers.delete(root);revision++;}
    for(const root of allRoots) {
      let observer=rootObservers.get(root);
      if(!observer) {
        observer=new MutationObserver(()=>revision++);
        observer.observe(root,{subtree:true,childList:true,characterData:true,attributes:true});
        rootObservers.set(root,observer);revision++;
      }
      // Flush synchronous mutations even before their observer callback ran.
      if(observer.takeRecords().length) revision++;
    }
    const key=[location.href,innerWidth,innerHeight,devicePixelRatio].join('|');
    if(key!==layoutKey) {layoutKey=key;revision++;}
  };

  const identities = new WeakMap(); let nextIdentity = 0;
  const identity = e => { if (!e) return null; if (!identities.has(e)) identities.set(e, ++nextIdentity); return identities.get(e); };
  const active = () => { let e = document.activeElement; while (e?.shadowRoot?.activeElement) e = e.shadowRoot.activeElement; return e; };
  const hitAt = (x, y) => {
    let hit = document.elementFromPoint(x, y);
    while (hit?.shadowRoot) { const next = hit.shadowRoot.elementFromPoint(x, y); if (!next || next === hit) break; hit = next; }
    return hit;
  };
  const receives = (e, x, y) => {
    const hit = hitAt(x, y);
    if (!contains(e, hit)) return false;
    // Do not hit an unrelated nested button when the model selected its container.
    for (let h = hit; h && h !== e; h = parent(h)) if (h.matches('button,a[href],input,select,textarea,[role=button]')) return false;
    return true;
  };
  const visible = e => {
    const s = getComputedStyle(e), r = e.getBoundingClientRect();
    return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.visibility !== 'collapse' && s.display !== 'none';
  };
  const points = (e, visibilityChecked=false) => {
    if (!visibilityChecked && !visible(e)) return [];
    // Client rects, not their union: multiline links can have empty space at their centre.
    const rects = Array.from(e.getClientRects()).sort((a,b) => b.width*b.height-a.width*a.height);
    const result = [];
    for (const r of rects.slice(0, 16)) {
      const left = Math.max(0, r.left), top = Math.max(0, r.top), right = Math.min(innerWidth, r.right), bottom = Math.min(innerHeight, r.bottom);
      if (right <= left || bottom <= top) continue;
      for (const [u,v] of [[.5,.5],[.15,.5],[.85,.5],[.5,.15],[.5,.85],[.15,.15],[.85,.85]]) result.push({x:left+(right-left)*u,y:top+(bottom-top)*v});
    }
    return result;
  };
  const clickable = e => points(e).find(p => receives(e, p.x, p.y));
  const rect = e => { const r = e.getBoundingClientRect(); return {x:r.x,y:r.y,width:r.width,height:r.height}; };
  const sameRect = (a,b) => a && b && ['x','y','width','height'].every(k => Math.abs(a[k]-b[k]) < .5);
  const stamp = () => JSON.stringify([location.href,innerWidth,innerHeight,scrollX,scrollY,visualViewport?.scale,visualViewport?.offsetLeft,visualViewport?.offsetTop]);
  const tick = () => new Promise(resolve => {
    let frame;
    const timer = setTimeout(() => { cancelAnimationFrame(frame); resolve(); }, 50);
    frame = requestAnimationFrame(() => { clearTimeout(timer); resolve(); });
  });
  const entryFor = ref => {
    const entry = entries.get(ref);
    if (!entry || !entry.e.isConnected || fingerprint(entry.e) !== entry.fingerprint) {revision++;throw Error('目标引用已失效（节点、名称或所属行已变化），请重新观察');}
    return entry;
  };
  const api = {
    apiVersion: 3,
    stamp,
    inputState(ref) {
      const e = active();
      return {identity:identity(e),editable:!!e && editable(e),password:e?.type==='password',matches:ref ? e === entryFor(ref).e : undefined};
    },
    verifyValue(ref, expected) {
      // Input may legitimately change its accessible name/context, so compare
      // the original node and actual value, not the pre-edit text fingerprint.
      const e=entries.get(ref)?.e;
      if (!e?.isConnected || !editable(e) || e.type==='password' || active()!==e) throw Error('填写后目标或焦点已变化，请重新观察');
      const value=e.isContentEditable ? e.innerText : e.value;
      return {matches:String(value??'').replace(/\r\n/g,'\n')===expected.replace(/\r\n/g,'\n')};
    },
    captureLayout(begin) {
      const root=document.documentElement;
      clearTimeout(captureTimer);
      if (!begin) {
        if (captureGutter) {
          const [value,priority]=captureGutter;
          if (value) root.style.setProperty('scrollbar-gutter',value,priority); else root.style.removeProperty('scrollbar-gutter');
          captureGutter=undefined;
        }
        return true;
      }
      if (root.clientWidth < innerWidth && getComputedStyle(root).scrollbarGutter === 'auto' && !captureGutter) {
        captureGutter=[root.style.getPropertyValue('scrollbar-gutter'),root.style.getPropertyPriority('scrollbar-gutter')];
        root.style.setProperty('scrollbar-gutter','stable','important');
      }
      captureTimer=setTimeout(()=>api.captureLayout(false),20000);
      return true;
    },
    observe(nonce, limit = 20000, scope = 'all') {
      const started=performance.now(), allRoots=roots();trackRoots(allRoots);
      entries = new Map(); version = nonce;
      const items = [], canvases=[], contextCache=new WeakMap();
      let labelCache;
      const labels = () => {
        if(scope==='viewport') return undefined;
        if(!labelCache) {
          labelCache=new WeakMap();
          for(const root of allRoots) for(const l of root.querySelectorAll('label')) {
            const control=l.control;if(control) labelCache.set(control,(labelCache.get(control)||'')+' '+l.innerText);
          }
        }
        return labelCache;
      }; let total=0, visualArea=0;
      const selector='button,a[href],input:not([type=hidden]),textarea,select,canvas,[role=button],[role=tab],[role=checkbox],[role=combobox],[contenteditable=true],summary,[role=region],[role=radio],[role=menuitem],[role=option],[aria-haspopup],[aria-expanded],[tabindex],[onclick]';
      const candidates=[], seen=new Set(), containers=new Set();
      for (const root of allRoots) {
        for (const e of root.querySelectorAll(selector)) { if (!seen.has(e)) {seen.add(e);candidates.push(e);} }
        for (const e of root.querySelectorAll('div,section,main,ul,ol,table,textarea')) {
          if (e.scrollHeight > e.clientHeight+1 && /auto|scroll/.test(getComputedStyle(e).overflowY)) {
            containers.add(e); if (!seen.has(e)) {seen.add(e);candidates.push(e);}
          }
        }
      }
      for (const e of candidates) {
        const r=e.getBoundingClientRect();
        const inView=r.bottom>0 && r.top<innerHeight && r.right>0 && r.left<innerWidth;
        if (scope==='viewport' && !inView) continue;
        if (!r.width || !r.height) continue;
        const visibility=getComputedStyle(e).visibility;
        if (visibility==='hidden' || visibility==='collapse') continue;
        total++;if (items.length >= limit) continue;
        const ref=`${nonce}:${items.length}`, isDisabled=disabled(e);
        // Cache semantics only, never geometry, hit tests, focus, values or
        // enabled state. prepare()/verifyPoint() always fingerprint live nodes.
        let cached=semanticCache.get(e);
        if(!cached || cached.revision!==revision) {
          const name=label(e,labels()),context=contextInfo(e,contextCache),region=context.region;
          cached={revision,name,region,fingerprint:fingerprint(e,name,context.identity)};
          semanticCache.set(e,cached);
        }
        const {name,region}=cached;
        entries.set(ref,{e,fingerprint:cached.fingerprint});
        // Most targets in a long document are offscreen. Keep their references
        // and geometry without probing client rects / hit-testing them twice.
        const candidates=inView ? points(e,true) : [], p=candidates.find(p=>receives(e,p.x,p.y));
        const center=candidates[0], blocker=center&&!p ? hitAt(center.x,center.y) : null;
        const item={ref,role:e.getAttribute('role')||e.tagName.toLowerCase(),tag:e.tagName.toLowerCase(),name,
          href:e.href||undefined,expanded:e.getAttribute('aria-expanded')??undefined,haspopup:e.getAttribute('aria-haspopup')??undefined,selected:e.getAttribute('aria-selected')??undefined,
          region,value:e.type==='password'?undefined:compact(e.value,100),disabled:isDisabled,editable:editable(e,isDisabled),
          inView,point:p,viewportRect:{x:r.x,y:r.y,width:r.width,height:r.height},documentRect:{x:r.left+scrollX,y:r.top+scrollY,width:r.width,height:r.height},
          scroll:containers.has(e)?{top:e.scrollTop,height:e.scrollHeight,viewportHeight:e.clientHeight}:undefined,
          blockedBy:blocker?compact(label(blocker)||blocker.outerHTML,180):undefined};
        if (e.tagName === 'CANVAS') {
          item.canvas={width:e.width,height:e.height,coordinateSpace:'CSS viewport, not backing-store pixels',content:'visual-only; DOM cannot enumerate drawn controls'};
          canvases.push({ref,name,inView,viewportRect:rect(e),backingWidth:e.width,backingHeight:e.height});
          if (p && r.width>=64 && r.height>=64) visualArea+=Math.max(0,Math.min(r.right,innerWidth)-Math.max(0,r.left))*Math.max(0,Math.min(r.bottom,innerHeight)-Math.max(0,r.top));
        }
        items.push(item);
      }
      const fullText=[document.body?.innerText||'',...allRoots.slice(1).map(root=>Array.from(root.children).filter(e=>!['STYLE','SCRIPT'].includes(e.tagName)).map(e=>e.innerText||'').join('\n'))].join('\n');
      const headings=allRoots.flatMap(root=>Array.from(root.querySelectorAll('h1,h2,h3,h4,h5,h6,[role=heading]')).map(e=>({level:e.getAttribute('aria-level')||e.tagName,text:compact(e.innerText,300)})));
      const declaredRows=allRoots.flatMap(root=>Array.from(root.querySelectorAll('[aria-rowcount],[aria-setsize]')).map(e=>({total:Number(e.getAttribute('aria-rowcount')||e.getAttribute('aria-setsize')),rendered:e.querySelectorAll('[role=row],[role=listitem]').length})));
      return {url:location.href,title:document.title,version,stamp:stamp(),text:fullText.slice(0,1000000),textLength:fullText.length,
        items,totalItems:total,headings:headings.slice(0,1000),canvases:canvases.slice(0,100),visualSuggested:visualArea>=innerWidth*innerHeight*.15,focus:api.inputState(),
        truncated:total>items.length||fullText.length>1000000||headings.length>1000||canvases.length>100,
        coverage:{lazyImages:allRoots.reduce((n,root)=>n+Array.from(root.querySelectorAll('img[loading=lazy]')).filter(e=>!e.complete||!e.naturalWidth).length,0),scope:scope==='viewport'?'viewport DOM only; use scope=all for offscreen targets':'loaded DOM including offscreen, open shadow roots and scroll containers',declaredRows,canvasCount:canvases.length,note:'Canvas/WebGL controls are pixels, not DOM targets. Use the imageId and image-pixel coordinates in the returned visual observation. Virtualized/unloaded content requires observation after scrolling.'},
        documentSize:{width:Math.max(document.documentElement.scrollWidth,document.body?.scrollWidth||0),height:Math.max(document.documentElement.scrollHeight,document.body?.scrollHeight||0)},
        viewport:{width:innerWidth,height:innerHeight,scrollX,scrollY,devicePixelRatio,scale:visualViewport?.scale||1},
        timingsMs:{observe:performance.now()-started},scannedRoots:allRoots.length};
    },
    async prepare(ref, mode='click') {
      const entry=entryFor(ref),e=entry.e,started=performance.now();
      if (mode==='fill' && !editable(e)) throw Error('目标不是可填写字段或只读');
      let previous,stableFrames=0,scrolled=false,reason='目标正在移动';
      do {
        entryFor(ref);
        if (disabled(e)) reason='目标不可用';
        else {
          const p=clickable(e),r=rect(e);
          const moving=ancestors(e).some(node=>node.getAnimations?.().some(animation=>
            (animation.pending || animation.playState==='running') && animation.effect?.getKeyframes?.().some(frame=>
              Object.keys(frame).some(key=>/^(transform|translate|rotate|scale|left|right|top|bottom|width|height|offsetPath|fontSize|margin.*|padding.*)$/.test(key)))));
          stableFrames=p && !moving && sameRect(previous,r) ? stableFrames+1 : 0;
          if (stableFrames>=2) return {...p,localX:p.x,localY:p.y,rect:r,editable:editable(e),password:e.type==='password',canvas:e.tagName==='CANVAS'};
          if (!p && !scrolled) {e.scrollIntoView({block:'nearest',inline:'nearest',behavior:'instant'});scrolled=true;previous=undefined;}
          else {previous=r;reason=p?'目标正在移动':'目标被遮挡或在视口之外';}
        }
        await tick();
      } while (performance.now()-started<1200);
      throw Error(reason+'；有界等待后仍不可安全操作，请重新观察');
    },
    verifyPoint(ref,x,y,expectedRect,mode='click') {
      const e=entryFor(ref).e;
      if (disabled(e) || !sameRect(rect(e),expectedRect) || !receives(e,x,y)) throw Error('目标在鼠标定位后改变或被遮挡，请重新观察');
      if (mode==='fill' && !editable(e)) throw Error('目标不是可填写字段或只读');
      return true;
    },
    async coordinate(x,y,expectedStamp,fullPage=false) {
      if (stamp()!==expectedStamp) throw Error('截图视口已变化，请重新截图');
      if (![x,y].every(Number.isFinite)||x<0||y<0) throw Error('坐标超出视口');
      if (fullPage) {
        if (x>=Math.max(document.documentElement.scrollWidth,document.body?.scrollWidth||0)||y>=Math.max(document.documentElement.scrollHeight,document.body?.scrollHeight||0)) throw Error('坐标超出文档');
        if (x<scrollX||y<scrollY||x>=scrollX+innerWidth||y>=scrollY+innerHeight) {
          scrollTo({left:Math.max(0,x-innerWidth/2),top:Math.max(0,y-innerHeight/2),behavior:'instant'}); await tick(); await tick();
        }
        x-=scrollX;y-=scrollY;
      }
      if (x>=innerWidth||y>=innerHeight) throw Error('坐标超出视口');
      const e=hitAt(x,y);
      // Reuse live-node validation after hover; CSS highlight is not a stale target.
      // Pixel-only surfaces still need the second visual check.
      let hoverTarget;
      if (e && !ancestors(e).some(node=>node.matches('canvas,iframe,frame,video,img,svg'))) {
        const target=ancestors(e).find(node=>node.matches('button,a[href],input,select,textarea,[role=button],[role=option],[role=menuitem],li'))||e;
        const ref=`${version}:coordinate:${identity(target)}`;
        entries.set(ref,{e:target,fingerprint:fingerprint(target)});
        hoverTarget={ref,rect:rect(target)};
      }
      return {x,y,stamp:stamp(),pageX:x+scrollX,pageY:y+scrollY,editable:!!e&&editable(e),password:e?.type==='password',canvas:e?.tagName==='CANVAS',hit:e?{tag:e.tagName.toLowerCase(),name:label(e)}:null,hoverTarget};
    },
  };
  Object.defineProperty(globalThis,'__novaWebview',{value:api,configurable:true});
})()
