# Vega（pi core）

Vega 是一个基于 pi agent core 的轻量 coding agent，目标是少往返、低复杂度和默认并行。

## 当前能力

- 使用 `@earendil-works/pi-agent-core` / `pi-ai` / `pi-coding-agent`（0.81+），工具执行策略固定为 `parallel`。
- `edit_files`：两个及以上互不依赖的已有文件走单次并行智能编辑。每个文件先精确匹配，再以稀有行倒排索引生成候选，依次尝试行尾空白、Unicode、相对缩进及保守的行/token 模糊评分；多个近似候选、低置信度或编辑重叠时拒绝。所有目标都在不可变快照上定位成功后才并行写入，并保留 BOM、换行风格和匹配位置的缩进。
- `edit_files` 支持访问工作区内外的相对或绝对路径，便于处理用户明确指定的外部文件。`edit_files` 会合并解析到同一文件的多个目标项，由智能 patch 算法统一判断定位歧义和编辑重叠。
- Skills 使用 pi 的 `loadSkillsFromDir` + Agent Skills 标准目录格式；根目录为 `~/.nova/alkaid/skills`。输入 `/skill:<name>` 可补全并显式调用 skill；普通任务中模型也可按需用 `read` 加载完整 `SKILL.md`（不再提供自定义 `load_skill` 工具）。
- 系统提示词：Vega 策略（批量编辑、最小读取、改后验证、shell 语法约束）为稳态前缀；`cwd` / skills 目录为动态后缀，便于 provider prompt/KV cache 命中。skills ≥ 4 时压缩目录体积。
- 命令终端：Windows 直接使用 PowerShell（`System32` 自带或 PATH 上的 `powershell.exe`，桌面端启用 shell shim 时按 kind 经 `NOVA_SHELL_SHIM_POWERSHELL` / `NOVA_SHELL_SHIM_BASH` 无窗口启动），找不到 PowerShell 时兜底回退 bash 探测；macOS / Linux 维持 bash。
- Provider 缓存：默认 `cacheRetention: "long"`，为 OpenAI 兼容请求补齐 `prompt_cache_key`（session id）；第三方 OpenAI/Anthropic 兼容代理默认开启 `sendSessionAffinityHeaders`（不覆盖用户显式配置）。OpenAI Responses/Chat Completions 支持在 provider 或 model 的 `options.serviceTier` 配置 `priority`、`default`、`flex` 等值，并转发为请求体的 `service_tier`。
- 支持并行连接多个 MCP stdio server，并把工具映射为 `mcp__<server>__<tool>`。
- 本机配置读取 `~/.nova/alkaid/config.jsonc`（OpenCode 风格），可与服务端下发配置合并；密钥仅从进程环境 / `{env:NAME}` 解析。

### OpenAI Priority Service Tier

对支持 OpenAI `service_tier` 的 provider，在 `options` 中加入：

```jsonc
{
  "options": {
    "baseURL": "https://api.openai.com/v1",
    "apiKey": "{env:OPENAI_API_KEY}",
    "serviceTier": "priority"
  }
}
```

也可以把 `serviceTier` 放在单个 model 的 `options` 中，以覆盖 provider 默认值。若希望同时保留普通枚举和 Fast 枚举，可以直接把它放在 `variants` 中：

```jsonc
{
  "model": "codex/gpt-5.6-sol/variant/medium",
  "provider": {
    "codex": {
      "options": { "baseURL": "https://api.openai.com/v1", "apiKey": "{env:OPENAI_API_KEY}" },
      "models": {
        "gpt-5.6-sol": {
          "options": { "reasoningEffort": "medium" },
          "variants": {
            "medium": { "reasoningEffort": "medium" },
            "high": { "reasoningEffort": "high" },
            "fast": {
              "name": "Fast",
              "reasoningEffort": "medium",
              "serviceTier": "priority"
            }
          }
        }
      }
    }
  }
}
```

这样模型列表会保留 `medium`、`high` 和 `fast` 三个枚举；只有选择 `.../variant/fast` 时才发送 `"service_tier": "priority"`。Vega 会对正常对话、标题/补全请求以及 Rust 直连的行内补全转发该字段；未配置时不发送。代理必须自行支持并转发此参数。
- 已作为独立的 `alkaid` 后端接入桌面端；后端选择顺序为“收藏 → Vega → 其他后端”。
- 会话消息持久化到 `~/.nova/alkaid/sessions`，支持跨 bridge 进程续接多轮上下文。开启“超级上下文”时，OpenAI/GPT 已完成轮次不再回传 reasoning；当前轮次及中断后续做所需的原生工具轨迹仍完整保留。重组后的提示词/结论上下文按实际用量或保守估算计数，达到模型窗口 60% 时压缩，且压缩阈值最低为 150k tokens；优先使用设置中的轻量级模型，失败后回退当前会话模型。
- Plan 模式不暴露写文件工具；Build 模式开放并行读写。

## 团队 / Relay 与额度共享

Relay 不再向客户端下发或合并 Vega provider/model 配置。Vega 始终只读取本机 `~/.nova/alkaid/config.jsonc`（开发版为 `~/.novadev/alkaid/config.jsonc`）。

额度共享仍然可用：出借方通过本机配置导出当前 Vega 配置，并经端到端加密凭证链路提供给借用方；借用方只在隔离的临时运行目录中使用，不会覆盖本机 Vega 配置或登录状态。

## 本机运行

当前机器的 provider 凭据变量必须已注入当前 shell。Vega 不会读取、打印或保存密钥值。

```bash
npm run alkaid -- --prompt "请只回复 Vega OK"
```

也可以通过 stdin 传入一行 JSON，同时配置 MCP：

```json
{
  "prompt": "调用 echo 工具",
  "cwd": "/path/to/project",
  "mcpServers": {
    "echo": {
      "command": "node",
      "args": ["/path/to/mcp-server.mjs"],
      "env": {}
    }
  }
}
```

输出为 NDJSON，事件包括 `ready`、`text_delta`、`tool_start`、`tool_end`、`done` 和 `error`，便于后续接入 Nova 的 Tauri manager。

## 验证

```bash
npm run test:alkaid
npm run check
npm run build
```

单测覆盖批量读写、智能锚点匹配、缩进保持、模糊歧义拒绝、跨文件写前验证、路径安全、Skills 发现、prompt 稳态结构、缓存 compat，以及 MCP stdio 的工具发现与调用。
