import { copyFileSync, mkdirSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const manifest = join(root, "native", "nova-tools-napi", "Cargo.toml");
const release = process.argv.includes("--release");
const targetFlag = process.argv.find((arg) => arg.startsWith("--target="));
const targetIndex = process.argv.indexOf("--target");
const target = targetFlag?.slice("--target=".length)
  || (targetIndex >= 0 ? process.argv[targetIndex + 1] : "")
  || process.env.TAURI_ENV_TARGET_TRIPLE
  || "";
const args = ["build", "--manifest-path", manifest];
if (release) args.push("--release");
if (target) args.push("--target", target);

const built = spawnSync("cargo", args, { cwd: root, stdio: "inherit" });
if (built.status !== 0) process.exit(built.status ?? 1);

const profile = release ? "release" : "debug";
const platform = target.includes("windows") ? "win32"
  : target.includes("apple") ? "darwin"
    : target ? "linux" : process.platform;
const library = platform === "win32"
  ? "nova_tools_napi.dll"
  : platform === "darwin"
    ? "libnova_tools_napi.dylib"
    : "libnova_tools_napi.so";
const source = join(root, "native", "nova-tools-napi", "target", ...(target ? [target] : []), profile, library);
const output = join(root, "src-tauri", "resources", "nova-tools-napi.node");
mkdirSync(dirname(output), { recursive: true });
copyFileSync(source, output);
console.log(`built ${pathToFileURL(output).href}`);
