// No browser/GUI: compile the real Solid component for SSR, replacing only DOM/IPC collaborators.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { build, transform } from 'esbuild';
import { transformAsync } from '@babel/core';
import solid from 'babel-preset-solid';

const result = await build({
  stdin: {
    contents: `export { HtmlTurn } from './src/components/HtmlTurn';
      export { state } from './src/store';
      export { renderToString } from 'solid-js/web';`,
    resolveDir: process.cwd(), loader: 'ts',
  },
  bundle: true, write: false, format: 'esm', platform: 'node', loader: { '.css': 'empty' },
  plugins: [{
    name: 'html-turn-headless-check',
    setup(build) {
      build.onResolve({ filter: /(?:^|\/)(store|ipc|Markdown|HtmlPromptText|FileContextMenu|ImageAttachmentStrip)$/ }, args => ({ path: args.path.split('/').at(-1), namespace: 'mock' }));
      build.onLoad({ filter: /.*/, namespace: 'mock' }, args => ({ loader: 'js', contents: {
        store: `export const state = { expanded: {}, currentId: 'thread', cwd: '/project' };
          export const isExpanded = (key, fallback = false) => state.expanded[key] ?? fallback;
          export const toggleExpanded = (key, value) => { state.expanded[key] = value ?? !state.expanded[key]; };
          export const editUserMessage = async () => {};`,
        ipc: `export const fileUriPath = uri => uri.startsWith('file://') ? uri.slice(7) : uri;`,
        Markdown: `export function Markdown(props) { return props.text; }`,
        HtmlPromptText: `export function HtmlPromptText(props) { return props.text; }`,
        FileContextMenu: `export const createFileContextMenu = () => ({ open() {}, Menu: () => '' });`,
        ImageAttachmentStrip: `export const attachmentPreviewSrc = image => image.uri ?? 'data:image/png;base64,' + image.data;
          export const takeWarmImage = () => undefined;
          export const ImageAttachmentStrip = () => '';
          export const createImageAttachments = () => ({ images: () => [], clear() {}, set() {}, remove() {}, onPaste() {}, pickFiles: async () => {} });`,
      }[args.path] }));
      build.onLoad({ filter: /\.tsx$/ }, async args => {
        const source = await readFile(args.path, 'utf8');
        const stripped = await transform(source, { loader: 'tsx', jsx: 'preserve', target: 'esnext' });
        const output = await transformAsync(stripped.code, { filename: args.path, configFile: false, babelrc: false, presets: [[solid, { generate: 'ssr', hydratable: false }]] });
        return { contents: output.code, loader: 'js' };
      });
    },
  }],
});
const { HtmlTurn, state, renderToString } = await import(`data:text/javascript;base64,${Buffer.from(result.outputFiles[0].text).toString('base64')}`);
const render = (group, expanded = {}, options = {}) => {
  state.expanded = expanded;
  return renderToString(() => HtmlTurn({ group, running: false, preview: false, threadId: 'thread', onReturnToCurrent() {}, ...options }));
};
const user = { id: 1, type: 'user', text: 'hello', ts: 0 };
const thought = { id: 2, type: 'thought', text: 'private reasoning', ts: 0 };
const output = 'TOOL DETAIL\n' + 'many lines\n'.repeat(5000);
const tool = { id: 3, type: 'tool', ts: 0, toolCallId: 'call', kind: 'read', title: '\x1b[31mread result\x1b[0m', status: 'failed', locations: [{ path: '/project/file.ts', line: 12 }], content: [{ type: 'content', content: { type: 'text', text: output } }], rawOutput: { duration_ms: 1500 } };
const assistant = { id: 4, type: 'assistant', text: 'final answer', ts: 0 };
const turn = { id: 5, type: 'turn', ts: 0, durationMs: 2000, totalTokens: 1000, outputTokens: 40, actualModel: 'test model', stopReason: 'end' };
const group = { user, body: [thought, tool, assistant], turn };

let html = render(group);
assert.match(html, /data-item-id="1"/);
assert.match(html, /data-item-id="4"/);
assert.match(html, /data-item-id="5"/);
assert.match(html, /实际模型：test model/);
assert.match(html, /平均输出 20 tok\/s/);
assert.match(html, /aria-expanded="false"/);
assert.doesNotMatch(html, /private reasoning|TOOL DETAIL|data-item-id="[23]"/);

html = render(group, { 'turn-5': true });
assert.match(html, /读取文件 ×1（1 项失败）/);
assert.doesNotMatch(html, /TOOL DETAIL|data-item-id="[23]"/);
html = render(group, { 'turn-5': true, 'process-2-done': true });
assert.match(html, /data-item-id="2"/);
assert.match(html, /data-item-id="3"/);
assert.match(html, /data-status="failed"/);
assert.match(html, /aria-label="失败"/);
assert.match(html, /1\.50s/);
assert.doesNotMatch(html, /private reasoning|TOOL DETAIL|\x1b/);
html = render(group, { 'turn-5': true, 'process-2-done': true, 'thought-2': true, 'tool-3': true });
assert.match(html, /private reasoning/);
assert.match(html, /file.ts:12/);
assert(html.includes(output.trim()), 'open tool output retains all text, not only its visible 320px');
assert.match(html, /class="ht-tool-output" tabindex="0"/);

