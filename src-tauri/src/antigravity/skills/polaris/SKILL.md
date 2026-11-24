---
name: polaris
description: 定位仓库中的代码、依赖与符号。
---
在当前会话的工作目录检索，不要把其他项目的结果套入本仓库。先带任务描述与有区分度的关键词调用 `polaris`，需要目录概览时调用 `code_map`。结果是候选源码而非全仓证明。

例：调用 `mcp_nova_polaris`，参数为 `{"tool":"polaris","input":"{\"task\":\"定位会话模型选择\",\"keywords\":[\"model\"]}"}`。只传 `tool` 不传 `input` 可查看该操作的参数。
