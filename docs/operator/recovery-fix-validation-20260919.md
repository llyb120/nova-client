# Operator 决策恢复、原生动作契约与异常处理修复验证

执行时间：2026-09-19 UTC。PR12，目标 pre-release；未合并、未发布，保持现有 PR 状态。

## 提交及验证身份

生产修复提交：`1c14d9c449a5d5f6eabd65a4116efd2500e58b93`。

仅修改 4 个生产文件：`src-tauri/src/operator/core.rs`、新增 `contract.rs`、`runtime.rs`、`src-tauri/src/jianlai.rs`。605 行新增包含契约和回归测试。未修改 Reasonix、模型选择、设置页、版本号或发布配置。

验证工作流提交：`c47c38a1838919909defb0964727e25bcaec89ff`。该提交清理了临时补丁传输和代码写回机制；保留的恢复验证工作流只有 contents:read，checkout 不持久化凭据，没有自动提交或发布步骤。临时传输文件已在生产修复提交中删除。

跨平台与原生验证检出的 GitHub PR 测试合并提交均为 `1dc9eb35f256e102a7e093fbecc29f1887c7ff29`。下载产物核对后，原生测试的 4 个生产文件及 Reasonix 哈希与修复提交产物完全一致，不是另一个未交付版本。

- Windows/Linux 编译和回归：https://github.com/llyb120/nova-client/actions/runs/35453230972
- 恢复与真实原生输入验证：https://github.com/llyb120/nova-client/actions/runs/35453230996
- 修复提交前的核心验证和生产文件快照：https://github.com/llyb120/nova-client/actions/runs/35452939200

## 实际修复

### 决策格式恢复

明确区分外层决策 `evidenceId` 和原生参数中的 `snapshotId`。缺失或错误层级现在给出明确错误，不再把格式问题误报成过期引用。

JSON 解析或决策校验失败后，通过 lastDecisionError 让同一个已绑定模型重新生成决策，每个阶段最多恢复两次，受阶段时间和总决策预算约束。没有把旧 native 动作作为重试请求，也不自动搬字段、编造 evidenceId/snapshotId。重复格式错误最终停止，不无限循环。

### 原生动作契约

新增由现有 chrome/jianlai 工具定义构造的受限分支契约；同一契约既展示给决策模型，也在任何 native dispatch 之前验证。

每种 action 只接受自己的字段：press 使用 key，不带 frame/ref；DOM click/fill 使用有效 frame/ref；Chrome drag 的 to_x/to_y 与剑来的 toX/toY 保持各自原生拼写；action/actions 互斥，批次 1..8 个动作。缺失字段、非法额外字段及越界参数在输入发送前拒绝。

原生 SELECT 不再被说明成可用 fill 填写；明确使用已有 click 加 Home/ArrowDown/Enter 原生按键，并观察确认结果。没有增加未实现的动作或绕过原生浏览器安全检查。

剑来快捷键对带修饰键的 ASCII 字母规范化，避免 X11 下 Ctrl+A 隐式附加 Shift 而未全选。显式 Shift 及独立文字输入语义保留，原 Windows 物理 VK 分支不变。

### 异常处理

只对模型推理的已识别临时错误（429/502/503/504、rate-limit）做最多两次有界退避，使用原绑定模型，不静默切换供应商或模型。401/403、取消或模型不匹配不按临时错误重试；退避期间可取消。

只读观察发生已识别的截图/视口变化时，可有界重新观察。原生 act 的错误、部分执行或未知结果仍然进入 needs_review，不自动重放输入。过期快照、资源租约、所有者及任务绑定验证继续保留。

## 已下载核对的结果

