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
  lifecycle). Wired into `nova_lib` via `src/vega_native.rs::run_native_turn`,
  which compiles cleanly.

All 35 tests (12 unit + 23 differential) are green and `cargo test` needs no
node.

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

The only substantial piece not ported is the **LLM provider transport** —
`@earendil-works/pi-ai` (~33k lines: `openai-completions`, `openai-responses`,
`anthropic-messages`, `google-generative-ai`, with SSE streaming). This is the
"大模型" boundary explicitly excluded from the parity requirement and cannot be
differentially tested without live providers.

To finish "去掉 node 依赖":

1. Implement a `StreamFn` over `reqwest` for each provider protocol (reuse the
   existing `http_stream::SseDecoder`), converting SSE chunks into the
   `StreamTurn` events `run_loop` consumes.
2. Build the native tool executors (`ToolFn`) on top of `pi_core`'s ported tool
   logic plus real filesystem/shell I/O.
3. In `sdk_runtime`, add a native `AlkaidAdapter` path that calls
   `vega_native::run_native_turn` and forwards `items` through the existing
   event pipeline, behind a feature flag for gray-release against the node
   bridge; then remove `resources/alkaid-bridge.mjs` and the `node` spawn.
