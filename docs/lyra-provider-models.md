# Lyra 自动模型配置

核对日期：2026-10-08。可用模型以 provider 的 `/models` 为准，models.dev 补充能力；成功返回空列表时不补回目录模型。官方没有提供模型列表的端点继续使用目录回退。手写的同名模型仍完整覆盖缓存，私有 `options` 不被刷新改写。

## 已核对的官方资料

| Provider | 官方依据 | 本次处理 |
| --- | --- | --- |
| DeepSeek | [Thinking Mode](https://api-docs.deepseek.com/guides/thinking_mode) | `thinking.type`、`reasoning_effort` 分开；工具调用完整回传 `reasoning_content`，不插入换行；JSON 回退也保留思考内容。 |
| 智谱 / Z.AI（含 Coding Plan） | [智谱思考模式](https://docs.bigmodel.cn/cn/guide/capabilities/thinking-mode)、[Z.AI Thinking Mode](https://docs.z.ai/guides/capabilities/thinking-mode)、[Deep Thinking](https://docs.z.ai/guides/capabilities/thinking) | 按模型能力区分开关与 effort；GLM-5.3 不发关闭思考参数。`clear_thinking` 不免除工具调用链的思考回传要求。 |
| Kimi / Moonshot | [Model Parameter Reference](https://platform.kimi.ai/docs/api/models-overview)、[Reasoning Effort](https://platform.kimi.ai/docs/guide/use-reasoning-effort) | K2.5/K2.6 使用 `thinking.type`；K2.7 Code 强制思考；K3 使用 `reasoning_effort`。不发送不受支持的字段，固定采样值由服务端决定。 |
| 阿里云百炼 | [深度思考](https://help.aliyun.com/zh/model-studio/deep-thinking)、[Coding Plan](https://help.aliyun.com/zh/model-studio/coding-plan) | Qwen 使用 `enable_thinking`；仅思考模型不发开关；`thinking_budget` 等手写参数保留。百炼托管和转售模型不能一概继承原厂开关。 |
| MiniMax | [Anthropic SDK](https://platform.minimax.io/docs/api-reference/text-anthropic-api)、[OpenAI SDK](https://platform.minimax.io/docs/api-reference/text-openai-api) | 使用 `adaptive`，不自动生成 Claude 的 `budget_tokens`；M3 可关闭思考，M2/M3.1 不关闭。仅 M3.1 Flash Preview 发送 effort；Anthropic 协议保留无签名思考块。 |
| SiliconFlow | [Chat Completions](https://docs.siliconflow.com/en/api-reference/chat-completions/chat-completions) | 官方列出的混合模型使用 `enable_thinking`；DeepSeek-V3.1 的工具调用固定非思考模式。 |
| OpenRouter | [Reasoning Tokens](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens)、[Models API](https://openrouter.ai/api/v1/models) | 读取 `reasoning.supported_efforts/default_effort/mandatory`、输出限制及输入模态；发送统一 `reasoning` 对象，保留并回传同模型的 `reasoning_details`。 |
| Command Code | [Provider API](https://commandcode.ai/docs/provider)、[模型接口](https://api.commandcode.ai/provider/v1/models) | 按 `supported_endpoints` 选协议；不把原厂私有字段直接复制给网关。 |
| OpenCode Zen / Go | [Zen](https://opencode.ai/docs/zen/) | 保留逐模型协议映射；目录补齐 effort 与开关能力。 |
| OpenAI | [Reasoning models](https://developers.openai.com/api/docs/guides/reasoning) | GPT-6 Astra / GPT-6.1 Sol 的工具调用选择 Responses；保留 `reasoning.mode/context/summary` 和自定义 `include`，不再被默认参数覆盖。 |
| Anthropic | [Thinking steering and cost](https://platform.claude.com/docs/en/build-with-claude/thinking-steering-and-cost)、[Effort](https://platform.claude.com/docs/en/build-with-claude/effort) | 结合模型能力配置自适应思考和真实 effort，保留旧模型的 budget 模式；Opus/Haiku 5.5 默认选 medium。 |
| 火山方舟 | [深度思考](https://docs.volcengine.com/docs/ark/deep-thinking?lang=zh) | `thinking.type` 与逐模型 effort；保留 `max_completion_tokens`；Chat 协议收集 `encrypted_content` 并原样回传给同一模型。 |
| 腾讯云 Coding Plan | [Coding Plan 概述](https://cloud.tencent.com/document/product/1772/128947) | 已核对专属 Base URL；模型库动态变化，以返回列表为准，未根据模型名猜测厂商私有开关。 |

## 能力与边界

Token 分项计数、Responses 续接、持久命令、子 agent 恢复和 MCP 配置见 [Lyra 运行时](lyra-runtime.md)。

- Devin / CodeBuddy / WorkBuddy（本地 API）由 Rust 直接请求上游，无需填写 API Key、安装 Node 或手动启动代理。自动读取本机登录、模型目录，网络跟随系统代理，支持工具往返、流式输出及输入补全；WorkBuddy 加密登录通过其桌面版自带运行时读取。见 [Devin](devin-openai-api.md) / [CodeBuddy、WorkBuddy](codebuddy-openai-api.md)。
- 本地 Codex / Claude Code 统一作为 Lyra provider 使用，不再提供独立 Agent 后端。优先读取 CLI 用户级自定义 API（地址、Key、认证头）；未配置 API 时，由 Lyra 内置转发读取本机订阅登录，无需 Node 或手动启动反代服务。显式自定义地址即使没有 Key 也不回退订阅，避免把订阅凭据发送到第三方。
- 订阅模式使用固定官方上游、不跟随重定向，凭据仅在请求时读取，不写入 Lyra 配置或模型缓存；网络跟随系统代理。支持 Codex `auth.json`、Claude Code `CLAUDE_CODE_OAUTH_TOKEN` / `.credentials.json` 及 macOS 对应 keychain；不执行 `apiKeyHelper`，不支持云平台认证或 Codex 的非 macOS keyring/内存登录。令牌由原 CLI 管理，过期时需先在 CLI 刷新登录；Lyra 不竞争轮换 refresh token。
- 获取模型时，Codex 订阅读取账号 Codex 目录，自定义 API 与 Claude Code 读取完整 `/models`（支持 Anthropic 分页），再合并本地模型与思考参数。Claude 订阅的 `sonnet` / `opus` / `haiku` 等别名由目录中的实际模型解析，首次使用别名前需要获取模型；失败保留旧缓存。对话、工具往返和补全共用订阅转发路径。
- 旧导入项在设置页自动转为对应预设，保存后生效；保留原 ID、默认模型及手写覆盖。JSONC 去注释/尾逗号时保留原始 UTF-8 字节，中文名称、路径不再二次转码；Lyra 冷启动从当前配置重新生成模型名称，替换旧乱码缓存。

- `options.supportsThinkingToggle: false`：强制思考模型，普通对话、标题、补全均不能自动发送 `disabled` / `none`。
- `options.supportsReasoningEffort: false`：不自动发送模型不支持的 effort。
- `thinkingFormat` 按接入端点生成；未知 OpenAI 兼容网关仅补能力，用户可显式配置其实际接受的参数。
- OpenAI、Anthropic、火山方舟文档通过浏览器读取。腾讯 Token Plan / TokenHub、百炼第三方模型、Kimi Coding 专属模型的额外私有约束尚需各自接口资料确认；不按原厂模型名推断这些转售/专属端点的参数。
- 本地订阅转发通过离线认证、请求构造和本机模拟服务测试验证；未使用真实订阅账号发起推理，实际可用性仍取决于账号权限及上游兼容性。

## 旧后端选择迁移

- 启动时自动迁移可确认的旧会话历史、场景模型、模型收藏和 `selectModel` 快捷键；前端初始化时迁移 local storage 中的最近模型、思考档位及收藏。两端使用同一模型映射，保留原模型和 effort，必要时编码为 Lyra 的 `/variant/<effort>`，不以默认模型替换原选择。
- Codex / Claude Code / Devin / CodeBuddy / WorkBuddy 使用对应 Lyra 本地 provider；已有兼容配置可复用，冲突配置不会被覆盖。迁移成功的会话由 Lyra 继续，未完成迁移的来源保留为不可执行的 `Legacy`，不再启动已删除的独立后端。
- 不迁移 Kimi / Cursor。OpenCode 仅在 Lyra 已有对应 provider、model 和 variant，且无无法确认的独立 effort 时迁移。其他支持迁移的旧后端若来源不明、别名未解析、旧 Auto 缺少实际绑定或 effort 未知，则保留原选择并提示待处理，不凭模型名称、前缀或 SDK Key 猜测 API。
- 新建的 Lyra 本地 provider 跟随系统代理，不继承旧后端代理设置；Claude `opusplan` 无已确认等价模型，保留原选择待处理。`alkaid` 视为 Lyra 旧名直接改名；缺少来源后端的最近模型不迁移，只提示一次。
- 迁移不删除用户 CLI 配置或凭据。无法自动完成的会话：配置对应 Lyra provider 后，在该会话的模型选择器中选择一个 Lyra 模型即可改绑（历史保留）；漫游客机历史由实际执行主机负责迁移。自动迁移不代表所有旧模型或登录方式均已支持。

运行检查：`cargo test --manifest-path src-tauri/Cargo.toml --lib lyra -- --test-threads=1`。
