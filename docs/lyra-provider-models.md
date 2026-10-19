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

- 本地 Codex / Claude Code 与其它 provider 共用添加、获取模型和缓存流程，无需在 Lyra 中填写 Key。运行时读取 CLI 的用户级 API 配置，始终请求完整 `/models` 列表（支持 Anthropic 分页），再合并本地选择及其私有参数；列表不可用时保留本地已配置模型。仍需 CLI 已配置 API 认证，订阅登录暂不支持。
- 旧导入项在设置页自动转为对应预设，保存后生效；保留原 ID、默认模型及手写覆盖。JSONC 去注释/尾逗号时保留原始 UTF-8 字节，中文名称、路径不再二次转码；Lyra 冷启动从当前配置重新生成模型名称，替换旧乱码缓存。

- `options.supportsThinkingToggle: false`：强制思考模型，普通对话、标题、补全均不能自动发送 `disabled` / `none`。
- `options.supportsReasoningEffort: false`：不自动发送模型不支持的 effort。
- `thinkingFormat` 按接入端点生成；未知 OpenAI 兼容网关仅补能力，用户可显式配置其实际接受的参数。
- OpenAI、Anthropic、火山方舟文档通过浏览器读取。腾讯 Token Plan / TokenHub、百炼第三方模型、Kimi Coding 专属模型的额外私有约束尚需各自接口资料确认；不按原厂模型名推断这些转售/专属端点的参数。
- 本次使用离线单元测试与公开文档/模型列表验证，未使用用户 API Key 发起付费推理请求。

运行检查：`cargo test --manifest-path src-tauri/Cargo.toml --lib lyra -- --test-threads=1`。
