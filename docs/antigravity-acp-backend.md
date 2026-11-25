# Nova 接入 Google Antigravity CLI（ACP）后端设计文档

> **当前接入：** agy 的 `tools/list` 仅提供仓库目录概览工具 `code_map`；只有 Nova 启用「代码上下文检索 → polaris」时才列出并推荐 `polaris`（如 `mcp_nova_polaris({"task":"定位会话模型选择"})`），开关变化从下一轮生效。Polaris 与 Lyra 共用工具定义和轻量模型检索流程，只需 `task`，跨项目可传 `root`，不改变会话目录；未启用 Polaris 或未配置 Lyra 轻量模型时明确报错，不回退到关键词检索。不再向 agy 暴露 Nova 的 `jianlai` / `chrome` 工具或 Skill，也不再通过 `disabledTools` 或提示词禁用 agy 内置的 `browser_*` 系列工具。
>
> 本文其余部分保留原提交的协议设计，下文的 `chrome_*` / `jianlai_*` / `disabledTools` 属于早期基线记录。
> 范围：在 Nova 现有 `Lyra`（`src-tauri/src/lyra/`）之外并列新增 `Antigravity`（`agy acp`）后端
> 硬约束：
> 1. **不新增任何 Cargo 依赖**（不引入 `acp` crate / `rmcp`），JSON-RPC 手写，风格对齐 `src-tauri/src/lyra/devin_provider.rs` 与 `src-tauri/src/lyra/mcp.rs`。
> 2. **协议核心零 `tauri` 依赖 + 薄 Tauri 胶水**，保证本机缺少 `webkit2gtk4.1` 时核心协议、映射、MCP 桥与看门狗仍可通过独立 Rust crate 完成 `cargo check` 与 `cargo test`。
> 3. **零破坏现有 `Lyra` 路径与会话存储格式**。
> 4. 参考实现调研细节见 [/home/admin/nova-dev/acp-reference-notes.md](file:///home/admin/nova-dev/acp-reference-notes.md)。

---

## 1. 架构总览与数据流

### 1.1 进程拓扑与文字版数据流

Nova 主进程负责 UI 状态、会话落盘与本地特权工具引擎（`ContextService` / `ChromeBridge` / `Jianlai` / `Polaris MEMO`）；每个活跃的 Antigravity 会话独占一个 `agy acp` 子进程；`agy acp` 通过标准 ACP `mcpServers`（stdio）拉起 `nova` 自身提供的轻量桥接子模式 `nova --antigravity-mcp-bridge`，后者通过已有 `ContextService` 本地 IPC（Unix Socket / Windows Named Pipe）回环到 Nova 主进程执行自定义工具。

```text
+---------------------------------------------------------------------------------+
| SolidJS 前端 (src/store.ts, src/ipc.ts, src/types.ts)                           |
|   invoke: create_thread / send_prompt / cancel_turn / set_thread_model / ...    |
|   listen: acp:update / acp:turn / acp:options / acp:permission / acp:log        |
+---------------------------------------^-----------------------------------------+
                                        | Tauri IPC
+---------------------------------------v-----------------------------------------+
| Nova 主进程 (src-tauri/src/lib.rs, AppState)                                    |
|                                                                                 |
|  +-----------------------+      +--------------------------------------------+  |
|  | LyraRuntime (保持不变) |      | AntigravityRuntime (薄 Tauri 胶水)          |  |
|  +-----------------------+      |  - 持有 SessionPool (1 thread = 1 agy)      |  |
|                                 |  - 读写 ThreadStore / Settings / model_cache|  |
|                                 |  - 发射 EV_UPDATE / EV_TURN / EV_OPTIONS    |  |
|                                 +---------------------+----------------------+  |
|                                                       | Trait AcpEventSink      |
|                                 +---------------------v----------------------+  |
|                                 | antigravity::core (纯 Rust，无 tauri 依赖)  |  |
|                                 |  - AcpProcess (spawn / PGID / stderr ring) |  |
|                                 |  - AcpConnection (NDJSON JSON-RPC 2.0)     |  |
|                                 |  - PromptWatchdog (10m/60m) & QuotaTracker |  |
|                                 |  - TurnMapper (ACP update -> Nova Item/Op) |  |
|                                 +---------------------+----------------------+  |
|                                                       | stdin / stdout NDJSON   |
|  +----------------------------------------------------+----------------------+  |
|  | ContextService (src-tauri/src/context_service.rs，已常驻)                 |  |
|  |  Unix: /tmp/nova-context-{pid}.sock | Win: \\.\pipe\nova-context-{pid}    |  |
|  |  Token 鉴权 JSONL 派发:                                                    |  |
|  |   - lyra::polaris_agent::run / nova_tools_native::context::code_map |  |
|  |   - jianlai::execute + jianlai_tools::{arguments,result}  (共享 screen_lock)|  |
|  |   - native_browser::execute_chrome + chrome_tools::arguments (Chrome 扩展) |  |
|  +------------------------------------^--------------------------------------+  |
+---------------------------------------|-----------------------------------------+
                                        | 本地 IPC 回环 (JSONL + Token)
                                        |
              +-------------------------+-------------------------+
              |                                                   |
   stdin/stdout NDJSON (ACP)                             stdin/stdout NDJSON (MCP)
              |                                                   |
+-------------v-------------------------------+     +-------------+---------------+
| agy acp 子进程 (1 活跃会话 = 1 进程)          |---->| nova --antigravity-mcp-bridge|
|  agy acp                                    |spawn|  (纯 Rust stdio MCP server) |
|    --app_data_dir=antigravity-acp           |<----|  - initialize / tools/list  |
|    --dangerously-skip-permissions           |stdio|  - tools/call -> IPC 回环   |
|  内置 57 工具 (fs/shell/subagent/...)       |     |  - Schema 18k token 预算裁剪 |
|  系统提示词禁止内置 browser_*，改走 mcp_nova_chrome_* |  - 图片读盘转 MCP image 块   |
+---------------------------------------------+     |  - 15s progress 心跳保活     |
                                                    +-----------------------------+
```

### 1.2 Cargo 依赖复核与模块分层边界

经查 `src-tauri/Cargo.toml`，以下依赖全部已存在，**无需新增任何 crate**：
- 异步与进程：`tokio`（`full` features：子进程 stdio、Unix/Named Pipe IPC、channel、timeout、Mutex）
- 序列化与工具：`serde`、`serde_json`、`uuid`、`chrono`、`anyhow`、`dunce`、`base64`、`image`、`sha2`、`regex`
- 平台进程树：`libc`（Unix `setpgid`/`kill`）、`windows-sys`（Job Object / 进程快照，已在 `src-tauri/src/acp.rs` 封装）

模块划分遵循**「纯 Rust 协议核心 + 薄 Tauri 胶水」**：

| 层级 | 目录/文件 | 依赖限制 | 职责 |
|---|---|---|---|
| **协议核心** | `src-tauri/src/antigravity/core/`<br>(`protocol.rs`, `process.rs`, `connection.rs`, `watchdog.rs`, `quota.rs`, `mapper.rs`, `schema_budget.rs`, `mcp_server.rs`, `system_prompt.rs`) | **禁止 `use tauri`、`use crate::AppState`**，仅依赖 `std` + `tokio` + `serde(_json)` + `base64` + `regex` + `libc`/`windows-sys` | NDJSON 帧收发、JSON-RPC 关联、进程树启停、stderr 脱敏与 429 检测、看门狗、ACP→Nova 数据结构映射、MCP stdio server、20k token schema 压缩 |
| **薄胶水层** | `src-tauri/src/antigravity/runtime.rs`<br>`src-tauri/src/antigravity/bridge_entry.rs` | 允许 `tauri::AppHandle`、`AppState`、`ThreadStore`、`model_cache`、`acp::*`、`context_service` | `SessionPool` 管理、读写 `Thread` 与落盘、向前端发射 `EV_*` 事件、连接 `ContextService`、解析 `Settings` |

---

## 2. 事件映射表（逐字段映射）

Nova 现有事件名常量位于 `src-tauri/src/acp.rs:7-16`，核心数据结构位于 `src-tauri/src/threads.rs`（`Item` L451-502、`ToolCall` L117-133、`UpdateOp` L809-838、`LiveUsage` L103-114），前端消费位于 `src/store.ts:2544-2768` 与 `src/types.ts`。Antigravity 后端**100% 复用现有事件名与前端状态机**，前端事件监听器零改动即可渲染全部流式输出、思考块、工具卡片、计划与用量。

### 2.1 `session/update` 逐变体与逐字段映射

所有 `session/update` 通知结构为 `params: { sessionId, update: { sessionUpdate, ... } }`。当处于 `session/load` 重放窗口（`suppress_replay == true`）时，丢弃以下除 `config_option_update`/`current_mode_update` 外的历史事件。

| ACP `sessionUpdate` | ACP 载荷字段 | Nova 目标类型 & 字段（复用路径） | 触发事件 & 处理规则 |
|---|---|---|---|
| `agent_message_chunk` | `content: { type: "text", text }`（或其他 ContentBlock） | `Item::Assistant { id, text, ts }` (`threads.rs:456`)；增量用 `UpdateOp::Delta { item_id, text }` (`threads.rs:818`) | 首次块调 `thread.append_assistant_delta(text)` 返回 `(id, is_new)`；`is_new` 发 `UpdateOp::Upsert`，续块发 `UpdateOp::Delta`。若识别到 `agy` 内联 `⚠` 诊断通告（见 2.5），分流为 `Item::System { level: "warn"\|"error" }`。非 text 块格式化为可读占位文本。 |
| `agent_thought_chunk` | `content: { type: "text", text }` | `Item::Thought { id, text, ts }` (`threads.rs:458`)；增量用 `UpdateOp::Delta` | 调 `thread.append_thought_delta(text)`，与 `lyra/runtime.rs:433-460` 完全一致；切换到 assistant 或 tool 时自动封口当前 thought item。 |
| `tool_call` | `toolCallId`, `title`, `name?`, `kind?`, `status?`, `content?`, `locations?`, `rawInput?`, `rawOutput?` | `Item::Tool { id, ts, call: ToolCall }` (`threads.rs:460`, `ToolCall` L117-133) | 调 `thread.upsert_tool(call)` → `UpdateOp::Upsert { item }`。**必须规范化 MCP 前缀**（见 4.6），并将 `toolCallId` 加入 `PromptWatchdog` 活跃工具集合。 |
| `tool_call_update` | `toolCallId`（必填），其余字段均可选 | `ToolCall` 同 `tool_call` | **禁止整对象覆盖！** `TurnMapper` 按 `toolCallId` 查找已有 `ToolCall`，仅覆盖 `Some(...)` 字段（`content` 追加或按非空替换，`raw_input`/`raw_output` 经 `threads::compact_tool_value` 压缩后写入），再调 `thread.upsert_tool(merged)` → `UpdateOp::Upsert`。当 `status` 为 `completed` 或 `failed` 时，从 `PromptWatchdog` 移除该 `toolCallId`。 |
| `plan` / `plan_update` | `entries: [{ content, priority, status }]` | `Thread.plan: Option<Value>` (`threads.rs:584`)，`UpdateOp::Plan { plan }` (`threads.rs:823`) | 直接将 `json!({ "entries": entries })` 写入 `thread.plan` 并发射 `EV_UPDATE(UpdateOp::Plan)`（后台会话也放行，与 `lyra/runtime.rs:985` 一致）。 |
| `plan_removed` | `{}` | `Thread.plan = None` | 置 `thread.plan = None`，发射 `UpdateOp::Plan { plan: Value::Null }`。 |
| `usage_update` | `used: u64`, `size: u64`, `cost?: { amount, currency }` | `LiveUsage { input_tokens, context_tokens, output_tokens, cache_read_tokens, cache_write_tokens, total_tokens, context_window, ... }` (`threads.rs:103-114`) | 若 `used == 0`（建会话初始空包）直接忽略。否则记录 `last_context_used = used`、`context_window = size`，发射 `UpdateOp::Usage { usage }`，供前端上下文进度条（`src/store.ts:2592`）实时显示。 |
| `current_mode_update` | `currentModeId: String` | `Thread.mode: Option<String>` (`threads.rs:547`) | 更新内部 `active_wire_mode`。因产品决策固定非 plan 模式，仅当变化时记录日志；向前端保持归一化后的 `"build"`（不切前端到已废弃的 plan 模式）。 |
| `config_option_update` | `configOptions: [...]` | 转换为 Nova `OptionsEvent` 格式 | 刷新内存模型/思考强度映射表，更新 `model_cache::save(&config_dir, "antigravity", ...)`，发射 `acp::EV_OPTIONS`。 |
| `available_commands_update` | `availableCommands: [{ name, description, input? }]` | 内存缓存（供 `get_slash_commands`） | 过滤掉会劫持会话的内置命令（`/plan`/`/logout` 等），仅保留可安全展示的命令（Phase 3）。 |
| `session_info_update` | `title?`, `updatedAt?` | `Thread.title`（可选） | 仅当 Nova 尚未通过轻量模型生成标题且 `thread.title == "新会话"` 时可采用，否则忽略以保持与 Nova 标题机制一致。 |
| `notice` | `level?`, `code?`, `message` | `Item::System { id, text, level, ts }` (`threads.rs:466`) | 映射 `level` 到 `"info" | "warn" | "error"`，调 `thread.push_system`，发射 `UpdateOp::Upsert`。 |
| `compaction_update` / `compaction_summary_chunk` | 压缩状态/摘要 | `Item::System`（仅在压缩完成时记录一条 `"info"` 提示） | Phase 3 可选接入；Phase 1/2 记入 `acp:log`，不污染主消息流。 |
| `subagent_update` | 子代理状态 | `acp:log` + 活跃看门狗保活 | 刷新 `PromptWatchdog` 计时器；若 `status == "error"` 写入一条 `Item::System("warn")`。 |
| `user_message_chunk` / `session_message` / `session_message_chunk` | 历史或系统消息块 | 忽略或记入 `acp:log` | 用户消息已由 Nova 在发 `session/prompt` 前写入 `Item::User`，`user_message_chunk` 必须忽略以防重复气泡。 |

### 2.2 `tool_call` 子结构逐字段映射

`src-tauri/src/threads.rs:117-133` 的 `ToolCall` 结构本就按 ACP 设计，映射几乎 1:1：

| ACP 字段 | 类型 / 枚举 | Nova `threads::ToolCall` 字段 | 映射细节 |
|---|---|---|---|
| `toolCallId` | `string` | `tool_call_id: String` | 原样透传（空串兜底生成 `uuid`）。 |
| `title` / `name` | `string` | `title: String` | 若为 `mcp_nova_<tool>` 或 `call_mcp_tool` 包装，按 4.6 剥离前缀并给出简洁中文/原始工具名标题。 |
| `kind` | `read \| edit \| delete \| move \| search \| execute \| think \| fetch \| switch_mode \| other` | `kind: Option<String>` | 直接透传上述 10 枚举；若 ACP 缺省或为 `other` 且命中 `mcp_nova_*`，按工具族补齐（`polaris`/`code_map`→`search`，`chrome_*`→`fetch`/`other`，`jianlai_*`→`other`）。 |
| `status` | `pending \| in_progress \| completed \| failed` | `status: String` | 直接透传（`tool_call` 未传时默认 `"in_progress"`）。 |
| `content` | `[{type:"content", content} \| {type:"diff", path, oldText?, newText} \| {type:"terminal", terminalId}]` | `content: Vec<Value>` | 逐项经 `threads::compact_tool_value`（`threads.rs:219`，64 KiB 上限 + base64 图摘要）裁剪，保证落盘和前端渲染不卡顿。 |
| `locations` | `[{ path, line? }]` | `locations: Vec<Value>` | 原样透传为 JSON 数组（前端 `ToolCard` 直接支持点击跳转文件与行号）。 |
| `rawInput` | `Value` | `raw_input: Option<Value>` | 经 `compact_tool_value` 保留。 |
| `rawOutput` | `Value` | `raw_output: Option<Value>` | 经 `compact_tool_value` 保留。 |

### 2.3 `session/prompt` 返回值（`usage` 与 `stopReason`）逐字段映射

**关键坑点预警**：检查 `src-tauri/src/threads.rs:773-805` 可见 `Thread::push_turn` 解析 cache 字段的键名是：
- `cacheReadTokens` / `cachedInputTokens` / `cached_input_tokens` / `cache_read_input_tokens`
- `cacheWriteTokens` / `cache_creation_input_tokens`

而实测 `agy` ACP 返回的是 **`cachedReadTokens`** 与 **`cachedWriteTokens`**（无 `cacheReadTokens`），直接传给 `push_turn` 会导致缓存命中数永远为 `0`！此外 ACP 1.5.0+ 的 `inputTokens` 是不含缓存读的净输入，而 Nova/Lyra（`lyra/runtime.rs:1006-1027`）的 `inputTokens` 约定为含缓存读的总输入。因此必须在 `mapper::reconcile_usage` 中做如下转换：

| ACP `PromptResponse.usage` / 实时状态 | 转换规则（兼容 ACP 1.5 与 1.4.3 口径） | Nova `push_turn` / `Item::Turn` 字段 (`threads.rs:475-501`) |
|---|---|---|
| `inputTokens` + `cachedReadTokens` + `cachedWriteTokens` | 令 `raw_in = inputTokens`, `cr = cachedReadTokens`, `cw = cachedWriteTokens`, `out = outputTokens`, `thought = thoughtTokens`。若 `raw_in + cr + cw + out == totalTokens`（ACP 1.5 净输入口径），则 `gross_in = raw_in + cr + cw`；否则（1.4.3 含缓存口径）`gross_in = raw_in`。 | `inputTokens` → `Item::Turn.input_tokens`（与 Lyra 一致为总输入） |
| `cachedReadTokens` | **必须重命名**为 `cacheReadTokens` | `Item::Turn.cache_read_tokens` |
| `cachedWriteTokens` | **必须重命名**为 `cacheWriteTokens` | `Item::Turn.cache_write_tokens` |
| `outputTokens` + `thoughtTokens` | 若 `raw_in + cr + cw + out + thought == totalTokens`（1.4.3 未含 thought），则 `total_out = out + thought`；否则 `total_out = out` | `outputTokens` → `Item::Turn.output_tokens` |
| `totalTokens` | 原值（为 0 时回退 `gross_in + total_out`） | `totalTokens` → `Item::Turn.total_tokens` |
| 最新非零 `usage_update.used` | 本轮上下文实际占用（绝不用多步累计的 `inputTokens` 冒充） | `contextTokens` → `Item::Turn.context_tokens` |
| 本地计时 `turn_started.elapsed().as_millis()` | 毫秒耗时 | `duration_ms` → `Item::Turn.duration_ms` |

**`stopReason` → `EV_TURN` (`{ threadId, running: false, stopReason }`) 映射**：

| ACP `stopReason` / 本地故障 | Nova `EV_TURN.stopReason` | 对现有系统的下游联动（必须对齐） |
|---|---|---|
| `"end_turn"` | `"end_turn"` | `lib.rs:4184` 保留本次浏览器经验轨迹；`store.ts:2687` 视为正常完成推进 Fire / 未读计数 |
| `"max_turn_requests"` | `"max_turn_requests"` | 同上，视为正常完成 |
| `"max_tokens"` | `"max_tokens"` | 写入一条 `Item::System("warn")` 提示触达单轮输出上限 |
| `"cancelled"` | `"cancelled"` | `store.ts:2670` 视为手动中断，不增加未读点，不触发 autoFailover，丢弃浏览器经验轨迹 |
| `"refusal"` | 若文本命中 `"Usage Limit Reached"` 且 `totalTokens == 0` 归为 `"error"`（写配额耗尽提示）；否则 `"refusal"` | 避免把配额耗尽伪装成正常回答 |
| 看门狗超时 / 进程崩溃 / 429 重试耗尽 / 协议错误 | `"error"` | 写入 `Item::System("error")`；`store.ts:2699` 可触发 `autoFailover` |

### 2.4 `session/new` 模型选项 → `acp::EV_OPTIONS` 与 `model_cache`

前端 `src/store.ts:267-278` 的 `modelChoices()` 严格消费 `OptionsEvent.options.configOptions` 中 `id === "model"` 的列表。映射如下：
- 优先从 `session/new` 返回的 `configOptions` 提取 `category == "model"`（映射为 `id: "model"`）及 `category == "thought_level" | id == "thinking_level"`（映射为 `id: "reasoning_effort"`，供模型选择器扩展或次级选项使用）。
- 若 `session/new` 仅返回旧字段 `models: { currentModelId?, availableModels: [{ modelId, name }] }`（实测 `agy 1.5.15` 含 23 个模型），组装为标准 Nova 格式：
  ```json
  {
    "configOptions": [{
      "id": "model",
      "name": "Model",
      "currentValue": "<currentModelId 或首项 modelId>",
      "options": [
        { "value": "<modelId>", "name": "<name>" }
      ]
    }],
    "modes": null
  }
  ```
- 写入 `model_cache::save(&state.config_dir, "antigravity", &options)`（复用 `src-tauri/src/model_cache.rs:33` 落盘到 `~/.nova/model-options/antigravity.json`），并发射 `acp::EV_OPTIONS { agentKind: "antigravity", options }`。

### 2.5 `agy` 内联 `⚠` 诊断通告分流

在 `TurnMapper::on_message_chunk` 中检测以 `⚠ ` 开头的独立行（`API error (attempt ...)`、`RESOURCE_EXHAUSTED`、`Agent execution terminated due to error.`、`Your AI credits balance is too low`、`There was a network issue`），将其拆出作为 `Item::System { level: "warn" | "error" }` 插入流中，避免错误提示永久混入助手最终 Markdown 回复中。

---

## 3. 会话生命周期

### 3.1 进程模型与并发池

- **严格 1 活跃会话 = 1 独立 `agy acp` 子进程**（绝不跨会话复用同一 `agy` 进程）：隔离 `cwd`、MCP 桥子进程、`stderr` 429 监测与取消强杀影响面。
- **多账号保存、按账号数据隔离与一键切换**：支持在「设置 → 后端 → Antigravity」以及新建会话/聊天输入框底部快捷入口保存多个 Google 账号并一键切换。Nova 自动读取本机 `agy` 当前登录并同步到系统原生凭据库（Windows Credential Manager / macOS Keychain / Linux Secret Service，元数据存于 `~/.nova/antigravity-accounts.json`，不落盘明文令牌）；每个账号分配独立的 `--app_data_dir=antigravity-acp-<8位ID>`，互不共用 `installation_id` 与本地 SQLite 会话库；切号或登录新账号前自动回写当前活跃账号的最新令牌，取消/失败登录自动回滚旧账号；切号后仅在无执行中请求时清理空闲 ACP 热进程并重新探测可用模型与订阅档位（`_meta.antigravity.tier`）。每个会话记录 `acpSessionId` 对应的归属账号 `acpAccountId`，切号后绝不拿旧账号的 `sessionId` 去跨号 `session/resume` / `session/load`，而是直接在新账号独立目录下新建会话并注入 Nova 本地历史续聊。
- **数据目录隔离**：所有 Nova 拉起的 `agy` 统一带 `--app_data_dir=antigravity-acp`，防止与用户在终端手跑的 `agy`（默认 `antigravity-cli`）共用 SQLite 会话库。
- **并发与空闲回收（`SessionPool`）**：
  - 最大常驻热进程数 `MAX_LIVE_BINDINGS = 4`（超限时按 LRU 驱逐最久未用的**空闲**绑定；正在运行 turn 的绑定永不驱逐）。
  - 空闲驱逐 TTL `IDLE_EVICT_SECS = 600`（10 分钟无新 turn 自动发 `session/close`（若支持）并关闭子进程释放内存，但保留 `thread.acp_session_id`）。
  - 进程启动隔离：POSIX 在 `Command::pre_exec` 调 `libc::setpgid(0, 0)`；Windows 复用 `acp::assign_to_agent_job(&child)`（`src-tauri/src/acp.rs:94`），保证 Nova 主进程崩溃或退出时操作系统自动清理全部 `agy` 及孙进程。
  - 环境准备：
    1. 向子进程 `NO_PROXY` 与 `no_proxy` 追加 `localhost,127.0.0.1,::1`；
    2. Linux/Nix 若未设 `SSL_CERT_FILE` 则从 `NIX_SSL_CERT_FILE` 或常见系统 CA 路径补齐；
    3. macOS 若 `~/.gemini/antigravity-acp/acp_token.json` 含 `refresh_token` 但 Keychain 无条目时补 `AGY_ACP_FORCE_FILE_STORAGE=1`。

### 3.2 新建、续聊、热切模型、取消与崩溃恢复

```text
用户发送 send_prompt(thread_id, text, images)
  │
  ├─► 1. 检查 SessionPool 中是否已有存活 Binding(thread_id) 且 cwd 未变
  │      ├─ 有 → 直接进入步骤 4
  │      └─ 无 → 冷启 agy acp 子进程 + initialize (30s 超时, 校验 protocolVersion == 1)
  │
  ├─► 2. 绑定/恢复 ACP 会话（操作超时 120s；启动 2s 内首次非 invalid_grant 鉴权失败等 250ms 重试 1 次）
  │      ├─ thread.acp_session_id == Some(sid) 且 handoff_from.is_none():
  │      │    ① 若 capabilities.session.resume → 调 session/resume(sid, cwd, mcpServers, _meta)
  │      │    ② 失败且 capabilities.loadSession → 设 suppress_replay=true，调 session/load(...)，完成后置 false
  │      │    ③ 仍失败 → 退化为 session/new，写入一条 Item::System("info": "远端会话已过期，已注入历史上下文续聊")
  │      │       并自动用 threads::render_handoff_context(thread) 拼接首条 prompt 上下文
  │      └─ 否则 → 调 session/new(cwd, mcpServers, _meta)
  │         将返回的 sessionId 立即写入 thread.acp_session_id 并触发 store.save_thread(&thread_id)
  │
  ├─► 3. 同步模式与模型（不重建 session）
  │      ├─ 模式：按决策固定协商 wire mode = "accept-edits"（若 session/new 已返回且当前非 accept-edits）
  │      └─ 模型/思考强度：若 thread.model 与当前 session 模型不同，优先调 session/set_config_option，
  │         回退 session/set_model；半途切换失败则销毁 binding 重建一次
  │
  ├─► 4. 构造 session/prompt 载荷并执行
  │      ├─ 若 text 以 '/' 开头，前插 '\u{200B}' 防止 agy 截获为内置斜杠命令
  │      ├─ 若存在 thread.handoff_from 或 pending_stage_context，按 Lyra 同样方式前置拼入文本块
  │      ├─ 单步文本严格限制 ≤ 188_000 UTF-8 字节（超限截断并附截断提示）
  │      ├─ 图片附件（PromptImage）：若 promptCapabilities.image == true，转 {type:"image", data, mimeType}
  │      ├─ 启动 PromptWatchdog (空闲 10min / 活跃工具 60min) + ResourceExhaustionTracker (stderr 429)
  │      └─ 阻塞等待 session/prompt 响应，期间实时经 TurnMapper 发射 EV_UPDATE
  │
  └─► 5. 若 end_turn 时原生命令仍在后台运行，保持本轮绑定，接收同一 CLI 自行发来的命令结果与后续回复；不补发用户消息、不重跑命令。最后再 reconcile_usage → push_turn → store.save_thread → EV_TURN(running:false, stopReason)
```

**取消流程（`cancel_turn` → `AntigravityRuntime::cancel(&thread_id)`）**：
1. 标记该 turn 的 `CancellationToken`；
2. 通过 `AcpConnection` 发送 JSON-RPC notification `session/cancel { sessionId }`；
3. 等待 `CANCEL_GRACE_MS = 1_500` ms 让 `session/prompt` 优雅收敛；
4. 若超时仍未退出，调 `acp::kill_process_tree(pid)` 强杀整棵进程树（同步带走该会话的 `nova --antigravity-mcp-bridge` 子进程），从 `SessionPool` 移除 binding；
5. 确保幂等发射一次 `EV_TURN { threadId, running: false, stopReason: "cancelled" }`，不重复落盘错误气泡。

**进程异常退出与崩溃恢复**：
- `AcpProcess` 后台监控线程检测到子进程退出或 stdout EOF 后，立即 fail 所有挂起 JSON-RPC 请求（错误码 `process_exit`，附脱敏后最近 4 KiB stderr），清空 `SessionPool` 中该 `thread_id` 的 binding；
- 若正在跑 turn，写入 `Item::System(level="error")` 并发射 `EV_TURN(running: false, stopReason: "error")`；
- **严禁自动重启死循环**；**保留 `thread.acp_session_id`**，待用户再次发消息时按步骤 1-2 自动拉起新进程并尝试 `session/resume` / `session/load`。

**切模型 / 切后端 / 截断与重启联动**：
- `set_thread_model`（`lib.rs:2958`）：会话空闲时若存在热 binding，**不杀进程**，记录待切换目标，下一轮发 prompt 前原地调 `session/set_config_option` / `session/set_model`；会话运行中只落盘，下轮生效。
- `set_thread_agent`（`lib.rs:3065`）与 `truncate_thread`（`lib.rs:3417`）：原有代码已将 `thread.acp_session_id = None` 并设置 `handoff_from`；补充调用 `state.antigravity.forget_session_of_thread(&thread_id)` 释放热进程。
- `restart_agents`（`lib.rs:4149`）与 `apply_runtime_settings`（`lib.rs:3728`）：`antigravity_enabled` / `antigravity_path` / `custom_env_vars` 变化时调用 `state.antigravity.shutdown()` 并后台刷新模型列表。

### 3.3 `sessionId` 落盘与 `thread_storage.rs` 对照

检查 `src-tauri/src/threads.rs:534-618` 与 `src-tauri/src/thread_storage.rs:1-340`：
- `Thread` 结构体**早已内置** `pub acp_session_id: Option<String>`（`threads.rs:542`，JSON 键名 `acpSessionId`）、`model`、`mode`、`reasoning_effort`、`handoff_from`、`plan`！
- `thread_storage.rs` 将每个会话存为 `threads/<id>.json`（`Manifest { version: 1, thread: ThreadMetaSnapshot, chunks, ... }`）+ `threads/<id>.parts/`（64 条一组的只读内容寻址 chunk + >32KiB blob），通过临时文件 + `.json.previous` 原子替换落盘。
- `Thread.acp_session_id` 会随 `Manifest.thread` 自动原子落盘，**不需要任何磁盘格式升级或迁移脚本**！
- **唯一需要扩展的持久化枚举**：`threads::AgentKind`（`threads.rs:49-113`）新增变体 `Antigravity`：
  - 序列化 / `as_str()`：`"antigravity"`
  - `label()`：`"Antigravity"`
  - `from_str("antigravity")` → `Some(AgentKind::Antigravity)`
  - 同步放开 `lib.rs` 中所有 `agent_kind != AgentKind::Lyra` 的准入检查（`agent_enabled`、`is_running`、`create_thread`、`set_thread_agent`、`get_model_options`、`dispatch_scene_prompt`、`truncate_thread`、`cancel_single_turn`、`get_status`、`spawn_backend_availability_check`），同时确保旧的 `Legacy(_)` / `Unsupported` 行为完全不受影响。

---

## 4. 工具桥设计

### 4.1 形态选择：自托管 stdio 子进程 + `ContextService` IPC 回环

| 维度 | 方案 A：stdio 子进程 + `ContextService` IPC 回环（**选用**） | 方案 B：Nova 内嵌 Streamable HTTP MCP 服务端 |
|---|---|---|
| ACP 协议兼容性 | **基线必选能力**（实测 `agy 1.5.15` 已确认 stdio `mcpServers` 生效），不依赖 `mcpCapabilities.http` | `agy` 不同版本对 `mcpCapabilities.http` 宣告不一致 |
| 复用现有基础设施 | **直接复用** `src-tauri/src/context_service.rs` 现有 Unix Socket / Windows Named Pipe + 随机 Token 鉴权 | 需新写 HTTP 路由、SSE/Streamable framing、端口分配、防 CSRF/Host 校验 |
| `chrome_*` / `polaris` 进程归属 | `chrome_browser::ChromeBridge`（浏览器扩展通信）与 `polaris::MEMO` 常驻索引都在 Nova 主进程，IPC 回环天然零拷贝/单实例 | 同在主进程，但需处理 HTTP 长连接超时与端口防火墙弹窗 |
| 生命周期清理 | 作为 `agy acp` 直接子进程，被 PGID / Windows Job Object 原子清理，零残留 | 需手工维护每会话 HTTP server 生命周期 |
| 代理干扰风险 | **完全免疫** `HTTP_PROXY` / `NO_PROXY` / 企业安全软件拦截 | 仍受本地回环端口策略与代理配置影响 |

**具体形态**：模仿 `src-tauri/src/main.rs:202`（`image_generation::maybe_run()`）与 `main.rs:247`（`nova_lib::maybe_run_lyra()`），在 `main()` 最早期拦截 CLI 标志 `--antigravity-mcp-bridge`：

```text
agy acp session/new mcpServers:
[{
  "name": "nova",
  "command": "<std::env::current_exe()>",
  "args": ["--antigravity-mcp-bridge", "--cwd", "<canonical_cwd>", "--owner", "<thread_id>"],
  "env": [
    { "name": "NOVA_CONTEXT_SERVICE_ENDPOINT", "value": "<socket_or_pipe>" },
    { "name": "NOVA_CONTEXT_SERVICE_TOKEN",    "value": "<random_token>" }
  ]
}]
```

子进程 `nova --antigravity-mcp-bridge` **绝不启动 Tauri/WebView**，仅跑轻量 tokio current-thread runtime，按 MCP 规范处理 `initialize`、`notifications/initialized`、`tools/list`、`tools/call`、`ping`，把 `tools/call` 经 `NOVA_CONTEXT_SERVICE_ENDPOINT` 转发给 Nova 主进程 `ContextService` 执行。

### 4.2 完整工具清单（全 24 个工具与 Schema 出处）

以下清单全部从 Nova 真实源码与 JSON 资产中核对并测算紧凑 UTF-8 字节与估算 token 数（按 `agy` 规则 `ceil(bytes / 3)`）：

#### (A) Polaris / Context 类（2 个，合计 ~1.6 KB ≈ 550 tokens）

| 工具名 | Schema 出处 | 必填参数 | 其余参数 | 主进程执行器 |
|---|---|---|---|---|
| `polaris` | `scripts/polaris-tool.json` | `task` | `root`（默认当前工作目录） | `lyra::polaris_agent::run` |
| `code_map` | `src-tauri/src/nova_tools_native/context.rs:3841-3908`（单参 `scope`） | 无 | `scope`(string，可选目录前缀；空返全仓行数/符号数，非空返目录内顶层符号大纲) | `nova_tools_native::context::code_map`（`context_service.rs:98`） |

#### (B) Jianlai 桌面操控类（8 个，源码 `scripts/jianlai-tools.json:1-920` + `src-tauri/src/jianlai_tools.rs`，紧凑总计 24,981 B ≈ 8,327 tokens）

| 工具名 | Schema 出处 | 必填参数 | 关键参数 | 紧凑体积 | `anyOf` |
|---|---|---|---|---|---|
| `jianlai_windows` | `scripts/jianlai-tools.json:3-46` | 无 | `query`, `windowId`, `observe`, `background`, `maxEdge` | 2,807 B (~936 tok) | 是 |
| `jianlai_screenshot` | `scripts/jianlai-tools.json:48-301` | `capture` | `capture`(screen/window/region/active), `background`, `maxEdge` | 6,476 B (~2,159 tok) | 是 |
| `jianlai_activate` | `scripts/jianlai-tools.json:303-335` | `windowId` | `windowId`, `maxEdge` | 1,034 B (~345 tok) | 是 |
| `jianlai_act` | `scripts/jianlai-tools.json:337-666` | `actions` | `snapshotId`, `imageId`, `windowId`, `background`, `feedback`, `notes`, `maxEdge`, `actions[]` | 9,756 B (~3,252 tok) | 是 |
| `jianlai_recall` | `scripts/jianlai-tools.json:668-684` | `imagePath` | `imagePath` | 513 B (~171 tok) | 否 |
| `jianlai_experience_search` | `scripts/jianlai-tools.json:686-720` | `scope`, `task` | `scope`, `task`, `prefix` | 729 B (~243 tok) | 否 |
| `jianlai_experience_save` | `scripts/jianlai-tools.json:722-849` | `scope`, `task`, `conditions`, `steps`, `checks`, `evidence`, `redacted` | `snapshotId`, `pitfalls`, `alternatives`, `recordingId` | 2,408 B (~803 tok) | 否 |
| `jianlai_experience_feedback` | `scripts/jianlai-tools.json:851-919` | `snapshotId`, `routeId`, `scope`, `task`, `outcome`, `reason`, `evidence`, `redacted` | 同左 | 1,249 B (~417 tok) | 否 |

> 执行链路：`jianlai_tools::arguments(name, args)`（`jianlai_tools.rs:28`）转换为内部 `operation` 载荷 → 持 `screen_lock()` 调 `crate::jianlai::execute` → `jianlai_tools::result(value)`（`jianlai_tools.rs:146`）重写恢复提示中的工具名。

#### (C) Chrome 浏览器操控类（14 个，源码 `scripts/chrome-tools.json:1-1675` + `src-tauri/src/chrome_tools.rs`，紧凑总计 36,574 B ≈ 12,192 tokens）

| 工具名 | Schema 出处 | 必填参数 | 关键参数 | 紧凑体积 | `anyOf` |
|---|---|---|---|---|---|
| `chrome_tabs` | `scripts/chrome-tools.json:3-40` | 无 | `query`, `incognito`, `visual`, `maxItems`, `maxTextChars` | 1,205 B (~402 tok) | 否 |
| `chrome_navigate` | `scripts/chrome-tools.json:42-141` | `url`, `target` | `url`, `target`(new/tab), `observe`, `settleMs`, `visual`, `maxItems`, `maxTextChars` | 2,447 B (~816 tok) | 是 |
| `chrome_tab_control` | `scripts/chrome-tools.json:143-205` | `tabTag`, `command` | `command`(focus/close/back/forward/reload/wait), `observe`, `settleMs`, `visual` | 1,750 B (~584 tok) | 否 |
| `chrome_inspect` | `scripts/chrome-tools.json:207-255` | `tabTag` | `query`, `scope`, `visual`, `maxItems`, `maxTextChars` | 1,542 B (~514 tok) | 否 |
| `chrome_screenshot` | `scripts/chrome-tools.json:257-442` | `tabTag`, `capture` | `capture`(viewport/region/full_page/element), `maxItems`, `maxTextChars` | 3,892 B (~1,298 tok) | 是 |
| `chrome_act` | `scripts/chrome-tools.json:444-910` | `tabTag`, `actions` | `actions[]`(click/fill/select/press/scroll/hover/upload/click_at/drag_at/scroll_at), `imageId`, `visual`, `settleMs` | 8,885 B (~2,962 tok) | 是 |
| `chrome_evaluate` | `scripts/chrome-tools.json:912-965` | `tabTag`, `expression` | `frame`, `observe`, `visual`, `maxItems`, `maxTextChars` | 1,646 B (~549 tok) | 否 |
| `chrome_run` | `scripts/chrome-tools.json:967-1332` | `tabTag`, `task`, `steps` | `steps[]`(click/fill/press/scroll), `authorization`, `expectedText`, `inputs`, `maxActions` | 7,775 B (~2,592 tok) | 是 |
| `chrome_connection` | `scripts/chrome-tools.json:1334-1357` | `command` | `command`(status/cancel_operation), `tabTag` | 723 B (~241 tok) | 否 |
| `chrome_downloads` | `scripts/chrome-tools.json:1359-1380` | 无 | `downloadId`, `since` | 642 B (~214 tok) | 否 |
| `chrome_devtools` | `scripts/chrome-tools.json:1382-1429` | `tabTag` | `kind`(all/console/network), `query`, `level`, `limit`, `requestId`, `clear` | 1,298 B (~433 tok) | 否 |
| `chrome_experience_search` | `scripts/chrome-tools.json:1431-1465` | `scope`, `task` | `scope`, `task`, `prefix` | 824 B (~275 tok) | 否 |
| `chrome_experience_save` | `scripts/chrome-tools.json:1467-1598` | `scope`, `task`, `conditions`, `steps`, `checks`, `evidence`, `redacted` | `recordingId`, `tabTag`, `snapshotId`, `pitfalls`, `alternatives` | 2,483 B (~828 tok) | 否 |
| `chrome_experience_feedback` | `scripts/chrome-tools.json:1600-1674` | `tabTag`, `snapshotId`, `scope`, `task`, `routeId`, `outcome`, `reason`, `evidence`, `redacted` | 同左 | 1,447 B (~483 tok) | 否 |

> 执行链路：`chrome_tools::arguments(name, args)`（`chrome_tools.rs:28`）转换载荷 → 持 `screen_lock()` 调 `crate::native_browser::execute_chrome`（主进程通过 `ChromeBridge` 控制用户真实 Chrome 扩展）。

### 4.3 20k Token 预算超限治理与 Schema 投影（关键发现）

上述 24 个工具原始紧凑 JSON 总计 **63,155 字节 ≈ 21,050 tokens**，**直接超过 `agy` 的 ~20,000 token 定制工具上限**！按参考包实证，超限后 `agy` 会从最大体积工具开始静默丢弃——首当其冲丢掉 `jianlai_act`(3,252 tok)、`chrome_act`(2,962 tok)、`chrome_run`(2,592 tok)。

`mcp_server.rs` 必须在导出 `tools/list` 时内置 `schema_budget::fit_tool_budget`：
1. **常驻预加载标记**：每条工具附加 `_meta: { "antigravity/alwaysLoad": true }`，且 `session/new` 的 `_meta.antigravity.eagerMcpServers` 传入 `["nova"]`。
2. **两级预算压缩（目标 `TOOL_TOKEN_BUDGET = 16_500`，留 3.5k 余量）**：
   - 第 1 级：保留全部 JSON Schema 结构字段（`type`、`properties`、`required`、`enum`、`anyOf`、`minimum`/`maximum`/`minItems`/`maxItems`、`additionalProperties`），仅对深层重复子字段（如 `anyOf` 分支内重复的 `when`/`expect`/`expectedText`/`repeat`/`optional` 说明）做精简，可将总量从 ~21k 压到 ~13–15k tokens，**不损失任何参数约束**。
   - 第 2 级（兜底）：若未来新增工具仍超 `16_500` tokens，按 `omp` 算法二分裁切各工具顶层 `description` 最大长度直至总估算 `Σ ceil(utf8_bytes / 3) ≤ 16_500`。
3. **Go RE2 兼容清洗**：当前 24 个工具经扫描无 `"pattern"` 字段，但 `schema_budget.rs` 仍统一递归检查 `"pattern"`，剔除含前瞻/后顾或非法 RE2 语法的正则防止炸毁 `agy` toolbox。

### 4.4 `ContextService` 升级、图片回传、并发锁与超时保活

现有 `src-tauri/src/context_service.rs:88-115` 仅接受原始大工具名 `"jianlai"` / `"chrome"`（要求调用方自己填 `operation`），且返回纯文本/JSON 不附带图像。Phase 2 在 `ContextService::dispatch` 新增统一入口 `"nova_tool"`（`params: { name, arguments }`）：
1. **窄工具参数转换与校验**：按 `name` 分流：
   - `"polaris"` → 使用 Lyra 的轻量模型和 `polaris_agent::run` 检索；
   - `"code_map"` → 执行 `context::code_map`；
   - `jianlai_tools::is_tool(name)` → `jianlai_tools::arguments` → **获取 `lyra::turn::screen_lock()`** → `jianlai::execute` → `jianlai_tools::result`；
   - `chrome_tools::is_tool(name)` → `chrome_tools::arguments` → **获取 `lyra::turn::screen_lock()`** → `native_browser::execute_chrome`。
2. **输出治理（`lyra/tools.rs:govern`）**：大输出走现有 `compact_browser_result`（保留 `snapshotId`/`status`/`images`/`nextTool`/`nextCall`/`altairRun`）与归档机制，防止超过 MCP 文本上限（硬顶 96 KiB）撑爆 `agy` 上下文。
3. **图像回传（核心能力）**：`chrome_screenshot`、`chrome_inspect(visual=auto)`、`chrome_act`、`jianlai_screenshot`、`jianlai_act`、`jianlai_recall` 返回的 JSON 中含 `images[].path`（WebP/PNG 本地文件路径）。`ContextService` 的 `"nova_tool"` 响应返回 `{ text, images: [{ data_base64, mime_type }], is_error }`（复用 `lyra/provider.rs:249-287` 的读盘与 MIME 推断逻辑），由 `mcp_server.rs` 组装为标准 MCP `CallToolResult`：
   ```json
   {
     "content": [
       { "type": "text", "text": "<governed JSON>" },
       { "type": "image", "data": "<base64>", "mimeType": "image/webp" }
     ],
     "isError": false
   }
   ```
4. **长耗时工具超时保活**：`chrome_run` 最长可运行 3 分钟，`jianlai_act` 亦可长达数十秒。`mcp_server.rs` 在等待 `ContextService` 回包期间，每 **15 秒**在 stdout 发送一条 MCP `notifications/progress`（携带请求 `_meta.progressToken`，若无则跳过并由 `ContextService` 客户端 240s 超时兜底），防止 `agy` 侧触发 MCP 单次调用超时；同时 `ContextService` 客户端单次超时提升至 `240s`（超过 `chrome_run` 3 分钟上限）。

### 4.5 权限归属、会话与 `cwd` 绑定

- **权限归属**：所有经 `mcp_nova_*` 进入 Nova 主进程的工具均运行在 Nova 既有能力与边界内（如 `chrome_browser` 扩展连接授权、`screen_lock` 防并发冲突、`jianlai` 本机桌面权限），**不再通过 ACP 反向弹窗二次审批**；`agy` 自带的 `run_command`/文件编辑等内置工具由 `agy` 自身权限策略（见第 5 节）管理。
- **会话与 `cwd` 绑定**：
  - `agy acp` 的 `session/new` 传入规范化后的 `cwd`（`dunce::canonicalize` 失败退回原值），同时将该 `cwd` 与 `thread_id` 通过 `--cwd` / `--owner` 固化在 `nova --antigravity-mcp-bridge` 启动参数中。
  - `ContextService` 收到的 `request.owner` 即 `thread_id`，`request.root` 即会话 `cwd`：
    - `polaris` / `code_map` 自动命中该项目的 `MEMO` 常驻索引；
    - `chrome_*` / `jianlai_*` 的标签页观察状态、快照缓存按 `owner = thread_id` 隔离；
    - `src-tauri/src/lib.rs:4180-4193` 的 `register_experience_trails` 监听 `acp::EV_TURN` 并按 `o == tid` 匹配，因此 Antigravity 会话的浏览器操作经验会自动录制与收尾，**零额外胶水**！

### 4.6 工具名前缀映射

- 注册 server 名称固定为 **`nova`**，MCP 工具暴露原始名（如 `polaris`、`chrome_tabs`、`jianlai_act`）。
- `agy` 侧给模型呈现的完整名称为 **`mcp_nova_<tool>`**（如 `mcp_nova_chrome_tabs`、`mcp_nova_polaris`）。
- 双向映射规则（集中在 `antigravity/core/mapper.rs`）：
  1. **入站 MCP `tools/call`**：按原名派发；若某些版本误带 `mcp_nova_` 前缀，防御性 `strip_prefix("mcp_nova_")` 后再查表。
  2. **出站 UI 渲染（`tool_call` / `tool_call_update`）**：
     - 若 `title` 或 `name` 以 `mcp_nova_` 开头，剥离前缀还原为原始工具名（如 `chrome_tabs`）供 Nova 前端 `ToolCard` 复用既有图标与摘要渲染；
     - 若 `agy` 经内置 `call_mcp_tool` 包装调用（`rawInput.server == "nova"` 且含 `rawInput.tool`），提取内部工具名与 `rawInput.arguments` 作为展示标题与入参，避免卡片一律只显示笼统的 `call_mcp_tool`。

---

## 5. 产品决策落地设计

### 5.1 决策 A：浏览器一律走 Nova 自研 `chrome_*` 工具

`agy` 内置 12 个浏览器工具（`browser_get_dom`、`browser_click_element`、`capture_browser_screenshot`、`execute_browser_javascript` 等），会另起无登录态的隔离浏览器，既无法复用用户当前 Chrome 会话，也绕开了 Nova 的扩展桥与经验图谱。落地双层约束：

1. **主保障（Guaranteed）：通过 `_meta.antigravity.systemPrompt.append` 注入强约束指令**（已实测 `agy 1.5.15` 宣告 `_meta.antigravity.systemPrompt.append: true`）：
   ```text
   [Nova Workspace Tool Policy]
   1. Browser Automation (MANDATORY):
      - NEVER use any built-in `browser_*` tools (such as `browser_get_dom`, `browser_click_element`, `capture_browser_screenshot`, `execute_browser_javascript`, etc.).
      - For ALL browser inspection, navigation, interaction, screenshots, downloads, or devtools tasks, you MUST exclusively use the Nova MCP tools prefixed with `mcp_nova_chrome_*` (start with `mcp_nova_chrome_tabs` or `mcp_nova_chrome_navigate`, then `mcp_nova_chrome_inspect` / `mcp_nova_chrome_act` / `mcp_nova_chrome_run`).
   2. Desktop & Codebase Retrieval:
      - Use `mcp_nova_jianlai_*` for native desktop window automation.
      - Prefer `mcp_nova_polaris` and `mcp_nova_code_map` for fast cross-file repository overview before reading many files.
   ```
   > **反指纹注意**：文案完全使用 Nova 自身中性措辞，不出现第三方 harness 特征短语，规避 `agy` 提示词指纹 429 拦截。同时按需拼入 `agent_config::global_instructions_for_agent(&config_dir, "antigravity")`（复用 `src-tauri/src/agent_config.rs`）。
2. **增强防线（Best-Effort，Phase 1 实测确认）**：在 `session/new` 的 `_meta.antigravity.disabledTools` 中列出全部 12 个内置 `browser_*` 工具名（在 Phase 1 实测若 `agy 1.5.15` 接受并从 toolbox 移除则保留；若忽略或报错则自动回落仅发 `systemPrompt.append`）。

### 5.2 决策 B：权限默认 `dangerous`（自动放行）+ 执行模式 `accept-edits`，不使用 `plan` 模式

对齐 Nova 前端 `UNIFIED_MODES = [{ value: "build", label: "Agent" }]`（`src/store.ts:297`，Plan 模式已废弃）：

1. **进程启动级（Dangerous）**：启动 `agy acp` 时默认携带 `--dangerously-skip-permissions`。
2. **会话执行模式级（`accept-edits`，禁止 `plan`）**：
   - `session/new` / `session/resume` / `session/load` 返回后，检查 `modes.availableModes` 或 `configOptions(category="mode")`：
     - 若存在 `accept-edits` 且当前值非 `accept-edits`，立即调 `session/set_mode`（或 `session/set_config_option`）切到 `accept-edits`；
     - 永不向 `agy` 请求 `plan` 模式；若服务端推送 `current_mode_update(plan)`，记日志并在需要时切回 `accept-edits`。
3. **反向权限 RPC 兜底级（`session/request_permission`）**：
   - 即使携带 `--dangerously-skip-permissions`，仍完整实现 `session/request_permission` 回调防止任何版本/特例操作挂死；
   - 先过 `PermissionIdentityRegistry` 校验同一 `toolCallId` 未被篡改；
   - 自动按优先级选中 `options`：`kind == "allow_once"`（优先）> `kind == "allow_always"` > `kind.starts_with("allow")` → 返回 `{ "outcome": { "outcome": "selected", "optionId": chosen } }`，同时写一条简洁审计日志到 `acp::EV_LOG`；
   - 无任何 `allow*` 选项或校验失败时返回 `{ "outcome": { "outcome": "cancelled" } }`。
   - 保留 `acp::EV_PERMISSION` / `lib.rs:respond_permission` 接口兼容，便于日后若用户显式关闭 dangerous 开关时无缝接入 UI 审批卡片。

---

## 6. 分阶段实施计划（Phase 1 / 2 / 3）

### Phase 1：纯 Rust ACP 协议核心 + 最小端到端对话 + 前端后端选择

**目标**：不挂自定义工具桥，先跑通「Nova 选 Antigravity 模型 → 启动 `agy acp` → 流式思考/正文/内置工具卡片/用量/取消/落盘」。

- **新增文件**：
  - `src-tauri/src/antigravity/mod.rs`
  - `src-tauri/src/antigravity/core/mod.rs`
  - `src-tauri/src/antigravity/core/protocol.rs`（JSON-RPC 2.0 请求/响应/通知结构、ACP DTO、`ToolKind`/`ToolCallStatus`/`StopReason`、32 MiB 有界 NDJSON 帧读写 + 忽略 `"Opening in existing browser session."`）
  - `src-tauri/src/antigravity/core/process.rs`（`agy acp` 启动参数、`NO_PROXY`/`SSL_CERT_FILE` 环境准备、PGID、16 KiB stderr 环形缓冲与 4 KiB 尾部脱敏）
  - `src-tauri/src/antigravity/core/connection.rs`（手写异步 JSON-RPC 复用器：出站 request/notification、入站 `session/update` 通知、`session/request_permission` 反向请求自动放行）
  - `src-tauri/src/antigravity/core/mapper.rs`（`TurnMapper`：ACP `sessionUpdate` → 纯 Rust `MappedEvent`；`reconcile_usage`；`⚠` 通告分离）
  - `src-tauri/src/antigravity/core/watchdog.rs`（双档不活跃看门狗 `10min` / `60min`）
  - `src-tauri/src/antigravity/runtime.rs`（薄 Tauri 胶水：`AntigravityRuntime`、单轮执行、取消、模型探测与缓存、`EV_*` 发射）
- **修改文件**：
  - `src-tauri/src/threads.rs`：`AgentKind` 新增 `Antigravity`（`"antigravity"` / `"Antigravity"`）
  - `src-tauri/src/settings.rs`：`Settings` 新增 `antigravity_enabled: bool`（默认 `false` 或检测到 `agy` 后可开）、`antigravity_path: String`（默认 `"agy"`）
  - `src-tauri/src/lib.rs`：`AppState` 增加 `pub antigravity: Arc<AntigravityRuntime>`；接入 `agent_enabled`、`is_running`、`spawn_backend_availability_check`（用 `acp::resolve_program_on_path` 探测 `agy`）、`create_thread`、`set_thread_agent`、`get_model_options`、`dispatch_scene_prompt`、`truncate_thread`、`cancel_single_turn`、`restart_agents`、`apply_runtime_settings`、启动时加载 `model_cache::load(&dir, "antigravity")`
  - `src/types.ts`：`ALL_AGENT_KINDS = ["lyra", "antigravity"]`；`Settings` 补充 `antigravityEnabled` / `antigravityPath`
  - `src/store.ts`：`agentEnabled`、`modelOptions`、`slashCommands`、`pickThreadModel` 支持 `"antigravity"` 与跨后端切换（复用 `api.setThreadAgent`）
  - 设置面板组件：增加 Antigravity 启用开关与 CLI 路径配置项
- **关键 Rust 签名草案**：
  ```rust
  // src-tauri/src/antigravity/core/protocol.rs
  pub const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
  pub enum AcpErrorKind { Aborted, Auth, Model, Protocol, Quota, Spawn, Timeout, ProcessExit, Transport, Refusal, InvalidInput }
  pub struct AcpError { pub kind: AcpErrorKind, pub message: String, pub stderr_tail: Option<String> }

  // src-tauri/src/antigravity/core/connection.rs
  #[async_trait_free] // 无需 async-trait crate，使用 mpsc channel + 同步/boxed 回调
  pub struct AcpConnection { /* stdin_tx, pending_rpc, update_tx, child_pid, stderr_ring */ }
  impl AcpConnection {
      pub async fn spawn(cfg: LaunchConfig) -> Result<(Self, mpsc::UnboundedReceiver<InboundEvent>), AcpError>;
      pub async fn initialize(&self, client_name: &str, version: &str) -> Result<InitializeResult, AcpError>;
      pub async fn session_new(&self, params: SessionNewParams) -> Result<SessionOpenResult, AcpError>;
      pub async fn session_prompt(&self, params: SessionPromptParams) -> Result<PromptResult, AcpError>;
      pub async fn session_cancel(&self, session_id: &str) -> Result<(), AcpError>;
      pub async fn respond_permission(&self, rpc_id: Value, option_id: Option<&str>) -> Result<(), AcpError>;
      pub async fn terminate(&self, grace_ms: u64);
  }

  // src-tauri/src/antigravity/core/mapper.rs
  pub struct TurnMapper { /* active_tools, last_context_used, context_window, suppress_replay */ }
  impl TurnMapper {
      pub fn on_update(&mut self, update: &Value) -> Vec<MappedAction>;
      pub fn reconcile_usage(&self, raw_usage: Option<&Value>) -> Value; // 输出兼容 Thread::push_turn 的 JSON
  }

  // src-tauri/src/antigravity/runtime.rs
  pub struct AntigravityRuntime { /* app, pool, model_options_cache */ }
  impl AntigravityRuntime {
      pub fn new(app: tauri::AppHandle) -> Arc<Self>;
      pub fn is_running(&self, thread_id: &str) -> bool;
      pub async fn run_prompt(&self, thread_id: String, text: String, images: Vec<PromptImage>);
      pub async fn cancel(&self, thread_id: &str);
      pub fn forget_session_of_thread(&self, thread_id: &str);
      pub async fn ensure_model_options(&self) -> Result<Value, String>;
      pub fn seed_model_options(&self, options: Value);
      pub fn shutdown(&self);
  }
  ```
- **验证命令**：
  1. 独立纯 Rust crate 校验（详见 7.2）：`cargo test --manifest-path /tmp/nova-agy-check/Cargo.toml`（覆盖 NDJSON 噪声行过滤、32 MiB 限帧、`tool_call_update` 字段合并、`reconcile_usage` 双口径、自动放行优先级、Mock ACP stdio 子进程全流程）。
  2. 真实 `agy acp` 冒烟（在独立验证 crate 设 `#[ignore]` 集成测试）：`cargo test --manifest-path /tmp/nova-agy-check/Cargo.toml -- --ignored live_agy_smoke`。
  3. 前端类型检查与构建：`npm run build`（或 `npx tsc --noEmit`）。

---

### Phase 2：Stdio MCP 工具桥 + `ContextService` 窄工具派发 + 浏览器决策落地

**目标**：让 `agy` 无缝调用 Nova 的 24 个自定义工具（polaris/code_map、8 个 jianlai、14 个 chrome），完成图片回传、18k token 预算裁剪与「浏览器一律走 `chrome_*`」强约束。

- **新增文件**：
  - `src-tauri/src/antigravity/core/schema_budget.rs`（24 工具静态/构建目录、估算 `ceil(utf8_bytes/3)`、两级预算压缩 ≤ 16,500 tokens、RE2 `pattern` 清洗、`_meta: { "antigravity/alwaysLoad": true }`）
  - `src-tauri/src/antigravity/core/mcp_server.rs`（纯 Rust stdio MCP JSON-RPC 2.0 服务端：`initialize`/`tools/list`/`tools/call`/`ping` + 15s `notifications/progress` 心跳 + `ContextService` IPC 客户端）
  - `src-tauri/src/antigravity/core/system_prompt.rs`（构造 `_meta.antigravity`：`systemPrompt.append` 浏览器禁用策略 + `eagerMcpServers: ["nova"]` + 可选 `disabledTools`）
  - `src-tauri/src/antigravity/bridge_entry.rs`（`pub fn maybe_run_antigravity_mcp_bridge() -> Option<i32>`，供 `main.rs` 早期拦截）
- **修改文件**：
  - `src-tauri/src/main.rs`：在 `maybe_run_lyra()` 旁新增 `antigravity::bridge_entry::maybe_run_antigravity_mcp_bridge()`
  - `src-tauri/src/context_service.rs`：在 `dispatch()` 新增 `"nova_tool"` 分支，统一接入 `polaris`/`code_map`/`jianlai_*`/`chrome_*`、`screen_lock()`、`lyra::tools::govern` 与 `details.images[].path` 读盘转 base64；暴露 `pub fn endpoint(&self)` / `pub fn token(&self)` 供 `AntigravityRuntime` 注入 MCP 子进程 env
  - `src-tauri/src/antigravity/runtime.rs`：`session/new` 传入 `mcpServers` 与 `_meta.antigravity`
  - `src-tauri/src/antigravity/core/mapper.rs`：补齐 `mcp_nova_*` 与 `call_mcp_tool` 前缀解包展示
- **关键 Rust 签名草案**：
  ```rust
  // src-tauri/src/antigravity/core/schema_budget.rs
  pub const TOOL_TOKEN_BUDGET: usize = 16_500;
  pub struct McpToolSpec { pub name: String, pub description: String, pub input_schema: Value }
  pub fn build_nova_mcp_tools(jianlai_json: &str, chrome_json: &str) -> Vec<McpToolSpec>;
  pub fn fit_tool_budget(tools: &mut [McpToolSpec], max_tokens: usize) -> BudgetReport;
  pub fn sanitize_re2_patterns(schema: &mut Value);

  // src-tauri/src/antigravity/core/mcp_server.rs
  pub struct BridgeConfig { pub cwd: PathBuf, pub owner: String, pub endpoint: String, pub token: String }
  pub async fn run_stdio_mcp_bridge(cfg: BridgeConfig, tools: Vec<McpToolSpec>) -> anyhow::Result<()>;

  // src-tauri/src/context_service.rs (新增辅助)
  pub(crate) async fn execute_nova_narrow_tool(root: &Path, owner: &str, name: &str, args: &Value) -> Value;
  ```
- **验证命令**：
  1. 单测断言 24 个真实工具（直接 `include_str!` 读取 `scripts/jianlai-tools.json` 与 `scripts/chrome-tools.json`）经 `fit_tool_budget` 后总估算 `≤ 16_500` tokens 且保留全部 24 个工具和所有 `required`/`enum` 字段：`cargo test --manifest-path /tmp/nova-agy-check/Cargo.toml schema_budget`。
  2. Mock IPC + stdio MCP 端到端单测（模拟 `tools/list`、`tools/call` 文本+图片回传、长任务 15s progress 通知）：`cargo test --manifest-path /tmp/nova-agy-check/Cargo.toml mcp_server`。
  3. 真实 `agy acp` 联调验证：发送「列出当前 Chrome 标签页」与「用 polaris 查找 `AntigravityRuntime`」，确认 `agy` 调用 `mcp_nova_chrome_tabs` / `mcp_nova_polaris` 而未调用任何内置 `browser_*` 工具。

---

### Phase 3：会话续聊（resume/load）、热切模型、429 配额检测与体验打磨

**目标**：补齐生产级可靠性与交互细节——跨重启无损续聊、热切模型与思考强度、静默 429 快速失败、进程池 LRU 回收与全局指令集成。

- **新增文件**：
  - `src-tauri/src/antigravity/core/quota.rs`（`ResourceExhaustionTracker`：解析 stderr 429 `RESOURCE_EXHAUSTED` + Go duration 退避，≥2 次或累计 >15s 触发 `AcpErrorKind::Quota`；识别 `"Usage Limit Reached"` 伪正常回复）
  - `src-tauri/src/antigravity/core/session_pool.rs`（`MAX_LIVE_BINDINGS = 4`、`IDLE_EVICT_SECS = 600`、LRU 驱逐、防并发重入锁）
- **修改文件**：
  - `src-tauri/src/antigravity/core/connection.rs`：新增 `session_resume`、`session_load`、`session_close`、`session_set_config_option`、`session_set_model`、`session_set_mode`
  - `src-tauri/src/antigravity/runtime.rs`：接入三级恢复链（`resume → load(suppress_replay) → new+handoff`）、热切模型/思考强度、启动 2s 瞬态 auth 重试 1 次、429 快速中止、`session_pool` 空闲回收定时任务
  - `src-tauri/src/agent_config.rs`：支持 `"antigravity"` 作为全局指令目标后端
  - `src-tauri/src/lib.rs`：`set_thread_model` / `set_thread_reasoning_effort` 对 `AgentKind::Antigravity` 保持热进程不 kill
- **关键 Rust 签名草案**：
  ```rust
  // src-tauri/src/antigravity/core/quota.rs
  pub struct ResourceExhaustionTracker { /* retries, total_delay_ms */ }
  impl ResourceExhaustionTracker {
      pub fn observe_stderr_line(&mut self, line: &str) -> Option<String>; // 触发阈值时返回中文配额报错
      pub fn is_usage_limit_reply(text: &str, total_tokens: u64, stop_reason: &str) -> bool;
  }

  // src-tauri/src/antigravity/core/connection.rs (扩展)
  impl AcpConnection {
      pub async fn session_resume(&self, params: SessionResumeParams) -> Result<SessionOpenResult, AcpError>;
      pub async fn session_load(&self, params: SessionResumeParams) -> Result<SessionOpenResult, AcpError>;
      pub async fn session_set_config_option(&self, session_id: &str, config_id: &str, value: &str) -> Result<Value, AcpError>;
      pub async fn session_set_model(&self, session_id: &str, model_id: &str) -> Result<(), AcpError>;
      pub async fn session_set_mode(&self, session_id: &str, mode_id: &str) -> Result<(), AcpError>;
  }
  ```
- **验证命令**：
  1. 纯 Rust 单测覆盖 `resume → load(重放抑制) → new` 三级回退、半切换模型回滚、429 stderr 重试识别、LRU 空闲驱逐：`cargo test --manifest-path /tmp/nova-agy-check/Cargo.toml`。
  2. 真实 `agy acp` 续聊回归：首轮设上下文变量 → 杀进程 → 新进程带同一 `sessionId` 发二轮验证记忆保留与 UI 无重复消息。
  3. 完整 Tauri 编译验证（在带 `webkit2gtk4.1` 的 CI / macOS / Windows 环境）：`cargo check --manifest-path src-tauri/Cargo.toml && cargo test --manifest-path src-tauri/Cargo.toml --lib antigravity`。

---

## 7. 风险、未知点与本机无 `webkit2gtk4.1` 验证方案

### 7.1 风险与未知点清单

| # | 风险 / 未知点 | 影响 | 验证办法与兜底策略 |
|---|---|---|---|
| 1 | **本机无 `webkit2gtk4.1`，`src-tauri` 无法直接 `cargo check`** | 无法在当前 Linux 开发机编译完整 Tauri crate | **核心/胶水硬隔离**：将 `antigravity/core/*` 保持零 `tauri` 依赖，通过 7.2 的临时独立验证 crate 在本机跑全量 `cargo check` / `cargo test`；薄胶水层 `runtime.rs` 与 `lib.rs` 变更控制在极小体积并在带 GTK/macOS/Windows 的 CI 上做最终编译。 |
| 2 | **`agy 1.5.15` 对 `_meta.antigravity.disabledTools` 与 `--dangerously-skip-permissions` + `accept-edits` 的实际支持度** | 若 `disabledTools` 未生效或报错，可能影响建会话或未拦截内置 `browser_*` | Phase 1 写独立集成用例实测 `agy acp --app_data_dir=antigravity-acp --dangerously-skip-permissions` + `session/new(_meta)`：观察返回 `modes` 列表与是否报错；`systemPrompt.append` 作为必选主保障，`disabledTools` 做特性探测/容错降级。 |
| 3 | **24 工具 schema 含 `anyOf` 且原始体量 21k tokens 超 20k 预算** | 未裁剪会静默丢弃 `jianlai_act`/`chrome_act`/`chrome_run`；部分 Gemini 变体对深层 `anyOf` 敏感 | Phase 2 强制执行 `fit_tool_budget(..., 16_500)` 并用单测锁定输出 token ≤ 16,500；若实测 `agy` 对某工具 `anyOf` 报 schema 错，在 `schema_budget.rs` 按 `omp` 的 `schema.ts` 规则对 `anyOf` 做扁平化投影。 |
| 4 | **长耗时工具（`chrome_run` 最长 3 分钟）触发 `agy` MCP 单次调用超时** | `chrome_run` 未跑完就被 `agy` 侧判 timeout | Phase 2 先实现 15s `notifications/progress` 心跳 + `ContextService` 240s 超时并实测 120s 模拟工具；若 `agy 1.5.15` 仍按墙钟 90s 硬断，Phase 3 在 `mcp_server.rs` 引入类似 `omp` 的 `85s` 后台中继（新增 `nova_await_result` 工具）。 |
| 5 | **`clientInfo.name` 影响 `agy` 返回的模型列表** | 若填错 client 名可能导致模型列表缩水或丢失部分 Claude/Gemini 模型 | Phase 1 用实测脚本对比不同 `clientInfo.name` 下 `session/new` 返回的 `availableModels` 数量（已知基线为 23 个模型），锁定不丢模型的标识。 |
| 6 | **`session/resume` 宣告支持但实际报错，或 `session/load` 重放格式差异** | 续聊失败或界面出现重复历史气泡 | 严格实现 `resume → load(suppress_replay) → new+handoff` 三级降级；Phase 3 用真实 `agy acp` 两次独立进程验证 `resume` 与 `load` 两种路径。 |
| 7 | **`agy` 静默 429 重试循环 / 提示词指纹 429 / 伪正常 `"Usage Limit Reached"`** | 用户界面永久转圈或把配额错误当成正常回复 | 同步落地 `PromptWatchdog` + `ResourceExhaustionTracker` + `is_usage_limit_reply` 检测；`systemPrompt.append` 文案上云前在 3 个主流模型上实测无 429。 |
| 8 | **多会话并发操控屏幕/浏览器冲突** | 两个 Antigravity 会话同时调 `chrome_*`/`jianlai_*` 抢焦点 | 因全部经 `ContextService` 回到主进程，统一复用现有 `lyra::turn::screen_lock()` 串行化屏幕与浏览器操作，与 Lyra 会话亦天然互斥。 |
| 9 | **Windows 路径 / `.cmd` shim / Named Pipe 兼容** | Windows 下 `agy` 可能是 `agy.cmd`，且子进程清理机制与 POSIX 不同 | 复用 `acp::resolve_program_on_path`（`acp.rs:145`）解析可执行文件、`acp::assign_to_agent_job` 绑定 Job Object、`context_service.rs` 现有 Windows Named Pipe 通信代码。 |

### 7.2 无 `webkit2gtk4.1` 环境下的纯 Rust 独立验证方案

在缺少 `webkit2gtk4.1` 的 Linux 主机上，禁止污染仓库内的 `Cargo.toml` / `Cargo.lock`。开发与自测时，在 `/tmp/nova-agy-check/` 建立临时独立 crate，通过 `#[path = "..."]` 直接编译 `src-tauri/src/antigravity/core/` 的真实源文件，并开启 `--offline`（或复用本机 `~/.cargo` 缓存中与 `src-tauri/Cargo.lock` 同版本的 `tokio`/`serde`/`serde_json`/`uuid`/`base64`/`regex`/`libc`）：

```toml
# /tmp/nova-agy-check/Cargo.toml（仅用于本机验证，绝不提交到仓库）
[package]
name = "nova-agy-check"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["v4"] }
base64 = "0.22"
regex = "1"
anyhow = "1"
libc = "0.2"
```

```rust
// /tmp/nova-agy-check/src/lib.rs
#[path = "/home/admin/nova-client/src-tauri/src/antigravity/core/mod.rs"]
pub mod core;
```

**可在本机 100% 离线/无 GTK 完成验证的模块清单**：
- `protocol.rs`：NDJSON 32 MiB 边界、CRLF、`"Opening in existing browser session."` 噪声过滤、非法 JSON 断开
- `process.rs`：命令构造、env 代理/CA 注入、16 KiB stderr 环形缓冲 + 4 KiB 尾部敏感信息脱敏、POSIX PGID 整棵进程树清理
- `connection.rs`：配对 Mock ACP 脚本（几行 `sh`/`python3` stdio 桩）完整验证 `initialize` → `session/new` → `session/update` → `session/request_permission` 自动放行 → `session/cancel` 1.5s 超时强杀
- `mapper.rs`：全部 19 种 `sessionUpdate` 映射、`tool_call_update` 增量合并、`mcp_nova_*` 与 `call_mcp_tool` 前缀还原、`reconcile_usage`（1.5 净输入 vs 1.4.3 含缓存输入）、`⚠` 内联通告拆分
- `watchdog.rs` & `quota.rs`：`tokio::time::pause()` 瞬时测试 10 分钟/60 分钟两档切换、stderr 429 计数与 Go duration 解析
- `schema_budget.rs` & `mcp_server.rs`：直接加载真实 `scripts/jianlai-tools.json` + `scripts/chrome-tools.json` 验证 token 压缩 `≤ 16_500`、RE2 清洗、Unix Socket Mock `ContextService` 回环（含图片 base64 回传与 15s progress 心跳）
- `session_pool.rs`：LRU 驱逐、活跃保护与空闲 TTL

**本机验证命令**：
```bash
# 1. 纯 Rust 协议核心编译 + 全量单元测试（无需 webkit2gtk4.1）
cargo check --manifest-path /tmp/nova-agy-check/Cargo.toml
cargo test  --manifest-path /tmp/nova-agy-check/Cargo.toml

# 2. 真实 agy acp 集成冒烟（按需手动触发）
cargo test  --manifest-path /tmp/nova-agy-check/Cargo.toml -- --ignored --nocapture

# 3. 前端 TypeScript 类型与打包验证
npm run build
```

验证完毕后 `rm -rf /tmp/nova-agy-check`，保持 `/home/admin/nova-client` 工作区整洁无临时文件。
