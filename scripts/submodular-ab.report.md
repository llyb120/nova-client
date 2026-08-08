# 次模打包（Submodular + Lazy Greedy）A/B 报告

- 模型：`opencode/deepseek-v4-flash`
- A 臂（对照）：`NOVA_CTX_SUBMODULAR=0` — 粗粒度特征 + O(n²) 朴素贪心
- B 臂（实验）：`NOVA_CTX_SUBMODULAR=1` — (term × file) 细化特征 + Lazy Greedy
- 并行耗时：72.0s

## 总计

| 指标 | A（旧） | B（次模） | Δ | Δ% |
|---|---:|---:|---:|---:|
| 总 token | 160563 | 166136 | +5573 | 3.5% |
| 输入 token | 73219 | 69825 | -3394 | -4.6% |
| 输出 token | 27056 | 27319 | +263 | 1.0% |
| 工具调用 | 12 | 13 | +1 | 8.3% |
| fast_context | 12 | 13 | +1 | 8.3% |
| read | 0 | 0 | 0 | n/a |
| bash | 0 | 0 | 0 | n/a |
| 工具耗时(ms) | 114110 | 113305 | -805 | -0.7% |
| 端到端(ms) | 599367 | 602114 | +2747 | 0.5% |

## 分案例

### S1

用且只用一次 fast_context，keywords 取 fast_context_run、stem_rows、planned_terms，task 描述分析 fast_context 的检索归因与打包流程。只基于工具输出回答：哪些文件承担了归因、哪些承担了打包？不要再调用其它检索工具，不改代码。

| Arm | Tokens | 工具调用 | fast_context | read | 工具耗时 | 端到端 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---|
| A | 12037 | 1 | 1 | 0 | 9967ms | 56320ms | fast_context |
| B | 12077 | 1 | 1 | 0 | 9842ms | 58452ms | fast_context |

**A 末尾**：打包**：`context.rs` 的 `merge_ranges`/`shown_ranges`（行段裁剪）与 `cursor-context-reasonix.mjs` 的 `formatSlimMemory`/`formatTurnTrace` 等（提示文本组装）。 - `nova-napi-tools.test.mjs`（FULL 展示）是测试文件，承担验证而非归因/打包。  局限：SIG

**B 末尾**：御书房-*.md`、`scripts/alkaid-context-*.mjs`、`scripts/alkaid-core.mjs` 等 59 个文件未展开、无正文，故不计入上述结论。  **一句话结论**：归因由 `context.rs`（评分/过滤/词边界 + `fast_context_run` 入口）和 `submodular-ab.eval.mjs`（归因评估）承担；打包由 `conte

### S2

用且只用一次 fast_context，keywords 取 fast_context、find_symbols、callNapiTool，task 描述理解 MCP 工具从 JS 定义到 Rust 实现的完整链路。只基于工具输出回答，列出链路各环节所在文件。不要再检索，不改代码。

| Arm | Tokens | 工具调用 | fast_context | read | 工具耗时 | 端到端 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---|
| A | 14076 | 1 | 1 | 0 | 11029ms | 45885ms | fast_context |
| B | 13743 | 1 | 1 | 0 | 10183ms | 46523ms | fast_context |

**A 末尾**：)              │      └─ src-tauri/src/nova_tools_native/context.rs (Rust 实现)              └─ (可选) nova-context-client.mjs (callGlobalContextTool 外部服务路径) ```  **注意**：Rust 侧（`native/nova-tools-napi/src

**B 末尾**：src/nova_tools_native/context.rs` 的正文也未展示——两者位置均由引用关系与头部注释（ctx-core.mjs 第 1-2 行）确证。 - 佐证闭环：`scripts/audit-native-context.mjs`（:75-93）与 `scripts/audit-native-context-random.mjs`（:141-178）通过 `callNapiTo

### S3

用且只用一次 fast_context，keywords 取 edit、applySmartEdits、candidate_starts，task 描述分析智能编辑的锚点定位策略。只基于工具输出回答定位顺序。不要再检索，不改代码。

| Arm | Tokens | 工具调用 | fast_context | read | 工具耗时 | 端到端 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---|
| A | 10719 | 1 | 1 | 0 | 9696ms | 40504ms | fast_context |
| B | 11850 | 1 | 1 | 0 | 10381ms | 42562ms | fast_context |

