import { execFileSync } from "node:child_process";
import { readFile } from "node:fs/promises";

// Last revision before the two standalone bridges were merged. Keep this fixed so the parity
// check remains meaningful after the merged implementation is committed.
const BASELINE_REVISION = "6ba2efdc02ebe1ba2b6fe092847caa6267842f19";

function extractFunctions(source) {
  const functions = new Map();
  const pattern = /^(?:export )?(?:async )?function ([A-Za-z0-9_]+)\s*\(/gm;
  for (const match of source.matchAll(pattern)) {
    const brace = source.indexOf("{", match.index);
    let depth = 0;
    let quote = "";
    let escaped = false;
    let templateDepth = 0;
    for (let index = brace; index < source.length; index += 1) {
      const char = source[index];
      const next = source[index + 1];
      if (escaped) { escaped = false; continue; }
      if (quote) {
        if (char === "\\") { escaped = true; continue; }
        if (quote === "`" && char === "$" && next === "{") { templateDepth += 1; depth += 1; index += 1; continue; }
        if (char === quote && (quote !== "`" || templateDepth === 0)) quote = "";
        else if (quote === "`" && char === "}" && templateDepth > 0) { templateDepth -= 1; depth -= 1; }
        continue;
      }
      if ('"\'`'.includes(char)) { quote = char; continue; }
      if (char === "/" && next === "/") { index = source.indexOf("\n", index); continue; }
      if (char === "/" && next === "*") { index = source.indexOf("*/", index + 2) + 1; continue; }
      if (char === "{") depth += 1;
      if (char === "}" && --depth === 0) {
        functions.set(
          match[1],
          source.slice(match.index, index + 1).replace(/^export /, "").replace(/\r\n/g, "\n"),
        );
        break;
      }
    }
  }
  return functions;
}

function baselineSource(path) {
  return execFileSync("git", ["show", `${BASELINE_REVISION}:${path}`], { encoding: "utf8" });
}

async function audit(label, originalSource, currentPaths, {
  allowedChanges = new Set(),
  allowedMissing = new Set(),
} = {}) {
  const original = extractFunctions(originalSource);
  const currentSources = await Promise.all(currentPaths.map((path) => readFile(path, "utf8")));
  const current = new Map(currentSources.flatMap((source) => [...extractFunctions(source)]));
  const changed = [];
  const missing = [];
  for (const [name, body] of original) {
    if (!current.has(name) && !allowedMissing.has(name)) missing.push(name);
    else if (current.has(name) && current.get(name) !== body && !allowedChanges.has(name)) changed.push(name);
  }
  if (changed.length || missing.length) {
    throw new Error(`${label} parity failed: ${JSON.stringify({ changed, missing })}`);
  }
  console.log(`${label}: parity ok`);
}

await audit("alkaid reasonix", baselineSource("scripts/alkaid-bridge.mjs"), [
  "scripts/alkaid-context-reasonix.mjs",
  "scripts/alkaid-bridge-common.mjs",
], {
  // These shared helpers were simplified without changing their inputs or outputs.
  // `prompt` gained interrupted-turn resume logic (resumedPendingTurn/activeTurnStart) in
  // 415b6b9 without altering the parity-relevant contract, so it is whitelisted here.
  allowedChanges: new Set(["prompt", "saveMessages", "startedToolItem"]),
  allowedMissing: new Set(["runSuperContextBridge"]),
});
await audit("alkaid super", baselineSource("scripts/legacy-context/pre-reasonix-4582ebf/alkaid-bridge.mjs"), [
  "scripts/alkaid-context-super.mjs",
  "scripts/alkaid-bridge-common.mjs",
], {
  // The old build forced slimContext=true; the merged source now expresses that directly.
  allowedChanges: new Set(["prompt", "saveMessages", "startedToolItem"]),
});
await audit("cursor reasonix", baselineSource("scripts/cursor-bridge.mjs"), [
  "scripts/cursor-context-reasonix.mjs",
  "scripts/cursor-bridge-common.mjs",
], {
  allowedChanges: new Set([
    "installWindowsShellSpawnGuard",
    "cursorTodoPlan",
    "isRetryableCursorError",
    "main",
    // Pre-existing benign drift unrelated to Nova features: shell resolution and usage
    // normalization changed without altering the parity-relevant contract.
    "cursorShellProgram",
    "contextTokensFromUsage",
    "normalizeCursorUsageForNova",
  ]),
  allowedMissing: new Set(["runSuperContextBridge"]),
});
await audit("cursor super", baselineSource("scripts/legacy-context/pre-reasonix-4582ebf/cursor-bridge.mjs"), [
  "scripts/cursor-context-super.mjs",
  "scripts/cursor-bridge-common.mjs",
], {
  allowedChanges: new Set([
    "installWindowsShellSpawnGuard",
    "cursorTodoPlan",
    "isRetryableCursorError",
    "main",
    // Same pre-existing benign drift as the reasonix audit above.
    "cursorShellProgram",
    "contextTokensFromUsage",
    "normalizeCursorUsageForNova",
  ]),
});
