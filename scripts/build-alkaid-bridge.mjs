import { spawnSync } from "node:child_process";
import { copyFileSync, mkdtempSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import * as esbuild from "esbuild";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const resourcesDir = join(root, "src-tauri", "resources");
const outfile = join(resourcesDir, "alkaid-bridge.mjs");
const wasmOut = join(resourcesDir, "photon_rs_bg.wasm");
const minify = process.argv.includes("--minify");
const skipSmoke = process.argv.includes("--skip-smoke");

/** ESM banner: Photon CJS uses free `__dirname` after esbuild inlining. */
const ALKAID_BRIDGE_BANNER = [
  "import{createRequire as __novaCreateRequire}from'node:module';",
  "import{fileURLToPath as __novaFileURLToPath}from'node:url';",
  "import{dirname as __novaDirname}from'node:path';",
  "const __novaFilename=__novaFileURLToPath(import.meta.url);",
  "const __dirname=__novaDirname(__novaFilename);",
  "const require=__novaCreateRequire(import.meta.url);",
].join("");

function resolvePhotonWasm() {
  const require = createRequire(import.meta.url);
  return require.resolve("@silvia-odwyer/photon-node/photon_rs_bg.wasm", {
    paths: [join(root, "node_modules", "@earendil-works", "pi-coding-agent")],
  });
}

async function buildBridge() {
  await esbuild.build({
    entryPoints: [join(root, "scripts", "alkaid-bridge.mjs")],
    bundle: true,
    platform: "node",
    format: "esm",
    outfile,
    minify,
    banner: { js: ALKAID_BRIDGE_BANNER },
  });
  copyFileSync(resolvePhotonWasm(), wasmOut);
}

async function smokeTestPhotonBundle() {
  const smokeDir = mkdtempSync(join(tmpdir(), "alkaid-photon-smoke-"));
  try {
    copyFileSync(wasmOut, join(smokeDir, "photon_rs_bg.wasm"));
    const smokeOut = join(smokeDir, "smoke.mjs");
    await esbuild.build({
      stdin: {
        contents: `
import { processImage } from "./node_modules/@earendil-works/pi-coding-agent/dist/utils/image-process.js";
const png = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
  "base64",
);
const result = await processImage(new Uint8Array(png), "image/png");
if (!result?.ok) {
  throw new Error(result?.message ?? "processImage failed");
}
console.log("photon-smoke-ok", result.mimeType);
`,
        resolveDir: root,
        sourcefile: "alkaid-photon-smoke.mjs",
      },
      bundle: true,
      platform: "node",
      format: "esm",
      outfile: smokeOut,
      banner: { js: ALKAID_BRIDGE_BANNER },
    });
    // Use a foreign cwd: Nova launches the bridge with the project cwd, not runtime/.
    const foreignCwd = mkdtempSync(join(tmpdir(), "alkaid-photon-cwd-"));
    try {
      const ran = spawnSync(process.execPath, [smokeOut], {
        encoding: "utf8",
        cwd: foreignCwd,
      });
      if (ran.status !== 0 || !ran.stdout.includes("photon-smoke-ok")) {
        throw new Error(
          `Photon smoke failed (status=${ran.status}):\n${ran.stdout}\n${ran.stderr}`,
        );
      }
    } finally {
      rmSync(foreignCwd, { recursive: true, force: true });
    }
  } finally {
    rmSync(smokeDir, { recursive: true, force: true });
  }
}

await buildBridge();
if (!skipSmoke) {
  await smokeTestPhotonBundle();
}
console.log(`built ${pathToFileURL(outfile).href}`);
console.log(`copied ${pathToFileURL(wasmOut).href}`);
