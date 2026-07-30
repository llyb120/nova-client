# pi_core

Rust port of the deterministic core of the node pi-agent that powers the
Vega/Alkaid backend (`scripts/alkaid-core.mjs` and the `@earendil-works/pi-*`
packages). The goal is **byte-for-byte output parity** with the node
implementation for every non-LLM computation.

## Parity methodology

- `testdata/golden.json` holds input/expected vectors produced once by the real
  node implementation. `cargo test` asserts the Rust output equals these
  vectors, so the test suite needs **no node**.
- Regenerate vectors after changing a ported node function:

  ```bash
  node scripts/pi-golden.mjs   # dev-time only
  cargo test                   # in this directory
  ```

- JS strings are UTF-16: `.length`/`.slice` use code units, `Buffer.byteLength`
  uses UTF-8 bytes, `Array.from` uses code points, and a split surrogate pair
  leaves a lone surrogate that Node counts as 3 bytes. The port reproduces all
  three semantics (see `src/text.rs`).

## Status

- M1 (done): text governance/truncation, OpenAI payload transforms (cache key
  injection, output clamping), usage merging, encoding detection, system prompt
  assembly.
- M2 (done): `edit_files` smart-edit locator/applier (exact → rstrip → unicode →
  relative-indent → fuzzy cascade, ambiguity/overlap rejection, indentation
  rebasing); `read`/`read_files` line reading (encoding detection, offset/limit,
  32KB byte budget, truncation/`nextOffset`); the shared truncation utilities
  (`formatSize`, `truncateHead`, `truncateTail`, `truncateLine`); path resolution
  (`normalizePath`, `resolvePath`, `resolveToCwd`, `dirname`); the `write` and
  `ls` tools; and skill prompt formatting (XML block + compressed by-root
  listing, XML escaping, `disableModelInvocation` filter).
- M3 (done): the agent runtime. Message construction, the `runLoop` state
  machine (two-level steering/follow-up loop, event emission), streaming deltas
  (`message_start`/`message_update`/`message_end` with the accumulating partial),
  sequential **and** parallel tool scheduling, and the stateful `Agent` wrapper
  (`PendingMessageQueue` drain semantics, `normalizePromptInput`, state
  reduction, `prompt`/`steer`/`continue`/`subscribe`). Verified end-to-end
  against node's real `runAgentLoop` and `Agent`.
- M4 (done, deterministic part): the bridge protocol glue — `startedToolItem`,
  the `tool_execution_end` handler, and the `ProtocolAccumulator` that assembles
  the full item stream (text/thinking accumulation, usage merging, tool item
  lifecycle).
- M5 (done): the native tool executor (`NativeTools`) — dispatches
  `read`/`read_files`/`edit`/`edit_files`/`write`/`ls` to the ported logic plus
  `std` I/O, and `bash`/`grep`/`find` to subprocesses. Fixture-tested.
- M6 (done): OpenAI Chat Completions provider conversion (`provider.rs`) —
  pi↔OpenAI message/tool conversion, request building, and an SSE-chunk
  accumulator producing a `StreamTurn`. Unit-tested without a live provider.
- M7 (done): the async provider transport (`nova_lib/src/vega_provider.rs`) over
  `reqwest` + the shared `SseDecoder`, and `run_native_turn_async` which runs the
  sync loop on a blocking thread while its `StreamFn` blocks on the transport.
- M8 (done): Vega config resolution (`alkaid_config.rs`) — `parseJsonc`,
  `mergeAlkaidConfig`, `providerApi`, `resolveEnv`, `mergeAlkaidCompatDefaults`,
  `resolveAlkaidModel`. Golden-tested.
- M9 (done): the feature-gated production seam — `native-vega` cargo feature,
  `prepare_native_turn` (config → `ProviderConfig` + system prompt + tool defs),
  and `sdk_runtime::run_prompt_native` routing the Alkaid adapter in-process when
  the feature is enabled. Compiles in both configurations; off by default.

