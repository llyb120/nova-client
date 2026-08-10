# fast_context 16k 冷启动 A/B 补充报告

- 日期：2026-08-10
- 16,000 个已提交 TypeScript 文件
- 每个样本使用全新 Node/N-API 进程与独立 `NOVA_DATA_DIR`
- 不预热；每臂 9 个真正首次调用
- A：`NOVA_CTX_SCALE_V2=0`
- B：`NOVA_CTX_SCALE_V2=1`

## 正式重复轮

| 指标 | A | B | 改善 |
|---|---:|---:|---:|
| median | 972.9 ms | 721.2 ms | **-25.9%** |
| p95 | 1,014.0 ms | 752.6 ms | **-25.8%** |
| 输出字节 | 2,954 | 2,954 | 0 |
| 目标/调用方召回 | 均命中 | 均命中 | 0 |

结论：不预热时，优化臂首次调用中位数约 **0.72 秒**，并没有因冷索引变成数秒；相对未启用 V2 的约 0.97 秒改善约 26%。

## 第一轮冷启动结果

第一轮 median 1,071.8 → 724.8 ms（-32.4%）；A 有一个 2,094 ms 系统抖动样本，导致 p95 失真。正式重复轮中 A/B 分别为 1,014.0/752.6 ms，结论仍一致。

## 口径边界

这里的“冷”是：

- 全新的 fast_context 进程；
- 全新的 codemap/index 数据目录；
- source cache、reverse graph、内存状态均为空；
- 不包含创建 16k fixture、`git init/add/commit` 的时间；
- 操作系统文件页缓存无法完全清空，因此不是重启机器后的物理磁盘冷读。

原始数据：`scripts/fast-context-scale-16k-cold-ab.report.json`
