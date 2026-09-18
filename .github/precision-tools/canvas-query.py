from pathlib import Path
p=Path('src-tauri/src/native_browser_page.js');s=p.read_text()
old='const items = [], headings = [], declaredRows = [], shadowText = [], candidates = [];'
assert s.count(old)==1;s=s.replace(old,'const items = [], headings = [], declaredRows = [], shadowText = [], candidates = [], visualRegions = [];')
old="        const name=label(e),region=regionOf(e);\n        if(search"
new="""        const name=label(e),region=regionOf(e);
        // Canvas pixels are not searchable DOM text. A query for a drawn label
        // must still return an image, even when no DOM candidate matches it.
        if (e.tagName === 'CANVAS' && visualRegions.length < 128) {
          const rect=geometry(e);
          if (rect.width>0 && rect.height>0 && rect.x<innerWidth && rect.y<innerHeight
            && rect.x+rect.width>0 && rect.y+rect.height>0 && perceptible(e))
            visualRegions.push({kind:'canvas',name,viewportRect:rect,bitmapWidth:e.width,bitmapHeight:e.height});
        }
        if(search"""
assert s.count(old)==1;s=s.replace(old,new)
old='const visualRequired = items.some(i => i.visual && i.inView && i.viewportRect.width>=96 && i.viewportRect.height>=48);'
assert s.count(old)==1;s=s.replace(old,'const visualRequired = visualRegions.some(i => i.viewportRect.width>=96 && i.viewportRect.height>=48);')
old='items,totalItems:total,headings:headings.slice(0,1000),visualRequired,'
assert s.count(old)==1;s=s.replace(old,'items,totalItems:total,headings:headings.slice(0,1000),visualRequired,visualRegions,')
p.write_text(s)
p=Path('scripts/browser-targeting.test.mjs');s=p.read_text()
old="    assert.equal(item('画布工作区').visual.bitmapWidth,1600);assert.equal(obs.visualRequired,true);"
assert s.count(old)==1
s=s.replace(old,old+"""
    const visualQuery=await evaluate("__novaWebview.observe('canvas-query',20000,'only painted pixels')");
    assert.equal(visualQuery.items.length,0);
    assert.equal(visualQuery.visualRequired,true,'query must not hide Canvas visual feedback');
    assert.equal(visualQuery.visualRegions[0].bitmapWidth,1600);
    obs=await observe();""")
p.write_text(s)
