// node scripts/markdown-chunks.test.mjs — real renderer, isolated browser; no user session/IPC.
import assert from 'node:assert/strict';
import { access, readFile, readdir } from 'node:fs/promises';
import { homedir } from 'node:os';
import { createServer } from 'node:http';
import { build } from 'esbuild';
import { transformAsync } from '@babel/core';
import solid from 'babel-preset-solid';
import { chromium } from 'playwright-core';
import { Marked } from 'marked';

const helper = await build({ entryPoints: ['src/markdownChunks.ts'], bundle: true, write: false, format: 'esm', platform: 'node' });
const { chunkMarkdown, markdownTokenText } = await import(`data:text/javascript;base64,${Buffer.from(helper.outputFiles[0].text).toString('base64')}`);
const marked = new Marked({ gfm: true, breaks: true });
const padding = 'large answer **bold** 中文\n\n'.repeat(160);
const cases = [
  padding + '```ts\na\n\nb\n```\n\n7. seven\n\n8. eight\n\n' + padding,
  padding + '| a | b |\n| - | - |\n| c | d |\n\n> quote\n>\n> next\n\n' + padding,
  '[label][target]\n\n' + padding + '[target]: https://example.org "title"\n',
  padding + '<div>\n\na\n\n</div>\n',
  (padding + 'tail').replace(/\n/g, '\r\n'),
];
for (const source of cases) {
  const model = chunkMarkdown(marked, source);
  assert.equal(model.chunks.map(c => marked.parser(c.tokens)).join(''), marked.parse(source), 'Chunk HTML must equal whole-document HTML');
  assert.equal(model.chunks.map(c => c.source).join(''), source.replace(/\r\n?/g, '\n'));
}
let model = chunkMarkdown(marked, '[late][ref]\n\n' + padding);
const stable = model.chunks[0];
model = chunkMarkdown(marked, model.source + 'ongoing paragraph', model);
assert.equal(model.chunks[0], stable, 'Append must retain stable chunks');
model = chunkMarkdown(marked, model.source + '\n\n[ref]: https://example.org\n', model);
assert.equal(model.chunks.map(c => marked.parser(c.tokens)).join(''), marked.parse(model.source), 'Late definitions must resolve old references');
assert.equal(markdownTokenText(marked.lexer('alpha **bold** omega')), 'alpha bold omega');
for (const tail of ['```ts\na\n\nb\n```\n', '7. a\n\n   b\n\n8. c\n', '[late][x]\n\n[x]: https://example.org\n']) {
  let growing=chunkMarkdown(marked,padding);
  for (let i=1;i<=tail.length;i++) {
    growing=chunkMarkdown(marked,padding+tail.slice(0,i),growing);
    assert.equal(growing.chunks.map(c=>marked.parser(c.tokens)).join(''),marked.parse(growing.source),'Partial streaming structure changed semantics');
  }
}

const fixture = await transformAsync(`import {render} from 'solid-js/web'; import {createSignal} from 'solid-js';
    import {Markdown} from './src/components/Markdown';
    const [text,setText]=createSignal(''); const [live,setLive]=createSignal(false);
    window.load=(value,stream=false)=>{setLive(stream);setText(value)};
    render(()=><div class="html-transcript" style="height:600px;overflow:auto;width:800px;overflow-anchor:none"><div class="html-turn"><Markdown text={text()} virtual live={live()} markFiles /></div></div>,document.body);`,
  {filename:'markdown-check.tsx',configFile:false,babelrc:false,presets:[[solid,{generate:'dom'}]],parserOpts:{plugins:['jsx','typescript']}});
