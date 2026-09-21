import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
import { LruMap } from '../src/lruMap.ts';
import { toolDisplayText } from '../src/utils.ts';

const source = fs.readFileSync(new URL('../src/components/CanvasTranscript.tsx', import.meta.url), 'utf8');
const ast = ts.createSourceFile('canvas.tsx', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
const names = new Set(['layoutTool', 'paintTextBlock', 'pushTrimmedLine', 'wrapTextSteps', 'wrapTextIndexed', 'wrapTextFull', 'wrappedLineSeps', 'wrapText', 'promptImageSrc']);
const units = [];
const ipc = fs.readFileSync(new URL('../src/ipc.ts', import.meta.url), 'utf8');
units.push(ipc.slice(ipc.indexOf('export function fileUriPath'), ipc.indexOf('export const api')).replace('export ', ''));
function visit(node) {
  if (ts.isFunctionDeclaration(node) && names.has(node.name?.text)) units.push(node.getText(ast));
  ts.forEachChild(node, visit);
}
visit(ast);
let measures = 0, draws = 0, imageUrls = 0;
const context = vm.createContext({
  toolDisplayText, toolTextLayouts: new LruMap(32),
  imageSourceCache: new WeakMap(), convertFileSrc: path => { imageUrls++; return path; },
  measure: text => { measures++; return text.length * 7; },
  pal: { mono: 'mono', sans: 'sans' }, state: { agentKind: 'kimi' },
  isExpanded: () => true, expandCodeTabs: text => text,
  displayToolTitle: text => text, stripAnsi: text => text,
  blockScrolls: new Map(), blockScrollKey: () => 'tool', blockScrollbarGeom: () => null,
  viewH: 600, fillTextCrisp: () => draws++,
});
vm.runInContext(ts.transpileModule(units.join('\n'), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText, context);
const input = text => ({ id: 1, kind: 'other', title: 'tool', status: 'completed', locations: [], content: [{type: 'content', content: {type: 'text', text}}] });
const attachment = {mimeType: 'image/png', uri: 'file:///C:/a.png'};
for (let i = 0; i < 100; i++) assert.equal(context.promptImageSrc(attachment), 'C:/a.png');
assert.equal(imageUrls, 1);
attachment.uri = 'file:///C:/b.png';
assert.equal(context.promptImageSrc(attachment), 'C:/b.png');
assert.equal(imageUrls, 2);
assert.equal(context.fileUriPath('file:///C:/截图%23%25%3F.png'), 'C:/截图#%?.png');
assert.equal(context.fileUriPath('file:///tmp/picture.png'), '/tmp/picture.png');
assert.equal(context.fileUriPath('file:///C:/100%.png'), 'C:/100%.png');
attachment.data = 'AAAA';
assert.equal(context.promptImageSrc(attachment), 'data:image/png;base64,AAAA');
const layout = text => { const blocks = []; context.layoutTool(input(text), blocks, 0, 0, 800, 0, false); return blocks.find(b => b.kind === 'tool-content'); };
const base64 = 'Ab09+/'.repeat(11000);
const legacy = '[输出过长，已省略前面内容，仅保留最后 64KB]\n' + base64 + '="}}]';
assert.ok(layout(legacy).text.length < 100);
assert.equal(toolDisplayText('normal\noutput'), 'normal\noutput');
assert.equal(toolDisplayText('path: data:image/png;base64,AAAA== done'), 'path: [图片编码已隐藏] done');
const log = 'result 中文 0123456789\n'.repeat(2000);
const first = layout(log);
measures = 0;
const block = layout(log);
assert.equal(measures, 0, 'Repeated tool layout must reuse line measurements');
assert.equal(block._lines, first._lines);
assert.equal(block._lines.join('\n'), log.trim(), 'All ordinary output remains available');
const ctx = {save(){}, beginPath(){}, rect(){}, clip(){}, restore(){}};
context.paintTextBlock(ctx, block, 0, 0, false);
assert.equal(block.textLines.length, 2000, 'Selection retains all text');
let reads = 0;
block._lines = new Proxy(block._lines, { get(target, key) { if (/^\d+$/.test(String(key))) reads++; return target[key]; } });
for (const scroll of [0, 10000, 30000]) {
  reads = draws = 0;
  context.blockScrolls.set('tool', scroll);
  context.paintTextBlock(ctx, block, 0, 0, false);
  assert.ok(reads < 100 && draws < 30 && draws > 0, `Only visible lines should be visited: ${reads}/${draws}`);
}
console.log('Tool display: legacy Base64, cached layout, visible-line painting and full-text selection passed.');
