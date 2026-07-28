import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import * as esbuild from "esbuild";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const scriptsDir = join(root, "scripts");
const legacyDir = join(scriptsDir, "legacy-context", "pre-reasonix-4582ebf");
const resourcesDir = join(root, "src-tauri", "resources");
const minify = process.argv.includes("--minify");

const ALKAID_BRIDGE_BANNER = [
  "import{createRequire as __novaCreateRequire}from'node:module';",
  "import{fileURLToPath as __novaFileURLToPath}from'node:url';",
  "import{dirname as __novaDirname}from'node:path';",
  "const __filename=__novaFileURLToPath(import.meta.url);",
  "const __dirname=__novaDirname(__filename);",
  "const require=__novaCreateRequire(import.meta.url);",
].join("");

function legacyCompatibility() {
  return {
    name: "legacy-context-compatibility",
    setup(build) {
      build.onResolve({ filter: /^\.\/.+\.mjs$/ }, (args) => {
        if (dirname(args.importer) !== legacyDir) return null;
        const requested = join(legacyDir, args.path);
        if (args.path === "./alkaid-slim-memory.mjs") return { path: requested };
        return { path: join(scriptsDir, args.path.slice(2)) };
      });
      build.onLoad({ filter: /legacy-context\/pre-reasonix-4582ebf\/alkaid-bridge\.mjs$/ }, async (args) => {
        const { readFile } = await import("node:fs/promises");
        const source = await readFile(args.path, "utf8");
        return {
          contents: source.replace(
            "const slimContext = request.vegaSlimContext === true;",
            "const slimContext = true;",
          ),
          loader: "js",
        };
      });
    },
  };
}

async function build(entry, outfile, banner) {
  await esbuild.build({
    entryPoints: [join(legacyDir, entry)],
    bundle: true,
    platform: "node",
    format: "esm",
    outfile: join(resourcesDir, outfile),
    minify,
    plugins: [legacyCompatibility()],
    external: ["bun:sqlite", "vendor/*"],
    loader: { ".map": "empty", ".ts": "empty", ".schemas": "empty" },
    banner: banner ? { js: banner } : undefined,
  });
  console.log(`built ${pathToFileURL(join(resourcesDir, outfile)).href}`);
}

await build("alkaid-bridge.mjs", "alkaid-super-context-bridge.mjs", ALKAID_BRIDGE_BANNER);
await build(
  "cursor-bridge.mjs",
  "cursor-super-context-bridge.mjs",
  "import{createRequire}from'node:module';const require=createRequire(import.meta.url);",
);
