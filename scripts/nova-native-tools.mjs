import { spawn } from "node:child_process";
import { resolve } from "node:path";

const MAX_FRAME_BYTES = 64 * 1024 * 1024;
const REQUEST_TIMEOUT_MS = 30_000;
let client;
let disabledReason;

function pushUnsigned(parts, value) {
  if (value <= 0x7f) parts.push(Buffer.from([value]));
  else if (value <= 0xff) parts.push(Buffer.from([0xcc, value]));
  else if (value <= 0xffff) { const b = Buffer.allocUnsafe(3); b[0] = 0xcd; b.writeUInt16BE(value, 1); parts.push(b); }
  else if (value <= 0xffffffff) { const b = Buffer.allocUnsafe(5); b[0] = 0xce; b.writeUInt32BE(value, 1); parts.push(b); }
  else { const b = Buffer.allocUnsafe(9); b[0] = 0xcf; b.writeBigUInt64BE(BigInt(value), 1); parts.push(b); }
}

function pushArrayHeader(parts, length) {
  if (length < 16) parts.push(Buffer.from([0x90 | length]));
  else if (length <= 0xffff) { const b = Buffer.allocUnsafe(3); b[0] = 0xdc; b.writeUInt16BE(length, 1); parts.push(b); }
  else { const b = Buffer.allocUnsafe(5); b[0] = 0xdd; b.writeUInt32BE(length, 1); parts.push(b); }
}

function pushMapHeader(parts, length) {
  if (length < 16) parts.push(Buffer.from([0x80 | length]));
  else if (length <= 0xffff) { const b = Buffer.allocUnsafe(3); b[0] = 0xde; b.writeUInt16BE(length, 1); parts.push(b); }
  else { const b = Buffer.allocUnsafe(5); b[0] = 0xdf; b.writeUInt32BE(length, 1); parts.push(b); }
}

function encodeValue(value, parts) {
  if (value === null || value === undefined) { parts.push(Buffer.from([0xc0])); return; }
  if (value === false) { parts.push(Buffer.from([0xc2])); return; }
  if (value === true) { parts.push(Buffer.from([0xc3])); return; }
  if (typeof value === "number") {
    if (Number.isSafeInteger(value)) {
      if (value >= 0) pushUnsigned(parts, value);
      else if (value >= -32) parts.push(Buffer.from([0x100 + value]));
      else if (value >= -128) { const b = Buffer.allocUnsafe(2); b[0] = 0xd0; b.writeInt8(value, 1); parts.push(b); }
      else if (value >= -32768) { const b = Buffer.allocUnsafe(3); b[0] = 0xd1; b.writeInt16BE(value, 1); parts.push(b); }
      else { const b = Buffer.allocUnsafe(9); b[0] = 0xd3; b.writeBigInt64BE(BigInt(value), 1); parts.push(b); }
    } else { const b = Buffer.allocUnsafe(9); b[0] = 0xcb; b.writeDoubleBE(value, 1); parts.push(b); }
    return;
  }
  if (typeof value === "string") {
    const bytes = Buffer.from(value, "utf8");
    if (bytes.length < 32) parts.push(Buffer.from([0xa0 | bytes.length]));
    else if (bytes.length <= 0xff) parts.push(Buffer.from([0xd9, bytes.length]));
    else if (bytes.length <= 0xffff) { const h = Buffer.allocUnsafe(3); h[0] = 0xda; h.writeUInt16BE(bytes.length, 1); parts.push(h); }
    else { const h = Buffer.allocUnsafe(5); h[0] = 0xdb; h.writeUInt32BE(bytes.length, 1); parts.push(h); }
    parts.push(bytes); return;
  }
  if (Buffer.isBuffer(value) || value instanceof Uint8Array) {
    const bytes = Buffer.from(value); const h = Buffer.allocUnsafe(5); h[0] = 0xc6; h.writeUInt32BE(bytes.length, 1); parts.push(h, bytes); return;
  }
  if (Array.isArray(value)) { pushArrayHeader(parts, value.length); for (const item of value) encodeValue(item, parts); return; }
  if (typeof value === "object") { const entries = Object.entries(value).filter(([, item]) => item !== undefined); pushMapHeader(parts, entries.length); for (const [key, item] of entries) { encodeValue(key, parts); encodeValue(item, parts); } return; }
  throw new TypeError(`Unsupported MessagePack value: ${typeof value}`);
}

