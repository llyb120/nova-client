# FastContext 原生常驻与 mmap 增量索引 A/B 报告

## 结论

使用 OpenCode 的 `deepseek-v4-flash`（high variant）完成 4 个软件工程检索任务的模型级 A/B：

- A：修改前基线可执行文件
- B：最终原生常驻 FastContext、启动 mmap 预载、Super 回退 Fast 的实现
- 两组均强制 `contextMode=fast`，每个任务只允许调用一次 `fast_context`

B 保持了召回质量：两组关键词召回均为 100%，平均路径召回均为 75%，4/4 均成功。B 的模型端到端平均耗时从 84.50 秒降至 32.71 秒，下降 61.29%；总 token 从 57,694 降至 54,751，下降 5.10%。

## 汇总

| 指标 | A 基线 | B 最终实现 | 变化 |
|---|---:|---:|---:|
| 成功任务 | 4/4 | 4/4 | 持平 |
| 平均端到端耗时 | 84,500.25 ms | 32,709.5 ms | -61.29% |
| 平均关键词召回 | 100% | 100% | 持平 |
| 平均路径召回 | 75% | 75% | 持平 |
| 总 token | 57,694 | 54,751 | -5.10% |

## 用例

1. 原生常驻服务路由：ContextService、Lyra 直连、bridge 客户端。
2. mmap 增量索引：启动预载、查询期内存缓存、增量写回。
3. SuperContext 回退：旧 super 配置迁移与 Fast 路由。
4. 反向 import 图兼容性：完整缓存图、别名与 barrel 调用方发现。

## 限制

本机历史会话目录中没有可重放的 `fast_context` 工具记录，因此评测脚本使用了 4 个固定工程任务，而不是历史会话回放。A/B 使用同一 OpenCode DeepSeek 模型并并行运行；报告衡量模型端到端行为，结果包含模型服务波动，不等同于纯工具微基准。原始逐用例数据见 `fast-context-native-resident-deepseek-ab.report.json`。
