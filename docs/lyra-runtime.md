# Lyra 运行时：计数、续接、命令、子任务与 MCP

## 历史回放

对齐 Codex 的普通请求路径：新用户轮次只追加历史，不主动剥离已完成轮次的 reasoning、推理签名或原生工具调用 ID，避免改写已发送前缀。缺失工具结果仍补中断占位，孤儿结果仍清理；ACP/legacy 显式折叠及旧浏览器观察裁剪规则不变。最终的协议序列化仍由 provider 负责（例如 `store:false` 不回传未存储的 item `id`，但保留加密 reasoning 状态和 `call_id`）。

## ACP 决策保真

只在模型主动 `compress` 或现有应急/legacy 摘要路径中提纯，不逐个 thinking 块追加改写调用，也不改变触发阈值。摘要及多层归并需保留有来源的决定与理由、否决方案及原因/重试条件、未决风险、验证范围和下一步定位；没有的类别不补写。计划、尝试和助手自述不能升级成工具已确认的完成结果。

独立摘要请求现在也读取非 redacted 的可见 `thinking` 文本（含 provider 已提供的可见推理摘要），单列为推敲而非执行证据；同一转写也用于原文搜索和召回。它不解析或发送 `thinkingSignature`、加密载荷或 `reasoningDetails`，不重建不可见推理。可见推理会交给已配置的摘要模型，沿用已有输入预算、缩短和失败降级路径；未折叠区间、规范历史及原生签名保持不变。

这些是输入补全和摘要指引，不是语义无损保证。超预算省略或摘要失败仍可能丢失细节，原文继续保留以便搜索/恢复；实际折叠仍可能改变缓存前缀。

## Token 预算

`context_budget` 返回 JSON 分项：system、tools、messagesText、toolArguments、toolResults、reasoning、images、other、overhead，以及校准差额、输出预留、协议余量和上次服务端 usage。

- 以服务端实际 usage 为准：上次请求实测的 prompt tokens（缓存输入不重复累加）作为锚点，当前视图仍以该次已发送消息开头时，只对之后新增的消息做本地估算；折叠或回滚改写前缀后回退为校准后的整体估算（`anchoredToActualUsage` 标明）。
- 本地估算为字符启发式（ASCII 约 4 字符/token，其它字符约 1 token），不内置 BPE 词表；`options.tokenizer` 已废弃并被忽略（仍不外发）。图片、协议封装与未知内容为占位估算，`promptExact:false`。
- 请求预算与折叠使用规范化后的消息视图，输出预留取模型配置（封顶窗口 25%）。固定提示已耗尽窗口时报告 `inputBudgetExhausted`，需缩短固定提示，不反复无效自动折叠。
- JSON BPE 计数不是服务端模型输入序列化的精确计数；图片、音频等载荷不能按 base64 长度当作 token（图片统一按每张 1600 估算）。
- 输出预留取 `min(max_output_tokens, 窗口×25%)`，输出上限接近窗口时自动折叠仍可用；provider 报溢出时的恢复折叠不受本地预算限制。
- 校准系数只随计数/请求规范化口径变化作废，系统提示变化（日期、cwd、技能）不清空校准。
- 计数按消息内容缓存，历史只追加时每次往返只对新消息做 BPE；出站 max_tokens 预算对未确认词表的模型（如 Claude）额外乘 1.25 保守系数。

## 项目记忆

高级设置里的「项目记忆」默认关闭。开启后，仅当模型声明的上下文窗口 ≥ 1M，才按 git 仓库把可用窗口（扣除输出预留）对半：前半段是按项目划分的稳定前缀（同一远程仓库即使在不同文件夹也视为同一项目），后半段是工作区。压缩、ACP 提示和应急折叠都只看见工作区，比例按工作区换算。ACP 下 45% 是建议压缩已完成工作的起点，不是一到点就折叠；大约 92% 才会兜底截断。前缀用简练白话，分节为索引、有效经验、失败经验、事实、代码；索引按路径排列，只给少数重点或高频文件加简短标注；用法说明冻在系统提示里，不写进记忆正文。每个项目只存一份结构化记忆（索引、经验、事实、代码）。新会话默认复用已发布的冻结区正文，不会每次都重新扫仓库。系统提示或工具定义若因产品更新而变化，只换组装后的缓存键，记忆正文仍可继续用。同一会话里前缀在开始时固定；读文件等命中先记在本会话里。一轮主会话正常结束后，后台用轻量模型跑一次整理提示（用户看不见这次整理，Butterfly 也不提示成败），会话列表上会出现短暂的做梦标记，整理完即消失。整理结果先追加为浅层记忆，跟在冻结区后面；用到大约三次才并入冻结区。浅层变化不改冻结区的 `prompt_cache_key`。关闭、窗口不足 1M、或当前目录不是 git 仓库时，行为与现在完全相同。快照落在数据根下的 `project-memory/`。只对新开始的会话生效；分身继承父会话的同一份前缀，不把前缀再写入分身历史。

