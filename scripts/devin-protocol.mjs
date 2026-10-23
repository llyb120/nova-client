// Connect/Protobuf fields verified against Devin CLI 3000.11.205.
// ponytail: this is Devin's private wire protocol; re-run the live probes when it changes.
import { randomUUID } from "node:crypto";
import { gunzipSync } from "node:zlib";
import { lookup } from "node:dns/promises";
import { isIP } from "node:net";
import { request } from "node:https";

export const LIMIT = 32 * 1024 * 1024;
export const failure = (status, message, code = "invalid_request") => Object.assign(new Error(message), { status, code });
const varint = value => {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error("Invalid protobuf integer");
  const bytes = [];
  do { const byte = value % 128; value = Math.floor(value / 128); bytes.push(byte | (value ? 128 : 0)); } while (value);
  return Buffer.from(bytes);
};
export const proto = (...fields) => Buffer.concat(fields);
export function field(number, value, wire) {
  wire ??= typeof value === "number" ? 0 : 2;
  if (wire === 0) return proto(varint(number * 8), varint(value));
  if (wire === 1) { const b = Buffer.alloc(8); b.writeDoubleLE(value); return proto(varint(number * 8 + 1), b); }
  const bytes = Buffer.isBuffer(value) ? value : Buffer.from(value);
  return proto(varint(number * 8 + 2), varint(bytes.length), bytes);
}
export function decode(bytes) {
  let offset = 0;
  const read = () => {
    let value = 0n, shift = 0n;
    for (let i = 0; i < 10; i++) {
      if (offset >= bytes.length) throw failure(502, "Truncated Devin protobuf.", "upstream_protocol_error");
      const byte = bytes[offset++]; value |= BigInt(byte & 127) << shift;
      if (byte < 128) { if (value > BigInt(Number.MAX_SAFE_INTEGER)) throw failure(502, "Oversized protobuf integer."); return Number(value); }
      shift += 7n;
    }
    throw failure(502, "Invalid protobuf varint.");
  };
  const result = [];
  while (offset < bytes.length) {
    const tag = read(), number = Math.floor(tag / 8), wire = tag % 8;
    if (!number) throw failure(502, "Invalid protobuf field.");
    if (wire === 0) { result.push([number, read()]); continue; }
    const size = wire === 2 ? read() : wire === 1 ? 8 : wire === 5 ? 4 : -1;
    if (size < 0 || offset + size > bytes.length) throw failure(502, "Invalid protobuf field length.");
    result.push([number, bytes.subarray(offset, offset + size)]); offset += size;
  }
  return result;
}
export const value = (fields, number) => fields.find(([id]) => id === number)?.[1];
export const string = (fields, number) => value(fields, number)?.toString("utf8") || "";
export function envelope(bytes, flags = 0) {
  const header = Buffer.alloc(5); header[0] = flags; header.writeUInt32BE(bytes.length, 1); return proto(header, bytes);
}