**A 末尾**：0_000) 的行；按 `(出现次数升序, 行长降序)` 排序（越罕见、越长越优先），截取前 4 个锚点。 4. 投票：每个锚点按 offset 反推候选起始位 `start = position - offset`（要求 `start + pattern.len() <= target.len()`）计票。 5. 最终候选行按 `(票数降序, start 升序)` 排序，截取前 20_000 个

**B 末尾**：date_starts(&target_lines, &pattern_lines, "rstrip")` → `starts_candidates`，即回退用 rstrip 模式再取候选。  即整体分两级候选：先 `relative-anchor` 模式，后 `rstrip` 模式。注意 `apply_smart_edits` 本身只展示了签名（edit.rs:639-640），其函数体内两级候

### S4

用且只用一次 fast_context，keywords 取 search_text、search_text_scopes、discover_stems，task 描述理解 fast_context 内部两次 rg 扫描的分工。只基于工具输出回答。不要再检索，不改代码。

| Arm | Tokens | 工具调用 | fast_context | read | 工具耗时 | 端到端 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---|
| A | 12920 | 1 | 1 | 0 | 9122ms | 54097ms | fast_context |
| B | 12458 | 1 | 1 | 0 | 9381ms | 49593ms | fast_context |

**A 末尾**：关键词扫描（`2517-2518` 的转发调用就是交汇点）。  ## 关于 discover_stems 输出中它只出现在查询头（`CTX q=...discover_stems`），没有给出任何定义/引用正文；其余命中文件（README.md、docs/alkaid.md 等 +110 个）均未展开，因此基于本输出无法为其归因具体定义。  ## 闭合结果 PROOF 段确认两个方向已合流：符号关

**B 末尾**：se 扫描负责"执行检索"；`search_text` 自身同时承担全仓扫描（files 为空）与限定文件扫描两种形态。JS 镜像 `scripts/ctx-core.mjs` 的 `searchText` 佐证同一模式：files 超过 128 个时按 128 分块并发（限定路径避免命令行上限），否则单次 rg，降级链为 rg → git grep → 有界进程内扫描。  ## 关于 disco

### S5

用且只用一次 fast_context，keywords 取 idle、timeout、session，task 描述分析会话 idle 超时从配置到 Agent 行为涉及哪些模块。只基于工具输出回答。不要再检索，不改代码。

| Arm | Tokens | 工具调用 | fast_context | read | 工具耗时 | 端到端 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---|
| A | 15746 | 1 | 1 | 0 | 8145ms | 45324ms | fast_context |
| B | 15336 | 1 | 1 | 0 | 7820ms | 45611ms | fast_context |

**A 末尾**：reateAlkaidIdleTimeout` 的 timer → `AlkaidProviderIdleTimeoutError` → `runAlkaidPromptWithRetry` 的重试/收尾逻辑 → `createAlkaidAgent` 的 streamFn/session 行为**，全部集中在 `scripts/alkaid-core.mjs` 一个文件内。调整默认值只需改该文件

**B 末尾**：。  **输出中未展开的部分**（诚实说明）：`alkaid-config.mjs` 在依赖中仅展示了 `alkaidDataRoot` 签名，未展示 idle 超时相关的文件/环境变量映射；`ALKAID_PROVIDER_IDLE_TIMEOUT_*` 常量是模块内硬编码（120_000 / false），输出里没有它们来自外部配置文件或 env 的证据。此外命中列表里的 `scripts/a

### S6

用且只用一次 fast_context，keywords 取 companion_test_files、co_changed_files、plan_terms_from_bodies，task 描述理解 fast_context 打包期的伴生测试与 git 共改耦合。只基于工具输出回答。不要再检索，不改代码。

| Arm | Tokens | 工具调用 | fast_context | read | 工具耗时 | 端到端 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---|
| A | 10123 | 1 | 1 | 0 | 6729ms | 41642ms | fast_context |
| B | 9298 | 1 | 1 | 0 | 7373ms | 35872ms | fast_context |

**A 末尾**：包但计划相关测试闭合性不足。  ## 其它 - `scripts/nova-napi-tools.test.mjs` 中的 fast_context 在线学习测试（linked worktree 复用同一内存状态与同一持久化模型文件）属于学习机制，与上述三个打包期函数不直接对应。 - 命中未展开：`scripts/ctx-index.mjs`、`scripts/context-learning-a

**B 末尾**：）  - 生产调用点全部在 `context.rs` 内：`plan_terms_from_bodies`（:3843）、`co_changed_files`（:4366）、`companion_test_files`（:4264）。 - 外部仅出现在评测脚本/报告文本（`scripts/submodular-ab.eval.mjs` 的 S6 用例、`context-learning-ab` 相