## Responses 原生续接

在支持 stored Responses 的 API provider/model `options` 中明确启用：

```json
{
  "responsesContinuation": "auto",
  "store": true
}
```

`responsesContinuation:"auto"` 即启用开关；`store:true` 是协议要求（未存储的响应无法被引用）。这是显式选择服务端存储；默认不替用户改变原有 `store:false` 行为。首次发送完整输入，成功后保存 response ID；相同会话、模型、认证、提示、工具定义及历史前缀一致时，后续只发送新增 input 并带 `previous_response_id`。ID 过期或被拒绝时，只有尚未生成输出才允许一次完整重发，不重放已执行工具。

- 状态是有界的进程内缓存（同一会话按配置作用域最多保留 4 个续接点，LRU 淘汰，切换思考档位等交替请求互不冲掉），重启、过期或驱逐后安全回退完整输入。
- `store:false`（默认及 Codex 订阅）重放历史时不带 item `id`（上游未保存会报 not found），仅保留 `encrypted_content` 与 `call_id`。
- 折叠、历史规范化变化、切换模型/认证/工具等会使前缀或作用域失配，回退完整输入；不是无条件增量。
- 取消、错误及不完整流不能提交有效续接点。
- 手写 `previous_response_id` 仍独立透传，不参加自动管理。
- Codex 订阅的 `store:false` 路径不启用此 HTTP 自动机制；本实现不新增模型 WebSocket 传输。
- `tokenizer`、`responsesContinuation` 属于客户端选项，不发送给上游；已废弃的 `supportsPreviousResponseId` 被忽略（仍不外发）。

## 持久命令会话

一次性命令仍用 `bash`，默认 120 秒、最大 600 秒不变；超过 10 秒仍未结束的命令会转后台并返回 `jobId`，用 `bash_poll` 继续取输出与退出码。后台仍受原 `timeout` 总时长限制（到点强杀），记录在超时后保留 60 秒，job 按会话隔离；会话取消会一并杀掉已转后台的命令树。

`exec_command` 启动长任务或交互命令，`yield_time_ms` 仅控制本次等待（默认 1000、最大 10000 毫秒）。命令总 `timeout` 默认 3600、最大 86400 秒。拿到 `session_id` 后用 `write_stdin` 轮询、输入、发送 EOF 或取消，不要重新启动同一命令。

- pipe 模式 stdin 保持打开，stdout/stderr 分开。`chars` 是原样输入，不自动换行；`input_queued_bytes` 只表示已入队，不代表程序已读取。
- Unix 可用 `tty:true`；Windows 目前仅支持可取消的 pipe，TTY 会明确拒绝。
- session ID 仅在当前进程中有效，按主/分身 独立隔离，不等于可跨应用重启恢复的进程。
- 每 agent 最多 4 个、全局最多 32 个命令；每会话输出最多缓存 256 KiB，溢出丢弃旧内容并报告 `dropped_bytes`。输出分段消费，`has_more:true` 时继续轮询；完成结果最多保留 60 秒。
- 正常一轮对话结束不会杀掉持久命令；显式取消、分身 中断/结束会请求终止相关命令树。计划模式不能使用执行或输入工具。

## 并行分身

只在命中规则列表的模型上开放（设置页「Lyra → 分身」，存于 `subagentModelKeywords`，默认空 = 全不启用；Legend 模式即 DeepSeek Flash 的 max 档不看规则，始终带分身工具，同档的 Epic 不再附带分身）：每行一条规则，`*` 为通配，同一行多个词（空格分隔）必须同时命中，不同行任一命中即可。开启后 system prompt 的可用工具清单、2 个分身工具（`spawn_agent` / `interrupt_agent`）与 `SUBAGENT_GUIDE` 一起出现；未开启的会话三者都不出现，提示与工具集始终一致。主 agent 用 `spawn_agent` 派生进程内分身，跑到一半方向错了用 `interrupt_agent` 叫停；同时最多 6 个、深度 1，不能再派生子分身。`spawn_agent` 的 `context` 三档：省略 = 当前可见历史（默认）；`none` = 空白分身；`turn:N` = 只共享到眼前历史里第 N 条用户消息（从 1 起，含该轮及其后、下一条用户消息之前）。写错、越界或 0 退回全部可见历史，不是空白。

分身是一次性的：`spawn_agent` 只接一件任务，跑完（completed / errored / interrupted）就把结果交回父会话并从注册表移除（Stage 记录保留供回看）；要继续同一领域就带着交回的结论再 `spawn_agent`，同名可重派，同名仍在跑则拒绝并提示先 `interrupt_agent`。父 turn 在无工具出口按完成先后交付已结束分身的结果（不必等最慢的那个，也不被仍在跑的后台命令挡住）；仍在跑的分身保持 pending，结束后再交。交付即消费，不会重复交回。用户取消本轮时不再交付旧结果。交回后若还剩可并行的活，父会话可以再开一波。

