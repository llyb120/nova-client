# fast_context 大仓库规模优化 A/B 最终报告

- 日期：2026-08-10
- 分支：`feat/fast-context-large-repo-ab`
- Worktree：`D:/code/nova-client-fast-context-large-repo-ab`
- 基线：`c9d742688accf3695c0bab5afe9e8eb86bd89a6a`
- 总开关：`NOVA_CTX_SCALE_OPT`（A=`0`，B=`1`）

## 实施内容

1. 保留 scope-first：优化臂把目录直接交给单个带代码扩展名 glob 的 rg；基线臂展开文件后每 128 文件启动一次 rg。没有恢复曾导致全仓退化的 merged full-repo search。
2. 持久化/增量 reverse import graph：优化臂无变化复用，小变化增量补边；基线臂每次全量重建。
3. Lazy Greedy：保持原候选价值函数不变，以最大堆重算边际收益替代 O(n²) 全量扫描，并加入稳定 index tie-break。
4. Demand-only source cache：32 MiB 总字节上限、单文件最大 8 MiB、size+mtime 校验、LRU 淘汰；没有预测预取和后台线程。
5. 未恢复在线学习、SPECULATIVE、跨调用预测预取、merged full-repo rg。

## 确定性仓库规模 A/B

合成仓库分别为 1,000 / 4,000 / 8,000 个 TypeScript 文件；每臂预热一次，随后 5 次，报告中取中位数。查询和输出质量相同，均命中目标与调用方。

| 文件数 | A 中位数 | B 中位数 | B-A |
|---:|---:|---:|---:|
| 1,000 | 91.3 ms | 85.0 ms | **-6.8%** |
| 4,000 | 224.7 ms | 226.3 ms | +0.7% |
| 8,000 | 427.7 ms | 432.6 ms | +1.1% |

判断：当前实现消除了明显的大仓灾难性退化，但在 4k/8k fixture 上没有获得可确认的工具侧收益；约 1% 属噪声区间。主要原因是 fixture 的主成本仍是初始文件枚举和首次关键词 rg，三个优化只覆盖二次 scoped search、重复 source 读取、reverse graph 和候选排序。

因此，**不能宣称大仓工具延迟已经显著优化**；可以确认的是优化臂没有重现 revert 前“随仓库规模严重恶化”的问题。

## Lyra + DeepSeek 配对 A/B

- 运行时：Rust 原生 Lyra CLI（`.cargo-target/debug/nova.exe lyra`），不是 Alkaid/Vega Node bridge。
- 模型：`opencode/deepseek-v4-flash/variant/high`
- 用例：scope / reverse graph / packing，各 3 轮；A/B 同时启动。
- 每个会话严格只调用一次 fast_context，零补 read、零额外检索。

### 汇总

| 指标 | A | B | B-A |
|---|---:|---:|---:|
| 配对会话 | 9 | 9 | 0 |
| 总 wall time | 362,833 ms | 306,539 ms | **-15.5%** |
| 总 tokens | 90,982 | 83,245 | **-8.5%** |
| 答案关键词召回 | 100% | 94.4% | **-5.6pp** |
| fast_context 调用 | 9 | 9 | 0 |

### 各类 wall time 中位数

| 用例 | A | B | B-A |
|---|---:|---:|---:|
| scoped search | 43,450 ms | 37,306 ms | **-14.1%** |
| reverse graph | 27,032 ms | 25,336 ms | **-6.3%** |
| packing | 42,423 ms | 26,782 ms | **-36.9%** |

质量回归来自 packing 第 3 轮：B 的答案只提到 `UnitCandidate`，没有复述 `scale_optimizations_enabled`，所以该 case 关键词召回 50%；工具调用本身成功且只调用一次。这更像 DeepSeek 单次输出随机性，但按保守口径仍计为回归。

判断：模型级 wall/token 信号正向，但样本只有 9 对且存在一次答案召回回归，**尚不足以作为默认上线依据**。工具侧规模 benchmark 又显示 4k/8k 基本持平，因此模型 wall 改善可能混有 provider 输出随机性，不能全部归因于 fast_context。

## 验证

- `cargo check --manifest-path src-tauri/Cargo.toml --lib`：通过（4 个已有 warning）。
- `cargo test --manifest-path src-tauri/Cargo.toml --lib nova_tools_native::context::tests -- --test-threads=1`：**28/28 通过**。
- `git diff --check`：通过。
- N-API addon 构建：通过。
- 原生 Lyra debug binary 构建：通过；MinGW linker 有既有 `.rsrc merge` warning，但产物可运行。

## 最终结论

1. scope-first / incremental graph / demand cache / Lazy Greedy 的组合是安全方向，没有恢复旧版 merged full-repo rg、学习和预测预取。
2. 当前版本在 1k 仓快 6.8%，4k/8k 与基线基本持平（+0.7%/+1.1%），未达到“大仓 warm p95 改善 20%”目标。
3. Lyra DeepSeek A/B 显示 wall -15.5%、tokens -8.5%，但召回 -5.6pp 且样本小，结论只能算正向信号。
4. **建议暂不合并为默认路径**。下一轮应直接优化主成本：持久化文件清单/内容倒排索引、避免每次初始全仓 rg，以及让 Lazy Greedy 按输出 budget 早停，而不是继续增加学习/预取复杂度。

原始数据：
- `scripts/fast-context-scale-ab.report.json`
- `scripts/fast-context-scale-lyra-deepseek-ab.report.json`