function publicAddress(address) {
  if (isIP(address) === 4) {
    const [a, b] = address.split(".").map(Number);
    return !(a === 0 || a === 10 || a === 127 || a >= 224 || (a === 169 && b === 254) || (a === 172 && b >= 16 && b <= 31)
      || (a === 192 && [0,168].includes(b)) || (a === 100 && b >= 64 && b <= 127) || (a === 198 && [18,19].includes(b)));
  }
  // Only globally routable IPv6 unicast; exclude transition/documentation ranges.
  return /^[23]/i.test(address) && !/^(2001:|2002:|3fff:)/i.test(address);
}
async function imageBytes(source, signal, redirects = 0) {
  if (source.startsWith("data:")) {
    const match = /^data:(image\/(?:png|jpeg|gif|webp));base64,([A-Za-z0-9+/]*={0,2})$/.exec(source);
    if (!match || !match[2] || match[2].length % 4) throw failure(400, "Invalid base64 image data URL.");
    const bytes = Buffer.from(match[2], "base64");
    if (bytes.toString("base64") !== match[2] || bytes.length > LIMIT / 2) throw failure(400, "Invalid or oversized image.");
    return { data: match[2], mime: match[1] };
  }
  let url;
  try { url = new URL(source); } catch { throw failure(400, "Invalid image URL."); }
  if (url.protocol !== "https:" || url.username || url.password || (url.port && url.port !== "443") || redirects > 3) throw failure(400, "Images require a public HTTPS URL or a base64 data URL.");
  const hostname = url.hostname.replace(/^\[|\]$/g, "");
  const addresses = await lookup(hostname, { all: true });
  if (!addresses.length || addresses.some(({ address }) => !publicAddress(address))) throw failure(400, "Private or reserved image addresses are not allowed.");
  // Pin the validated DNS result, including every redirect, to prevent rebinding.
  return new Promise((resolve, reject) => {
    const req = request(url, { signal, agent: false, lookup: (_host, options, callback) => {
      const selected = addresses[0]; options.all ? callback(null, [selected]) : callback(null, selected.address, selected.family);
    } }, async response => {
      try {
        if ([301,302,303,307,308].includes(response.statusCode)) {
          response.resume();
          if (!response.headers.location) throw failure(400, "Image redirect is missing its location.");
          return resolve(await imageBytes(new URL(response.headers.location, url).href, signal, redirects + 1));
        }
        if (response.statusCode !== 200) throw failure(400, "Image URL could not be downloaded.");
        const mime = (response.headers["content-type"] || "").split(";")[0].trim();
        if (!/^image\/(png|jpeg|gif|webp)$/.test(mime)) throw failure(400, "Unsupported image content type.");
        const chunks = []; let size = 0;
        for await (const chunk of response) { size += chunk.length; if (size > LIMIT / 2) throw failure(413, "Image exceeds 16 MiB."); chunks.push(chunk); }
        resolve({ data: Buffer.concat(chunks).toString("base64"), mime });
      } catch (error) { response.destroy(); reject(error); }
    });
    req.on("error", () => reject(signal.reason || failure(400, "Image download failed."))); req.end();
  });
}

export async function encodeRequest(body, credentials, version, signal) {
  const f = field, messages = [], systems = [];
  for (const message of body.messages) {
    const text = [], images = [];
    for (const part of Array.isArray(message.content) ? message.content : [{ type: "text", text: message.content || "" }]) {
      if (part.type === "text") text.push(part.text);
      else {
        const image = await imageBytes(part.image_url.url, signal);
        images.push(f(10, proto(f(1, image.data), f(2, image.mime))));
      }
    }
    if (["system", "developer"].includes(message.role)) { systems.push(text.join("\n")); continue; }
    messages.push(f(3, proto(f(1, randomUUID()), f(2, { user: 1, assistant: 2, tool: 4 }[message.role]), f(3, text.join("\n")),
      ...(message.tool_calls || []).map(call => f(6, proto(f(1, call.id), f(2, call.function.name), f(3, call.function.arguments)))),
      ...(message.tool_call_id ? [f(7, message.tool_call_id)] : []), ...images)));
  }
  const tools = body.tools || [];
  if (body.tool_choice === "required" || typeof body.tool_choice === "object") {
    // ponytail: this backend has no native tool_choice field. Limit the exposed
    // tools, ask for a call, and reject a nonconforming response instead of faking it.
    systems.push("For this turn, you must invoke one of the provided functions. Do not answer without a function call.");
  }
  if (body.parallel_tool_calls === false) systems.push("Invoke at most one function in this turn.");
  const metadata = proto(f(1, "nova-openai"), f(2, version), f(3, credentials.key), f(4, "en"), f(5, process.platform), f(7, version), f(12, "chisel"), f(28, "chisel"));
  return proto(f(1, metadata), f(2, systems.join("\n\n")), ...messages, f(7, 5),
    f(8, proto(f(1, 1), f(2, body.max_completion_tokens ?? body.max_tokens ?? 128000), f(3, 400), f(5, body.temperature ?? 1, 1), f(7, 40), f(8, body.top_p ?? 0.95, 1))),
    ...tools.map(({ function: tool }) => f(10, proto(f(1, tool.name), f(2, tool.description || ""), f(3, JSON.stringify(tool.parameters || { type: "object", properties: {} }))))),
    f(15, proto(f(1, randomUUID()), f(3, 4), f(4, 14))), f(16, randomUUID()), f(20, 1), f(21, body.model), f(22, randomUUID()), f(28, "nova-api"));
}

