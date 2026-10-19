// Node >=18 wrapper args: <CLI> [--native] [--print <restricted args>]
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { homedir } from 'node:os';
import { createDecipheriv, createHash, randomBytes, randomUUID } from 'node:crypto';
import { createConnection, createServer } from 'node:net';
import { execFileSync, spawn } from 'node:child_process';
import { once } from 'node:events';

const LIMIT = 4 * 1024 * 1024, INPUT_LIMIT = 32 * 1024 * 1024;
// ponytail: WorkBuddy 5.7.x pins "fields"; track desktop policy changes.
const POLICY = 'fields';
const native = process.argv[2] === '--native', cliArgs = process.argv.slice(native ? 3 : 2);
const print = cliArgs[0] === '--print';
const owner = `nova-${randomUUID()}`, password = randomBytes(32).toString('hex');
const controllers = new Set(), pending = new Set();
let endpoint, headers, worker, readyServer, starting, stopping = false, cleanup;
let startupDeadline = 0, ready = false;

// Bound both unterminated lines and complete SSE events; decode split UTF-8 safely.
async function* lines(stream, limit = LIMIT) {
  const decoder = new TextDecoder();
  let parts = [], size = 0, skipLF = false;
  const append = text => {
    size += Buffer.byteLength(text);
    if (size > limit) throw Error('Line limit');
    if (text) parts.push(text);
  };
  for await (const chunk of stream) {
    let text = decoder.decode(chunk, { stream: true });
    if (!text) continue;
    if (skipLF && text.startsWith('\n')) text = text.slice(1);
    skipLF = text.endsWith('\r');
    let offset = 0;
    for (const match of text.matchAll(/\r\n|\r|\n/g)) {
      append(text.slice(offset, match.index));
      yield parts.join('');
      parts = [];
      size = 0;
      offset = match.index + match[0].length;
    }
    append(text.slice(offset));
  }
  append(decoder.decode());
  if (parts.length) yield parts.join('');
}

function validMessage(message) {
  return message && typeof message === 'object' && !Array.isArray(message) &&
    message.jsonrpc === '2.0' &&
    (!Object.hasOwn(message, 'id') || message.id === null ||
      typeof message.id === 'string' || (typeof message.id === 'number' && Number.isFinite(message.id))) &&
    (typeof message.method === 'string' || (Object.hasOwn(message, 'id') &&
      (Object.hasOwn(message, 'result') || Object.hasOwn(message, 'error'))));
}

async function forward(message) {
  if (!validMessage(message)) throw Error('Invalid JSON-RPC');
  if (!Object.hasOwn(message, 'method')) {
    for (const request of pending) if (request.id === message.id) request.replied = true;
  }
  await new Promise((resolve, reject) => {
    process.stdout.write(JSON.stringify(message) + '\n', error => error ? reject(error) : resolve());
  });
}

async function relay(body) {
  let data = [], size = 0;
  for await (const line of lines(body)) {
    if (!line) {
      if (data.length) await forward(JSON.parse(data.join('\n')));
      data = [];
      size = 0;
    } else if (line.startsWith('data:')) {
      const value = line.slice(5).replace(/^ /, '');
      size += Buffer.byteLength(value) + 1;
      if (size > LIMIT) throw Error('Event limit');
      data.push(value);
    }
  }
  if (data.length) throw Error('Truncated SSE event');
}

async function jsonBody(body) {
  if (!body) return undefined;
  const chunks = [];
  let size = 0;
  for await (const chunk of body) {
    size += chunk.length;
    if (size > LIMIT) throw Error('Body limit');
    chunks.push(Buffer.from(chunk));
  }
  return size ? JSON.parse(Buffer.concat(chunks).toString('utf8')) : undefined;
}

