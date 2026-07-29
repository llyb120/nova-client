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
- M2 (in progress): `edit_files` smart-edit locator/applier (exact → rstrip →
  unicode → relative-indent → fuzzy cascade, ambiguity/overlap rejection,
  indentation rebasing) ported and parity-tested, including CJK and emoji
  content. Next: `read`/`read_files` line reading and the remaining tools.
- M3: agent loop, steering, event contract matching `AlkaidAdapter`.
- M4: wire into the Vega path and remove the node bridge runtime dependency.