All 55 tests (27 unit + 28 differential) are green and `cargo test` needs no
node. `nova_lib` compiles with 0 errors both with and without `native-vega`.

## Function map (node → Rust)

| node source | Rust (`pi_core`) |
| --- | --- |
| `alkaid-core.mjs` `clampToolOutputText`/`governToolResult`/head-tail | `text.rs` |
| `alkaid-core.mjs` payload transforms (cache key, clamp) | `payload.rs` |
| `alkaid-core.mjs` `mergeAlkaidUsage` | `payload::merge_usage` |
| `alkaid-core.mjs` `decodeTextBuffer`/`detectTextEncoding` | `encoding.rs` |
| `alkaid-core.mjs` `buildAlkaidSystemPrompt`/`optimizeAlkaidSystemPrompt` | `prompt.rs` |
| `alkaid-core.mjs`/`skills.js` skill formatting | `skills.rs` |
| `alkaid-smart-edit.mjs` `applySmartEdits` | `smart_edit.rs` |
| `alkaid-core.mjs` `readTextLines` + `read_files` tool | `read.rs` |
| `pi-coding-agent` `truncate.js` | `truncate.rs` |
| `pi-coding-agent` `paths.js`/`path-utils.js` | `paths.rs` |
| `pi-coding-agent` `write.js`/`ls.js` | `write.rs`/`ls.rs` |
| `pi-agent-core` `agent-loop.js` (`runAgentLoop`/`runLoop`/streaming/tools) | `agent/run_loop.rs` |
| `pi-agent-core` `agent.js` (`Agent`, queues, lifecycle) | `agent/agent.rs` |
| `alkaid-bridge-common.mjs` `startedToolItem` + reasonix subscribe handler | `bridge.rs` |
| tool execution (read/edit/write/ls/bash/grep/find dispatch) | `tools.rs` |
| `pi-ai` openai-completions message/SSE conversion | `provider.rs` |
| `alkaid-config.mjs` config/model resolution | `alkaid_config.rs` |

## Verified parity boundaries (honest limits)

- **UTF-16 semantics** reproduced exactly (code-unit length/slice, UTF-8 byte
  budgets, code-point iteration, lone-surrogate = 3 bytes).
- **`formatSize`** reproduces JS `toFixed(1)` round-half-up via integer
  arithmetic (not Rust banker's rounding).
- **`write`** reproduces the JS UTF-16 `content.length` count.
- **Not byte-parity (by nature):** OS error *messages* (libuv vs `io::Error` —
  presence is checked); `ls`/skill *sorting* for non-ASCII (node ICU/UTF-16
  collation approximated by byte order, exact for ASCII); `bash` tool output
  (depends on the real shell/environment).

## Remaining work (outside the verifiable scope)

The deterministic core, the native tool executor, the OpenAI Chat Completions
transport, the Vega config resolution, and the feature-gated `sdk_runtime` seam
are all in place. What remains is inherently online / production-cutover work
that cannot be differentially tested here:

1. **Other provider protocols.** `openai-completions` is implemented;
   `openai-responses`, `anthropic-messages`, and `google-generative-ai` are
   explicit extension points in `vega_provider.rs` (part of the excluded `pi-ai`
   surface, ~33k lines).
2. **Skills + MCP.** `prepare_native_turn` does not yet load skills from disk
   (the system prompt's skills section is empty) or connect MCP servers, both of
   which the node bridge does via `pi-coding-agent`.
3. **Session-history continuity.** `run_prompt_native` starts each turn from an
   empty transcript; mapping the thread store's persisted messages into pi
   messages (and the `restoreAt`/time-machine flow) is not yet wired.
4. **Live-provider verification + gray-release.** Enable `native-vega`, exercise
   real turns against each provider, and compare against the node bridge before
   flipping the default.
5. **Retire the node bridge.** Only after the above: remove
   `resources/alkaid-bridge.mjs` and the `node` spawn in `spawn_bridge`. The
   bridge is intentionally left in place and remains the default path.
