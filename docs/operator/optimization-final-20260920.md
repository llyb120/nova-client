# PR12 Operator 性能优化最终回测（2026-09-20）

## 结论

本轮对生产基线 `5c10b1f3197537ec73419d9030f7dd96140fb201` 与优化生产代码 `b96dfa037bd475af242a14c9f99f855f4a309797` 做了固定顺序的真实模型 + 原生 GUI before/after 配对回测。优化版 **9/9 业务验收通过、0 假完成**；基线 8/9 通过、0 假完成。

全部 9 个尝试计入时，优化版累计耗时 750.66s → 438.97s（**-41.52%**），模型调用 97 → 61（**-37.11%**），API 成功响应报告的总 token 593,955 → 375,350（**-36.80%**）。

为了排除基线唯一失败任务对总耗时的放大，只比较双方都成功的 8 对任务：累计耗时 579.17s → 383.00s（**-33.87%**），模型调用 81 → 54（**-33.33%**），总 token 467,844 → 319,182（**-31.78%**）。8 对中 **6 对更快，7 对 token 更少**。

## 测试范围

- PR：#12，分支 `work/operator-context-20260919`
- baseline：`5c10b1f3197537ec73419d9030f7dd96140fb201`
- optimized：`b96dfa037bd475af242a14c9f99f855f4a309797`
- 固定 GUI fixture：`3d7ac249129e3f25c703fb03a1dd5617c3560a7a`
- GitHub Actions run：`35486248847`
- 模型：`deepseek/deepseek-v4.1-flash`
- 环境：隔离 Linux/Xvfb，实际 production Operator runtime、Chrome/CDP 与剑来原生输入；Playwright 只负责环境准备和最终业务 oracle
- 任务：Chrome/Jianlai 的表单、订单读取、Canvas，共 9 对 before/after，18 个尝试
- 不是完整 Windows Nova 客户端或 CodeBuddy CLI 的端到端测试

## 汇总

| 指标 | baseline | optimized | 变化 |
|---|---:|---:|---:|
| 成功任务 | 8/9 | **9/9** | +1 |
| 假完成 | 0 | **0** | 持平 |
| 累计耗时 | 750.66s | **438.97s** | **-41.52%** |
| 模型调用 | 97 | **61** | **-37.11%** |
| Prompt token | 479,914 | **313,591** | **-34.66%** |
| Completion token | 114,041 | **61,759** | **-45.84%** |
| 总 token | 593,955 | **375,350** | **-36.80%** |

### 双方都成功的 8 对

| 指标 | baseline | optimized | 变化 |
|---|---:|---:|---:|
| 累计耗时 | 579.17s | **383.00s** | **-33.87%** |
| 模型调用 | 81 | **54** | **-33.33%** |
| Prompt token | 381,643 | **267,492** | **-29.91%** |
| Completion token | 86,201 | **51,690** | **-40.04%** |
| 总 token | 467,844 | **319,182** | **-31.78%** |

## 每对任务

| Pair | 任务 | baseline | optimized | 耗时变化 | token 变化 | 调用 |
|---:|---|---:|---:|---:|---:|---:|
| 1 | Chrome Canvas | 17.66s / 25,121 | 23.78s / 24,019 | +34.7% | -4.4% | 4→3 |
| 2 | Jianlai Form | 130.78s / 83,655 | 74.89s / 50,352 | **-42.7%** | **-39.8%** | 18→10 |
| 3 | Chrome Orders | 171.49s / 126,111（失败） | 55.98s / 56,168（成功） | -67.4% | -55.5% | 16→7 |
| 4 | Jianlai Canvas | 19.58s / 15,563 | 15.26s / 15,723 | **-22.0%** | +1.0% | 4→4 |
| 5 | Chrome Form | 83.46s / 69,006 | 67.64s / 62,023 | **-19.0%** | **-10.1%** | 9→8 |
| 6 | Jianlai Orders | 60.88s / 41,736 | 24.08s / 24,867 | **-60.5%** | **-40.4%** | 10→6 |
| 7 | Chrome Orders | 138.12s / 124,072 | 50.60s / 67,698 | **-63.4%** | **-45.4%** | 16→9 |
| 8 | Chrome Canvas | 45.49s / 53,423 | 15.53s / 22,140 | **-65.9%** | **-58.6%** | 8→3 |
| 9 | Jianlai Form | 83.20s / 55,268 | 111.21s / 52,360 | +33.7% | -5.3% | 12→11 |

## 本次针对慢任务的改动

生产提交 `b96dfa0`：

1. 重复操作指纹不再把 `snapshotId`、`imageId`、模型 notes 和 checkpoint 的自由文本改写当成“新进展”。
2. 只有数字/布尔/数组等客观 checkpoint 变化才会打断无进展循环检测。
3. 对不可读的原生 SELECT/dropdown，明确要求停止反复开关，改用聚焦后键盘导航或首字母选择 + Enter，再验证关闭后的值。
4. 保留已有的结构化 acceptance facts、`checkpointPatch`、自动只读初始观察、截图/拖拽能力约束与 read-only retry。

目标问题在 live 中得到改善：
- Pair 2 的剑来表单从 baseline 18 次模型调用降到 optimized 10 次；只打开一次下拉后改用键盘 `h + Enter`。
- Pair 9 optimized 直接使用 `click + h + Enter`，没有再出现之前的反复打开 Priority → Escape → 再打开循环，并最终通过业务验收。

## 剩余尾延迟

优化版仍有两个配对任务比基线慢：

- Chrome Canvas Pair 1：模型调用反而更少（4→3），token 也更少（-4.4%），但总耗时 +34.7%，属于本轮单次模型响应延迟波动，而不是更多工具步骤。
- Jianlai Form Pair 9：虽然下拉循环已消失，但发生了一次上游 HTTP 429（约 14.6s）以及一次因把历史 evidence 放进当前 `verified` 而被生产校验拒绝后的重试；最终仍成功，token 比基线少 5.3%。

这说明当前主要剩余问题已从“反复看一步走一步”转为 provider 尾延迟/限流和少量无效 finish 重试。为保持这轮完整回测与最终生产 SHA 一致，本报告没有继续修改生产代码。

## Token 口径

summary 中的 token 是成功 API 响应实际返回的 usage，不是字符估算。本轮 transport audit 有 158 个 HTTP 200 尝试和 9 个 HTTP 429 尝试；429 不报告 token usage，因此 benchmark 的 `usageComplete` 会因为存在 retry attempt 而为 false。这里不把 token 降幅直接等同为账单费用降幅，也没有查询实际账单。

## 回归

生产提交 `b96dfa0` 已通过：

- Operator recovery core：成功
- 原生 GUI 输入回归：成功（7/7）
- Ubuntu Operator regressions：成功
- Reasonix 未修改

Windows/部分 compile job 在随后测试/凭据提交推动分支时被 concurrency 的 `cancel-in-progress` 取消，并非测试失败。最终分支清理提交会再触发常规 Operator tests；以最终 head 的结果为准。

## Benchmark workflow 判定修正

此前 benchmark 脚本要求 baseline 与 optimized 18/18 全成功，因此本轮即使 optimized 9/9，仍把 workflow 标成 failure。最终脚本改为：报告必须完整产生 18 行，并且 **optimized 9/9、0 假完成** 才作为优化回归门槛；baseline 的失败仍完整记录，但不再错误地把优化版判红。

一次性加密凭据文件与临时源码导出 workflow 在最终树中删除；模型调用已结束。
