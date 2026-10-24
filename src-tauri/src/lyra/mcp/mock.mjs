// Real JSONL MCP stdio peer for mcp/tests.rs; no SDK or npm install required.
import fs from 'node:fs';
import readline from 'node:readline';
import { spawn } from 'node:child_process';
const [mode, trace] = process.argv.slice(2);
const record = value => fs.appendFileSync(trace, JSON.stringify(value) + '\n');
const send = value => process.stdout.write(JSON.stringify(value) + '\n');
const result = (id, value) => send({ jsonrpc: '2.0', id, result: value });
record({ event: 'start', pid: process.pid });
if (mode === 'descendant') {
  const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore' });
  record({ event: 'descendant', pid: child.pid });
}
let initialized = false;
let lists = 0;
let handshake;
let replies = 0;
const initResult = { protocolVersion: '2025-06-18', capabilities: { tools: { listChanged: true } }, serverInfo: { name: 'mock', version: '1' } };
const lookup = () => ({
  name: 'lookup', description: mode === 'secret' ? 'test ' + process.env.MCP_TEST_SECRET : 'Look up a value',
  inputSchema: { type: 'object', properties: { query: { type: 'string', description: mode === 'changed' && lists > 1 ? 'NEW_SCHEMA' : 'LOOKUP_SCHEMA_ONLY' } } },
});
readline.createInterface({ input: process.stdin }).on('line', line => {
  const m = JSON.parse(line);
  if (!m.method) {
    const valid = m.id === 'unsupported'
      ? m.error?.code === -32601 && !('result' in m)
      : m.id === 999 && m.result && !('error' in m);
    record({ event: 'client_reply', valid });
    if (!valid) process.exit(7);
    if (++replies === 2) result(handshake, initResult);
    return;
  }
  record({ method: m.method, cursor: m.params?.cursor, name: m.params?.name });
  if (m.method === 'initialize') {
    if (mode === 'init_hang') return;
    if (mode === 'bad_version') { result(m.id, { ...initResult, protocolVersion: '2099-01-01' }); return; }
    if (mode === 'batch_version') { result(m.id, { ...initResult, protocolVersion: '2025-03-26' }); return; }
    if (mode === 'client_request') {
      handshake = m.id;
      send({ jsonrpc: '2.0', id: 'unsupported', method: 'sampling/createMessage', params: {} });
      send({ jsonrpc: '2.0', id: 999, method: 'ping' });
      return;
    }
    result(m.id, initResult);
  } else if (m.method === 'notifications/initialized') {
    initialized = true;
  } else if (m.method === 'tools/list') {
    if (!initialized) process.exit(8);
    lists++;
    if (mode === 'oversize') { process.stdout.write(' '.repeat(1024 * 1024 + 1)); return; }
    if (mode === 'bad_id') { result(m.id + 1, { tools: [] }); return; }
    if (mode === 'disconnect') { process.exit(0); }
    if (mode === 'logs') for (let i = 0; i < 100; i++) send({ jsonrpc: '2.0', method: 'notifications/message', params: { level: 'info', data: i } });
    if (mode === 'notification' && lists > 1) send({ jsonrpc: '2.0', method: 'notifications/tools/list_changed' });
    if (mode === 'disappeared' && lists > 1) { result(m.id, { tools: [] }); return; }
    if (mode === 'cycle') { result(m.id, { tools: [], nextCursor: 'loop' }); return; }
    if (mode === 'search_pages') {
      const page = Number(m.params?.cursor ?? '0');
      result(m.id, { tools: page === 33 ? [{ ...lookup(), description: 'needle in later page' }] : [], ...(page < 33 ? { nextCursor: String(page + 1) } : {}) });
      return;
    }
    if (mode === 'empty_page' && !m.params?.cursor) { result(m.id, { tools: [], nextCursor: 'empty-next' }); return; }
    if (mode === 'wide_page') {
      result(m.id, { tools: Array.from({ length: 55 }, (_, i) => ({ name: `tool-${i}`, description: '\t'.repeat(240), inputSchema: { type: 'object' } })) });
      return;
    }
    if (!m.params?.cursor || m.params.cursor === 'empty-next') {
      result(m.id, { tools: [lookup(), { name: 'mutate', description: 'Writes data, despite the annotation', annotations: { readOnlyHint: true }, inputSchema: { type: 'object', description: 'WRITE_SCHEMA_HIDDEN' } }], nextCursor: 'next-page' });
    } else if (m.params.cursor === 'next-page') {
      result(m.id, { tools: [{ name: 'huge', description: 'Too large to describe', inputSchema: { type: 'object', description: 'H'.repeat(18000) } }] });
    } else process.exit(9);
  } else if (m.method === 'tools/call') {
    if (['hang', 'descendant'].includes(mode)) return;
    if (mode === 'rpc_error') {
      send({ jsonrpc: '2.0', id: m.id, error: { code: -32000, message: 'mock failure ' + (process.env.MCP_TEST_SECRET ?? ''), data: { hidden: 'do not disclose' } } });
      return;
    }
    const text = mode === 'secret' ? process.env.MCP_TEST_SECRET
      : mode === 'large_result' ? 'X'.repeat(100000) : `called ${m.params.name}`;
    result(m.id, { content: [{ type: 'text', text }], structuredContent: mode === 'secret' ? { [text]: text } : { ok: true }, isError: mode === 'tool_error' });
  } else {
    send({ jsonrpc: '2.0', id: m.id, error: { code: -32601, message: 'Method not found' } });
  }
});
