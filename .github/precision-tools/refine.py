from pathlib import Path

def change(path, old, new):
    p=Path(path);s=p.read_text(encoding='utf8')
    assert s.count(old)==1,(path,old,s.count(old))
    p.write_text(s.replace(old,new),encoding='utf8')

change('src-tauri/src/native_browser.rs','.abs()>.5)', '.abs()>0.5)')
change('src-tauri/src/native_browser.rs', '''    let mut observation = observe(app,args["query"].as_str().unwrap_or("")).await?;''', '''    if args["fullPage"] == true && !args["region"].is_null() {
        return Err("局部 region 只能与 fullPage=false 一起使用".into());
    }
    let mut observation = observe(app,args["query"].as_str().unwrap_or("")).await?;''')
change('src-tauri/src/native_browser_page.js', '''  const editable = e => !e.readOnly && (e.isContentEditable || e.matches('textarea,input:not([type=button]):not([type=submit]):not([type=checkbox]):not([type=radio]):not([type=file]):not([type=hidden])'));''', '''  const editable = e => !e.readOnly && (e.isContentEditable || e.tagName==='TEXTAREA'
    || (e.tagName==='INPUT' && ['text','search','tel','url','email','number','password'].includes(e.type)));
  const perceptible = e => !ancestor(e,n=>{const s=getComputedStyle(n);return s.display==='none' || s.visibility==='hidden' || s.visibility==='collapse' || Number(s.opacity)===0;});''')
change('src-tauri/src/native_browser_page.js', "    if (unavailable(entry.e)) throw Error('目标不可用');", "    if (unavailable(entry.e) || !perceptible(entry.e)) throw Error('目标不可用或不可见');")
change('src-tauri/src/native_browser_page.js', "        if (!r.width || !r.height || style.visibility === 'hidden' || style.visibility === 'collapse' || style.display === 'none') continue;", "        if (!r.width || !r.height || style.visibility === 'hidden' || style.visibility === 'collapse' || style.display === 'none' || Number(style.opacity)===0) continue;")
change('src-tauri/src/native_browser_page.js', "      const deadline = performance.now()+ms;", "      if (!entries.has(ref)) throw Error('等待目标引用不存在，请重新观察');\n      const deadline = performance.now()+ms;")
change('src-tauri/src/native_browser_page.js', '        const visible = exists && !!clickable(e);', '        const visible = exists && perceptible(e) && !!clickable(e);')
change('src-tauri/src/native_browser_page.js', "      if (!e || unavailable(e)) throw Error('目标不可用');", "      if (!e || unavailable(e) || !perceptible(e)) throw Error('目标不可用或不可见');")
change('src-tauri/src/native_browser_page.js', '      if (!t || !t.e.isConnected || hitAt(t.x,t.y)!==t.e', '      if (!t || !t.e.isConnected || unavailable(t.e) || !perceptible(t.e) || hitAt(t.x,t.y)!==t.e')
change('scripts/browser-targeting.test.mjs', "const baseline = process.env.BROWSER_BASELINE ? await readFile(process.env.BROWSER_BASELINE,'utf8') : null;", "const baselinePath = process.env.BROWSER_BASELINE || process.env.BROWSER_PAGE_BASELINE;\nconst baseline = baselinePath ? await readFile(baselinePath,'utf8') : null;")
change('scripts/browser-targeting.test.mjs', "    reports.push({dpr,checks:", '''    await page.evaluate(()=>{scrollTo(0,0);document.body.insertAdjacentHTML('beforeend',`<div id="extra" style="position:fixed;inset:0;z-index:20;background:white"><div id="transparent"><button id="hidden-target">隐藏按钮</button></div><input readonly aria-label="只读字段"><input type="range" aria-label="滑动数值"><iframe id="scaled-frame" style="position:absolute;left:50px;top:250px;width:200px;height:100px;border:4px solid;transform-origin:0 0;transform:scale(1.5,1.25)"></iframe></div>`);});
    obs=await observe();
    const hidden=item('隐藏按钮');
    await page.evaluate(()=>document.querySelector('#transparent').style.opacity='0');
    await assert.rejects(prepare(hidden.ref),/不可见/);
    assert.equal((await prepare(item('只读字段').ref)).editable,false);
    assert.equal((await prepare(item('滑动数值').ref)).editable,false);
    await assert.rejects(evaluate("__novaWebview.waitFor('invented','hidden','',0)"),/引用不存在/);
    let mapped=await evaluate("__novaWebview.frameOwner(document.querySelector('#scaled-frame'),{x:100,y:50})");
    assert.deepEqual(mapped,{x:206,y:317.5});
    await page.evaluate(()=>document.querySelector('#scaled-frame').style.transform='rotate(5deg)');
    await assert.rejects(evaluate("__novaWebview.frameOwner(document.querySelector('#scaled-frame'),{x:100,y:50})"),/旋转/);
    await page.evaluate(()=>document.querySelector('#scaled-frame').style.transform='scale(1.5,1.25)');
    await page.evaluate(()=>document.querySelector('#extra').insertAdjacentHTML('beforeend','<div style="position:absolute;left:150px;top:300px;width:150px;height:70px;background:red"></div>'));
    await assert.rejects(evaluate("__novaWebview.frameOwner(document.querySelector('#scaled-frame'),{x:100,y:50})"),/遮挡/);
    await page.evaluate(()=>document.querySelector('#extra').remove());
    reports.push({dpr,checks:''')