const bundle = await build({
  stdin: { contents: fixture.code, resolveDir: process.cwd(), sourcefile: 'markdown-check.tsx', loader: 'ts' },
  bundle: true, write: false, format: 'esm', platform: 'browser', conditions: ['browser'], outdir: '/tmp/markdown-check',
  plugins: [{ name: 'check', setup(builder) {
    builder.onResolve({ filter: /(?:^|\/)(store|ipc|FileContextMenu)$/ }, args => ({ path: args.path.split('/').at(-1), namespace: 'mock' }));
    builder.onLoad({ filter: /.*/, namespace: 'mock' }, args => ({ contents: {
      store: 'export const state={currentId:null};',
      ipc: 'export const api={}; export const fileUriPath=s=>s;',
      FileContextMenu: 'export const createFileContextMenu=()=>({open(){},Menu:()=>null});',
    }[args.path], loader: 'js' }));
    builder.onLoad({ filter: /\.tsx$/ }, async ({path}) => {
      const output = await transformAsync(await readFile(path, 'utf8'), {filename:path,configFile:false,babelrc:false,presets:[[solid,{generate:'dom'}]],parserOpts:{plugins:['jsx','typescript']}});
      return {contents:output.code,loader:'ts'};
    });
  }}],
});
const js = bundle.outputFiles.find(f => f.path.endsWith('.js')).text;
const css = (bundle.outputFiles.find(f => f.path.endsWith('.css'))?.text ?? '') + '\n.html-turn .markdown,.md-virtual>.md-seg{display:flex;flex-direction:column}.markdown p{margin:0 0 10px;line-height:24px}';
const server = createServer((req,res)=>{res.setHeader('Content-Type',req.url==='/bundle.js'?'text/javascript':'text/html');res.end(req.url==='/bundle.js'?js:`<style>${css}</style><script type="module" src="/bundle.js"></script>`)});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const cache = `${homedir()}/.cache/ms-playwright`;
const candidates = [process.env.TEST_BROWSER, chromium.executablePath(), '/usr/bin/chromium',
  ...(await readdir(cache).catch(()=>[])).filter(name=>name.startsWith('chromium_headless_shell-')).sort().reverse().map(name=>`${cache}/${name}/chrome-headless-shell-linux64/chrome-headless-shell`)];