### R1

真实任��回放：先用且只用一次 fast_context，keywords 取 remote、model、switch、session，task 描述理解远程控制会话与模型切换功能，分析在远程控制会话内支持切换模型需要改哪些地方。只基于工具输出回答，不再调用其它检索工具，不改代码。

| Arm | Tokens | 工具调用 | fast_context | read | 工具耗时 | 端到端 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---|
| A | 16983 | 1 | 1 | 0 | 12477ms | 66362ms | fast_context |
| B | 13955 | 1 | 1 | 0 | 11996ms | 57689ms | fast_context |

**A 末尾**：Snapshot.models`（按当前会话 agent 取对应列表），选择后发送上述切换指令，并在确认后更新本地展示副本。  ## 输出未覆盖、无法仅凭本结果定稿的点  - `RemoteCommand` 完整变体列表与 guest→host 反向请求的具体编码（输出只展示了 host→guest 的 `commands` 字段与 `sync` 入口）。 - host 端接收指令后的线程更新/模

**B 末尾**：h 类命令（本次未展开）。 - 是否已存在"切换模型"的 Tauri command（本次只见 `prewarm`/`remove_project`）。 - cursor-bridge.test.mjs 里 `modelSelection` 的具体实现（仅出现符号名）。 - 前端调用点与 UI 侧（输出全部是 Rust 侧）。  **总体判断**：数据层（`Thread.model` + `Rem

### R2

真实任务回放：先用且只用一次 fast_context，keywords 取 kimi、moonshot、defaultConfig、providers，task 描述查找 /setup 或 alkaid 配置中为何默认出现 Kimi 和 GPT 的 provider 预设。只基于工具输出回答，不再检索，不改代码。

| Arm | Tokens | 工具调用 | fast_context | read | 工具耗时 | 端到端 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---|
| A | 10149 | 1 | 1 | 0 | 4787ms | 40019ms | fast_context |
| B | 10190 | 1 | 1 | 0 | 4624ms | 40791ms | fast_context |

**A 末尾**： `url.contains("moonshot")` 判定（URL 识别），`remote.rs:1329-1331` 是测试数据（`models: ["kimi"]`），均非预设定义。  ## 一句话回答  默认出现的 Kimi/GPT 不是代码里写死的 provider 预设：下拉选项由 OpenCode+ `provider.list()` 动态返回（默认即包含 OpenAI/GPT、Mo

**B 末尾**：里，是 fixture 而非默认配置。 - `src-tauri/src/lyra/config.rs:122-125` 只是解析配置里已有的 `provider` 段，不生成默认值。  ## 一句话总结  Kimi/GPT 默认出现是因为模型下拉直接透传 OpenCode+ 的完整 provider 列表（`providers.all`），UI 层再用 `PROVIDER_LABEL`/`gro

### R3

真实任务回放：先用且只用一次 fast_context，keywords 取 tool_start、usage、session_end、createAlkaidAgent、saveSession，task 描述分析会话生命周期中工具调用与 token 用量的记录链路。只基于工具输出回答，不再检索，不改代码。

| Arm | Tokens | 工具调用 | fast_context | read | 工具耗时 | 端到端 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---|
| A | 16630 | 1 | 1 | 0 | 9622ms | 56747ms | fast_context |
| B | 14308 | 1 | 1 | 0 | 8951ms | 49633ms | fast_context |

**A 末尾**： 的 stdout JSON 协议采集（工具名/参数/耗时/错误 + provider usage）；**生产层**靠 `createAlkaidAgent` 的工具包装 + `governToolResult` 按 `sessionId` 归档到 `tool-results/<sessionId>`，usage 经 `done` 上报，会话缓存键与归档目录都由同一 `sessionId` 串起，

