import {createServer} from 'vite';import {chromium} from 'playwright-core';import assert from 'node:assert/strict';import ExcelJS from 'exceljs';
import fs from 'node:fs/promises';
const name = 'workspace-sheet-check-' + process.pid;
const seed = new ExcelJS.Workbook();seed.addWorksheet('需求').getCell('A1').value = 'original';
const encoded = Buffer.from(await seed.xlsx.writeBuffer()).toString('base64');
await fs.writeFile(name + '.html', '<div id="root"></div><script type="module" src="/' + name + '.tsx"></script>');
await fs.writeFile(name + '.tsx', "\nimport {render} from 'solid-js/web';\nimport {createSignal,Show} from 'solid-js';\nimport WorkspacePanel from './src/components/WorkspacePanel';\nimport {api} from './src/ipc';import {setState} from './src/store';import './src/app.css';\nimport { Webview } from '@tauri-apps/api/webview';\nimport { mockWindows } from '@tauri-apps/api/mocks';\nimport { createImageAttachments } from './src/components/ImageAttachmentStrip';\nmockWindows('main');\nconst dropListeners = new Set();\nWebview.prototype.onDragDropEvent = function (callback) { dropListeners.add(callback); return Promise.resolve(() => dropListeners.delete(callback)); };\nwindow.simulateFileDrop = payload => { for (const listener of dropListeners) listener({ payload }); };\nfunction DropReceiver() { const attachments = createImageAttachments({ enableFileDrop: true }); window.attachmentCount = () => attachments.images().length; return null; }\nlet data=__DATA__;window.saved=()=>data;window.saves=0;\napi.previewWorkspaceFile=async(_,path)=>path.endsWith('xlsx')?{path,kind:'spreadsheet',text:null,data,size:1234}:{path,kind:'text',text:'other',data:null,size:5};\napi.saveWorkspaceFile=async(_,path,original,text)=>{if(original!==data)throw Error('external conflict');data=text;window.saves++};\napi.listWorkspaceDirectory=async()=>({entries:[],truncated:false});\nsetState({currentId:'sheet-check',cwd:'D:/demo',items:[]});\nconst [visible,setVisible]=createSignal(true),[request,setRequest]=createSignal({path:'test.xlsx'});\nwindow.openFile=path=>setRequest({path});window.showPanel=()=>setVisible(true);\nrender(()=><div style=\"display:flex;height:800px;width:1200px\"><main style=\"flex:1\"><DropReceiver /></main><Show when={visible()}><WorkspacePanel threadId=\"sheet-check\" request={request()} onClose={()=>setVisible(false)}/></Show></div>,document.getElementById('root'));\n".replace('__DATA__', JSON.stringify(encoded)));
const server=await createServer({server:{host:'127.0.0.1',port:5199,strictPort:true}});await server.listen();const browser=await chromium.launch({channel:'msedge',headless:true});const page=await browser.newPage({viewport:{width:1300,height:900},deviceScaleFactor:1.5});const errors=[];page.on('pageerror',e=>errors.push(e.message));
try{
await page.goto('http://127.0.0.1:5199/'+name+'.html',{waitUntil:'domcontentloaded',timeout:90000});
const canvas=page.locator('.workspace-sheet-host canvas').filter({visible:true});
await page.waitForFunction(()=>[...document.querySelectorAll('.workspace-sheet-host canvas')].some(c=>c.height>100),{timeout:90000});
await page.evaluate(async () => {
  const { api } = await import('/src/ipc.ts');
  api.listWorkspaceDirectory = async () => ({ entries: Array.from({ length: 24 }, (_, i) => ({ name: `file-${i}.txt`, path: 'other.txt', directory: false })), truncated: false });
});
await page.getByRole('button', { name: '选择项目文件', exact: true }).click();
await page.getByRole('treeitem').first().waitFor();
const pickerCovered = await page.locator('.workspace-picker').evaluate(picker => {
  const rect = picker.getBoundingClientRect();
  const covered = [];
  for (let y = rect.top + 12; y < rect.bottom - 12; y += 24) {
    for (let x = rect.left + 12; x < rect.right - 12; x += 40) {
      if (!picker.contains(document.elementFromPoint(x, y))) covered.push({ x, y });
    }
  }
  return covered;
});
assert.deepEqual(pickerCovered, [], 'file picker must cover every Univer canvas and editor layer');
await page.getByRole('button', { name: '选择项目文件', exact: true }).click();
await page.getByRole('button',{name:'保存表格',exact:true}).click();
assert.equal(await page.evaluate(()=>window.saves),0,'unchanged sheet should not save');
await canvas.last().click({position:{x:80,y:40}});await page.keyboard.type('edited');
await page.evaluate(()=>window.openFile('other.txt'));await page.getByRole('textbox',{name:'文件内容编辑'}).waitFor();
await page.getByRole('tab',{name:/test.xlsx/}).click();
await page.waitForFunction(()=>[...document.querySelectorAll('.workspace-sheet-host canvas')].some(c=>c.height>100));
await page.getByRole('button',{name:'保存表格',exact:true}).click();
await page.waitForFunction(()=>window.saves===1);
const b=new ExcelJS.Workbook();await b.xlsx.load(Buffer.from(await page.evaluate(()=>window.saved()),'base64'));assert.equal(b.worksheets[0].getCell('A1').value,'edited');
await page.evaluate(()=>window.openFile('test.xlsx'));await page.getByRole('button',{name:'关闭文件面板',exact:true}).click();await page.waitForFunction(()=>!document.querySelector('.workspace-panel'));await page.evaluate(()=>window.showPanel());
await page.waitForFunction(()=>[...document.querySelectorAll('.workspace-sheet-host canvas')].some(c=>c.height>100));
if (process.env.TEST_SCREENSHOT) await page.screenshot({path:process.env.TEST_SCREENSHOT});
await canvas.last().click({position:{x:80,y:40}});await page.keyboard.type('saved with shortcut');await page.keyboard.press('Control+s');
await page.waitForFunction(()=>window.saves===2);
const second = new ExcelJS.Workbook();await second.xlsx.load(Buffer.from(await page.evaluate(()=>window.saved()),'base64'));assert.equal(second.worksheets[0].getCell('A1').value,'saved with shortcut');
const dropPosition = await page.locator('.workspace-sheet-host').evaluate(host => {
  const r = host.getBoundingClientRect();
  return { x: (r.left + r.width / 2) * devicePixelRatio, y: (r.top + r.height / 2) * devicePixelRatio };
});
await page.evaluate(position => window.simulateFileDrop({ type: 'enter', position, paths: ['D:/outside/drop.txt'] }), dropPosition);
assert.equal(await page.locator('.workspace-panel.is-dragging').count(), 1);
await page.evaluate(position => window.simulateFileDrop({ type: 'drop', position, paths: ['D:/outside/drop.txt', 'D:/outside/second.txt', 'D:/outside/drop.txt'] }), dropPosition);
await page.getByRole('tab', { name: /second.txt/ }).waitFor();
assert.equal(await page.getByRole('tab', { name: /drop.txt/ }).count(), 1, 'repeated drop paths share one tab');
assert.equal(await page.evaluate(() => window.attachmentCount()), 0, 'sidebar drops must not also become chat attachments');
assert.equal(await page.locator('.workspace-panel.is-dragging').count(), 0);
await page.evaluate(async position => {
  const { setFileDropBlocked } = await import('/src/utils.ts');
  setFileDropBlocked(true);
  window.simulateFileDrop({ type: 'drop', position, paths: ['blocked.txt'] });
  setFileDropBlocked(false);
}, dropPosition);
assert.equal(await page.getByRole('tab', { name: /blocked.txt/ }).count(), 0);
await page.evaluate(() => window.simulateFileDrop({ type: 'drop', position: { x: 50 * devicePixelRatio, y: 100 * devicePixelRatio }, paths: ['chat.txt'] }));
assert.equal(await page.evaluate(() => window.attachmentCount()), 1, 'chat drops still become attachments');
console.log('panel spreadsheet layers/edit/save/drop routing passed',errors);assert.deepEqual(errors,[]);
}finally{await browser.close();await server.close();await Promise.all([name+'.html',name+'.tsx'].map(path=>fs.rm(path,{force:true})));}
