# Operator: inherited-model interactive decisions

Operator adds the `operate` tool without another agent/model selector or setting.
The existing running session registers an inference callback. A task snapshots
that session's agent, selected model, applicable reasoning selection and trusted
runtime environment. The tool schema cannot override the model, agent, credentials
or scope. Missing/unsupported bindings fail explicitly; there is no fallback to
Lyra, a title model or a cheaper model.

## Integration status

| Parent | Implementation | Important limits |
| --- | --- | --- |
| CodeBuddy ACP | Fresh isolated ACP process/session per decision, same executable/environment/model; empty built-in tools and strict empty MCP configuration | Requires a CLI that supports the isolation flags and confirms model/effort via `session/set_config_option`. No authenticated live CodeBuddy test was available during development. |
| Lyra | Direct inference using the parent's resolved provider/model/http client/effort | No Lyra coding loop, Reasonix state or historical image loader is used. Screenshot tasks require image support. |
| Cursor SDK | Dedicated tool-less decision worker in the existing bundled bridge | Exact selection inherited; unresolved Auto is rejected. No authenticated live SDK test was available. |
| Codex, Devin, Kimi and arbitrary ACP | Not implemented as executors in this change | No silent fallback. The generic runtime/protocol is reusable, but these backends still need an adapter and validation. |

This is deliberately an initial implementation, not a claim that every agent is
already supported. No desktop automation service is installed or configured by it.
The existing Nova Chrome connection and desktop capture prerequisites still apply.

The first version starts **a fresh inference context per decision**, not a process
per click in the parent. CodeBuddy currently also starts a process per decision.
That precisely prevents old observations entering subsequent child requests, but
process startup and extra model work may outweigh the input savings on short tasks.
A later worker pool must preserve empty-context and exact-model guarantees.

## Calling the tool

```json
{
  "op": "run",
  "requestKey": "login-verification-001",
  "channel": "chrome",
  "target": {"tabTag": "login-test"},
  "goal": "Check the already-open test login page without submitting real credentials",
  "constraints": ["Do not modify source files", "Do not submit or send"],
  "acceptance": ["Inspect current controls and report visible validation messages"]
}
```

A task returns `completed`, `yielded`, `blocked`, `needs_review` or `cancelled`,
compact business results, inherited model metadata, evidence references and
unverified actions. It does not return raw screenshots or full DOM to the parent.
`run` with the same key retrieves the previous task; it never restarts it. A
changed contract with the same key is rejected. Use `resume` for `yielded` only.
Other operations are `status`, `result`, and `cancel`, each with `taskId`.
Changing the parent's model cannot silently resume an existing task under another
model. The user's explicit choice applies to a newly started task.

Only supported, active, non-plan parent sessions expose the tool. Existing raw
Chrome/Jianlai tools are retained for compatibility and explicit single-step use.
While Operator owns the interactive lease, competing raw calls are rejected.
The lease is conservatively global (no parallel independent-tab optimization yet).

## Context and execution boundaries

`src-tauri/src/operator/core.rs` holds the task contract, bounded checkpoint,
recent action ledger and latest observation. Every decision receives the contract,
checkpoint, last three actions, current observation and native tool schema. The
runtime independently loads images from the **current observation group only**.
Historical screenshots are archived rather than re-injected. Large checkpoints,
observations or results fail with an explicit scope/budget error instead of
silently dropping business facts.

The current phase is capped at six decisions / approximately 90 seconds; the
whole task at 120 decisions. Model requests are cancellation-aware. A dispatched
native call is allowed to settle under its own tool bounds before lease release;
cancellation is not a rollback. Parent cancellation or loss of the parent binding
prevents subsequent actions.

Actions require a current evidence ID and native snapshot ID, preserving the
existing native snapshot/visual safeguards. This version accepts observation
operations and snapshot-guarded native `act` only. In particular it does not
implement Chrome `open`/`goto`/tab lifecycle dispatch: start with an existing target.
A refreshed observation is not a transaction lock on a page that changes externally.

An action is journaled before dispatch. `executed` means input sent, not business
success. Partial execution, uncertain outcomes and interrupted dispatched actions
require review; neither a retry nor `resume` automatically repeats them. Duplicate
request protection is not an exactly-once guarantee for real GUI side effects.
Completion needs current evidence and is labelled **agent observed**, not a backend
verification or an independent proof of task success.

The decision model marks potentially irreversible actions `requiresConfirmation`.
These stop before dispatch. **Risk identification is model-dependent**, not a
semantic sandbox guaranteed to recognize every send/delete/payment button.
The first version has no automatic approval or approval-resume operation; review
and take over manually. Do not use this as an unattended high-stakes executor.

