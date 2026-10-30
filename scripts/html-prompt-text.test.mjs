// Isolated native-layout regression: node scripts/html-prompt-text.test.mjs
import assert from 'node:assert/strict';
import { readFile, readdir, access } from 'node:fs/promises';
import { homedir } from 'node:os';
import { createServer } from 'node:http';
import { build } from 'esbuild';
import { transformAsync } from '@babel/core';
import solid from '@solidjs/babel-plugin';
import { chromium } from 'playwright-core';

const fixture = `import {render} from '@solidjs/web'; import {createSignal,Show} from 'solid-js';
import {HtmlPromptText} from './src/components/HtmlPromptText';
const [text,setText]=createSignal(''), [shown,setShown]=createSignal(true);
const raf=window.requestAnimationFrame.bind(window);window.slices=[];window.requestAnimationFrame=cb=>raf(time=>{const start=performance.now();try{cb(time)}finally{window.slices.push({duration:performance.now()-start,name:cb.name||cb.toString().slice(0,80)})}});
window.load=value=>{setShown(true);setText(value)};window.unmount=()=>setShown(false);
render(()=><div id="scroll"><div class="ht-user-column"><div class="ht-bubble-wrap"><div class="ht-user-bubble"><Show when={shown()}><HtmlPromptText text={text()} /></Show></div></div></div></div>,document.body);
window.frames=[];let last=performance.now();window.resetFrames=()=>{window.frames=[];window.slices=[];last=performance.now();window.started=last};const tick=()=>{const now=performance.now();window.frames.push(now-last);last=now;requestAnimationFrame(tick)};requestAnimationFrame(tick);`;
const compiledFixture=await transformAsync(fixture,{filename:'prompt-text-check.tsx',configFile:false,babelrc:false,plugins:[[solid,{generate:'dom'}]],parserOpts:{plugins:['jsx','typescript']}});
const bundle = await build({
  stdin:{contents:compiledFixture.code,resolveDir:process.cwd(),sourcefile:'prompt-text-check.tsx',loader:'ts'},
  bundle:true,write:false,format:'esm',platform:'browser',conditions:['browser'],jsx:'preserve',
  plugins:[{name:'solid',setup(builder){builder.onLoad({filter:/\.tsx$/},async({path})=>({
    contents:(await transformAsync(await readFile(path,'utf8'),{filename:path,configFile:false,babelrc:false,plugins:[[solid,{generate:'dom'}]],parserOpts:{plugins:['jsx','typescript']}})).code,loader:'ts'
  }));}}]
});
const final = bundle;
const css = `body{margin:0} #scroll{width:800px;height:600px;overflow:auto;overflow-anchor:none}
.ht-user-column{max-width:85%;min-width:34px;display:flex;flex-direction:column;align-items:flex-end}
.ht-bubble-wrap{position:relative;max-width:100%}.ht-user-bubble{padding:10px 16px;font:14px/1.6 sans-serif}
.ht-user-text{display:block;padding-right:2px;white-space:pre-wrap;overflow-wrap:anywhere}`;
const server = createServer((req,res)=>{res.setHeader('Content-Type',req.url==='/bundle.js'?'text/javascript':'text/html');res.end(req.url==='/bundle.js'?final.outputFiles[0].text:`<style>${css}</style><script type="module" src="/bundle.js"></script>`)});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const cache=homedir()+'/.cache/ms-playwright';
const candidates=[process.env.TEST_BROWSER,chromium.executablePath(),'/usr/bin/chromium',...(await readdir(cache).catch(()=>[])).filter(x=>x.startsWith('chromium_headless_shell-')).sort().reverse().map(x=>`${cache}/${x}/chrome-headless-shell-linux64/chrome-headless-shell`)];
let executablePath;
for(const path of candidates.filter(Boolean)){try{await access(path);executablePath=path;break;}catch{}}
assert.ok(executablePath,'Set TEST_BROWSER to installed Chromium');
const browser=await chromium.launch({headless:true,executablePath,args:['--no-sandbox']});
try {
  const page=await browser.newPage();const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.goto(`http://127.0.0.1:${server.address().port}`);await page.waitForFunction(()=>window.load);
  async function load(text){
    await page.evaluate(text=>{getSelection().removeAllRanges();window.load(text)},text);
    await page.waitForFunction(()=>!document.querySelector('.ht-user-text').hasAttribute('aria-busy'));
  }
  // Compare every character near every artificial boundary, plus regular samples, against
  // one unsplit native Text node. Force only these small test fixtures visible for geometry.
  async function geometry(text){
    await load(text);
    const result=await page.evaluate(text=>{
      const host=document.querySelector('.ht-user-text');
      for(const child of host.children)child.style.contentVisibility='visible';
      const width=host.getBoundingClientRect().width;
      const baseline=document.createElement('span');baseline.className='ht-user-text';
      baseline.style.cssText=`position:absolute;left:850px;top:0;box-sizing:border-box;width:${width}px;font:14px/1.6 sans-serif`;
      baseline.textContent=text;document.body.append(baseline);
      const a=host.getBoundingClientRect(),b=baseline.getBoundingClientRect();
      const points=new Set();for(let i=0;i<text.length;i+=37)points.add(i);
      let at=0;const nodes=[];const walker=document.createTreeWalker(host,NodeFilter.SHOW_TEXT);let node;
      while(node=walker.nextNode()){nodes.push({node,start:at});at+=node.length;for(let i=Math.max(0,at-8);i<Math.min(text.length,at+8);i++)points.add(i)}
      const range=new Range();let mismatch=null;
      for(const i of points){
        const entry=nodes.find(e=>i>=e.start&&i<e.start+e.node.length);
        range.setStart(entry.node,i-entry.start);range.setEnd(entry.node,i-entry.start+1);const r=range.getBoundingClientRect();
        range.setStart(baseline.firstChild,i);range.setEnd(baseline.firstChild,i+1);const s=range.getBoundingClientRect();
        if(Math.abs((r.top-a.top)-(s.top-b.top))>.1||Math.abs((r.left-a.left)-(s.left-b.left))>.1){mismatch={i,near:text.slice(i-20,i+20),got:[r.left-a.left,r.top-a.top],expected:[s.left-b.left,s.top-b.top]};break;}
      }
      range.selectNodeContents(host);getSelection().removeAllRanges();getSelection().addRange(range);
      const copy=getSelection().toString();getSelection().removeAllRanges();range.selectNodeContents(baseline);getSelection().addRange(range);
      const nativeCopy=getSelection().toString();getSelection().removeAllRanges();baseline.remove();
      return {mismatch,copy,nativeCopy,source:host.textContent,height:a.height,expectedHeight:b.height,width,children:host.childElementCount};
    },text);
    assert.deepEqual(result.mismatch,null,JSON.stringify({mismatch:result.mismatch,width:result.width,children:result.children}));
    assert.ok(result.copy===result.nativeCopy,`Native selection differs: ${result.copy.length} vs ${result.nativeCopy.length}`);
    assert.ok(result.source===text,'No source characters inserted/deleted');
    assert.equal(result.height,result.expectedHeight,'No extra visual lines');
    return result;
  }
  for(const unit of ['长中文标点，。！？「引用」','English words and longUnbrokenWord0123456789 '.repeat(3),'😀 👩🏽‍💻 👨‍👩‍👧‍👦 🇨🇳 e\u0301 日本語 한국어 ','ไทยภาษา ทดสอบ देवनागरी नमस्ते ','a\t  b\n\n中文\r\n  next\rfinal\n']){
    const text=unit.repeat(Math.ceil(34000/unit.length));await geometry(text);
  }
  const bidi=('中文 abc مرحبا بالعالم עברית \u2067abc\u2069\n').repeat(1100);
  assert.equal((await geometry(bidi)).children,0,'Bidi keeps its complete native paragraph context');
  await load('existing preview');
  assert.equal(await page.evaluate(()=>{window.load('长'.repeat(724000));return document.querySelector('.ht-user-text').textContent}),'existing preview','Keep existing preview until atomic commit');
  // Replacement/unmount during a queued measurement must never resurrect stale content/probes.
  await page.evaluate(()=>{window.load('长'.repeat(724000));window.load('replacement')});
  await page.waitForTimeout(150);assert.equal(await page.locator('.ht-user-text').textContent(),'replacement');
  await page.evaluate(()=>{window.load('长'.repeat(724000));window.unmount()});
  await page.waitForTimeout(150);assert.equal(await page.locator('.ht-user-text').count(),0);
  assert.equal(await page.locator('body > [aria-hidden="true"]').count(),0,'Probe cleaned up on unmount');
  const huge='huge-start '+ '长'.repeat(724000)+' huge-end';
  await page.evaluate(()=>{window.resetFrames();window.load('huge-start '+'长'.repeat(724000)+' huge-end')});
  await page.waitForFunction(()=>!document.querySelector('.ht-user-text').hasAttribute('aria-busy'));
  await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
  const metrics=await page.evaluate(()=>({max:Math.max(...window.frames),slice:Math.max(...window.slices.map(s=>s.duration)),slowest:window.slices.sort((a,b)=>b.duration-a.duration).slice(0,3),chunks:document.querySelector('.ht-user-text').childElementCount,width:document.querySelector('.ht-user-text').getBoundingClientRect().width,totalMs:performance.now()-window.started}));
  assert.ok(metrics.max<250,`724K expand max frame ${metrics.max}ms`);
  // The callback timing includes setup and final DOM commit, not just range measurements.
  assert.ok(metrics.slice<=8,`724K preparation exceeded 8ms slice: ${metrics.slice}`);
  assert.ok(metrics.chunks>80&&metrics.chunks<400,'Bounded native chunks, not a giant fallback');
  const heights=await page.evaluate(async()=>{const host=document.querySelector('.ht-user-text'),scroll=document.querySelector('#scroll');const heights=[host.getBoundingClientRect().height];for(const part of [1,.5,0]){scroll.scrollTop=scroll.scrollHeight*part;await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));heights.push(host.getBoundingClientRect().height)}return heights;});
  assert.ok(heights.every(height=>Math.abs(height-heights[0])<.1),`Skipped chunks retain measured height: ${heights.join(',')}; ${JSON.stringify(metrics)}`);
  const full=await page.evaluate(()=>{const range=new Range();range.selectNodeContents(document.querySelector('.ht-user-text'));getSelection().removeAllRanges();getSelection().addRange(range);return getSelection().toString()});
  assert.equal(full,huge,'Full cross-screen native selection includes offscreen content');
  await page.evaluate(()=>getSelection().removeAllRanges());
  // Width changes restart native measurements, preserving source and backward selections.
  const text=('selection emoji 😀 中文 words ').repeat(1800);await load(text);
  await page.evaluate(()=>{const host=document.querySelector('.ht-user-text');const a=host.firstElementChild.firstChild,b=host.children[2].firstChild;getSelection().setBaseAndExtent(b,20,a,5);window.selected=getSelection().toString();document.querySelector('#scroll').style.width='630px'});
  await page.waitForFunction(()=>document.querySelector('.ht-user-text').hasAttribute('aria-busy'));
  await page.waitForFunction(()=>!document.querySelector('.ht-user-text').hasAttribute('aria-busy'));
  assert.ok(await page.evaluate(()=>getSelection().toString()===window.selected),'Resize keeps backward native selection');
  await geometry(text);
  assert.deepEqual(errors,[]);
  console.log('HtmlPromptText: exact native wrap/selection, multilingual/bidi/emoji, cancel/unmount, resize and 724K bounded expand passed.',metrics);
} finally {await browser.close();await new Promise(resolve=>server.close(resolve))}
