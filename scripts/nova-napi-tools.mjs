import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const moduleDir = typeof __dirname === "string"
  ? __dirname
  : dirname(fileURLToPath(import.meta.url));
let binding;
let loadError;

function backendMode() {
  return String(process.env.NOVA_TOOLS_BACKEND ?? "").trim().toLowerCase();
}

function bindingCandidates() {
  const configured = String(process.env.NOVA_TOOLS_NAPI_PATH ?? "").trim();
  return [
    configured ? resolve(configured) : "",
    join(moduleDir, "nova-tools-napi.node"),
    join(moduleDir, "..", "src-tauri", "resources", "nova-tools-napi.node"),
  ].filter(Boolean);
}

function loadBinding() {
  if (binding) return binding;
  if (loadError) throw loadError;
  const attempts = [];
  for (const candidate of bindingCandidates()) {
    if (!existsSync(candidate)) continue;
    try {
      binding = require(candidate);
      return binding;
    } catch (error) {
      attempts.push(`${candidate}: ${error instanceof Error ? error.message : String(error)}`);
    }
  }
  loadError = new Error(attempts.length
    ? `failed to load nova-tools N-API addon: ${attempts.join("; ")}`
    : "nova-tools N-API addon was not found");
  loadError.code = "NAPI_UNAVAILABLE";
  throw loadError;
}

export function napiToolsAvailable() {
  if (backendMode() === "js") return false;
  try {
    loadBinding();
    return true;
  } catch {
    return false;
  }
}

export async function callNapiTool(method, root, params) {
  const native = loadBinding();
  if (method === "read_files") return native.readFiles(resolve(root), params ?? {});
  if (method === "edit_files") return native.editFiles(resolve(root), params ?? {});
  if (method === "fast_context") return native.fastContext(resolve(root), params ?? {});
  if (method === "find_symbols") return native.findSymbols(resolve(root), params ?? {});
  if (method === "code_map") return native.codeMap(resolve(root), params ?? {});
  throw new Error(`unknown N-API tool: ${method}`);
}

export async function callNapiToolOrFallback(method, root, params, fallback) {
  const mode = backendMode();
  if (mode === "js") return fallback();
  try {
    loadBinding();
  } catch (error) {
    if (mode === "native" || mode === "napi") throw error;
    return fallback();
  }
  // Once native execution starts, propagate errors. edit_files may already have touched disk;
  // retrying through JS could apply the same mutation twice.
  return callNapiTool(method, root, params);
}
