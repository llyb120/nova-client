// CDP isolated world: references are never recovered by text or a page-supplied selector.
(() => {
  if (globalThis.__novaWebview?.apiVersion === 20) return;
  const compact = (text, max = 160) => String(text ?? '').replace(/\s+/g, ' ').trim().slice(0, max);
  // Icon-only controls (sort carets, filter funnels, close ×) carry their meaning in class names.
  const iconHint = e => {
    const words = new Set();
    for (const n of [e, ...Array.from(e.querySelectorAll('*')).slice(0, 8)]) {
      const source = `${n.getAttribute('class') || ''} ${n.getAttribute('data-icon') || ''} ${n.getAttribute('data-testid') || ''} ${n.getAttribute('aria-sort') || ''}`;
      for (const m of source.matchAll(/sort|caret|arrow|chevron|filter|order|asc|desc|active|search|close|clear|calendar|expand|collapse|more|setting|delete|edit|info|question|help/gi)) words.add(m[0].toLowerCase());
    }
    return words.size ? [...words].slice(0, 8).join(' ') : undefined;
  };
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
      if (!scope.matches('tr,[role=row],[role=listitem],li,ul,ol,[role=menu],[role=listbox],form,[role=dialog],section,nav')) continue;
      if (cache?.has(scope)) return cache.get(scope);
      const key=['id','data-id','data-key','data-row-key','aria-rowindex','aria-label'].map(a=>scope.getAttribute(a)||'').join('|');
      const text=compact(scope.matches('ul,ol') ? `${scope.previousElementSibling?.innerText||''} ${scope.innerText}` : scope.innerText,300), row=scope.matches('tr,[role=row],[role=listitem],li');
      // A row's text identifies its record. A form's transient validation/status
      // text must not invalidate every other field in a safe fill batch.
      const heading=compact(scope.querySelector('legend,h1,h2,h3,[role=heading]')?.textContent,200);
      const info={region:key.replace(/\|/g,'') ? key+' '+text : text,identity:[scope.tagName,key,row?text:heading].join('|')};
      cache?.set(scope,info);return info;
    }
    return {region:'',identity:''};
  };
  const fieldContext = e => {
    // ponytail: six ancestors, single fields or a two-date range; other compound forms need real labels.
    for (let scope=parent(e),depth=0; scope && depth<6; scope=parent(scope),depth++) {
      const fields=Array.from(scope.querySelectorAll('input:not([type=hidden]):not([type=checkbox]):not([type=radio]),textarea,select,[contenteditable=true]'));
      const text=compact(scope.innerText,300);
      if (fields.length!==1) {
        const range=fields.length===2 && fields.every(f=>f.matches('input') && (f.type==='date' || /date|日期/i.test(f.getAttribute('placeholder')||''))) && /[~～–—至]/.test(text);
        if (!range) break;
        if (text.replace(/[\s~～–—至]/g,'')) return `${text} [field ${fields.indexOf(e)+1}/2]`;
        continue;
      }
      if (text) return text;
    }
    return '';
  };
  const fingerprint = (e, name=label(e), context=contextInfo(e).identity) => JSON.stringify([e.tagName, name, ...['id','name','type','role','href','data-testid','aria-controls','aria-sort','data-column-key','data-field'].map(a => e.getAttribute(a)), context,
    e.type!=='password' && e.matches('input,textarea,select,[contenteditable=true]') ? fieldContext(e) : '']);
  const disabled = e => !!e.disabled || e.matches(':disabled') || ancestors(e).some(p => p.inert || p.getAttribute('aria-disabled') === 'true');
  const editable = (e, isDisabled=disabled(e)) => (e.matches('textarea,input:not([type]),input[type=text],input[type=search],input[type=email],input[type=url],input[type=tel],input[type=number],input[type=password]') || e.isContentEditable)
    && !e.readOnly && !ancestors(e).some(p=>p.getAttribute('aria-readonly')==='true') && !isDisabled;
  const roots = () => {
    const result = [document];
    for (let i = 0; i < result.length; i++) for (const e of result[i].querySelectorAll('*')) if (e.shadowRoot) result.push(e.shadowRoot);
    return result;
  };
  let entries = new Map(), version = '', documentId, captureGutter, captureTimer;
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
  const scrollable = (e, axis) => {
    const horizontal=axis==='x';
    if(!(horizontal?e.scrollWidth>e.clientWidth+1:e.scrollHeight>e.clientHeight+1))return false;
    const style=getComputedStyle(e);
    return e===document.scrollingElement || /auto|scroll|overlay/.test(horizontal?style.overflowX:style.overflowY);
  };
  const wheelPoint = (e, axis) => points(e).find(p=>{
    const hit=hitAt(p.x,p.y);
    if(!contains(e,hit))return false;
    // A descendant button can receive a wheel; a nested scrolling pane must not steal it.
    for(let node=hit;node && node!==e;node=parent(node))if(scrollable(node,axis))return false;
    return true;
  });
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
    apiVersion: 20,
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
      documentId ||= nonce;
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
      const controlSelector='button,a[href],input:not([type=hidden]),textarea,select,[role=button],[role=link],[role=tab],[role=checkbox],[role=combobox],[contenteditable=true],summary,[role=radio],[role=menuitem],[role=menuitemcheckbox],[role=menuitemradio],[role=option],[aria-haspopup],[onclick]';
      const selector=controlSelector+',canvas,[role=region],[aria-expanded],[tabindex],th,[role=columnheader]';
      const candidates=[], seen=new Set(), containers=new Set(), pointerTargets=new Set();
      let pointerScanned=0,customTargetsTruncated=false;
      for (const root of allRoots) {
        for (const e of root.querySelectorAll(selector)) { if (!seen.has(e)) {seen.add(e);candidates.push(e);} }
        // ponytail: O(n) geometry/visibility scan, at most 4000 visible custom nodes; huge DOMs need a spatial index.
        for (const e of root.querySelectorAll('*')) {
          if (e.matches(controlSelector)) continue;
          const r=e.getBoundingClientRect();
          if (!(r.width>0 && r.height>0 && r.bottom>0 && r.top<innerHeight && r.right>0 && r.left<innerWidth)) continue;
          const style=getComputedStyle(e);
          if (style.visibility==='hidden' || style.visibility==='collapse') continue;
          if (++pointerScanned>4000) {customTargetsTruncated=true;break;}
          // Vimium-style hints represent controls, not their inherited pointer-styled text children.
          // Keep a cursor boundary as a fallback for framework widgets without roles or onclick attributes.
          if (style.cursor!=='pointer' || r.width*r.height>=innerWidth*innerHeight*.25) continue;
          // Text-less pointer icons (e.g. a sort caret inside a clickable header) are separate targets;
          // nested icon wrappers keep only the outermost small one.
          const text=label(e), up=parent(e), upRect=up?.getBoundingClientRect();
          const icon=!text && r.width<=48 && r.height<=48
            && !(up && upRect.width<=48 && upRect.height<=48 && getComputedStyle(up).cursor==='pointer' && !label(up)) && iconHint(e);
          if ((text || icon)
              && (icon || e.tagName==='LI' || !up || getComputedStyle(up).cursor!=='pointer') && !e.querySelector(controlSelector+',li')
              && !ancestors(parent(e)).some(a=>a.matches(controlSelector))) {
            if (!seen.has(e)) {seen.add(e);candidates.push(e);} pointerTargets.add(e);
          }
        }
        for (const e of root.querySelectorAll('*')) {
          if (e!==document.scrollingElement && (scrollable(e,'y') || scrollable(e,'x'))) {
            containers.add(e); if (!seen.has(e)) {seen.add(e);candidates.push(e);}
          }
        }
      }
      for (const e of candidates) {
        // A semantic span wrapping another control is a likely duplicate, as in Vimium's false-positive pass.
        if (e.tagName==='SPAN' && e.querySelector(controlSelector)) continue;
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
        const checkable=e.matches('input[type=checkbox],input[type=radio]');
        const table=e.closest('table,[role=table],[role=grid],[role=treegrid]');
        const header=e.closest('th,[role=columnheader]');
        const column=header&&table&&header.closest('table,[role=table],[role=grid],[role=treegrid]')===table ? {
          table:allRoots.flatMap(root=>Array.from(root.querySelectorAll('table,[role=table],[role=grid],[role=treegrid]'))).indexOf(table),
          index:Array.from(table.querySelectorAll('th,[role=columnheader]')).indexOf(header),name:compact(header.innerText,160),
          row:header.closest('tr')?.rowIndex??null,colSpan:header.colSpan||1,rowSpan:header.rowSpan||1,
          group:compact(header.getAttribute('aria-description')||header.getAttribute('title')||'',160)
        }:undefined;
        const field=e.matches('input,textarea,select,[contenteditable=true]')&&e.type!=='password'?fieldContext(e):undefined;
        const item={ref,nodeId:`${documentId}:${identity(e)}`,role:e.getAttribute('role')||(checkable?e.type:e.tagName.toLowerCase()),tag:e.tagName.toLowerCase(),name,
          href:e.href||undefined,expanded:e.getAttribute('aria-expanded')??undefined,haspopup:e.getAttribute('aria-haspopup')??undefined,selected:e.getAttribute('aria-selected')??e.getAttribute('aria-checked')??(checkable?e.checked:undefined),
          column,sort:e.getAttribute('aria-sort')??undefined,columnKey:e.getAttribute('data-column-key')??e.getAttribute('data-field')??undefined,
          actionable:e.hasAttribute('onclick') || !!e.onclick || pointerTargets.has(e),
          icon:!name || (r.width<=48 && r.height<=48) || header ? iconHint(e) : undefined,
          region,fieldContext:field,
          dateValue:field?.match(/\[field [12]\/2\]$/) && /^(?:\d{4}-\d{2}-\d{2})?$/.test(e.value)?e.value:undefined,
          password:e.type==='password',tabIndex:e.tabIndex,value:e.type==='password'?undefined:compact(e.value,100),disabled:isDisabled,editable:editable(e,isDisabled),
          inView,point:p,viewportRect:{x:r.x,y:r.y,width:r.width,height:r.height},documentRect:{x:r.left+scrollX,y:r.top+scrollY,width:r.width,height:r.height},
          scroll:containers.has(e)?{top:e.scrollTop,height:e.scrollHeight,viewportHeight:e.clientHeight,
            left:e.scrollLeft,width:e.scrollWidth,viewportWidth:e.clientWidth,
            minLeft:getComputedStyle(e).direction==='rtl'?Math.min(0,e.clientWidth-e.scrollWidth):0,
            maxLeft:getComputedStyle(e).direction==='rtl'?0:Math.max(0,e.scrollWidth-e.clientWidth),
            pointY:scrollable(e,'y')?wheelPoint(e,'y'):undefined,pointX:scrollable(e,'x')?wheelPoint(e,'x'):undefined}:undefined,
          blockedBy:blocker?compact(label(blocker)||blocker.outerHTML,180):undefined};
        if (e.tagName === 'CANVAS') {
          item.canvas={width:e.width,height:e.height,coordinateSpace:'CSS viewport, not backing-store pixels',content:'visual-only; DOM cannot enumerate drawn controls'};
          canvases.push({ref,name,inView,viewportRect:rect(e),backingWidth:e.width,backingHeight:e.height});
          if (p && r.width>=64 && r.height>=64) visualArea+=Math.max(0,Math.min(r.right,innerWidth)-Math.max(0,r.left))*Math.max(0,Math.min(r.bottom,innerHeight)-Math.max(0,r.top));
        }
        items.push(item);
      }
      const fullText=[document.body?.innerText||'',...allRoots.slice(1).map(root=>Array.from(root.children).filter(e=>!['STYLE','SCRIPT'].includes(e.tagName)).map(e=>e.innerText||'').join('\n'))].join('\n');
      // ponytail: scan at most 20k text nodes / 12k visible characters; larger pages need scoped observation.
      // Retain fullText for data extraction and completion checks.
      const visibleLines=[];let visibleChars=0,scannedText=0;
      for(const root of allRoots) {
        const walker=document.createTreeWalker(root,NodeFilter.SHOW_TEXT);
        for(let node; (node=walker.nextNode()) && scannedText++<20000 && visibleChars<12000;) {
          const e=node.parentElement,text=compact(node.textContent,500);
          if(!text || !e || e.closest('script,style,noscript') || e.checkVisibility?.({checkOpacity:true,checkVisibilityCSS:true})===false)continue;
          const r=e.getBoundingClientRect();
          if(r.bottom<=0 || r.top>=innerHeight || r.right<=0 || r.left>=innerWidth || !visible(e))continue;
          visibleLines.push(text);visibleChars+=text.length;
        }
      }
      const headings=allRoots.flatMap(root=>Array.from(root.querySelectorAll('h1,h2,h3,h4,h5,h6,[role=heading]')).map(e=>({level:e.getAttribute('aria-level')||e.tagName,text:compact(e.innerText,300)})));
      const declaredRows=allRoots.flatMap(root=>Array.from(root.querySelectorAll('[aria-rowcount],[aria-setsize]')).map(e=>({total:Number(e.getAttribute('aria-rowcount')||e.getAttribute('aria-setsize')),rendered:e.querySelectorAll('[role=row],[role=listitem]').length})));
      // Only explicit pagination totals are evidence; a label such as "Metrics 4" is not a row count.
      const paginationTotals=Array.from(fullText.matchAll(/(?:^|\n)\s*((?:\d{1,3}(?:,\d{3})+|\d+))\s+total\s+(?:items|records)\s*(?=\n|$)|(?:^|\n)\s*共\s*([\d,]+)\s*条(?:记录|数据)?\s*(?=\n|$)/gi),m=>({totalRows:Number((m[1]||m[2]).replaceAll(',','')),evidence:m[0].trim()}));
      const tableSelector='table,[role=table],[role=grid],[role=treegrid]';
      const tableNodes=allRoots.flatMap(root=>Array.from(root.querySelectorAll(tableSelector)))
        .filter(e=>e.getBoundingClientRect().width>0 && !['hidden','collapse'].includes(getComputedStyle(e).visibility));
      let cellBudget=24000;
      // ponytail: preview at most 8 tables / 10 rows / 16 cells within 24k chars; use documentPath or query for more.
      const tables=tableNodes.slice(0,8).map((table,index)=>{
        const rows=Array.from(table.querySelectorAll('tr,[role=row]')).filter(r=>r.closest(tableSelector)===table
          && r.getBoundingClientRect().height>0 && !['hidden','collapse'].includes(getComputedStyle(r).visibility));
        const dataRows=rows.filter(r=>r.querySelector('td,[role=cell],[role=gridcell]'));
        const headers=Array.from(table.querySelectorAll('th,[role=columnheader]')).filter(c=>c.closest(tableSelector)===table).slice(0,16).map(c=>compact(c.innerText,160));
        const declared=Number(table.getAttribute('aria-rowcount'));
        const hasDeclared=table.hasAttribute('aria-rowcount') && Number.isSafeInteger(declared) && declared>=0;
        const pagination=tableNodes.length===1 && paginationTotals.length===1 ? paginationTotals[0] : null;
        const totalRows=pagination?.totalRows ?? (hasDeclared ? Math.max(0,declared-(rows.length-dataRows.length)) : null);
        let cellsTruncated=false;
        const preview=[];
        for(const row of dataRows.slice(0,10)) {
          if(cellBudget<=0) break;
          const cells=Array.from(row.querySelectorAll('td,th,[role=cell],[role=gridcell],[role=rowheader]')).filter(c=>c.closest('tr,[role=row]')===row);
          const values=cells.slice(0,16).map(c=>{
            const full=String(c.innerText||'').replace(/\s+/g,' ').trim();
            const text=full.slice(0,Math.min(160,cellBudget));cellBudget-=text.length;
            cellsTruncated ||= text.length<full.length;return text;
          });
          cellsTruncated ||= cells.length>values.length;
          preview.push(values);
        }
        const columns=Array.from(table.querySelectorAll('th,[role=columnheader]')).filter(c=>c.closest(tableSelector)===table).slice(0,16).map((c,columnIndex)=>({
          index:columnIndex,row:c.closest('tr')?.rowIndex??null,colSpan:c.colSpan||1,rowSpan:c.rowSpan||1,name:compact(c.innerText,160),sort:c.getAttribute('aria-sort'),key:c.getAttribute('data-column-key')||c.getAttribute('data-field'),
          ref:items.find(i=>entries.get(i.ref)?.e===c)?.ref,group:compact(c.getAttribute('aria-description')||c.getAttribute('title')||'',160)}));
        return {index,headers,columns,totalRows,totalRowsSource:pagination?.evidence||(hasDeclared?'aria-rowcount minus observed header rows':null),
          loadedRows:dataRows.length,returnedRows:preview.length,rows:preview,
          previewTruncated:preview.length<dataRows.length||cellsTruncated,
          countMismatch:totalRows!==null&&totalRows<dataRows.length,
          moreRows:totalRows===null||totalRows<dataRows.length?null:totalRows>dataRows.length,
          next:'日期、区域和表格排序字段/方向必须分别验证；图表 Metrics 不代表表格排序，同名列需核对列索引/分组。Top N 必须读取足够的数据行并核对筛选及排序；不足时读取 documentPath、inspect(query) 或翻页/滚动后观察。Metrics 等标签数字不是总条数。'};
      });
      const loading=document.readyState!=='complete'||visibleLines.some(text=>/^(?:loading|加载中)(?:\.{3}|…)?$/i.test(text))
        ||allRoots.some(root=>Array.from(root.querySelectorAll('[aria-busy=true],[role=progressbar]')).some(visible));
      return {url:location.href,title:document.title,readyState:document.readyState,loading,version,stamp:stamp(),text:fullText.slice(0,1000000),visibleText:visibleLines.join('\n'),textLength:fullText.length,
        tables,tableCount:tableNodes.length,tablesTruncated:tableNodes.length>tables.length,paginationTotals,
        items,totalItems:total,customTargetsTruncated,headings:headings.slice(0,1000),canvases:canvases.slice(0,100),visualSuggested:visualArea>=innerWidth*innerHeight*.15,focus:api.inputState(),
        truncated:customTargetsTruncated||total>items.length||fullText.length>1000000||headings.length>1000||canvases.length>100,
        coverage:{lazyImages:allRoots.reduce((n,root)=>n+Array.from(root.querySelectorAll('img[loading=lazy]')).filter(e=>!e.complete||!e.naturalWidth).length,0),scope:scope==='viewport'?'viewport DOM only; use scope=all for offscreen targets':'loaded DOM including offscreen, open shadow roots and scroll containers',declaredRows,canvasCount:canvases.length,note:'Canvas/WebGL controls are pixels, not DOM targets. Use the imageId and image-pixel coordinates in the returned visual observation. Virtualized/unloaded content requires observation after scrolling.'},
        documentSize:{width:Math.max(document.documentElement.scrollWidth,document.body?.scrollWidth||0),height:Math.max(document.documentElement.scrollHeight,document.body?.scrollHeight||0)},
        viewport:{width:innerWidth,height:innerHeight,scrollX,scrollY,minScrollX:getComputedStyle(document.scrollingElement).direction==='rtl'?Math.min(0,innerWidth-document.scrollingElement.scrollWidth):0,devicePixelRatio,scale:visualViewport?.scale||1},
        timingsMs:{observe:performance.now()-started},scannedRoots:allRoots.length};
    },
    async prepare(ref, mode='click') {
      const wheel=mode==='scroll_x'||mode==='scroll_y';
      const e=ref?entryFor(ref).e:wheel?document.scrollingElement:null,started=performance.now();
      if(!e)throw Error('缺少目标引用');
      if (mode==='fill' && !editable(e)) throw Error('目标不是可填写字段或只读');
      let previous,stableFrames=0,scrolled=false,reason='目标正在移动';
      do {
        if(ref)entryFor(ref);
        if (disabled(e)) reason='目标不可用';
        else {
          const p=wheel?wheelPoint(e,mode==='scroll_x'?'x':'y'):clickable(e),r=rect(e);
          const moving=ancestors(e).some(node=>node.getAnimations?.().some(animation=>
            (animation.pending || animation.playState==='running') && animation.effect?.getKeyframes?.().some(frame=>
              Object.keys(frame).some(key=>/^(transform|translate|rotate|scale|left|right|top|bottom|width|height|offsetPath|fontSize|margin.*|padding.*)$/.test(key)))));
          stableFrames=p && !moving && sameRect(previous,r) ? stableFrames+1 : 0;
          if (stableFrames>=2) return {...p,localX:p.x,localY:p.y,rect:r,editable:editable(e),password:e.type==='password',canvas:e.tagName==='CANVAS',scroll:wheel?{left:e.scrollLeft,top:e.scrollTop}:undefined};
          if (!p && !scrolled && !wheel) {e.scrollIntoView({block:'nearest',inline:'nearest',behavior:'instant'});scrolled=true;previous=undefined;}
          else {previous=r;reason=p?'目标正在移动':'目标被遮挡或在视口之外';}
        }
        await tick();
      } while (performance.now()-started<1200);
      throw Error(reason+'；有界等待后仍不可安全操作，请重新观察');
    },
    async scrollFeedback(ref, before) {
      const e=ref?entries.get(ref)?.e:document.scrollingElement;
      if(!e?.isConnected)return {changed:null,reason:'scroll container replaced'};
      const started=performance.now();let last,quiet=0;
      do {
        await tick();
        const now={left:e.scrollLeft,top:e.scrollTop};
        const changed=now.left!==before.left || now.top!==before.top;
        quiet=changed && last?.left===now.left && last?.top===now.top?quiet+1:0;
        if(quiet>=2)return {...now,changed};
        last=now;
      } while(performance.now()-started<600);
      return {...last,changed:last.left!==before.left || last.top!==before.top};
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
