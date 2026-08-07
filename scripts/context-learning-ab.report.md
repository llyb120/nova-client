# Fast Context 在线增量学习 A/B 报告

- 模型：`opencode/deepseek-v4-flash`
- 分支：`feat/fast-context-online-learning-ab`
- 执行方式：6 个 DeepSeek 会话并行（每个 case 的 A/B 同时启动）
- 并行评测端到端实际耗时：45006ms（汇总 wallMs 是各会话耗时之和）
- 训练回放：23 个最近真实会话，19 次 fast_context，19 个 edit 正反馈
- 训练后模型：observations=369, positives=19

## 汇总

| 指标 | A 关闭学习 | B 在线模型 | B-A |
|---|---:|---:|---:|
| wallMs | 113422 | 108876 | -4546 |
| inputTokens | 17087 | 15264 | -1823 |
| outputTokens | 9421 | 9283 | -138 |
| cacheReadTokens | 15104 | 15104 | 0 |
| totalTokens | 41612 | 39651 | -1961 |
| toolCalls | 3 | 3 | 0 |
| fastContextCalls | 3 | 3 | 0 |
| readCalls | 0 | 0 | 0 |
| bashCalls | 0 | 0 | 0 |
| toolTimeMs | 992 | 1154 | 162 |

## 结论与解读

- 总 token：-4.7%（41612 → 39651）。
- 输入 token：-10.7%；输出 token：-1.5%。
- 各会话 wall time 求和：-4.0%；6 路并行端到端为 45006ms。
- 工具调用数保持 3 → 3，read 保持 0 → 0；本组聚焦回放主要验证上下文内容/排序，而不是工具调用策略。
- fast_context 自身工具耗时增加 162ms（+16.3%），绝对值仅 1154ms，主要总耗时仍来自模型推理。
- 三个 case 的 token 均下降，但这是每个 arm 单次采样；DeepSeek 输出有随机性，因此当前结果可作为正向信号，不能视为统计显著结论。上线前建议固定模型参数后至少重复 5 轮。

## 分案例

### R1（来源：`6097d0a6-916d-48ad-ac16-b68c320116d6.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 fast_context_run、co_changed_files，分析还能如何提高 fast_context 效率并减少后续 read。只基于工具输出回答，不再调用其它检索工具，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 11780 | 2776 | 4012 | 1 | 1 | 0 | 346ms | 44994ms | fast_context |
| B | 10699 | 2745 | 2962 | 1 | 1 | 0 | 603ms | 36818ms | fast_context |

### R2（来源：`6097d0a6-916d-48ad-ac16-b68c320116d6.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 native_edit、edit、fast_context，分析原生 edit 使用独立工具实现时如何消费 fast_context 编辑锚点。只基于工具输出回答，不再检索，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 12921 | 5721 | 2208 | 1 | 1 | 0 | 383ms | 33897ms | fast_context |
| B | 12431 | 4149 | 3162 | 1 | 1 | 0 | 172ms | 35029ms | fast_context |

### R3（来源：`09774430-c287-4f78-bfc0-bfc9116cd4e7.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 buildAlkaidSystemPrompt、fast_context，分析为什么 Agent 开始会用 fast_context、后续却可能不再使用。只基于工具输出回答，不再检索，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 16911 | 8590 | 3201 | 1 | 1 | 0 | 263ms | 34531ms | fast_context |
| B | 16521 | 8370 | 3159 | 1 | 1 | 0 | 379ms | 37029ms | fast_context |

## 说明

- A 与 B 使用完全相同的代码、提示词、DeepSeek 模型和并发时刻；唯一差异是 `NOVA_CONTEXT_LEARNING=0/1`。
- B 在评测前按原始工具调用顺序回放最近真实会话中的 `fast_context → edit`，每个 edit 自动触发一次增量 Logistic 更新。
- token 来自 provider usage；实际耗时为子进程端到端 wall time；工具耗时来自 tool_start/tool_end。
- 模型仅重排可选候选，显式文件、seed、required 单元和预算硬约束不受学习模型控制。
