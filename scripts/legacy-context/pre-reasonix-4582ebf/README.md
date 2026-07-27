# Pre-Reasonix context implementation backup

This directory is a source snapshot of the Vega and Cursor context mechanisms
before the Reasonix-style session/context refactor.

Source revision: `4582ebf` (`refactor(v2): replace websocket transport with HTTP and SSE`)

Archived files:

- `alkaid-bridge.mjs`: previous Vega bridge integration and context lifecycle.
- `alkaid-slim-memory.mjs`: previous Vega full/slim memory, rolling summary, and turn compaction implementation.
- `cursor-bridge.mjs`: previous Cursor fresh-Agent, prewarm, checkpoint/session seeding, pending-turn, and compact-memory implementation.
- `SHA256SUMS`: checksums for verifying that the snapshots remain unchanged.

These files are archival only. They are intentionally not imported by production
code or included in bridge builds.

To inspect a change against the active implementation:

```bash
diff -u scripts/legacy-context/pre-reasonix-4582ebf/alkaid-slim-memory.mjs scripts/alkaid-slim-memory.mjs
diff -u scripts/legacy-context/pre-reasonix-4582ebf/cursor-bridge.mjs scripts/cursor-bridge.mjs
```

To verify the backup:

```bash
cd scripts/legacy-context/pre-reasonix-4582ebf
sha256sum -c SHA256SUMS
```

Do not delete this snapshot when replacing or simplifying the active context
implementation. Restore or selectively port code from it instead of depending
on Cursor checkpoint BLOBs.
