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
  if (ts.isFunctionDeclaration(node) && [
    'tryBuiltinPrompt', 'deliverPrompt', 'sendPromptTo', 'isSubagentThread', 'sendPrompt',
    'setThreadModel', 'pickThreadModel', 'setThreadMode', 'setThreadReasoningEffort',
    'implementProposedPlan', 'startWorkflowOnThread', 'startFireRelay', 'editUserMessage',
    'cancelTurn', 'compactThread',
  ].includes(node.name?.text)) units.push(node.getText(parsed));
  if (ts.isCallExpression(node) && node.expression.getText(parsed) === 'listen' && node.arguments[0]?.text === 'remote-prompt:dispatch') units.push(node.getText(parsed) + ';');
  ts.forEachChild(node, visit);
}
visit(parsed);
assert.equal(units.length, 16);
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
// 子 Stage 的所有前端执行入口必须在任何状态/API 副作用前退出。
state.currentId = 'agent';
state.threads = [{ id: 'agent', subagent: true, parentThreadId: 'thread' }];
state.proposedPlan = 'plan';
state.running.agent = true;
const before = structuredClone(state);
const callsBefore = structuredClone(calls);
const actions = {
  sendPrompt: ['/fire 不应启动'], sendPromptTo: ['agent', '/stage 不应创建', []],
  setThreadModel: ['other'], pickThreadModel: ['codex', 'other'], setThreadMode: ['build'],
  setThreadReasoningEffort: ['high'], implementProposedPlan: [],
  startWorkflowOnThread: ['agent', 'goal', [], 'workflow'], startFireRelay: ['goal', null, 'agent'],
  editUserMessage: [1, '不应截断'], cancelTurn: [], compactThread: [],
};
for (const [name, args] of Object.entries(actions)) {
  await module.exports[name](...args);
  assert.deepEqual(state, before, `${name} must not mutate child Stage state`);
  assert.deepEqual(calls, callsBefore, `${name} must not call the backend`);
}
const dispatch = lib.slice(lib.indexOf('pub(crate) fn dispatch_prompt('), lib.indexOf('\nfn truncate_thread('));
assert.match(dispatch, /if t\.subagent\s*\{\s*return Err\(/);
assert.ok(dispatch.indexOf('if t.subagent') < dispatch.indexOf('remote::route_fire_command('), 'reject before built-in routing');
assert.ok(dispatch.indexOf('if t.subagent') < dispatch.indexOf('mgr.steer_prompt('), 'reject before steer');
const cancel = lib.slice(lib.indexOf('async fn cancel_turn('), lib.indexOf('async fn compact_thread('));
assert.match(cancel, /is_some_and\(\|t\| t\.subagent\)\s*\{\s*return Err\(/);
assert.ok(cancel.indexOf('t.subagent') < cancel.indexOf('.pending_prompt_restores'), 'reject before clearing restores or emitting running=false');
const chat = readFileSync(new URL('../src/components/ChatView.tsx', import.meta.url), 'utf8');
const canvas = readFileSync(new URL('../src/components/CanvasTranscript.tsx', import.meta.url), 'utf8');
assert.match(chat, /readOnly=\{isSubagent\(\)\}/);
assert.match(chat, /when=\{!isSubagent\(\)\} fallback=\{/);
assert.match(chat, /子 Agent 执行记录 · 由主会话调度/);
assert.match(chat, /openThread\(parentId\(\)\)/);
assert.match(canvas, /if \(!props\.running && !props\.readOnly\)/);
console.log('Prompt replay and subagent read-only entry checks passed.');