| 验证 | 结果 | 说明 |
|---|---|---|
| Windows Operator 核心/运行时 Rust 测试 | 30/30 | 包括格式恢复、非法参数零派发、临时错误恢复、取消、重复调用及未知结果不重放 |
| Linux Operator 核心/运行时 Rust 测试 | 30/30 | 与 Windows 同一套用例，不把双平台执行算成 60 个独立用例 |
| Windows Node 路由/MCP/上下文回归 | 29/29 | 含 9 项 Operator 专项及 Reasonix 一致性检查 |
| Linux Node 路由/MCP/上下文回归 | 29/29 | 同一套用例 |
| Windows/Linux SDK bridges 与类型检查 | 通过 | 不是仅测试脚本解析 |
| Windows/Linux 完整应用库 cargo check | 通过 | 编译应用库，不等于 Windows 交互式 GUI 验收 |
| 原生模块中的快捷键单元测试 | 1/1 | 其余 48 项过滤，未冒充已执行 |
| Linux 实际原生鼠标/键盘回归 | 7/7 | 实际 Chromium/CDP、XCap/Enigo；不是 mock 输入 |

Linux 实际原生 7 项逐项结果：

1. Chrome Title 与屏外 Notes 填写：页面实际值正确，有可信 input 事件。
2. 原生 SELECT：click + Home/ArrowDown/Enter 后实际值为 High。
3. press 携带不支持的 frame/ref：not_executed，completedActions=0，页面没有新增输入事件。
4. 剑来 Ctrl+A：Old draft 被替换为 Replacement，而不是拼接。
5. 剑来 Ctrl+a：同样正确替换为 Replacement。
6. 剑来过期截图：not_executed，completedActions=0，未修改原文本。
7. 剑来 Canvas 原生拖动：dragged=true、aligned=true，保存实际页面截图。

共 8 次原生 act 调用，其中两次预期拒绝；不能把“未执行的危险请求”当成失败或把工具 executed 字段单独当成任务成功。测试核对页面实际状态和事件。

初次原生测试编译未完成，原因是独立 GUI 测试工程在编译生产模块的 cfg(test) 代码时缺少 tempfile 开发依赖；未把该失败计为通过。补齐测试依赖后，以上成功运行完整完成。该修正没有改变生产动作逻辑或放宽断言。

## 产物与哈希

- 原生成功产物 `operator-recovery-native-results`，ID `10586794280`，ZIP SHA256 `7fdd6b23f4b925a7a2055f6bcea6ade56797eb1db1f4ce33af37216ea516a551`。
- Windows 回归产物 `10587338518`；Linux 回归产物 `10587865961`，均记录测试合并提交及完整测试输出。
- Windows 编译产物 `10587766330`；Linux 编译产物 `10586933934`。
- 生产快照 `operator-recovery-core-results`，ID `10587506159`，ZIP SHA256 `bbac2a10220ec162fac679c15805781b18ccb5dbcdaef9b90bbb2583923e5070`。

生产文件 SHA256：

```text
77d780d586de27b522dc1af074aacb0a49361231448b1ed117a0b7a342b63432  operator/core.rs
29c8bb600e1f7767f12c80a728d8dace9eca363fab6e4680c3b27989bfe14d53  operator/contract.rs
6941e21f1794423fad22fd69599d6610d2e94e61e8e0fd569e4205b9a20e9f9a  operator/runtime.rs
74687dff5cd3a4b5f34e5b184a9ecd2a8c2e4c234adba48af1baf5a8d8b10c99  jianlai.rs
011dcad69bda5714d2634a1035ee01623c6e1737ff8f218c4ac50f9afa3ce536  lyra/reasonix.rs
```

## 不能外推的结论

本轮没有调用真实决策模型，也没有复用此前暴露的 API 凭据。核心恢复测试使用故障注入；原生输入测试使用固定决策与测试页面。因此它证明上述代码路径和原生控制用例已修复，不证明大模型在任意网页上的完整任务成功率。

前一份报告的 0/12 真实模型 GUI 任务结果仍是历史事实，本轮没有重跑这组在线模型实验，不能拿 7/7 原生用例替代它。也没有做用户 Windows 桌面的实际鼠标键盘端到端验证。PR12 保持未合并、未发布，供审查和客户端实测。