let executablePath;
for (const candidate of candidates.filter(Boolean)) { try { await access(candidate); executablePath=candidate; break; } catch {} }
assert.ok(executablePath, 'Set TEST_BROWSER to an installed Chromium');
const browser = await chromium.launch({headless:true,executablePath,args:['--no-sandbox']});
try {
  const page = await browser.newPage();
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.goto(`http://127.0.0.1:${server.address().port}`);
  await page.waitForFunction(()=>window.load);
  const source = padding.repeat(75) + '\n\nhidden alpha **bold** omega\n\n```ts\nconsole.log("copy")\n```\n\n`src/check.ts`\n\n<script>window.BAD=true</script>\n\nend-marker';
  // Raw HTML is separately tested for safety/indivisibility, not used in bounded fixture.
  const bounded = source.replace('<script>window.BAD=true</script>', 'safe');
  await page.evaluate(text=>window.load(text),bounded);
  await page.waitForFunction(()=>document.querySelector('[data-markdown-chunk] p'));
  assert.ok(await page.locator('.markdown *').count()<1000,'Single 300k reply must have bounded DOM');
  await page.evaluate(()=>document.querySelector('.html-transcript').scrollTop=180000);
  await page.waitForTimeout(160);
  const anchorBefore=await page.evaluate(()=>{
    const root=document.querySelector('.html-transcript'),top=root.getBoundingClientRect().top;
    window.anchorNode=[...document.querySelectorAll('.md-virtual p')].find(p=>p.getBoundingClientRect().bottom>top);
    return window.anchorNode.getBoundingClientRect().top-top;
  });
  // Simulate font/image reflow above the reading block. Real ResizeObserver must
  // compensate without user scroll or relying on the parent's group-only anchor.
  await page.evaluate(()=>{
    const chunk=window.anchorNode.closest('[data-markdown-chunk]');
    const previous=chunk.previousElementSibling;
    if(previous?.firstElementChild)previous.firstElementChild.style.paddingTop='90px';
    else chunk.style.paddingTop='90px';
  });
  await page.waitForTimeout(100);
  const anchorAfter=await page.evaluate(()=>window.anchorNode.getBoundingClientRect().top-document.querySelector('.html-transcript').getBoundingClientRect().top);
  assert.ok(Math.abs(anchorBefore-anchorAfter)<2,`Internal reflow moved reading anchor by ${anchorAfter-anchorBefore}px`);
  await page.evaluate(()=>document.querySelector('.html-transcript').style.width='480px');
  await page.waitForTimeout(140);
  assert.ok(await page.locator('.markdown *').count()<1000,'Resize must keep DOM bounded');
  const resizedAnchor=await page.evaluate(()=>window.anchorNode.getBoundingClientRect().top-document.querySelector('.html-transcript').getBoundingClientRect().top);
  assert.ok(Math.abs(anchorBefore-resizedAnchor)<2,'Resize must retain the native reading block');
  const found=await page.evaluate(()=>{
    const detail={query:'hidden alpha bold omega'};
    document.querySelector('[data-markdown-virtual]').dispatchEvent(new CustomEvent('markdown-reveal',{detail}));
    const found=!!detail.element?.textContent.includes('hidden alpha bold omega');
    detail.element?.scrollIntoView();return found;
  });
  assert.ok(found,'Source search must reveal offscreen cross-inline match');
  await page.waitForFunction(()=>document.querySelector('.code-copy') && document.querySelector('.md-file-ref'));
  // Pin native selected nodes while their chunk scrolls offscreen, then release.
  await page.evaluate(()=>{
    const p=[...document.querySelectorAll('p')].find(p=>p.textContent.includes('hidden alpha'));
    const range=document.createRange();range.selectNodeContents(p);const s=getSelection();s.removeAllRanges();s.addRange(range);
    document.querySelector('[data-markdown-virtual]').dispatchEvent(new CustomEvent('markdown-reveal',{detail:{query:''}}));
    document.querySelector('.html-transcript').scrollTop=0;
  });
  await page.waitForTimeout(180);
  assert.equal(await page.evaluate(()=>getSelection().toString()),'hidden alpha bold omega');
  await page.evaluate(()=>getSelection().removeAllRanges());
  await page.waitForFunction(()=>!document.body.textContent.includes('hidden alpha'));
  assert.ok(await page.locator('.markdown *').count()<1000);
  // Streaming retains the stable prefix's actual nodes and flushes completion.
  await page.evaluate(()=>{window.saved=document.querySelector('[data-markdown-chunk="0"] p')});
  await page.evaluate(text=>window.load(text+'\n\nstream-tail',true),bounded);
  await page.waitForTimeout(300);
  assert.ok(await page.evaluate(()=>window.saved===document.querySelector('[data-markdown-chunk="0"] p')));
  await page.evaluate(text=>window.load(text+'\n\nstream-tail'),bounded);
  assert.ok(await page.evaluate(()=>{const detail={query:'stream-tail'};document.querySelector('[data-markdown-virtual]').dispatchEvent(new CustomEvent('markdown-reveal',{detail}));return !!detail.element}));
  await page.evaluate(text=>window.load(text),source);
  await page.evaluate(()=>document.querySelector('.html-transcript').scrollTop=0);
  await page.waitForFunction(()=>document.querySelector('.md-virtual p'));
  assert.equal(await page.evaluate(()=>window.BAD),undefined);
  assert.equal(await page.locator('.markdown script').count(),0);
  await page.evaluate(()=>window.load('short **again**'));
  await page.waitForFunction(()=>document.querySelector('.markdown')?.textContent==='short again\n');
  assert.equal(await page.locator('[data-markdown-virtual]').count(),0,'Shrinking must restore regular Markdown');
  assert.deepEqual(errors,[]);
  console.log('Markdown chunks: boundaries/references, bounded 300k DOM, hidden search, native selection, streaming identity, copy/files and sanitization passed.');
} finally { await browser.close(); await new Promise(resolve=>server.close(resolve)); }
