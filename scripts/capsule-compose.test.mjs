import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import ts from 'typescript';

// Exercise the real listener, including draft-before-navigation ordering. No model or desktop needed.
const path = new URL('../src/store.ts', import.meta.url);
const source = ts.createSourceFile('store.ts', readFileSync(path, 'utf8'), ts.ScriptTarget.Latest, true);
let listener;
function visit(node) {
  if (ts.isCallExpression(node) && node.expression.getText(source) === 'listen' && node.arguments[0]?.text === 'capsules:compose') listener = node.getText(source);
  ts.forEachChild(node, visit);
}
visit(source);
assert.ok(listener, 'capsule composition listener exists');
async function check(failure) {
  let handler;
  const calls = [];
  const payload = { text: '完整原文\n不是摘要 🌿', images: [{ name: '记录.txt', mimeType: 'text/plain', uri: 'file:///saved/record.txt' }] };
  runInNewContext(ts.transpile(`(${listener})`, { target: ts.ScriptTarget.ES2022 }), {
    listen: (_, fn) => { handler = fn; },
    sceneModel: scene => { assert.equal(scene, 'lightweight'); return { agentKind: 'lyra', model: 'small' }; },
    api: {
      scratchDir: async () => '/scratch',
      createThread: async (...args) => { calls.push(['create', ...args]); if (failure) throw Error('offline'); return { id: 'fresh' }; },
    },
    saveSessionDraft: (...args) => calls.push(['draft', ...args]),
    rememberThreadSnapshot: thread => calls.push(['snapshot', thread.id]),
    setView: view => calls.push(['view', view]),
    openThread: async id => { assert.equal(calls.find(c => c[0] === 'draft')?.[1], id); calls.push(['open', id]); },
    refreshThreads() {}, refreshProjects() {},
    showToast: message => calls.push(['error', message]),
    // Deliberately no sendPromptTo: collecting context must never submit a prompt.
  });
  handler({ payload });
  await new Promise(setImmediate);
  if (failure) {
    assert.equal(calls.some(c => c[0] === 'open' || c[0] === 'draft'), false);
    assert.match(calls.find(c => c[0] === 'error')[1], /原内容仍保留.*offline/);
  } else {
    assert.deepEqual(calls.find(c => c[0] === 'draft'), ['draft', 'fresh', payload.text, payload.images]);
    assert.deepEqual(calls.at(-1), ['open', 'fresh']);
    assert.deepEqual(calls[0], ['create', '/scratch', 'lyra', 'small', 'build', null]);
  }
}
await check(false);
await check(true);
console.log('Capsule composition preserves full content and attachments, seeds before navigation, never sends, and retains capsules on failure.');
