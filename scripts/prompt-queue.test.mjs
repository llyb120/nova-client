// Solid 2 batching regression: real queue/signals, isolated IPC, no requests to Nova.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { build } from 'esbuild';

const bundled = await build({
  stdin: { resolveDir: process.cwd(), loader: 'ts', contents: `
    export * from './src/promptQueue';
    export { sent, complete, mirrored } from './src/store';
    export { flush } from 'solid-js';
  ` },
  bundle: true, write: false, platform: 'browser', format: 'esm',
  conditions: ['browser', 'development'],
  plugins: [{ name: 'queue-ipc-fixture', setup(builder) {
    builder.onResolve({ filter: /^(?:\.\/store|\.\/ipc|@tauri-apps\/api\/event)$/ }, ({ path }) => ({ path, namespace: 'fixture' }));
    builder.onResolve({ filter: /^\.\/src\/store$/ }, () => ({ path: './store', namespace: 'fixture' }));
    builder.onLoad({ filter: /.*/, namespace: 'fixture' }, ({ path }) => ({ loader: 'js', contents: {
      './store': `
        export const state = { currentId: 'thread', running: {} };
        export const sent = []; export let mirrored = []; let resolve;
        export const complete = () => resolve();
        export const setPromptQueuedThreads = ids => { mirrored = [...ids]; };
        export async function sendPromptTo(id, text) {
          sent.push({ id, text }); await new Promise(done => { resolve = done; });
        }
        export const sendPrompt = text => sendPromptTo(state.currentId, text);
      `,
      './ipc': 'export const api = { setPromptQueuePending: async () => {} };',
      '@tauri-apps/api/event': 'export const listen = async () => () => {};',
    }[path] }));
  } }],
});
const queue = await import(`data:text/javascript;base64,${Buffer.from(bundled.outputFiles[0].text).toString('base64')}`);

test('same-frame dispatch and reactive dispatcher cannot send a prompt twice', async () => {
  queue.holdPromptQueue('thread');
  queue.enqueuePrompt('thread', 'one prompt', [], 'queued-1');
  queue.flush();
  assert.equal(queue.sent.length, 0);
  const item = queue.queuedPrompts()[0];
  const first = queue.dispatchQueuedPrompt(item);
  await queue.dispatchQueuedPrompt(item);
  await new Promise(setImmediate);
  assert.equal(queue.sent.length, 1, 'the synchronous in-flight guard precedes the batched UI flag');
  queue.complete();
  await first;
  queue.flush();
  assert.equal(queue.queuedPrompts().length, 0);
  assert.equal(queue.dispatchingQueueIds().size, 0);
  assert.deepEqual(queue.mirrored, []);

  queue.holdPromptQueue('thread');
  queue.enqueuePrompt('thread', 'withdrawn', [], 'queued-2');
  queue.flush();
  assert.equal(queue.removeQueuedPrompt('queued-2').text, 'withdrawn');
  queue.flush();
  await new Promise(setImmediate);
  assert.equal(queue.sent.length, 1, 'removing and releasing a held queue in one batch must not send it');
});