export function encodeMessagePack(value) { const parts = []; encodeValue(value, parts); return Buffer.concat(parts); }

function decoder(buffer) {
  let offset = 0;
  const take = (count) => { if (offset + count > buffer.length) throw new Error("Truncated MessagePack payload"); const start = offset; offset += count; return start; };
  const length = (tag, low) => {
    if (low < 24) return low;
    if (low === 24) return buffer.readUInt8(take(1));
    if (low === 25) return buffer.readUInt16BE(take(2));
    if (low === 26) return buffer.readUInt32BE(take(4));
    if (low === 27) { const n = buffer.readBigUInt64BE(take(8)); if (n > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("MessagePack integer exceeds JS safe range"); return Number(n); }
    throw new Error(`Unsupported MessagePack length tag: ${tag}`);
  };
  const read = () => {
    const tag = buffer.readUInt8(take(1));
    if (tag <= 0x7f) return tag;
    if (tag >= 0xe0) return tag - 0x100;
    if ((tag & 0xe0) === 0xa0) { const n = tag & 0x1f; return buffer.toString("utf8", take(n), offset); }
    if ((tag & 0xf0) === 0x90) { const n = tag & 0x0f; return Array.from({ length: n }, read); }
    if ((tag & 0xf0) === 0x80) { const n = tag & 0x0f; const out = {}; for (let i = 0; i < n; i += 1) out[read()] = read(); return out; }
    switch (tag) {
      case 0xc0: return null; case 0xc2: return false; case 0xc3: return true;
      case 0xc4: { const n = buffer.readUInt8(take(1)); return buffer.subarray(take(n), offset); }
      case 0xc5: { const n = buffer.readUInt16BE(take(2)); return buffer.subarray(take(n), offset); }
      case 0xc6: { const n = buffer.readUInt32BE(take(4)); return buffer.subarray(take(n), offset); }
      case 0xca: return buffer.readFloatBE(take(4)); case 0xcb: return buffer.readDoubleBE(take(8));
      case 0xcc: return buffer.readUInt8(take(1)); case 0xcd: return buffer.readUInt16BE(take(2)); case 0xce: return buffer.readUInt32BE(take(4));
      case 0xcf: { const n = buffer.readBigUInt64BE(take(8)); return n <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(n) : n; }
      case 0xd0: return buffer.readInt8(take(1)); case 0xd1: return buffer.readInt16BE(take(2)); case 0xd2: return buffer.readInt32BE(take(4));
      case 0xd3: { const n = buffer.readBigInt64BE(take(8)); return n >= BigInt(Number.MIN_SAFE_INTEGER) && n <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(n) : n; }
      case 0xd9: { const n = buffer.readUInt8(take(1)); return buffer.toString("utf8", take(n), offset); }
      case 0xda: { const n = buffer.readUInt16BE(take(2)); return buffer.toString("utf8", take(n), offset); }
      case 0xdb: { const n = buffer.readUInt32BE(take(4)); return buffer.toString("utf8", take(n), offset); }
      case 0xdc: { const n = buffer.readUInt16BE(take(2)); return Array.from({ length: n }, read); }
      case 0xdd: { const n = buffer.readUInt32BE(take(4)); return Array.from({ length: n }, read); }
      case 0xde: case 0xdf: { const n = tag === 0xde ? buffer.readUInt16BE(take(2)) : buffer.readUInt32BE(take(4)); const out = {}; for (let i = 0; i < n; i += 1) out[read()] = read(); return out; }
      default: throw new Error(`Unsupported MessagePack tag 0x${tag.toString(16)}`);
    }
  };
  const value = read(); if (offset !== buffer.length) throw new Error("Trailing bytes in MessagePack payload"); return value;
}

export function decodeMessagePack(payload) { return decoder(Buffer.from(payload)); }

class NativeToolsClient {
  constructor(executable) {
    this.nextId = 1; this.pending = new Map(); this.buffer = Buffer.alloc(0); this.closed = false;
    this.child = spawn(executable, ["--nova-tools-native"], { stdio: ["pipe", "pipe", "pipe"], windowsHide: true });
    // A cached sidecar must not keep short-lived test/CLI processes alive after all calls finish.
    this.child.unref();
    this.child.stdin.unref?.();
    this.child.stdout.unref?.();
    this.child.stderr.unref?.();
    this.child.stdout.on("data", (chunk) => this.onData(chunk));
    this.child.stderr.on("data", (chunk) => { if (process.env.NOVA_TOOLS_NATIVE_DEBUG === "1") process.stderr.write(chunk); });
    this.child.once("error", (error) => this.close(error));
    this.child.once("exit", (code, signal) => this.close(new Error(`native tools sidecar exited (${code ?? signal ?? "unknown"})`)));
  }
  onData(chunk) {
    this.buffer = this.buffer.length ? Buffer.concat([this.buffer, chunk]) : Buffer.from(chunk);
    while (this.buffer.length >= 4) {
      const length = this.buffer.readUInt32LE(0);
      if (length < 1 || length > MAX_FRAME_BYTES) { this.close(new Error(`invalid native MessagePack frame length: ${length}`)); return; }
      if (this.buffer.length < 4 + length) return;
      const payload = this.buffer.subarray(4, 4 + length); this.buffer = this.buffer.subarray(4 + length);
      let response; try { response = decodeMessagePack(payload); } catch (error) { this.close(error); return; }
      const pending = this.pending.get(response.id); if (!pending) continue; this.pending.delete(response.id); clearTimeout(pending.timer);
      if (response.error) { const error = new Error(response.error.message); error.code = response.error.code; error.definitelyNotExecuted = response.error.definitely_not_executed === true; pending.reject(error); }
      else pending.resolve(response.result);
    }
  }
  close(error) { if (this.closed) return; this.closed = true; for (const pending of this.pending.values()) { clearTimeout(pending.timer); pending.reject(error); } this.pending.clear(); }
  call(method, root, params) {
    if (this.closed) return Promise.reject(new Error("native tools sidecar is closed"));
    const id = this.nextId++; const payload = encodeMessagePack({ id, method, root: resolve(root), params: params ?? {} });
    if (payload.length > MAX_FRAME_BYTES) return Promise.reject(new Error("native tools request exceeds 64MB"));
    const frame = Buffer.allocUnsafe(4 + payload.length); frame.writeUInt32LE(payload.length, 0); payload.copy(frame, 4);
    return new Promise((resolvePromise, reject) => { const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`native ${method} timed out after ${REQUEST_TIMEOUT_MS}ms`)); }, REQUEST_TIMEOUT_MS); this.pending.set(id, { resolve: resolvePromise, reject, timer }); this.child.stdin.write(frame, (error) => { if (error) { const pending = this.pending.get(id); if (pending) { this.pending.delete(id); clearTimeout(timer); reject(error); } } }); });
  }
}

