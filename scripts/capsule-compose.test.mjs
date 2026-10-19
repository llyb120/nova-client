import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import ts from 'typescript';

// Exercise the real listener: it seeds the new-session draft, then opens the new-session page. No model or desktop needed.
const path = new URL('../src/store.ts', import.meta.url);
const source = ts.createSourceFile('store.ts', readFileSync(path, 'utf8'), ts.ScriptTarget.Latest, true);
let listener;
function visit(node) {
  if (ts.isCallExpression(node) && node.expression.getText(source) === 'listen' && node.arguments[0]?.text === 'capsules:compose') listener = node.getText(source);
  ts.forEachChild(node, visit);
}
visit(source);
assert.ok(listener, 'capsule composition listener exists');
function check(previous) {
  let handler;
  const calls = [];
  const payload = { text: '完整原文\n不是摘要 🌿', images: [{ name: '记录.txt', mimeType: 'text/plain', uri: 'file:///saved/record.txt' }] };
  runInNewContext(ts.transpile(`(${listener})`, { target: ts.ScriptTarget.ES2022 }), {
    listen: (_, fn) => { handler = fn; },
    takeSessionDraft: id => { assert.equal(id, null); return previous; },
    saveSessionDraft: (...args) => calls.push(['draft', ...args]),
    openNewSession: () => calls.push(['new-session']),
    // Deliberately no sendPromptTo or createThread: collecting context must never submit or create a session.
  });
  handler({ payload });
  assert.deepEqual(JSON.parse(JSON.stringify(calls.at(-1))), ['new-session']);
  return { draft: calls[0], payload };
}
let { draft, payload } = check(null);
assert.deepEqual(JSON.parse(JSON.stringify(draft)), ['draft', null, payload.text, payload.images]);
({ draft, payload } = check({ text: '已有草稿', images: [{ name: 'a.png' }] }));
assert.deepEqual(JSON.parse(JSON.stringify(draft)), ['draft', null, `已有草稿\n\n${payload.text}`, [{ name: 'a.png' }, ...payload.images]]);
console.log('Capsule composition seeds the new-session draft (merged with an existing one), opens the new-session page and never sends.');

