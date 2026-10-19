import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import ts from 'typescript';

// Run the recorder's real event lifecycle without rendering the rest of Settings.
const source = readFileSync(new URL('../src/components/SettingsModal.tsx', import.meta.url), 'utf8');
const component = source.slice(source.indexOf('function ShortcutKeyCapture('), source.indexOf('/** 后端代理输入框'));
const logic = component.slice(component.indexOf('  const [recording,'), component.indexOf('  return ('));
const js = ts.transpileModule(logic + '\nreturn { startRecording, stopRecording, recording };', { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
const listeners = new Map();
const window = {
  addEventListener(name, listener) { if (!listeners.has(name)) listeners.set(name, new Set()); listeners.get(name).add(listener); },
  removeEventListener(name, listener) { listeners.get(name)?.delete(listener); },
};
const events = [], values = [], cleanup = [];
const mount = () => new Function('createSignal', 'window', 'onCleanup', 'setShortcutRecordingActive', 'formatShortcutKeys', 'props', js)(
  initial => { let value = initial; return [() => value, next => { value = next; }]; },
  window, fn => cleanup.push(fn), active => events.push(active),
  event => event.key === 'Alt' ? null : `Ctrl+${event.key}`,
  { onChange: value => values.push(value) },
);
const emit = (name, key) => { for (const listener of [...(listeners.get(name) ?? [])]) listener({ key, preventDefault() {}, stopPropagation() {} }); };
const recorder = mount();
const inactive = mount();
recorder.startRecording();
assert.deepEqual(events, [true]);
inactive.stopRecording();
assert.deepEqual(events, [true], 'an inactive recorder cannot release another recorder');
emit('keydown', 'Alt');
assert.equal(recorder.recording(), true, 'modifier-only input keeps recording');
emit('blur');
assert.equal(recorder.recording(), false, 'switching away cancels capture');
assert.deepEqual(events, [true, false]);
assert.equal(listeners.get('keydown').size, 0);
assert.equal(listeners.get('blur').size, 0);
recorder.startRecording();
emit('keydown', 'M');
assert.deepEqual(values, ['Ctrl+M']);
assert.equal(recorder.recording(), false);
recorder.startRecording();
emit('keydown', 'Escape');
assert.equal(recorder.recording(), false);
recorder.startRecording();
cleanup[0]();
assert.equal(recorder.recording(), false);
assert.equal(listeners.get('keydown').size, 0);
assert.match(component, /onBlur=\{stopRecording\}/, 'clicking another control also ends native suppression');
console.log('Meteor recorder: only actual capture suppresses the hook, and blur/Escape/commit/unmount always release it.');