失败也要回流：provider 报错等以 errored + 错误原因（附分身最后输出）交回，父会话据此重派、自己接手或告诉用户；`run_turn` 内部 panic 也会被收口成 errored，不会留下卡在 running 的僵尸让父 turn 永远等。进程内分身的取消靠共享的 `cancelled` 标志，没有队列、steer 或交付序号，所以不存在继续已结束分身的入口。

为了命中前缀缓存，分身会话的 system prompt、工具定义与父会话**完全相同**（所以分身也带着那 2 个分身工具，但 `agents` 为空不能调用；提示在可变段说明「你是分身、不能派分身」，不动物理缓存前缀），并用 `cache_key` 锁父会话的缓存路由键（OpenAI 系协议的 `prompt_cache_key`），继承的 `context` 前缀能直接复用父会话已经写入的缓存，只有历史尾部和新任务需要重新计价。`session_id` 保持独立 uuid，所以 Responses 续接与后台命令的作用域不会和父会话互相清；归档目录也同样独立，不覆盖父会话的工具结果。执行过程投影到只读 Stage 会话（`subagent` 标记），只用于回看，不参与调度。

没有跨重启恢复：分身的会话与未交付结果都不落盘，进程退出后要重新 `spawn_agent`。

## MCP 配置与渐进式披露

Lyra 设置页有 MCP JSON 编辑区。配置保存在 `~/.nova/alkaid/config.jsonc` 顶层 `mcpServers`（自定义 Nova 数据目录时跟随数据根）：

```json
{
  "mcpServers": {
    "local-tools": {
      "enabled": true,
      "command": "node",
      "args": ["/absolute/path/to/server.mjs"],
      "env": {"API_TOKEN": "{env:MY_MCP_TOKEN}"},
      "readOnlyTools": ["lookup"],
      "startup_timeout_ms": 10000,
      "request_timeout_ms": 30000
    },
    "remote-tools": {
      "enabled": false,
      "url": "https://example.com/mcp",
      "headers": {"Authorization": "Bearer {env:MY_MCP_TOKEN}"},
      "readOnlyTools": []
    }
  }
}
```

支持标准 JSONL stdio 和 Streamable HTTP（JSON/SSE POST 响应），协商 MCP 2025-06-18（stdio 兼容 2024-11-05；不支持允许批量数组的 2025-03-26）。`enabled` 必填，字段值不能为 null。stdio 的 `cwd` 必须是绝对路径；省略时固定为会话创建时的初始目录，不随 change_working_directory 变化。HTTP 不与 command/args/env/cwd（即使为空）混用；`http://` 仅限 localhost/127.x/[::1]，其它地址须用 `https://`，并跟随系统代理。不实现旧 GET SSE 入口、OAuth、resources、prompts、sampling 或 elicitation。保存只做结构校验（前端校验与后端一致，以后端为准）；启用的环境引用在会话初始化时解析，某个服务解析失败（如缺少 `{env:X}`）只让该服务不可用，`list` 返回原因，会话照常启动。连接和启动程序在实际发现/调用时进行。

模型只看一个固定 `mcp` wrapper，不预装服务器的全量工具 schema：

1. `{"op":"list"}` 一次列出全部已启用服务（不分页），`unavailable` 给出不可用服务及原因。
2. `{"op":"list","server":"local-tools","query":"lookup","limit":20}` 按关键词发现工具，返回有界名称/简短描述，不返回 schema。按 `nextCursor` 继续；扫描受限时明确标识未扫描范围。
3. `{"op":"describe","server":"local-tools","tool":"lookup"}` 只获取所需工具 schema。
4. `{"op":"call","server":"local-tools","tool":"lookup","arguments":{}}` 调用；必须在当前 MCP 实例中成功 describe，调用前复核定义。

list/describe 输出上限 16 KiB。每次搜索最多扫描 32 页并返回继续游标；每连接最多保留 64 个已描述定义，每个最多 16 KiB。成功、失败结果均先脱敏（只脱敏实际由 `{env:}` 展开的值及展开结果，忽略短于 8 字节的值），再经过现有工具输出治理；历史累计体积仍由上下文折叠管理。大 schema 明确拒绝，不截成不可执行的假 schema。

**信任边界：** 启用 stdio 服务意味着授权执行该程序，不是 OS 沙箱。只启用可信服务；计划模式调用需本地 `readOnlyTools` 明确逐工具授权，不依赖服务端 annotations 自报只读。服务返回内容不是指令。HTTP 不跟随重定向携带凭据；超时、取消、断线、协议错误会丢弃该服务连接与 describe 授权，普通 JSON-RPC 错误保留；均不自动重放调用，不能保证撤销远端已经发生的副作用。MCP 连接/describe 授权不跨主会话轮次、分身 或工作目录变化共享，旧历史 schema 不恢复调用授权。
