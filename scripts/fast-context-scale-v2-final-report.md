# fast_context 大仓库规模优化第二轮 A/B 报告

- 日期：2026-08-10
- 分支：`feat/fast-context-large-repo-ab`
- Worktree：`D:/code/nova-client-fast-context-large-repo-ab`
- 第一轮提交：`c4fe94b`
- 第二轮 A/B：A=`NOVA_CTX_SCALE_V2=0`，B=`NOVA_CTX_SCALE_V2=1`；两臂均保留第一轮 `NOVA_CTX_SCALE_OPT=1`

## 第二轮改动

第一轮 profile 显示：8k fixture 的 `search_and_files` 约 258 ms，占 warm 总耗时约 62%；reverse graph/source cache/Lazy Greedy 已不是主瓶颈。

第二轮只优化初始全仓文本检索：

1. 对 Git index 大于 256 KiB 的仓库，未指定 `files` 的全仓搜索优先使用 `git grep --untracked`；普通仓库和所有 scoped 搜索仍优先 rg。
2. 修正 command fallback：命令非零退出且 stdout 为空时视为失败，允许回退到下一搜索后端；有匹配输出时即使退出码非零仍接受。
3. git-grep 结果显式执行 `MAX_HITS_PER_FILE=60`，与 rg 的 `--max-count` 语义对齐，避免高频符号改变排序/召回。
4. 没有恢复 merged full-repo rg、预测预取、在线学习或 SPECULATIVE。

`git_index_path` 直接解析 `.git` 目录或 worktree 的 `gitdir:` marker；阈值判断只做 metadata，不增加 Git 子进程。

## 确定性规模 A/B

fixture 已改为真实 Git 仓库，所有代码文件提交到 index。每臂预热 3 次、采样 9 次；报告中给出中位数与 p95。A/B 输出字节数一致，均命中 `scaleTarget` 和 `scaleCaller`。

| 文件数 | A median | B median | Δ | A p95 | B p95 | p95 Δ |
|---:|---:|---:|---:|---:|---:|---:|
| 1,000 | 138.1 ms | 133.7 ms | **-3.2%** | 145.7 ms | 134.7 ms | **-7.5%** |
| 4,000 | 271.7 ms | 220.8 ms | **-18.7%** | 278.5 ms | 248.6 ms | **-10.7%** |
| 8,000 | 471.8 ms | 349.3 ms | **-26.0%** | 493.1 ms | 358.4 ms | **-27.3%** |

结论：优化随仓库规模增长而生效。8k 仓 median/p95 均超过 20% 改善，达到第一轮设定的大仓目标；1k 仓没有退化。

## 原生 Lyra + DeepSeek 配对 A/B

- 运行时：Rust 原生 Lyra CLI，不经过 Alkaid/Vega Node bridge。
- 模型：`opencode/deepseek-v4-flash/variant/high`
- 3 个用例 × 3 轮 = 9 对；每个会话只调用一次 fast_context，零补 read/grep。

| 指标 | A | B | Δ |
|---|---:|---:|---:|
| 总 wall time | 343,258 ms | 289,554 ms | **-15.6%** |
| 总 tokens | 87,560 | 81,870 | **-6.5%** |
| 答案关键词召回 | 100% | 100% | 0pp |
| fast_context 调用 | 9 | 9 | 0 |

各用例 wall 中位数：

| 用例 | A | B | Δ |
|---|---:|---:|---:|
| scoped search | 41,777 ms | 35,942 ms | **-14.0%** |
| reverse graph | 37,602 ms | 35,166 ms | **-6.5%** |
| packing | 34,506 ms | 26,745 ms | **-22.5%** |

与第一轮相比，本轮关键词召回没有回归（100%/100%）。模型 wall/token 仍受 provider 输出随机性影响，主要上线依据应是确定性 4k/8k 工具 benchmark；模型级结果作为质量与行为无回归佐证。

## 验证

- `cargo check --manifest-path src-tauri/Cargo.toml --lib`：通过。
- `cargo test --manifest-path src-tauri/Cargo.toml --lib nova_tools_native::context::tests -- --test-threads=1`：28/28 通过。
- N-API addon 构建：通过。
- 原生 Lyra binary 构建并完成 DeepSeek A/B。
- `git diff --check`：通过。
- 编译仅有仓库既有 warning（unused import/dead code 与 MinGW `.rsrc merge` warning）。

## 最终判断

第二轮已出现可确认的大仓收益：4k median -18.7%，8k median -26.0%、p95 -27.3%，且确定性输出一致、DeepSeek 召回不降。相比第一轮“4k/8k 基本持平”，本轮可以进入代码评审和更真实大仓灰度。

剩余风险：

1. 优化只对 Git 仓库且 index 足够大生效；大量未跟踪文件依靠 `--untracked`，其性能依赖 Git ignore 配置。
2. Git index 字节阈值是文件数代理，超大单体文件但 index 小的仓库仍走 rg；这是保守选择。
3. `git grep --untracked` 与 rg 的 ignore/二进制处理需继续用真实多语言 monorepo 做差分 fuzz；现有 28 个 context 测试已通过。

原始报告：
- `scripts/fast-context-scale-ab.report.json`
- `scripts/fast-context-scale-lyra-deepseek-ab.report.json`
