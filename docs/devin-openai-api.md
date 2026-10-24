# Devin → OpenAI 兼容接口

本地服务直接调用 Devin CLI 使用的 Connect/Protobuf 模型接口，复用已有 Devin 登录。模型推理不再经过 ACP summarizer，也不会在本机执行模型提出的工具。CLI 只用于读取可用模型、版本和默认模型设置。

Lyra 使用 Rust 原生实现，协议基于 Devin CLI 3000.11.205 验证，没有新增依赖。Nova 已移除 Devin 独立 ACP 后端，但保留此 Lyra provider，以及下方供其他客户端使用和协议验证的可选 Node.js 兼容服务。

## Lyra 自动接入

在「设置 → Lyra → 添加 Provider」中选择 **Devin（本地 API）** 后保存即可，无需填写地址、协议或 API Key，也无需运行 npm 命令。Nova 在 Rust 进程内直接请求 Devin 的 Connect/Protobuf 接口，自动读取本机 CLI 登录，不启动 Node 代理或监听本地端口。

需要本机已有已登录的 Devin CLI。CLI 路径沿用 Nova 的 Devin 设置，网络跟随系统代理。Lyra 自动使用 `devin` 原生协议；「获取模型」读取 CLI 目录。共享配置只需 `{"provider":{"devin":{"preset":"devin"}}}`，不依赖 Node.js、项目目录、`devin-api.json` 或固定端口。输入补全和标题生成也使用同一 Rust provider。

不同思考档位直接选择对应模型 ID（如 `gpt-5-5-high`）；模型名称、上下文和输出限制从 Devin CLI 目录读取。目录未声明的图片能力可在模型设置中显式开启；`gpt-5-5-none` 已实测支持图片。

Lyra 的函数工具和历史结果编码为原生协议字段，支持流式文字/思考、并行工具、图片、用量和取消。工具参数在 Connect trailer 完整、JSON 和工具名称验证通过后才交给 Lyra 执行。缓存路由由 Devin 决定；思考档位使用对应模型 ID。

原生回归检查：`cargo test --manifest-path src-tauri/Cargo.toml --lib lyra`。覆盖 protobuf、分块/gzip、工具交接状态码 10、并行参数、截断和取消；本机已验证文本、真实工具往返、图片、标题和输入补全。

## 其他客户端的独立服务（可选）

先确认 `devin models list` 能返回模型；没有登录则运行 `devin auth login`。需要凭据文件中的 `windsurf_api_key`，服务只读原始凭据，不复制或刷新 OAuth token。

```powershell
npm run devin:api
```

- Base URL：`http://127.0.0.1:8787/v1`
- 其他客户端可从 `~/.nova/alkaid/devin-api.json` 读取本地访问密钥（不是 Devin 登录凭据）
- 模型：`devin` 使用配置的默认模型；或选择 `GET /v1/models` 返回的实际模型 ID
- 协议：Chat Completions 或 Responses

独立服务首次启动自动生成密钥，重启复用，且不会在日志中打印密钥。连接信息保存到 `NOVA_DATA_DIR/alkaid/devin-api.json`（默认 `~/.nova`）；Lyra 原生 provider 不启动或依赖此服务。

## 已支持能力

| 功能 | 行为 |
| --- | --- |
| 聊天 | `/v1/chat/completions`，普通 JSON、SSE、多轮消息 |
| 函数调用 | 原生 `tools`、`tool_calls`、参数增量、`role: tool` 结果回传和继续生成 |
| 工具选择 | `auto`、`none`、`required`、指定函数；支持并行调用及 `parallel_tool_calls: false` |
| 图片 | user/tool 消息中的 `image_url`；PNG/JPEG/GIF/WebP，base64 data URL 或公开 HTTPS URL |
| Responses | `/v1/responses`，文本/图片输入、`function_call` / `function_call_output`、普通及流式事件、`previous_response_id` |
| 生成参数 | `temperature`、`top_p`、`max_tokens` / `max_completion_tokens`；Responses 的 `max_output_tokens` |
| 推理强度 | `reasoning_effort` / Responses `reasoning.effort`，选择对应 Devin 模型变体；没有该变体则报错 |
| 结构化输出 | `response_format: json_object/json_schema`，或 Responses `text.format`；输出通过 Schema 校验后返回 |
| 停止词 | `stop`，支持跨流片段匹配，命中即关闭上游流 |
| 多结果 | Chat Completions `n: 1..4`，按顺序独立生成 |
| 用量 | 上游真实 input/output/cache token 数，支持 `stream_options.include_usage: true` |

`system` 和 `developer` 消息合入上游 system 提示；user、assistant、tool 使用原生角色。请求中只提供客户端声明的函数，不注入 Devin 自带的文件、Shell、MCP 或项目工具。

