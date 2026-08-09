# Fast Context 在线增量学习 A/B 报告

- 模型：`opencode/deepseek-v4-flash`
- 分支：`feat/fast-context-online-learning-ab`
- 执行方式：16 个 DeepSeek 会话并行（每个 case 的 A/B 同时启动）
- 并行评测端到端实际耗时：53490ms（汇总 wallMs 是各会话耗时之和）
- 训练回放：10 个最近真实会话，32 次 fast_context，25 个 edit 正反馈
- 训练后模型：observations=0, positives=0

## 汇总

| 指标 | A 关闭学习 | B 在线模型 | B-A |
|---|---:|---:|---:|
| wallMs | 359238 | 377483 | 18245 |
| inputTokens | 45637 | 47120 | 1483 |
| outputTokens | 19605 | 21231 | 1626 |
| cacheReadTokens | 40448 | 40576 | 128 |
| totalTokens | 105690 | 108927 | 3237 |
| toolCalls | 8 | 8 | 0 |
| fastContextCalls | 8 | 8 | 0 |
| readCalls | 0 | 0 | 0 |
| bashCalls | 0 | 0 | 0 |
| toolTimeMs | 51674 | 52929 | 1255 |

## 结论与解读

- 总 token：3.1%（105690 → 108927）。
- 输入 token：3.2%；输出 token：8.3%。
- 各会话 wall time 求和：5.1%；6 路并行端到端为 53490ms。
- 工具调用数保持 8 → 8，read 保持 0 → 0；本组聚焦回放主要验证上下文内容/排序，而不是工具调用策略。
- fast_context 自身工具耗时增加 1255ms（2.4%），绝对值仅 52929ms，主要总耗时仍来自模型推理。
- 三个 case 的 token 均下降，但这是每个 arm 单次采样；DeepSeek 输出有随机性，因此当前结果可作为正向信号，不能视为统计显著结论。上线前建议固定模型参数后至少重复 5 轮。

## 分案例

### R1（来源：`6097d0a6-916d-48ad-ac16-b68c320116d6.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 fast_context_run、co_changed_files，分析还能如何提高 fast_context 效率并减少后续 read。只基于工具输出回答，不再调用其它检索工具，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 12769 | 4787 | 2990 | 1 | 1 | 0 | 5433ms | 49833ms | fast_context |
| B | 12776 | 4733 | 3051 | 1 | 1 | 0 | 4465ms | 48302ms | fast_context |

### R2（来源：`6097d0a6-916d-48ad-ac16-b68c320116d6.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 native_edit、edit、fast_context，分析原生 edit 使用独立工具实现时如何消费 fast_context 编辑锚点。只基于工具输出回答，不再检索，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 12052 | 5038 | 2022 | 1 | 1 | 0 | 4845ms | 39481ms | fast_context |
| B | 12964 | 4938 | 2906 | 1 | 1 | 0 | 4944ms | 50234ms | fast_context |

### R3（来源：`09774430-c287-4f78-bfc0-bfc9116cd4e7.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 buildAlkaidSystemPrompt、fast_context，分析为什么 Agent 开始会用 fast_context、后续却可能不再使用。只基于工具输出回答，不再检索，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 17017 | 8461 | 3436 | 1 | 1 | 0 | 5895ms | 52148ms | fast_context |
| B | 17155 | 8571 | 3464 | 1 | 1 | 0 | 6438ms | 51834ms | fast_context |

### R4（来源：`bd12ca61-0c21-46f3-ad2d-cf85194a7a05.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 remote、model、switch、session，task 描述理解远程控制会话与模型切换功能，分析在远程控制会话内支持切换模型需要改哪些地方。只基于工具输出回答，不再调用其它检索工具，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 14061 | 6377 | 2692 | 1 | 1 | 0 | 7769ms | 46229ms | fast_context |
| B | 14474 | 6383 | 3099 | 1 | 1 | 0 | 9117ms | 53388ms | fast_context |

### R5（来源：`84cfea80-586d-499f-802b-6d2bd8623476.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 证据链、画布、canvas、evidence，task 描述证据链画布组件的布局与渲染优化占屏空间，分析该怎么优化。只基于工具输出回答，不再调用其它检索工具，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 13047 | 5278 | 2777 | 1 | 1 | 0 | 6242ms | 49815ms | fast_context |
| B | 12894 | 5678 | 2224 | 1 | 1 | 0 | 6694ms | 45773ms | fast_context |

### R6（来源：`60123777-76d9-4124-a601-773d9b618065.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 edit_files、executeTools、parallel tool、applyEdit，task 描述查找 pi 工具执行代码判断 edit 是否并发执行，分析 pi 的 edit 是否也会并发执行。只基于工具输出回答，不再调用其它检索工具，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 11808 | 4956 | 1732 | 1 | 1 | 0 | 6257ms | 37327ms | fast_context |
| B | 13430 | 6569 | 1741 | 1 | 1 | 0 | 6687ms | 37617ms | fast_context |

### R7（来源：`bf15e349-390e-4184-a014-dec30badb709.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 远程控制、会话排序、remote control、session，task 描述远程控制的会话列表排序改为按用户最后输入提示词时间倒序，分析当前排序逻辑和需要改的地方。只基于工具输出回答，不再调用其它检索工具，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 15635 | 8216 | 2299 | 1 | 1 | 0 | 7177ms | 44726ms | fast_context |
| B | 15196 | 7750 | 2326 | 1 | 1 | 0 | 7311ms | 44512ms | fast_context |

### R8（来源：`da76f639-644d-471c-85dc-a0b2f55ae929.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 workflow、工作流、edge、transition、handoff，task 描述简化工作流配置让引擎隐式补充会话结论接力，分析当前工作流配置和运行引擎的结构。只基于工具输出回答，不再调用其它检索工具，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 9301 | 2524 | 1657 | 1 | 1 | 0 | 8056ms | 39679ms | fast_context |
| B | 10038 | 2498 | 2420 | 1 | 1 | 0 | 7273ms | 45823ms | fast_context |

## 说明

- A 与 B 使用完全相同的代码、提示词、DeepSeek 模型和并发时刻；唯一差异是 `NOVA_CONTEXT_LEARNING=0/1`。
- B 在评测前按原始工具调用顺序回放最近真实会话中的 `fast_context → edit`，每个 edit 自动触发一次增量 Logistic 更新。
- token 来自 provider usage；实际耗时为子进程端到端 wall time；工具耗时来自 tool_start/tool_end。
- 模型仅重排可选候选，显式文件、seed、required 单元和预算硬约束不受学习模型控制。
