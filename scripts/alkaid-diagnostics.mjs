import { appendFile, mkdir } from "node:fs/promises";
import { join } from "node:path";

export const ALKAID_PROVIDER_DIAGNOSTIC_LOG = "provider-timeouts.jsonl";

/**
 * Append-only timeout diagnostics. No file is opened on healthy requests; writes are queued only
 * after a timeout and flush() lets the bridge preserve the final record before it exits.
 */
export function createAlkaidDiagnosticLog(root) {
  const directory = join(root, "logs");
  const path = join(directory, ALKAID_PROVIDER_DIAGNOSTIC_LOG);
  let pending = Promise.resolve();
  let hasPendingWrites = false;

  return {
    path,
    record(event) {
      hasPendingWrites = true;
      const line = `${JSON.stringify(event)}\n`;
      pending = pending
        .then(async () => {
          await mkdir(directory, { recursive: true });
          await appendFile(path, line, "utf8");
        })
        // Diagnostics must never break a provider request or cause an unhandled rejection.
        .catch(() => {});
    },
    async flush() {
      if (hasPendingWrites) await pending;
    },
  };
}

/** Keep credentials and provider-specific URL parameters out of diagnostic logs. */
export function alkaidDiagnosticEndpoint(baseUrl) {
  try {
    return new URL(baseUrl).origin;
  } catch {
    return "invalid-url";
  }
}
