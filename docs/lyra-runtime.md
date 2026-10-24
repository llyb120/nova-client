# Lyra 运行时：计数、续接、命令、子任务与 MCP

## Token 预算

`context_budget` 返回 JSON 分项：system、tools、messagesText、toolArguments、toolResults、reasoning、images、other、overhead，以及校准差额、输出预留、协议余量和上次服务端 usage。

- 以服务端实际 usage 为准：上次请求实测的 prompt tokens（缓存输入不重复累加）作为锚点，当前视图仍以该次已发送消息开头时，只对之后新增的消息做本地估算；折叠或去 reasoning 改写前缀后回退为校准后的整体估算（`anchoredToActualUsage` 标明）。
- 本地估算为字符启发式（ASCII 约 4 字符/token，其它字符约 1 token），不内置 BPE 词表；`options.tokenizer` 已废弃并被忽略（仍不外发）。图片、协议封装与未知内容为占位估算，`promptExact:false`。
- 请求预算与折叠使用规范化后的消息视图，输出预留取模型配置（封顶窗口 25%）。固定提示已耗尽窗口时报告 `inputBudgetExhausted`，需缩短固定提示，不反复无效自动折叠。
- JSON BPE 计数不是服务端模型输入序列化的精确计数；图片、音频等载荷不能按 base64 长度当作 token（图片统一按每张 1600 估算）。
- 输出预留取 `min(max_output_tokens, 窗口×25%)`，输出上限接近窗口时自动折叠仍可用；provider 报溢出时的恢复折叠不受本地预算限制。
- 校准系数只随词表/去 reasoning 策略作废，系统提示变化（日期、cwd、技能）不清空校准。
- 计数按消息内容缓存，历史只追加时每次往返只对新消息做 BPE；出站 max_tokens 预算对未确认词表的模型（如 Claude）额外乘 1.25 保守系数。

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

一次性命令仍用 `bash`，默认 120 秒、最大 600 秒不变。

`exec_command` 启动长任务或交互命令，`yield_time_ms` 仅控制本次等待（默认 1000、最大 10000 毫秒）。命令总 `timeout` 默认 3600、最大 86400 秒。拿到 `session_id` 后用 `write_stdin` 轮询、输入、发送 EOF 或取消，不要重新启动同一命令。

- pipe 模式 stdin 保持打开，stdout/stderr 分开。`chars` 是原样输入，不自动换行；`input_queued_bytes` 只表示已入队，不代表程序已读取。
- Unix 可用 `tty:true`；Windows 目前仅支持可取消的 pipe，TTY 会明确拒绝。
- session ID 仅在当前进程中有效，按主/子 agent 独立隔离，不等于可跨应用重启恢复的进程。
- 每 agent 最多 4 个、全局最多 32 个命令；每会话输出最多缓存 256 KiB，溢出丢弃旧内容并报告 `dropped_bytes`。输出分段消费，`has_more:true` 时继续轮询；完成结果最多保留 60 秒。
- 正常一轮对话结束不会杀掉持久命令；显式取消、子 agent 中断/关闭会请求终止相关命令树。计划模式不能使用执行或输入工具。

## 子 agent 恢复与父任务收尾

普通子 agent 的规范历史、上下文账本及安全 manifest 分开持久化。重启后 `list_agents` 可发现，`followup_task` 明确追加任务时才恢复；旧的排队任务、未知结果工具调用不会自动重放。关闭的实例不会自动复活。恢复不包含 OS 进程或终端会话，不把模型凭据写入 manifest。

子 agent 仍最多并行 6 个、深度 1。恢复继承当前父会话配置，并与原授权取交集，不能借恢复从只读升级为可写。

`send_message` 在运行中注入、空闲时开启新轮次；`followup_task` 入队立即计入 pending，不能继续显示旧的 completed。父 turn 最终出口等待运行/排队的子任务，并将未读取结果交给主模型汇总；已读取结果不重复交付。用户取消会清空排队任务并取消父子执行；不需要的子任务应主动中断或关闭，而不是留下“排队”后宣称全部完成。

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

**信任边界：** 启用 stdio 服务意味着授权执行该程序，不是 OS 沙箱。只启用可信服务；计划模式调用需本地 `readOnlyTools` 明确逐工具授权，不依赖服务端 annotations 自报只读。服务返回内容不是指令。HTTP 不跟随重定向携带凭据；超时、取消、断线、协议错误会丢弃该服务连接与 describe 授权，普通 JSON-RPC 错误保留；均不自动重放调用，不能保证撤销远端已经发生的副作用。MCP 连接/describe 授权不跨主会话轮次、子 agent 或工作目录变化共享，旧历史 schema 不恢复调用授权。