## 函数调用示例

向 `/v1/chat/completions` 发送：

```json
{
  "model": "gpt-5-5-none",
  "messages": [{"role": "user", "content": "上海天气如何？"}],
  "tools": [{
    "type": "function",
    "function": {
      "name": "get_weather",
      "description": "查询城市实时天气",
      "parameters": {
        "type": "object",
        "properties": {"city": {"type": "string"}},
        "required": ["city"],
        "additionalProperties": false
      }
    }
  }],
  "tool_choice": "required",
  "stream": true
}
```

客户端收到 `tool_calls` 后执行函数，将完整 assistant 消息和对应结果加入原有 messages，再次请求即可：

```json
{"role": "tool", "tool_call_id": "调用返回的 call_id", "content": "晴，23°C"}
```

多个工具调用必须分别提交结果，不能遗漏、重复或错配 call ID。代理负责协议转发，实际工具由调用方执行。

Responses 的工具定义采用平铺格式 `{"type":"function","name":"get_weather","parameters":{...}}`。续传结果使用：

```json
{
  "previous_response_id": "上一轮返回的 resp_...",
  "input": [{"type": "function_call_output", "call_id": "上一轮返回的 call_...", "output": "晴，23°C"}]
}
```

## 行为边界

- 上游是私有协议，当前字段已对照 CLI 3000.11.205 的实际请求验证。Devin 更新协议或账户策略时可能需要同步调整；模型、额度和计费继续由 Devin 决定。
- 原生工具定义及调用来自上游。该后端没有独立 `tool_choice` 开关：适配器通过筛选函数、附加要求和结果校验实施 required/指定函数/禁用并行；模型不遵守时返回错误，不伪造调用。
- JSON Schema 输出通过一个内部结构化函数获取，验证成功后转换为 JSON 文本。因此结构化输出会先缓冲再发出；不承诺提供供应商原生 grammar decoding。
- 上游输出 token 下限实测为 32，所以 token 限制接受 `32..128000`。具体模型仍可能采用更低上限或拒绝某些采样参数。`n > 1` 会产生多次计费请求。
- 图片的 detail 仅支持 `auto`；图片 URL 不允许内网、localhost、非 HTTPS 或带凭据地址，重定向也逐次验证。单图上限 16 MiB，总请求上限 32 MiB。
- `previous_response_id` 只在当前进程保存，最多 100 条、64 MiB、1 小时；重启或淘汰后需重传历史。`store: false` 不保存，调用方可自行携带完整历史。
- 没有上游用量时（例如 stop 提前取消），`usage` 为 null；不估算 token 数。没有独立 reasoning token 统计，不填假值。Chat 的可见推理文本通过 `reasoning_content` 返回。
- 尚未实现音频/视频、OpenAI 托管 web_search/file_search/computer 工具、Responses 后台任务、自动截断、远端持久化及音频/文件 API。这些不是 Devin 模型接口提供的对应服务。其余未知参数会报错。
- 错误流不发送成功结束事件：Chat SSE 不会补发 `[DONE]`；Responses 返回 `error` 事件。请求不自动重试、不回退模型。客户端断开或超时会关闭上游请求。

## 可选配置与检查

| 环境变量 | 默认值 | 用途 |
| --- | --- | --- |
| `NOVA_DEVIN_API_KEY` | 首次自动生成，后续复用 | 可选覆盖独立服务的本地接口访问密钥 |
| `NOVA_DATA_DIR` | `~/.nova` | 独立服务保存本地连接信息的数据目录 |
| `NOVA_DEVIN_BIN` | `devin` | CLI 可执行文件路径 |
| `NOVA_DEVIN_HOST` | `127.0.0.1` | 监听地址；局域网可改 `0.0.0.0` |
| `NOVA_DEVIN_PORT` | `8787` | HTTP 端口 |
| `NOVA_DEVIN_TIMEOUT_MS` | `180000` | 请求总超时，毫秒 |

默认最大并发为 2，超出返回 429。HTTP(S) 代理使用近期 Node.js 24 的原生 `HTTP_PROXY` / `HTTPS_PROXY` / `NO_PROXY` 支持；图片下载单独固定到已验证的公网地址。默认不启用 CORS，公网使用时由前置代理提供 HTTPS。

```powershell
npm run test:devin-api
```

回归检查使用本地模拟 Connect 服务，验证原生请求字段、工具循环、图片映射、参数、JSON Schema、Responses 流、压缩帧、错误脱敏、取消和超时，不调用收费模型。真实验证已覆盖函数调用及回传、红色图片识别、JSON Schema、Responses 流式工具调用和续传结果。