export async function* inference(body, credentials, version, signal) {
  const encoded = await encodeRequest(body, credentials, version, signal);
  const response = await fetch(new URL("/exa.api_server_pb.ApiServerService/GetChatMessage", credentials.url), {
    method: "POST", headers: { "content-type": "application/connect+proto", "connect-protocol-version": "1", "connect-accept-encoding": "gzip" },
    body: envelope(encoded), signal,
  });
  if (!response.ok) throw failure([401,403,429].includes(response.status) ? response.status : 502, `Devin upstream returned HTTP ${response.status}.`, "upstream_error");
  if (!response.headers.get("content-type")?.startsWith("application/connect+proto")) throw failure(502, "Unexpected Devin response type.", "upstream_protocol_error");
  let buffer = Buffer.alloc(0), total = 0, expanded = 0, ended = false;
  for await (const chunk of response.body) {
    total += chunk.length;
    if (total > LIMIT) throw failure(502, "Devin output exceeds 32 MiB.", "output_limit");
    buffer = proto(buffer, chunk);
    while (buffer.length >= 5) {
      const flags = buffer[0], size = buffer.readUInt32BE(1);
      if (flags > 3 || size > LIMIT || ended) throw failure(502, "Invalid Connect frame.", "upstream_protocol_error");
      if (buffer.length < size + 5) break;
      let payload = buffer.subarray(5, size + 5); buffer = buffer.subarray(size + 5);
      if (flags & 1) payload = gunzipSync(payload, { maxOutputLength: LIMIT });
      expanded += payload.length;
      if (expanded > LIMIT) throw failure(502, "Expanded Devin output exceeds 32 MiB.", "output_limit");
      if (flags & 2) {
        let terminal;
        try { terminal = JSON.parse(payload.toString()); } catch { throw failure(502, "Invalid Connect trailer.", "upstream_protocol_error"); }
        if (terminal.error) {
          const status = { unauthenticated: 401, permission_denied: 403, resource_exhausted: 429, invalid_argument: 400 }[terminal.error.code] || 502;
          throw failure(status, `Devin rejected the request (${terminal.error.code || "upstream_error"}); check model access and parameters.`, "upstream_error");
        }
        ended = true; continue;
      }
      const fields = decode(payload);
      for (const [id, data] of fields) {
        if (id === 3 || id === 9) yield { type: id === 3 ? "text" : "reasoning", text: data.toString() };
        if (id === 6) { const call = decode(data); yield { type: "tool", id: string(call, 1), name: string(call, 2), arguments: string(call, 3) }; }
        if (id === 5) yield { type: "stop", reason: data };
        if (id === 7) {
          const usage = decode(data);
          if (value(usage, 2) !== undefined || value(usage, 3) !== undefined) {
            const input = value(usage, 2) || 0, output = value(usage, 3) || 0;
            yield { type: "usage", usage: { prompt_tokens: input, completion_tokens: output, total_tokens: input + output,
              prompt_tokens_details: { cached_tokens: value(usage, 5) || 0 } } };
          }
        }
      }
    }
  }
  if (!ended || buffer.length) throw failure(502, "Devin stream ended without a valid trailer.", "upstream_incomplete");
}
