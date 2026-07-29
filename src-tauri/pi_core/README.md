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
  assembly — all covered by differential tests.
- M2 (done): `edit_files` smart-edit locator/applier (exact → rstrip →
  unicode → relative-indent → fuzzy cascade, ambiguity/overlap rejection,
  indentation rebasing); `read`/`read_files` line reading (encoding detection,
  offset/limit, 32KB byte budget, truncation/`nextOffset`); the shared
  truncation utilities (`formatSize`, `truncateHead`, `truncateTail`,
  `truncateLine`) used by grep/find/ls/bash; path resolution (`normalizePath`,
  `resolvePath`, `resolveToCwd`, `dirname` with lexical `..`/`.`
  normalization, Unicode-space folding, `@`-prefix stripping, `~` expansion);
  the `write` and `ls` tools; and skill prompt formatting (XML block and
  compressed by-root listing, XML escaping, `disableModelInvocation` filter) —
  all ported and parity-tested, including CJK and emoji content. `formatSize`
  reproduces JS `toFixed(1)` round-half-up via integer arithmetic; `write`
  reproduces the JS UTF-16 `content.length` count. `ls` sorting and skill
  root/name sorting approximate node's ICU/UTF-16 collation with byte order
  (exact for ASCII; punctuation/non-ASCII may differ). OS error *messages* are
  not parity-checked (libuv vs `io::Error`); error presence is.
- M3 (in progress): agent runtime port. Message construction
  (`createErrorToolResult`, `createToolResultMessage`), the `runLoop` state
  machine (`runAgentLoop`/`runLoop`: two-level steering/follow-up loop,
  sequential tool scheduling, event emission), streaming deltas
  (`streamAssistantResponse`: `message_start`/`message_update`/`message_end`
  with the accumulating partial), and the stateful `Agent` wrapper
  (`PendingMessageQueue` drain semantics, `normalizePromptInput`, state
  reduction, `prompt`/`steer`/`continue`/`subscribe`) are ported and verified
  end-to-end against node's real `runAgentLoop` and `Agent` driven by scripted
  mock LLMs and mock tools (plain text, single/double tool calls, unknown tool,
  error stop, streaming text, and steering injection). The LLM boundary is
  abstracted (`StreamFn`/`ToolFn`) and timestamps are injected, since node uses
  non-deterministic `Date.now()`. Parallel tool execution and the LLM provider
  transport land next.
- M4: wire into the Vega path and remove the node bridge runtime dependency.
