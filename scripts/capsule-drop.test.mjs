import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import ts from 'typescript';

const source = ts.createSourceFile('TaskbarBubble.tsx', readFileSync(new URL('../src/components/TaskbarBubble.tsx', import.meta.url), 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
let expression;
function visit(node) {
  if (ts.isVariableDeclaration(node) && node.name.getText(source) === 'capture') expression = node.initializer.getText(source);
  ts.forEachChild(node, visit);
}
visit(source);
assert.ok(expression);
async function drop(data, { types = Object.keys(data), files = [], items = [], fail = false } = {}) {
  const captured = [], errors = [], tabs = [], uploads = [];
  let prevented = false, dragging = true, busy = false;
  const capture = runInNewContext(ts.transpile(`(${expression})`, { target: ts.ScriptTarget.ES2022 }), {
    setDragging: value => { dragging = value; },
    setCaptureError: value => errors.push(value), setTab: tab => tabs.push(tab),
    capturing: () => busy, setCapturing: value => { busy = value; },
    invoke: async (command, args) => {
      if (command === 'taskbar_bubble_activate') return;
      assert.equal(command, 'capsule_capture');
      assert.equal(args.text, '');
      assert.equal(args.paths.length, 0);
      uploads.push(...args.uploads);
      if (fail) throw Error('disk full');
    },
    fileToAttachment: async file => ({ name: file.name, data: 'Ynl0ZXM=' }),
    captureCapsule: async text => { captured.push(text); if (fail) throw Error('disk full'); },
    DOMParser: class { parseFromString() { return { body: { textContent: '' } }; } },
  });
  await capture({ preventDefault() { prevented = true; }, dataTransfer: { files, items, types, getData: kind => data[kind] ?? '' } });
  assert.equal(prevented, true, 'drop cannot navigate the WebView');
  assert.equal(dragging, false, 'overlay clears on drop');
  assert.equal(busy, false);
  return { captured, errors, tabs, uploads };
}
const original = '  原文 🦋\n保留换行  ';
assert.deepEqual((await drop({ 'text/plain': original })).captured, [original]);
assert.deepEqual((await drop({ 'text/uri-list': '# browser comment\r\nhttps://example.test/a\r\n' })).captured, ['https://example.test/a']);
const fileDrop = await drop({ 'text/plain': 'file description' }, { types: ['Files', 'text/plain'], files: [{ name: '原件.png', size: 5 }] });
assert.deepEqual(fileDrop.captured, [], 'file description is not captured as a duplicate text capsule');
assert.equal(fileDrop.uploads.length, 1);
assert.equal(fileDrop.uploads[0].name, '原件.png');
assert.equal(fileDrop.uploads[0].data, 'Ynl0ZXM=');
assert.match((await drop({}, { files: [{ size: 21 * 1024 * 1024 }] })).errors.at(-1), /20 MiB/);
assert.match((await drop({}, { items: [{ webkitGetAsEntry: () => ({ isDirectory: true }) }] })).errors.at(-1), /不支持文件夹/);
assert.match((await drop({ 'application/unsupported': 'no text' })).errors.at(-1), /没有可保存的文字/);
assert.match((await drop({ 'text/plain': original }, { fail: true })).errors.at(-1), /disk full/);
const native = readFileSync(new URL('../src-tauri/src/taskbar_status/bubble.rs', import.meta.url), 'utf8');
assert.match(native, /\.disable_drag_drop_handler\(\)/, 'Wry native file-only handler must not consume text/URL drops');
assert.doesNotMatch(native, /WindowEvent::DragDrop/, 'the panel must use one DOM drop path, not competing native + HTML handlers');
console.log('星尘 drops preserve Unicode text, accept links/files once, reject oversized files/directories and expose failures.');
