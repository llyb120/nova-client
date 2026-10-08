// Run: node scripts/prompt-replay.test.mjs
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { transformSync } from 'esbuild';
import ts from 'typescript';

const source = readFileSync(new URL('../src/store.ts', import.meta.url), 'utf8');
const lib = readFileSync(new URL('../src-tauri/src/lib.rs', import.meta.url), 'utf8');
const replay = lib.slice(lib.indexOf('        if should_send {'), lib.indexOf('\n#[tauri::command]\nasync fn cancel_turn'));
assert.match(replay, /server::is_headless\(\)/);
assert.match(replay, /remote::EV_REMOTE_PROMPT_DISPATCH/);
assert.match(replay, /"restored": true/);

const parsed = ts.createSourceFile('store.ts', source, ts.ScriptTarget.Latest, true);
const units = [];
function visit(node) {
  if (ts.isFunctionDeclaration(node) && ['tryBuiltinPrompt', 'deliverPrompt', 'sendPromptTo'].includes(node.name?.text)) units.push(node.getText(parsed));
  if (ts.isCallExpression(node) && node.expression.getText(parsed) === 'listen' && node.arguments[0]?.text === 'remote-prompt:dispatch') units.push(node.getText(parsed) + ';');
  ts.forEachChild(node, visit);
}
visit(parsed);
assert.equal(units.length, 4);
const calls = [], toasts = [];
const state = { currentId: 'thread', mode: 'plan', items: [], threads: [], running: {} };
let handler;
const deps = {
  console: { error() {} },
  state, api: {
    async setThreadMode(id, mode) { calls.push(['mode', id, mode]); },
    async sendPrompt(...args) { calls.push(['send', ...args]); },
  },
  setState(key, ...args) {
    if (key === 'items') {
      if (typeof args[0] === 'function') state.items = args[0](state.items);
      else state.items[args[0]] = args[1];
    } else if (key === 'running') state.running[args[0]] = args[1];
    else state[key] = args[0];
  },
  parseStageInput: () => null, findTriggeredWorkflow: () => null, getThreadSnapshot: () => null,
  buildPlanPrompt: goal => `expanded plan: ${goal}`,
  resumeFireRelay: () => null, prepareWorkflowPrompt: () => null,
  bumpChatScrollToBottom() {}, lastUsed: { setMode() {} },
  optimisticRunningThreads: new Set(), zenHoldThreads: new Set(),
  showToast: text => toasts.push(text),
  listen: (_, callback) => { handler = callback; },
};
const module = { exports: {} };
const compiled = transformSync(units.join('\n'), { loader: 'ts', format: 'cjs' }).code;
new Function('module', 'exports', ...Object.keys(deps), compiled)(module, module.exports, ...Object.values(deps));
const images = [{ name: '参考.png', mimeType: 'image/png', data: 'abc' }];
for (const text of ['保持原文\n第二行', '/plan 修复问题']) {
  state.mode = 'plan'; state.items = []; calls.length = 0;
  await module.exports.sendPromptTo('thread', text, images);
  const direct = structuredClone(calls);
  state.mode = 'plan'; state.items = [{ id: 1, type: 'user', text: '保留的历史' }, { id: -1, type: 'user', text }]; calls.length = 0;
  handler({ payload: { threadId: 'thread', text, images, restored: true } });
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(calls, direct, 'replay must use the same mode, expanded prompt and attachments');
  assert.equal(state.items[0].id, 1);
  assert.equal(state.items.filter(item => item.id < 0).length, 1, 'only one optimistic message');
}
state.currentId = 'other';
state.items = [{ id: -2, type: 'user', text: '另一个会话的消息' }];
calls.length = 0;
handler({ payload: { threadId: 'thread', text: '/plan 后台重放', images, restored: true } });
await new Promise(resolve => setImmediate(resolve));
assert.equal(state.items[0].id, -2, 'switching threads must preserve the new thread');
assert.deepEqual(calls.at(-1), ['send', 'thread', 'expanded plan: 后台重放', images]);
deps.api.sendPrompt = async () => { throw new Error('test failure'); };
handler({ payload: { threadId: 'thread', text: 'retry', restored: true } });
await new Promise(resolve => setImmediate(resolve));
assert.equal(state.running.thread, false);
assert.match(toasts.at(-1), /编辑后重新发送失败/);
console.log('Prompt replay preserves history and matches direct send processing; errors release running state.');
