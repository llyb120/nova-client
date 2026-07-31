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
- M10 (done, deterministic + orchestration): the Reasonix context-management
  port and its wiring. **M10a** `slim_memory.rs` — full port of
  `alkaid-slim-memory.mjs` (`SlimMemory` state, append/conclusion/normalize,
  `formatSlimMemory`, `memoryWithoutCurrent`, `estimateContextTokens`,
  `contextPressureTier`, `contextTokensFromMessages`,
  `stripCompletedOpenAIReasoning`, `compactNativeToolResults`,
  `shouldUseFullContext`, `rebaseNativeContextForSlimMemory`,
  `seedSlimMemoryFromMessages`, and the `compactSlimMemory` decision logic with
  the `summarize` callback as the injected LLM boundary). **M10b**
  `skills_discovery.rs` — pi-coding-agent skill discovery (`SKILL.md` stops
  recursion, root `.md` loaded, `node_modules`/dot skipped), minimal frontmatter
  parser, `stripSkillFrontmatter`, `expandAlkaidSkillCommand`; `prepare_native_turn`
  now loads skills + `AGENTS.md`, detects the shell, and honors `read_only`.
  **M10c** `vega_reasonix.rs` + `run_prompt_native` — session IO
  (`<id>.slim.json`), `stableHash` fingerprinting, pending-prompt checkpoint,
  slim-record prompt prefix, and the per-turn flow (capacity tiers, pressure
  compaction, full/slim switching, snapshot freeze, conclusion/capacity/error
  persistence). This fixes the prior empty-transcript-per-turn gap (multi-turn
  memory now works natively).
- M10d–h (done): the remaining production surfaces. **M10d** the other three
  provider protocols — `transform.rs` (shared cross-provider message
  normalization), `provider_anthropic.rs`, `provider_responses.rs`,
  `provider_google.rs` (request building + SSE/chunk accumulators), all
  dispatched from `vega_provider::stream_turn`. **M10e** Reasonix digest
  compaction wired via a `plan_compaction`/`apply_compaction` split around the
  async lightweight-model summary turn. **M10f** the mid-turn
  `prepareNextTurnWithContext` hook in the agent loop (optional, parity-safe),
  wired to Reasonix per-round tool-result compaction + slim rebase. **M10g**
  prompt-image pass-through and a minimal MCP stdio client (`mcp.rs`).
  **M10h** `reasoningEffort` threaded into each provider's thinking config.
- M11 (done): production-behavior parity with the node bridge. **M11a**
  progressive streaming (each protocol item forwarded as produced via a channel
  + concurrent consumer task) and in-process steer/cancel (`NativeRunController`
  registered per thread; cancel races the in-flight transport, steer drains into
  the loop between tool rounds). **M11b** tool-result governance + disk archiving
  (`govern_tool_result`; `ToolFn` gains the call id). **M11c** provider retry with
  the node backoff delays + retryable-error classification. **M11d** token-cost
  calculation (`attach_cost`) + per-provider `max_tokens` enforcement. **M11e**
  SSE stream idle timeout (45s, shared `run_sse_stream`). **M11f** the legacy
  super-context mode (`super_memory.rs` + `run_prompt_native_super`, selected by
  `vega_context_mode == "super"`). **M11g** fixture tests for the three new
  accumulators (caught + fixed an Anthropic tool-index bug and a usage-merge bug).

All 78 pi_core tests (49 unit + 29 differential) are green and `cargo test`
needs no node. `nova_lib` compiles with 0 errors both with and without
`native-vega`. (One unrelated pre-existing nova test,
`path_env::windows_fallback_includes_claude_and_opencode_native_bins`, fails on
this Linux host independent of the native path.)

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
| `pi-ai` anthropic-messages message/SSE conversion | `provider_anthropic.rs` |
| `pi-ai` openai-responses message/SSE conversion | `provider_responses.rs` |
| `pi-ai` google-generative-ai message/chunk conversion | `provider_google.rs` |
| `pi-ai` `transform-messages.js` cross-provider normalization | `transform.rs` |
| `alkaid-slim-memory.mjs` Reasonix context management | `slim_memory.rs` |
| `pi-coding-agent` skill discovery + `/skill:` expansion | `skills_discovery.rs` |
| `alkaid-core.mjs` `connectMcpServers`/`mcpResult` | `nova_lib/src/mcp.rs` |
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

The deterministic core, the native tool executor, all four provider protocols
(`openai-completions`, `anthropic-messages`, `openai-responses`,
`google-generative-ai`), the Vega config resolution, the full Reasonix port
(slim-memory + between-turn orchestration + mid-turn hook + digest compaction),
the legacy super-context mode, native skill discovery, prompt images,
reasoning-effort pass-through, the MCP stdio client, progressive streaming,
in-process steer/cancel, provider retry, tool-result governance + archiving,
token-cost calculation, the SSE idle timeout, and the feature-gated
`sdk_runtime` seam are all in place. What remains is inherently online /
production-cutover work that cannot be differentially tested here:

1. **Live-provider verification + gray-release.** Enable `native-vega`, exercise
   real turns against each of the four providers, and compare against the node
   bridge before flipping the default. The three new transports and the MCP
   client compile and are fixture-tested but have not been run against live
   endpoints.
2. **MCP live verification.** `mcp.rs` needs a running MCP server to validate
   the initialize/list/call handshake end to end.
3. **Retire the node bridge.** Only after the above: remove
   `resources/alkaid-bridge.mjs` and the `node` spawn in `spawn_bridge`, and
   drop the `@earendil-works/pi-*` packages from `package.json`. The bridge is
   intentionally left in place and remains the default path.

Deferred advanced provider features (niche / auth-variant, not part of the
core Vega workflow, deliberately out of scope): Anthropic OAuth / Claude-Code
stealth headers, deferred tools + tool references, session-affinity headers,
fine-grained tool-streaming beta, `clampMaxTokensToContext`/
`adjustMaxTokensForThinking` (native uses fixed thinking budgets), OpenAI
Responses service-tier pricing + tool search, Google Vertex AI, and a
user-facing temperature control (Vega's request carries no temperature).

Known minor parity gaps (documented, non-blocking): the three new provider
accumulators are fixture-tested but not golden-diffed against node (only
`openai-completions` and the deterministic core have differential vectors —
regenerating golden vectors needs `node_modules`, which is not installed in
this environment). The previously documented UTF-16 vs scalar/UTF-8 slicing
differences in the slim-memory compaction heuristics have been fixed (M11h):
all length checks and head/tail slices now use UTF-16 code units, matching JS
exactly.