function nativeExecutable() { if (process.env.NOVA_TOOLS_BACKEND === "js") return null; return process.env.NOVA_TOOLS_NATIVE_EXE || null; }
function getClient() { if (client && !client.closed) return client; const executable = nativeExecutable(); if (!executable || disabledReason) return null; try { return (client = new NativeToolsClient(executable)); } catch (error) { disabledReason = error; return null; } }

export function nativeToolsAvailable() { return Boolean(getClient()); }
export function closeNativeToolsClient() {
  if (!client) return;
  client.child.stdin.end();
  client.child.kill();
  client = undefined;
}
export function mayFallbackFromNative(method, error) {
  return process.env.NOVA_TOOLS_BACKEND !== "native"
    && (method !== "edit_files" || error?.definitelyNotExecuted === true);
}
export async function callNativeTool(method, root, params) { const active = getClient(); if (!active) { const error = new Error(disabledReason?.message || "native tools backend is unavailable"); error.code = "NATIVE_UNAVAILABLE"; error.definitelyNotExecuted = true; throw error; } return active.call(method, root, params); }
export async function callNativeToolOrFallback(method, root, params, fallback) {
  try { return await callNativeTool(method, root, params); }
  catch (error) {
    if (!mayFallbackFromNative(method, error)) throw error;
    return fallback();
  }
}
