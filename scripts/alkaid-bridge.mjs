import { runContextBridge as runReasonixContext } from "./alkaid-context-reasonix.mjs";
import { runContextBridge as runSuperContext } from "./alkaid-context-super.mjs";

const runContextBridge = process.env.NOVA_CONTEXT_MODE === "super"
  ? runSuperContext
  : runReasonixContext;

await runContextBridge();
