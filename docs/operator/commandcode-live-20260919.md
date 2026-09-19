# Operator / Command Code 实测报告（2026-09-19）

## 结论

完成 35 次真实 Command Code API 请求，全部 HTTP 成功：1 次连通性检查、32 次文字 A/B 决策、2 次图片输入能力探测。所有成功响应返回的模型均为 `deepseek/deepseek-v4.1-flash`，没有模型回退。

在本样本中，B 的输入 token 减少 43.57%，中位完整响应耗时减少 7.02%；A/B 正确率均 100%，P95 则 B 慢 3.55%。不能声称准确率已经改善，也不能将此结果当作完整桌面流程提速结论。

## 配置与方法

- 官方接口：`https://api.commandcode.ai/provider/v1/chat/completions`；先读取官方 `/models` 确认精确模型 ID。
- temperature=0；reasoning_effort=low；max_tokens=1024；stream=false。
- 8 个固定用例，各重复 2 次；首轮 AB，次轮 BA 且逆序遍历用例。
- 两组均为独立 API 请求，使用相同模型、系统提示、工具 schema 和当前观察。
- B 使用生产 `Task::project`；A 为相同输入额外附带 observationHistory。
- 使用生产 `Decision::parse`、`Task::validate_decision` 与独立预期值验证回答。
- 历史长度 3/10/30 是合成观察条数，不是实际完成的连续 GUI 操作步数。
- 真实 GUI 操作 0。没有运行 CodeBuddy CLI、主会话继承、完整 Operator 执行循环或 Reasonix 的模型端测试。

## A/B 结果（不含 smoke 与图片探测）

| 指标 | A | B | 变化 |
|---|---:|---:|---:|
| HTTP 成功 | 16/16 | 16/16 | 相同 |
| 正确结果 | 16/16 | 16/16 | 未发现差异 |
| 生产校验通过 | 16/16 | 16/16 | 相同 |
| 输入 token | 103362 | 58330 | -43.57% |
| 输出 token | 4704 | 4044 | -14.03% |
| 缓存读取 token | 50560 | 29952 | 实际 API 返回 |
| 中位完整响应 | 2.938 秒 | 2.732 秒 | -7.02% |
| P95 完整响应 | 3.445 秒 | 3.568 秒 | +3.55%（更慢） |
| 所有样本耗时之和 | 47.714 秒 | 44.146 秒 | -7.48% |

16 个匹配对中 B 有 12 个更快。匹配对延迟比 B/A 的中位数为 0.9035，这是不同于“两组中位数之比”的统计量。样本较少，不声称统计显著或可推广到复杂任务。

覆盖：历史干扰下的当前目标存在/不存在、未确认提交时停止且不重放、保留检查点中的已读业务数据。各用例 A/B 均 2/2 正确。

## 图片探测

同一模型路由接受两张 360×110 PNG data URI，分别正确读出 731946、285073；答案只存在于图片中，没有以提示文本/文件名泄露给模型。PNG SHA-256 经校验。

- image-1：2.224 秒；输入 270、输出 33 token。
- image-2：1.563 秒；输入 270、输出 34 token。

这仅验证该模型 API 路由在两张简单图上的图像处理，不证明上游原生视觉实现，也不是多截图 A/B 或剑来桌面定位测试。

全部 35 次请求共 171114 个 API 报告 token；没有查询实际账单，不推算实际费用。

## 证据与代码

- 被测生产基线：PR #12，`517cb2d43c54acb89d38c33b9d89095362394a8b`。
- 测试分支：`work/operator-commandcode-ab-20260919`。
- A/B 测试提交：`d5c5cf516f570826b65eee0a87178515585b49fa`。
- A/B CI：https://github.com/llyb120/nova-client/actions/runs/35444134250
- A/B artifact `operator-commandcode-live-results`，SHA-256 `0fa55fc10de1f63f8f2a79e40d711f926308b981252eb0371ec2527f31e19d81`。
- 图片测试提交：`8121667573a45a105ebdb3f24471b3eabee4086c`。
- 图片 CI：https://github.com/llyb120/nova-client/actions/runs/35444567697
- 图片 artifact `operator-commandcode-image-results`，SHA-256 `895767dad5a247205aebf29ab8ddc38df95fd7363103c2a56c4cda136bb1e3b0`。
- 官方说明：https://commandcode.ai/docs/provider
- 模型变更记录：https://commandcode.ai/changelog

测试脚本为 `scripts/operator-commandcode-ab.mjs`、`scripts/operator-commandcode-image-probe.mjs`；Rust harness 为 `bench/operator-commandcode-harness`，直接引入生产 core.rs。

测试分支最终只增加无凭据的测试脚本、样本与文档。生产 Operator、应用设置与 Reasonix 均未修改；PR #12 未合并或发布。

## 安全与限制

API key 没有以明文写入仓库、日志、报告或模型提示词。临时传递使用每次 CI 独立的 RSA-OAEP-SHA256 加密；私钥不上传且调用前删除，最终清理步骤通过。两份密文信封、两份临时工作流与凭据传递脚本已从分支最新文件树删除。Git 历史未重写，密文可能仍在历史中，但对应私钥已销毁。

本实验没有严格隔离冷热缓存、网络和输出长度影响。满分的小规模固定用例不能证明新策略在所有场景都更准或更快。CodeBuddy 每决策新进程的真实开销、浏览器/桌面动作、窗口抢焦点和恢复流程仍需端到端验收。