// Plain-Node WorkBuddy.exe exposes the build key; unwrap the user key from keyblob.
function credentials(cli, root) {
  const exe = process.env.WORKBUDDY_EXE || join(dirname(cli), '../../../../WorkBuddy.exe');
  const payload = JSON.parse(execFileSync(exe, ['-e',
    "process.stdout.write(process._linkedBinding('electron_browser_workbuddy_storage').loggerGet())"], {
    env: { ...process.env, ELECTRON_RUN_AS_NODE: '1' }, stdio: ['ignore', 'pipe', 'ignore'],
    windowsHide: true, timeout: 15000, maxBuffer: 65536,
  }).toString('utf8'));
  const sha = data => createHash('sha256').update(data).digest();
  const encode = key => ({ keyId: sha(key).toString('hex').slice(0, 16), keyBase64: key.toString('base64') });
  if (typeof payload?.atRestSecretKey !== 'string') throw Error('Invalid WorkBuddy build key');
  const symmetric = sha(payload.atRestSecretKey), symmetricKey = encode(symmetric);
  const bootstrap = { version: 1, policy: POLICY, symmetricKey };
  try {
    const { alg, id, key: keyDerBase64 } = payload.atRestDeveloperPublicKey;
    const blob = JSON.parse(readFileSync(join(root, 'keyblob'), 'utf8'));
    const keyId = symmetricKey.keyId;
    const slot = blob.slots.find(slot => slot.type === 'static-v1' && (slot.protectorKeyId ?? keyId) === keyId);
    const sealed = JSON.parse(Buffer.from(slot.wrapped, 'base64').toString('utf8'));
    if (sealed.suite !== 1 || sealed.keyId !== keyId) throw Error('Unsupported keyblob');
    const u32 = value => { const buffer = Buffer.alloc(4); buffer.writeUInt32BE(value); return buffer; };
    const text = value => { const buffer = Buffer.from(value, 'utf8'); return Buffer.concat([u32(buffer.length), buffer]); };
    const decipher = createDecipheriv('aes-256-gcm', symmetric, Buffer.from(sealed.nonce, 'base64'), { authTagLength: 16 });
    decipher.setAAD(Buffer.concat([Buffer.from('WB-AAD\0'), Buffer.from([1]), text('WBEF1'), text('sym-v1'),
      u32(1), text(keyId), Buffer.from([1, 0, 0])]));
    decipher.setAuthTag(Buffer.from(sealed.authTag, 'base64'));
    const user = Buffer.concat([decipher.update(Buffer.from(sealed.ciphertext, 'base64')), decipher.final()]);
    const userKey = encode(user);
    if (user.length !== 32 || userKey.keyId !== blob.keyId || !keyDerBase64) throw Error('Invalid keyblob');
    Object.assign(bootstrap, { userKey, developerPublicKey: { alg, id, keyDerBase64 } });
  } catch {
    bootstrap.asymmetricUnavailable = 'missing-key';
  }
  return bootstrap;
}

const remaining = () => Math.max(1, startupDeadline - Date.now());

async function sendCredentials(path, message) {
  for (;;) {
    if (stopping || worker.exitCode !== null || Date.now() > startupDeadline) throw Error('Credential bootstrap failed');
    const socket = createConnection({ path, signal: AbortSignal.timeout(remaining()) });
    socket.on('error', () => {});
    try {
      try {
        await once(socket, 'connect');
      } catch {
        await new Promise(resolve => setTimeout(resolve, 50));
        continue;
      }
      socket.write(JSON.stringify(message) + '\n');
      for await (const line of lines(socket, 65536)) {
        if (JSON.parse(line)?.ok === true) return;
        break;
      }
      throw Error('Credential bootstrap rejected');
    } finally {
      socket.destroy();
    }
  }
}

