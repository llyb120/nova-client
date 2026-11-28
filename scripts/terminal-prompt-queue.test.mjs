import assert from "node:assert/strict";
import { test } from "node:test";
import { build } from "esbuild";

const bundled = await build({
  stdin: {
    resolveDir: process.cwd(),
    loader: "ts",
    contents: `
      export * from './src/terminalPromptQueue';
      export { state, setState } from './src/store';
      export { getTerminalGroup } from './src/terminalSessions';
      export { flush } from 'solid-js';
    `,
  },
  bundle: true,
  write: false,
  platform: "browser",
  format: "esm",
  conditions: ["browser", "development"],
  plugins: [
    {
      name: "terminal-queue-fixture",
      setup(builder) {
        builder.onResolve(
          { filter: /^(?:\.\/store|\.\/ipc|\.\/terminalSessions|\.\/agyTerminalPrompt)$/ },
          ({ path }) => ({ path, namespace: "fixture" }),
        );
        builder.onResolve({ filter: /^\.\/src\/store$/ }, () => ({
          path: "./store",
          namespace: "fixture",
        }));
        builder.onResolve({ filter: /^\.\/src\/terminalSessions$/ }, () => ({
          path: "./terminalSessions",
          namespace: "fixture",
        }));
        builder.onLoad({ filter: /.*/, namespace: "fixture" }, ({ path }) => ({
          loader: "js",
          resolveDir: process.cwd(),
          contents: {
            "./store": `
              import { createStore } from 'solid-js';
              export const [state, setState] = createStore({ running: {} });
            `,
            "./ipc": `
              export const pendingCalls = [];
              export const api = {
                setPromptQueuePending: async (threadId, pending) => {
                  pendingCalls.push({ threadId, pending });
                },
                saveTerminalAttachments: async (imgs) => imgs.map((img, i) => '/tmp/' + i + '-' + img.name),
              };
            `,
            "./terminalSessions": `
              import { createSignal } from 'solid-js';
              const tabStatus = createSignal('running');
              const fakeTab = { disposed: false, status: tabStatus[0] };
              export const sentInputs = [];
              export function getTerminalGroup() {
                return { tabs: () => [fakeTab] };
              }
              export async function sendTerminalInput(_tab, payload, options) {
                sentInputs.push({ payload, options });
                return true;
              }
            `,
            "./agyTerminalPrompt": `
              export function formatTerminalPromptWithPaths(text, paths) {
                const t = text.trim();
                if (!paths.length) return t;
                return t ? t + '\\n\\n附件文件：\\n' + paths.map(p => '- ' + p).join('\\n') : paths.join('\\n');
              }
            `,
          }[path],
        }));
      },
    },
  ],
});

const mod = await import(
  `data:text/javascript;base64,${Buffer.from(bundled.outputFiles[0].text).toString("base64")}`
);

test("terminal prompt queue supports FIFO auto-dispatch, steer (Alt+M), hold, withdraw, and failure retry", async () => {
  const tid = "term-thread-1";
  const dispatched = [];
  let failNext = false;

  const unregister = mod.registerTerminalQueueDispatcher(tid, async (item, steerNow) => {
    if (failNext) {
      failNext = false;
      return false;
    }
    dispatched.push({ id: item.id, text: item.text, steerNow });
    if (!steerNow) {
      mod.setTerminalQueueViewBusy(tid, true);
    }
    return true;
  });

  try {
    // 1. Running turn: enqueue two prompts without auto-dispatching.
    mod.setState((s) => {
      s.running[tid] = true;
    });
    mod.flush();

    mod.enqueueTerminalPrompt(tid, "first queued", [], "tq-1");
    mod.enqueueTerminalPrompt(tid, "second queued", [], "tq-2");
    mod.flush();
    await new Promise(setImmediate);
    assert.equal(dispatched.length, 0);
    assert.deepEqual(
      mod.terminalQueuedPrompts().map((i) => i.id),
      ["tq-1", "tq-2"],
    );

    // 2. Steer immediately (empty-input Enter -> Alt+M) sends head while running.
    const steered = await mod.dispatchTerminalQueuedPrompt(mod.terminalQueuedPrompts()[0], true);
    mod.flush();
    assert.equal(steered, true);
    assert.deepEqual(dispatched, [{ id: "tq-1", text: "first queued", steerNow: true }]);
    assert.deepEqual(
      mod.terminalQueuedPrompts().map((i) => i.id),
      ["tq-2"],
    );

    // 3. Enqueue another item, then hold (e.g. Stop pressed): idle transition must NOT auto-dispatch.
    mod.enqueueTerminalPrompt(tid, "third queued", [], "tq-3");
    mod.holdTerminalPromptQueue(tid);
    mod.setState((s) => {
      s.running[tid] = false;
    });
    mod.flush();
    await new Promise(setImmediate);
    assert.equal(dispatched.length, 1);

    // 4. Withdraw tq-2, release hold -> tq-3 auto-dispatches once and marks busy synchronously.
    const removed = mod.removeTerminalQueuedPrompt("tq-2");
    assert.equal(removed.text, "second queued");
    mod.enqueueTerminalPrompt(tid, "fourth queued", [], "tq-4");
    mod.releaseTerminalPromptQueue(tid);
    mod.flush();
    await new Promise(setImmediate);
    assert.deepEqual(dispatched.slice(1), [{ id: "tq-3", text: "third queued", steerNow: false }]);
    assert.deepEqual(
      mod.terminalQueuedPrompts().map((i) => i.id),
      ["tq-4"],
    );

    // 5. While busy, tq-4 waits; when busy clears and dispatcher fails once, item stays in failedQueueIds.
    failNext = true;
    mod.setTerminalQueueViewBusy(tid, false);
    mod.flush();
    await new Promise(setImmediate);
    assert.equal(mod.terminalQueueFailedIds().has("tq-4"), true);
    assert.equal(mod.terminalQueuedPrompts().length, 1);

    // Manual retry succeeds and drains the queue.
    const retried = await mod.dispatchTerminalQueuedPrompt(mod.terminalQueuedPrompts()[0], false);
    mod.flush();
    assert.equal(retried, true);
    assert.equal(mod.terminalQueueFailedIds().has("tq-4"), false);
    assert.equal(mod.terminalQueuedPrompts().length, 0);
  } finally {
    mod.setTerminalQueueViewBusy(tid, false);
    mod.clearTerminalPromptQueue(tid);
    unregister();
  }
});
