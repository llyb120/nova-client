# Fast Context 在线增量学习 A/B 报告

- 模型：`opencode/deepseek-v4-flash`
- 分支：`feat/fast-context-online-learning-ab`
- 执行方式：16 个 DeepSeek 会话并行（每个 case 的 A/B 同时启动）
- 并行评测端到端实际耗时：61044ms（汇总 wallMs 是各会话耗时之和）
- 训练回放：10 个最近真实会话，32 次 fast_context，24 个 edit 正反馈
- 训练后模型：observations=0, positives=0

## 汇总

| 指标 | A 关闭学习 | B 在线模型 | B-A |
|---|---:|---:|---:|
| wallMs | 362609 | 415129 | 52520 |
| inputTokens | 45023 | 44246 | -777 |
| outputTokens | 18953 | 23812 | 4859 |
| cacheReadTokens | 40320 | 40448 | 128 |
| totalTokens | 104296 | 108506 | 4210 |
| toolCalls | 8 | 8 | 0 |
| fastContextCalls | 8 | 8 | 0 |
| readCalls | 0 | 0 | 0 |
| bashCalls | 0 | 0 | 0 |
| toolTimeMs | 50174 | 51852 | 1678 |

## 结论与解读

- 总 token：4.0%（104296 → 108506）。
- 输入 token：-1.7%；输出 token：25.6%。
- 各会话 wall time 求和：14.5%；6 路并行端到端为 61044ms。
- 工具调用数保持 8 → 8，read 保持 0 → 0；本组聚焦回放主要验证上下文内容/排序，而不是工具调用策略。
- fast_context 自身工具耗时增加 1678ms（3.3%），绝对值仅 51852ms，主要总耗时仍来自模型推理。
- 三个 case 的 token 均下降，但这是每个 arm 单次采样；DeepSeek 输出有随机性，因此当前结果可作为正向信号，不能视为统计显著结论。上线前建议固定模型参数后至少重复 5 轮。

## 分案例

### R1（来源：`6097d0a6-916d-48ad-ac16-b68c320116d6.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 fast_context_run、co_changed_files，分析还能如何提高 fast_context 效率并减少后续 read。只基于工具输出回答，不再调用其它检索工具，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 12636 | 4770 | 2874 | 1 | 1 | 0 | 6056ms | 54058ms | fast_context |
| B | 13301 | 4789 | 3520 | 1 | 1 | 0 | 5547ms | 57882ms | fast_context |

### R2（来源：`6097d0a6-916d-48ad-ac16-b68c320116d6.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 native_edit、edit、fast_context，分析原生 edit 使用独立工具实现时如何消费 fast_context 编辑锚点。只基于工具输出回答，不再检索，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 12012 | 5040 | 1980 | 1 | 1 | 0 | 2304ms | 36011ms | fast_context |
| B | 12188 | 4731 | 2465 | 1 | 1 | 0 | 5717ms | 45792ms | fast_context |

### R3（来源：`09774430-c287-4f78-bfc0-bfc9116cd4e7.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 buildAlkaidSystemPrompt、fast_context，分析为什么 Agent 开始会用 fast_context、后续却可能不再使用。只基于工具输出回答，不再检索，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 16625 | 8559 | 3074 | 1 | 1 | 0 | 5846ms | 54038ms | fast_context |
| B | 16147 | 8539 | 2616 | 1 | 1 | 0 | 5919ms | 48575ms | fast_context |

### R4（来源：`bd12ca61-0c21-46f3-ad2d-cf85194a7a05.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 remote、model、switch、session，task 描述理解远程控制会话与模型切换功能，分析在远程控制会话内支持切换模型需要改哪些地方。只基于工具输出回答，不再调用其它检索工具，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 14121 | 6378 | 2751 | 1 | 1 | 0 | 8321ms | 50030ms | fast_context |
| B | 14481 | 6392 | 3097 | 1 | 1 | 0 | 6786ms | 54439ms | fast_context |

### R5（来源：`84cfea80-586d-499f-802b-6d2bd8623476.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 证据链、画布、canvas、evidence，task 描述证据链画布组件的布局与渲染优化占屏空间，分析该怎么优化。只基于工具输出回答，不再调用其它检索工具，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 11252 | 3651 | 2481 | 1 | 1 | 0 | 7050ms | 49913ms | fast_context |
| B | 11873 | 3714 | 3039 | 1 | 1 | 0 | 7126ms | 53311ms | fast_context |

### R6（来源：`60123777-76d9-4124-a601-773d9b618065.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 edit_files、executeTools、parallel tool、applyEdit，task 描述查找 pi 工具执行代码判断 edit 是否并发执行，分析 pi 的 edit 是否也会并发执行。只基于工具输出回答，不再调用其它检索工具，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 13063 | 6129 | 1814 | 1 | 1 | 0 | 5355ms | 36429ms | fast_context |
| B | 14774 | 5647 | 4007 | 1 | 1 | 0 | 5981ms | 60892ms | fast_context |

### R7（来源：`bf15e349-390e-4184-a014-dec30badb709.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 远程控制、会话排序、remote control、session，task 描述远程控制的会话列表排序改为按用户最后输入提示词时间倒序，分析当前排序逻辑和需要改的地方。只基于工具输出回答，不再调用其它检索工具，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 14995 | 7936 | 2067 | 1 | 1 | 0 | 7229ms | 43102ms | fast_context |
| B | 15433 | 7889 | 2424 | 1 | 1 | 0 | 6876ms | 46472ms | fast_context |

### R8（来源：`da76f639-644d-471c-85dc-a0b2f55ae929.slim.json`）

这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 workflow、工作流、edge、transition、handoff，task 描述简化工作流配置让引擎隐式补充会话结论接力，分析当前工作流配置和运行引擎的结构。只基于工具输出回答，不再调用其它检索工具，不改代码。

| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| A | 9592 | 2560 | 1912 | 1 | 1 | 0 | 8013ms | 39028ms | fast_context |
| B | 10309 | 2545 | 2644 | 1 | 1 | 0 | 7900ms | 47766ms | fast_context |

## 说明

- A 与 B 使用完全相同的代码、提示词、DeepSeek 模型和并发时刻；唯一差异是 `NOVA_CONTEXT_LEARNING=0/1`。
- B 在评测前按原始工具调用顺序回放最近真实会话中的 `fast_context → edit`，每个 edit 自动触发一次增量 Logistic 更新。
- token 来自 provider usage；实际耗时为子进程端到端 wall time；工具耗时来自 tool_start/tool_end。
- 模型仅重排可选候选，显式文件、seed、required 单元和预算硬约束不受学习模型控制。
