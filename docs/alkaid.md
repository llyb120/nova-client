# Alkaid（pi core）

Alkaid 是一个基于 pi agent core 的轻量 coding agent，目标是少往返、低复杂度和默认并行。

## 当前能力

- 使用 `@earendil-works/pi-agent-core` / `pi-ai` / `pi-coding-agent`（0.81+），工具执行策略固定为 `parallel`。
- `read_files`：两个及以上路径已知的 UTF-8 文本文件优先走单次并行读取。
- `edit_files`：两个及以上互不依赖的已有文件优先走单次并行精确编辑，复用原生 `edit` 的唯一、非重叠文本替换语义。
- 文件工具限制在当前工作区内，拒绝目录穿越和重复写目标。
- Skills 使用 pi 的 `loadSkillsFromDir` + Agent Skills 标准目录格式；根目录为 `~/.nova/alkaid/skills`。模型按需用 `read` / `read_files` 加载完整 `SKILL.md`（不再提供自定义 `load_skill` 工具）。
- 系统提示词：Alkaid 策略（批量读写、最小读取、改后验证、Bash）为稳态前缀；`cwd` / skills 目录为动态后缀，便于 provider prompt/KV cache 命中。skills ≥ 4 时压缩目录体积。
- Provider 缓存：默认 `cacheRetention: "long"`，为 OpenAI 兼容请求补齐 `prompt_cache_key`（session id）；第三方 OpenAI/Anthropic 兼容代理默认开启 `sendSessionAffinityHeaders`（不覆盖用户显式配置）。
- 支持并行连接多个 MCP stdio server，并把工具映射为 `mcp__<server>__<tool>`。
- 本机配置读取 `~/.nova/alkaid/config.jsonc`（OpenCode 风格），可与服务端下发配置合并；密钥仅从进程环境 / `{env:NAME}` 解析。
- 已作为独立的 `alkaid` 后端接入桌面端；后端选择顺序为“收藏 → Alkaid → 其他后端”。
- 会话消息持久化到 `~/.nova/alkaid/sessions`，支持跨 bridge 进程续接多轮上下文。
- Plan 模式不暴露写文件工具；Build 模式开放并行读写。

## 服务端配置同步

`nova-server` 可按团队 token 前缀通过 v2 WebSocket 定向下发 Alkaid 配置。客户端收到后只保存在运行内存，并按“服务端配置为基线、本地 `~/.nova/alkaid/config.jsonc` 递归覆盖”的规则合并；不会把服务端配置或合并结果写回磁盘。服务端配置变化会清空并重新探测 Alkaid 模型列表，当前正在执行的轮次不被打断，后续 bridge 使用新配置。

服务端配置中的密钥建议继续写成 `{env:NAME}`；对应环境变量必须注入客户端 Nova 进程，服务端不会代替客户端保存运行凭据。

## 本机运行

当前机器的 provider 凭据变量必须已注入当前 shell。Alkaid 不会读取、打印或保存密钥值。

```bash
npm run alkaid -- --prompt "请只回复 Alkaid OK"
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

单测覆盖批量读写、路径安全、Skills 发现、prompt 稳态结构、缓存 compat，以及 MCP stdio 的工具发现与调用。