**B 末尾**：_end/done` 协议事件 → eval 端 `runCase` 收集；真实执行与结果保存 = `createAlkaidAgent` 的 execute 包装（edit 反馈 + `governToolResult` 归档到 `tool-results/{sessionId}`）；token 统计 = `mergeAlkaidUsage` 四键累加（cursor 需先 normalize）；

### R4

真实任务回放：先用且只用一次 fast_context，keywords 取 证据链、画布、canvas、evidence，task 描述证据链画布组件的布局与渲染优化占屏空间，分析该怎么优化。只基于工具输出回答，不再检索，不改代码。

| Arm | Tokens | 工具调用 | fast_context | read | 工具耗时 | 端到端 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---|
| A | 16844 | 1 | 1 | 0 | 11553ms | 61544ms | fast_context |
| B | 12725 | 1 | 1 | 0 | 11653ms | 71725ms | fast_context |

**A 末尾**：anvas 维持现状（裁剪已完备），仅优化 `hasSpinner` 的持续渲染：可改为仅当存在活动流式组时才 requestRender，空闲时停帧。  ## 五、说明与限制  - "证据链"本体组件（纸迹/线索卡，可能对应未展开的 `docs/cluecard-paper-trail-design.md` 与 +91 个未展开命中文件）不在本次闭包内，其布局/渲染无法据本次输出分析。 - 若要

**B 末尾**：确认）。  ## 剩余风险与边界  - `layout.ts` 正文（2639L）本次未展开，布局是否为"全量计算、按需绘制"、block 是否支持按包围盒独立重绘，以及表格（`md-table` 多行 block）的重绘边界，需补读 layout.ts 才能落定建议 1 的具体实现面。 - 建议 2/3 依赖 `requestRender` 的实现细节（未展开），落地前需确认渲染队列语义。 - 

### R5

真实任务回放：先用且只用一次 fast_context，keywords 取 edit_files、executeTools、parallel tool、applyEdit，task 描述查找 pi 工具执行代码判断 edit 是否并发执行。只基于工具输出回答，不再检索，不改代码。

| Arm | Tokens | 工具调用 | fast_context | read | 工具耗时 | 端到端 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---|
| A | 12928 | 1 | 1 | 0 | 9886ms | 42791ms | fast_context |
| B | 28104 | 2 | 2 | 0 | 10191ms | 50275ms | fast_context → fast_context |

**A 末尾**：sx`**    - 前端画布渲染组件，与本问题无关。  另外注意输出契约区（CTX 头部）显示命中文件未展开列表超过 100 个，且 PROOF 段仅闭合了 3 个目标定义——`executeTools`、`applyEdit`、并发调度相关符号的具体实现**没有出现在展示范围内**。  **结论**：本次输出未提供 pi 工具 executor（`executeTools`/`applyEdi

**B 末尾**：放 harness，非 pi 运行时执行代码。  ### 判断  基于本次输出，唯一可确证的是：**edit 执行逻辑位于未展开的原生层（lib.rs）与 agent 入口（alkaid.mjs），输出不含其并发性证据**；测试与回放侧的调用均为串行 `await`，但不足以证明 pi 运行时的真实行为。要给出确定性结论，需展开 `scripts/alkaid.mjs`（executeTools）

### R6

真实任务回放：先用且只用一次 fast_context，keywords 取 fast_context、oldText、edit、virtual、anchor，task 描述分析 fast_context 与 edit 工具的锚点定位集成。只基于工具输出回答，不再检索，不改代码。

| Arm | Tokens | 工具调用 | fast_context | read | 工具耗时 | 端到端 | 序列 |
|---|---:|---:|---:|---:|---:|---:|---|
| A | 11408 | 1 | 1 | 0 | 11097ms | 48132ms | fast_context |
| B | 12092 | 1 | 1 | 0 | 10910ms | 53388ms | fast_context |

**A 末尾**：dText` 锚点定位执行后，通过 `observe_context_feedback(action="edit")` 自动回流，fast_context 在线学习该锚点/路径，跨 worktree 共享状态——即"编辑锚点→上下文模型"的闭环集成。  一句话：fast_context 负责"哪些锚点值得检索/纠错"（显式锚点校验 + 变体扩展 + 噪声排除 + 命中确认 + did-you-me

**B 末尾**：anchor` 分类 → `naming_variants` 扩变体（含 `${1}_${2}` 边界修复）→ 过滤噪声路径后按 `production_anchor_hit` 判定 → 未命中时 `anchor_correction`+`similar_enough` 产出 did-you-mean 校正；edit 以 `oldText` 为字面锚点定位，实际 edit 经 `observe_c

## 判读口径

- **核心指标**：`readCalls`（fast_context 之后是否还需要补 read）。次模覆盖多样性更好 → read 应减少。
- **次要指标**：`totalTokens` / `wallMs`。若 B 臂 read 减少但总 token 反而升高，说明打包更宽但单次信息密度下降，需权衡。
- **反指标**：若 B 臂 fast_context 调用次数增加，说明单次打包不足、模型被迫二次检索——次模反而更差。
- A 与 B 使用相同代码、提示词、模型和并发时刻；唯一差异是 `NOVA_CTX_SUBMODULAR=0/1`。