Tasks are stored separately under `<profile>/operator/tasks`; raw evidence is
stored under `<profile>/operator/evidence`. Task IDs are bound to the parent scope
and canonical workspace. Nothing is written to Reasonix slim-memory files.
Evidence may contain sensitive page content; it is local and not sent to the
parent by default. Unix evidence files are created owner-only. A retention/cleanup
UI is not included in this change. Cancellation does not delete audit evidence.

## Reasonix compatibility

`src-tauri/src/lyra/reasonix.rs` is untouched. Cursor Reasonix/Super scripts only
receive the private scope in tool creation and scope-aware prewarm identity. Their
compaction, summaries, storage and restore algorithms are unchanged. Without a
scope, existing identity values stay identical. `operator.test.mjs` removes only
those explicitly reviewed routing additions and compares the complete files to
baseline SHA-256 hashes. This is a code-path regression check, not a claim of
identical model responses or unchanged cache keys when adding a new tool.

## Reproducible validation

```bash
node --test scripts/operator.test.mjs scripts/nova-tools-mcp.test.mjs scripts/ctx-core.test.mjs
cargo test --manifest-path bench/operator-harness/Cargo.toml -- --test-threads=1
cargo run --quiet --manifest-path bench/operator-harness/Cargo.toml
node scripts/operator-live-ab.mjs --self-test --out operator-live-self-test.json
```

The standalone Rust harness imports the **production core and runtime**, not
copies. Native tool execution and model calls are fixture callbacks. Tests cover
ownership, current evidence, duplicate requests, persistence, partial execution,
cancellation, model change, current-group image loading and resource interleaving.
The Node tests cover MCP routing, read-only/unbound sessions, scope isolation,
Cursor worker options/cleanup and Reasonix baseline preservation.

The initial implementation (`b3170003ebf219f2e10b36cdeac3feab657a01b3`) passed
26 Node tests, 13 Rust tests, SDK bridge build, TypeScript checking and Linux
`cargo check --lib` in Actions run 35438448440. Additional hardening tests are in
this change; their current results are in the branch's CI artifacts.
Windows/macOS builds and real desktop end-to-end behavior are not established by
that Linux compile. No credentialed real-model or real-GUI run was available.

## A/B findings: input footprint only

`footprint-ab.json` is the output of the production-projector fixture benchmark
from the initial tested implementation. A is **append-only fixture observations**,
not an instrumented, unmodified CodeBuddy. B is Operator's current-observation
projector. Both use the same fixtures, schema and checkpoint. Text means cumulative
UTF-8 serialized input bytes, not tokens. Image counts are cumulative references
in requests, not numbers of distinct screenshots or billed image tokens.

| Decisions | A text bytes | B text bytes | Text reduction | A image references | B image references |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 10 | 609441 | 219922 | 63.91% | 55 | 10 |
| 30 | 4425851 | 659882 | 85.09% | 465 | 30 |
| 60 | 16644737 | 1319843 | 92.07% | 1830 | 60 |
| 120 | 64462561 | 2639908 | 95.90% | 7260 | 120 |

**Model calls: 0. GUI actions: 0.** This establishes that the constructed working
context stops growing with the full screenshot history. It establishes neither
speedup, cost reduction, success rate nor reduced stale-target mistakes by a model.
Real providers may cache or compress the baseline differently.

## Opt-in live-model A/B entry point

```bash
node scripts/operator-live-ab.mjs --codebuddy /path/to/codebuddy --model CURRENT_PARENT_MODEL_ID --repeats 2 --out operator-live-ab.json
```

On Windows prefer the installed CodeBuddy JS entry path when the launcher is a
`.cmd` shim. `--effort` can match the active parent's explicit effort setting.
These are **standalone benchmark arguments**, not Operator settings: outside a
running Nova session the script has no access to the parent's model binding.
The application itself always inherits its current binding automatically.

This command uses the user's existing CLI login and makes potentially billable
calls. It verifies model selection, refuses tools and rejects fallback. Six
synthetic DOM cases include stale refs and an absent current-page target. The
order alternates A/B and B/A across repeats, each call uses a fresh isolated
session, and the production Rust projector generates B. Reports retain every
failed trial, measured wall time (including subprocess startup), and token usage
only if the backend actually reports it. `--self-test` only validates harness
plumbing with deterministic expected decisions; it is labelled as such.

Even the real-model mode is a **synthetic DOM decision test**, not the full GUI
loop. A proper desktop benchmark must additionally reset the application between
paired trials, test image interpretation, network delays, focus changes, recovery,
side effects and all failures. Do not turn fixture percentages into product speed
or accuracy claims.
