import assert from 'node:assert/strict';
import fs from 'node:fs';
import { transformSync } from 'esbuild';
import { toolDisplayText } from '../src/utils.ts';

const ipc = fs.readFileSync(new URL('../src/ipc.ts', import.meta.url), 'utf8');
const fileUriPath = new Function(`${transformSync(ipc.slice(ipc.indexOf('export function fileUriPath'), ipc.indexOf('export const api')).replace('export ', ''), { loader: 'ts' }).code}; return fileUriPath;`)();
assert.equal(fileUriPath('file:///C:/截图%23%25%3F.png'), 'C:/截图#%?.png');
assert.equal(fileUriPath('file:///tmp/picture.png'), '/tmp/picture.png');
assert.equal(fileUriPath('file:///C:/100%.png'), 'C:/100%.png');
const base64 = 'Ab09+/'.repeat(11000);
assert.ok(toolDisplayText('[输出过长，已省略前面内容，仅保留最后 64KB]\n' + base64 + '="}}]').length < 100);
assert.equal(toolDisplayText('normal\noutput'), 'normal\noutput');
assert.equal(toolDisplayText('path: data:image/png;base64,AAAA== done'), 'path: [图片编码已隐藏] done');
console.log('Tool display: file URIs and legacy Base64 output passed.');