async function http(method, body, suffix = '', timeout) {
  if (stopping && method !== 'DELETE') throw Error('Closing');
  const controller = new AbortController();
  controllers.add(controller);
  const streaming = method === 'GET' || body?.method === 'session/prompt';
  const timer = setTimeout(() => controller.abort(), timeout ??
    (!ready || body?.method === 'initialize' ? remaining() : 45000));
  let background = false;
  const release = () => {
    clearTimeout(timer);
    controllers.delete(controller);
    controller.abort();
  };
  try {
    const response = await fetch(endpoint + suffix, {
      method, redirect: 'error', signal: controller.signal,
      headers: { ...headers, ...(body === undefined ? {} : { 'Content-Type': 'application/json' }) },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!response.ok) throw Error('HTTP failure');
    const sse = response.headers.get('content-type')?.split(';')[0].trim().toLowerCase() === 'text/event-stream';
    if (method === 'GET') {
      if (!sse || !response.body) throw Error('Expected event stream');
      clearTimeout(timer);
      background = true;
      void relay(response.body).then(() => {
        if (!stopping) void shutdown('ACP event stream closed', 1);
      }, () => {
        if (!stopping) void shutdown('ACP event stream failed', 1);
      }).finally(release);
      return;
    }
    if (sse) {
      if (suffix || method === 'DELETE') throw Error('Unexpected event stream');
      if (streaming) clearTimeout(timer); // No deadline while streaming.
      await relay(response.body);
    } else {
      const message = await jsonBody(response.body);
      if (suffix) return message;
      if (message !== undefined && method !== 'DELETE') await forward(message);
    }
  } finally {
    if (!background) release();
  }
}

async function start() {
  startupDeadline = Date.now() + 45000;
  const cli = process.argv[1];
  if (!cli || (cliArgs.length && !print)) throw Error('Invalid CLI arguments');
  // Strip Nova capabilities before credential bootstrap.
  if (print) for (const key of Object.keys(process.env)) if (/^NOVA_/i.test(key)) delete process.env[key];
  const root = process.env.WORKBUDDY_CONFIG_DIR || join(homedir(), '.workbuddy');
  const bootstrap = credentials(cli, root);
  const token = randomBytes(32).toString('hex'), pipe = name => `\\\\.\\pipe\\nova-workbuddy-${name}-${randomUUID()}`;
  const readyPipe = pipe('ready'), credentialPipe = pipe('credential');
  let reportReady, failReady;
  const port = new Promise((resolve, reject) => { reportReady = resolve; failReady = reject; });
  port.catch(() => {});
  if (!print) readyServer = createServer(socket => {
    socket.on('error', () => {});
    void (async () => {
      for await (const line of lines(socket, 65536)) {
        const { id, method, params: p } = JSON.parse(line);
        if (method !== 'session.ready' || p?.sessionId !== owner || p.token !== token || p.pid !== worker?.pid ||
            !Number.isInteger(p.port) || p.port < 1 || p.port > 65535) break;
        socket.end(JSON.stringify({ jsonrpc: '2.0', id, result: { ok: true } }) + '\n');
        readyServer.close();
        return reportReady(p.port);
      }
      socket.destroy();
    })().catch(() => socket.destroy());
  });
  if (!print) await new Promise((resolve, reject) => readyServer.once('error', reject).listen(readyPipe, resolve));
  if (stopping) throw Error('Closing');
  const env = {
    ...process.env,
    WORKBUDDY_CONFIG_DIR: root, CODEBUDDY_CONFIG_DIR: root,
    CODEBUDDY_HOST: 'workbuddy-desktop', CODEBUDDY_FORCE_HEADLESS_BUNDLE: '1',
    CODEBUDDY_GATEWAY_AUTH: 'password', CODEBUDDY_GATEWAY_PASSWORD: password,
    DISABLE_AUTOUPDATER: '1', CODEBUDDY_DEFER_TOOL_LOADING: '0',
    CODEBUDDY_CODE_SHELL: process.env.CODEBUDDY_CODE_SHELL || 'powershell',
    SERVER__HOST: '127.0.0.1', SERVER__PORT: '0', WORKBUDDY_AT_REST_ENCRYPTION: POLICY,
    CODEBUDDY_SIDECAR_READY_SOCKET: readyPipe, CODEBUDDY_SIDECAR_READY_TOKEN: token,
    CODEBUDDY_SIDECAR_READY_SESSION_ID: owner, CODEBUDDY_SIDECAR_CREDENTIAL_BOOTSTRAP_SOCKET: credentialPipe,
  };
  // Discard stale desktop snapshots in favor of authenticated cloud policy.
  for (const key of Object.keys(env)) {
    if (/FORCE_LITE_WB/i.test(key) || /^ACC_PRODUCT_CONFIG(?:_PATH|_V[23])?$/i.test(key)) delete env[key];
  }
  if (print) delete env.CODEBUDDY_SIDECAR_READY_SOCKET;
  const args = print ? cliArgs : ['--serve', '--port', '0', '--host', '127.0.0.1'];
  // CLI's kill-on-close job cleans descendants.
  worker = spawn(native ? cli : process.execPath, native ? args : [cli, ...args],
    { cwd: process.cwd(), env, stdio: print ? 'inherit' : 'ignore', windowsHide: true });
  const lost = error => {
    failReady(error);
    if (!stopping) void shutdown('WorkBuddy worker exited', 1);
  };
  worker.once('error', lost).once('exit', code => print ? void shutdown(undefined, code ?? 1) : lost(Error('Worker exited')));
  await sendCredentials(credentialPipe, { version: 1, sessionId: owner, token, pid: worker.pid, bootstrap });
  if (print) return; // Input EOF is completion, not cancellation.
  let timer;
  const deadline = new Promise((_, reject) => { timer = setTimeout(() => reject(Error('Ready timeout')), remaining()); });
  endpoint = `http://127.0.0.1:${await Promise.race([port, deadline]).finally(() => clearTimeout(timer))}/api/v1/acp`;
  headers = { 'x-codebuddy-request': '1', Authorization: `Bearer ${password}`, Accept: 'application/json, text/event-stream' };
  if (stopping) return;
  const connection = await http('POST', {}, '/connect');
  const headerValue = value => typeof value === 'string' && value.length > 0 && value.length <= 4096 && !/[\r\n]/.test(value);
  if (!headerValue(connection?.connectionId)) throw Error('Invalid ACP connection');
  headers['acp-connection-id'] = connection.connectionId;
  if (connection.sessionToken !== undefined) {
    if (!headerValue(connection.sessionToken)) throw Error('Invalid ACP session token');
    headers['acp-session-token'] = connection.sessionToken;
  }
  if (!stopping) await http('GET');
  ready = true;
}

async function shutdown(reason, exitCode = 0) {
  if (cleanup) return cleanup;
  stopping = true;
  process.exitCode = exitCode;
  if (reason) console.error(`WorkBuddy ACP: ${reason}`);
  process.stdin.destroy();
  for (const controller of controllers) controller.abort();
  cleanup = (async () => {
    if (headers?.['acp-connection-id']) {
      try { await http('DELETE', undefined, '', 750); }
      catch { console.error('WorkBuddy ACP: connection cleanup failed'); process.exitCode = 1; }
    }
    if (worker && worker.exitCode === null && worker.signalCode === null) {
      const exited = once(worker, 'exit').catch(() => {});
      worker.kill();
      await Promise.race([exited, new Promise(resolve => setTimeout(resolve, 2000).unref())]);
    }
    // Drain pipe close before exit (Windows).
    await new Promise(resolve => readyServer ? readyServer.close(() => resolve()) : resolve());
  })();
  return cleanup;
}

async function handle(message) {
  const request = typeof message.method === 'string' && Object.hasOwn(message, 'id') ? { id: message.id, replied: false } : undefined;
  if (request) pending.add(request);
  try {
    await (starting ??= start());
    if (!stopping) {
      await http('POST', message);
      if (request && !request.replied) throw Error('Missing RPC reply');
    }
  } catch {
    if (!stopping) {
      if (request && !request.replied) {
        try { await forward({ jsonrpc: '2.0', id: request.id, error: { code: -32000, message: 'WorkBuddy ACP 启动或通信失败。请确认已安装 WorkBuddy 并至少登录过一次。' } }); }
        catch { /* stdout may already be closed. */ }
      }
      void shutdown('启动或通信失败。请确认已安装 WorkBuddy 并至少登录过一次。', 1);
    }
  } finally {
    if (request) pending.delete(request);
  }
}

process.stdout.on('error', () => { void shutdown('stdout failed', 1); });
for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.on(signal, () => { void shutdown(); });
if (process.platform !== 'win32') {
  void shutdown('only Windows is supported', 1);
} else if (print) {
  void start().catch(() => {
    if (!stopping) void shutdown('print 启动失败，请检查 WorkBuddy 登录', 1);
  });
} else {
  void (async () => {
    try {
      for await (const line of lines(process.stdin, INPUT_LIMIT)) {
        if (stopping) break;
        if (!line.trim()) continue;
        const message = JSON.parse(line);
        if (!validMessage(message)) throw Error('Invalid JSON-RPC');
        if (message.method === 'nova/workbuddy/shutdown') {
          if (Object.hasOwn(message, 'id')) throw Error('Shutdown must be a notification');
          await shutdown();
          process.exit(process.exitCode || 0);
        }
        void handle(message); // Do not block permission replies.
      }
      await shutdown();
    } catch {
      if (!stopping) await shutdown('invalid stdin or input failure', 1);
    }
  })();
}
