import { runContextBridge as runReasonixContext } from "./cursor-context-reasonix.mjs";
import { runContextBridge as runSuperContext } from "./cursor-context-super.mjs";

export * from "./cursor-context-reasonix.mjs";

if (process.env.NOVA_CURSOR_BRIDGE_TEST !== "1") {
  const runContextBridge = process.env.NOVA_CONTEXT_MODE === "super"
    ? runSuperContext
    : runReasonixContext;
  runContextBridge().catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
    process.stdout.write(`${JSON.stringify({ ok: false, error: error instanceof Error ? error.message : String(error) })}\n`);
    process.exitCode = 1;
  });
}