const live = { user, body: [thought, { ...tool, status: 'in_progress' }] };
html = render(live, {}, { running: true });
assert.match(html, /进行中/);
assert.match(html, /class="ht-live-preview"/);
assert.match(html, /class="ht-live-lines"/);
assert.doesNotMatch(html, /TOOL DETAIL|data-item-id="3"/);
html = render(live, { 'process-2-live': true }, { running: true });
assert.match(html, /TOOL DETAIL/);
assert.match(html, /aria-label="进行中"/);
assert.doesNotMatch(html, /aria-label="编辑消息"/);
assert.doesNotMatch(render(group, {}, { readOnly: true }), /aria-label="编辑消息"/);
assert.match(render(group, {}, { preview: true }), /aria-label="编辑消息"/);

const longPrompt = 'a'.repeat(999) + '😀' + 'z'.repeat(5000);
html = render({ user: { ...user, text: longPrompt }, body: [] });
assert.match(html, /展开完整提示词/);
assert(html.includes('a'.repeat(999) + '…'));
assert(!html.includes('\uD83D…'), 'collapsed preview must not leave an unpaired surrogate');
assert(!html.includes('z'.repeat(1000)));
assert(render({ user: { ...user, text: longPrompt }, body: [] }, { 'user-text-1': true }).includes(longPrompt));
const rewritten = '[蝴蝶助理投递 test]\n用户原文：\nshort prompt\n\n补充上下文（不得扩大用户授权）：\nsecret system suffix';
html = render({ user: { ...user, text: rewritten }, body: [] });
assert.match(html, /short prompt/);
assert.doesNotMatch(html, /secret system suffix/);
assert.match(render({ user: { ...user, text: rewritten }, body: [] }, { 'user-sys-1': true }), /secret system suffix/);
assert.match(render({ user: { ...user, text: '', images: [{ name: 'attached.png', mimeType: 'image/png', data: 'AA==' }] }, body: [] }), /alt="attached.png"/);
assert.doesNotMatch(render({ body: [{ ...assistant, text: ' None ' }, { ...thought, text: '' }] }), /ht-assistant|ht-thought|ht-process-fold/);
assert.match(render({ body: [{ id: 8, type: 'system', text: '<script>unsafe</script>', level: 'error', ts: 0 }] }), /&lt;script/);
assert.match(render({ body: [{ ...assistant, text: 'large markdown\n'.repeat(3000) }] }), /ht-large-markdown/);
assert.match(render({ body: [{ ...assistant, deferred: true }] }), /data-item-id="4"/);
assert.match(render({ body: [{ ...assistant, deferred: true }] }), /data-history-loading/);
assert.doesNotMatch(render({ body: [{ ...assistant, deferred: true }] }), /final answer/);
// Deferred tools/thoughts in a collapsed turn are not painted: render the turn instead of a placeholder.
const deferredProcess = { user, body: [{ ...thought, text: '', deferred: true }, { ...tool, content: [], deferred: true }, assistant], turn };
assert.doesNotMatch(render(deferredProcess), /data-history-loading/);
assert.match(render(deferredProcess), /final answer/);
assert.match(render(deferredProcess, { 'turn-5': true }), /data-history-loading/, 'an opened fold still waits for its bodies');
assert.match(render({ user, body: [tool, { ...assistant, text: '', deferred: true }], turn }), /data-history-loading/, 'a deferred answer is visible content');

const source = await readFile(new URL('../src/components/HtmlTurn.tsx', import.meta.url), 'utf8');
const css = await readFile(new URL('../src/components/HtmlTurn.css', import.meta.url), 'utf8');
assert.doesNotMatch(source, /<Show[^>]*\bkeyed\b/);
assert.match(source, /<For each=\{ids\(\)\}/, 'process rows key primitive IDs, never freshly allocated segment objects');
assert.match(css, /\.html-turn\s*\{[^}]*margin: 0;/);
assert.match(css, /\.ht-tool-output\s*\{[^}]*max-height: 320px;[^}]*overflow: auto;/);
assert.match(css, /content-visibility: auto;/);
assert.match(css, /\.ht-live-preview\s*\{[^}]*justify-content: flex-end;[^}]*max-height: 48px;[^}]*overflow: hidden;/);
assert.match(source, /<Index each=\{props.item.content\}/, 'streaming tool-content replacements keep scroll/focus');
assert.match(source, /<Index each=\{props.item.images\}/, 'attachment projections retain their image slots');
assert.match(source, /createHtmlPromptImage\(image\)/);
assert.match(source, /src=\{shown\(\)\.src\} width=\{shown\(\)\.width\} height=\{shown\(\)\.height\}/);
assert.match(source, /img\.currentSrc === shown\(\)\.src && img\.complete && !img\.naturalWidth/, 'ignore stale image errors');
assert.equal([...source.matchAll(/<Markdown[^>]*\bvirtual\b/g)].length, 2, 'assistant and thought Markdown both opt into virtual chunks');
assert.match(css, /\.ht-bubble-images img\s*\{[^}]*flex: none;/);
console.log('HtmlTurn headless checks passed (folding/search keys, lazy bodies, full output, images, preview/edit policy, stats, placeholders and large-message guards).');
