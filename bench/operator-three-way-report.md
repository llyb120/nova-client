# Operator three-way replay benchmark

Deterministic structural replay over six representative UI traces. It measures orchestration cost, not live-model accuracy or real desktop wall time.

| strategy | model rounds | tool calls | accumulated context chars | peak context chars | modeled latency | unresolved side-effect replays |
|---|---:|---:|---:|---:|---:|---:|
| legacy-main | 40 | 41 | 1922700 | 61500 | 45480 ms | 1 |
| isolated-step | 40 | 41 | 549700 | 26300 | 45480 ms | 1 |
| operator | 19 | 40 | 181100 | 14500 | 27350 ms | 0 |

- Isolation only vs legacy: model rounds 0.0%, accumulated context -71.4%, peak context -57.2%.
- Optimized Operator vs legacy: model rounds -52.5%, tool calls -2.4%, accumulated context -90.6%, peak context -76.4%, modeled latency -39.9%.
- The optimized replay does not repeat an unresolved side-effect action; it requires outcome verification first.

## Interpretation

The second variant proves that context isolation alone cuts prompt volume but does not remove the step-by-step decision loop. The third variant adds bounded task-level execution, deterministic compaction of superseded observations, optional experience lookup, and free chrome/jianlai switching. That is where model-round reduction comes from.

The latency column uses the same fixed replay assumptions for all three strategies (850 ms/model round, 280 ms/tool call). It is therefore a structural estimate, not a claim about real desktop wall time. Real end-to-end latency and success rate require an interactive Windows/Chrome test environment.
