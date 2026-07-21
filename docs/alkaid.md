# Alkaid（pi core）

Alkaid 是一个基于 pi agent core 的轻量 coding agent，目标是少往返、低复杂度和默认并行。

## 当前能力

- 使用维护中的 `@earendil-works/pi-agent-core`，工具执行策略固定为 `parallel`。
- `read_files`：单次并行读取多个 UTF-8 文件。
- `edit_files`：单次并行精确编辑多个互不依赖的已有文件，复用原生 `edit` 的唯一、非重叠文本替换语义。
- 文件工具限制在当前工作区内，拒绝目录穿越和重复写目标。
- 启动时发现 `~/.nova/skills`、`~/.agents/skills`、`~/.codex/skills`；只把目录注入提示词，使用时再通过 `load_skill` 读取完整 `SKILL.md`。
- 支持并行连接多个 MCP stdio server，并把工具映射为 `mcp__<server>__<tool>`。
- 自动读取本机 `~/.codex/config.toml` 的模型、provider URL 和 `env_key`，密钥仅从进程环境读取。
- 已作为独立的 `alkaid` 后端接入桌面端；后端选择顺序为“收藏 → Alkaid → 其他后端”。
- 会话消息持久化到 `~/.nova/alkaid-sessions`，支持跨 bridge 进程续接多轮上下文。
- Plan 模式不暴露写文件工具；Build 模式开放并行读写。

## 本机运行

当前机器的 Codex provider 凭据变量必须已注入当前 shell。Alkaid 不会读取、打印或保存密钥值。

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

单测覆盖批量读写、路径安全、Skills 发现/加载，以及 MCP stdio 的工具发现与调用。
