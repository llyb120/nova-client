# Operator（实验性，默认关闭）

基线：`pre-release@3da28d30f1adfd0da3b993813a47aa3fff20fdeb`，v0.1.554。本分支独立于其他 PR。当前实现不是已经证明更快、更准确的生产版；启用前必须补实际模型与业务环境的端到端对照。

## 做什么

主 agent 使用一个 `operator` 工具委派完整任务。一个隔离上下文的 Operator 自主使用、组合、切换 Chrome 和剑来；没有 DOM/桌面硬编码路由，也没有工具切换次数限制。代码检查任务作用域、当前观察、原生动作参数和执行状态，不代替模型规划业务流程。

复用原有 1–8 动作批量能力、操作后反馈、单次快照消费、焦点/几何/遮挡校验。此次没有重写 Chrome 动作引擎，没有增加任意脚本执行，也没有实现新的跨工具工作流语言。安全动作合批依赖模型正确选择当前已确定的动作段，性能效果尚待实际模型测量。

## 启用和回退

在启动 Nova 前设置进程环境变量，并新建测试会话：

```powershell
# A：原有工具入口（默认）
Remove-Item Env:NOVA_OPERATOR -ErrorAction SilentlyContinue
npm run tauri dev

# B：隔离任务上下文，保留完整任务内观察历史
$env:NOVA_OPERATOR = 'isolated'
npm run tauri dev

# C：隔离 + 最近四轮工作上下文 + 进度账本 + 精简能力说明
$env:NOVA_OPERATOR = 'adaptive'
npm run tauri dev
```

三个命令分别启动，不能同时争用同一桌面。回退只需移除环境变量并重启，不迁移/删除主会话历史或截图文件。没有新增默认全局开启的开关。

Lyra 使用当前实际解析的模型、思考配置、连接及凭据，通过独立模型请求上下文做决策。CodeBuddy 适配通过独立 ACP 进程继承会话模型/档位；无法确认或无法设置原模型时明确停止，不改用其他模型。CodeBuddy 的本地 CLI 版本、实际登录配置和隔离参数还没有通过真实模型会话验证。

Cursor、Codex、Devin 等其他宿主当前保留原来的 Chrome/剑来入口，不应据此宣称 Operator 已支持所有 agent。代码工具、Polaris 和 Reasonix 算法未改。

## 任务与结果

```json
{
  "goal": "完成已授权的指定页面交互任务",
  "target": {"tabTag": "来自实际 tabs 结果的标识"},
  "facts": {"必要事实": "仅当前任务需要的内容"},
  "constraints": ["不得重复提交", "新的授权要求需要暂停"],
  "successCriteria": ["指定结果在最新观察中可核实"],
  "allowedTools": ["chrome", "jianlai"]
}
```

不传父会话完整历史。宿主绑定模型、根目录与父会话作用域，不由模型提供这些路由参数。用户明确限定工具时，委派者必须保留相同限制。Task 的业务约束是模型指令，不是通用权限证明；页面和操作经验都是不可信资料。

结果为 `done`、`blocked`、`cancelled` 或 `needs_review`。完成结论绑定最新观察证据，但是否满足业务目标仍由模型判断；状态检查不是机械证明业务成功。

## 执行与上下文边界

同一物理桌面的输入任务互斥；取消后不发送新动作。已知未执行的逻辑步骤可以改用另一工具；已执行步骤不按同一 ID 重放。超时或部分执行后允许只读观察，但禁止通过另一工具继续冲突写入。

当前不自动消除未知执行状态：无法确认的部分执行/超时会进入 `needs_review`。重新截图本身不会清除未决状态。这是保守暂停，而不是已经完成的智能恢复。账本仅覆盖当前 Operator 任务，不保证跨进程、跨重发任务或延迟 CDP 副作用的全局 exactly-once。

C 保留任务、模型进度笔记、动作状态账本、各工具最新观察与最近四轮结果。笔记标记为模型生成且未核实，不能替代原始证据。历史图像仍按原生工具机制保存。工作帧超出 96 KiB 时停止而不是截掉目标；未实现自动重新查询以缩小超大结果。CodeBuddy 无法直接删除服务端历史，因此通过有界 ACP 会话重建收敛历史；重建耗时未实测。

没有每步总结模型，没有独立路由模型。现阶段也没有新的前端实时 Operator 任务卡，结果和指标通过现有工具展示。

## 计量与保密

结果的 `metrics` 记录模型调用次数、原生调用次数、格式修复、分段耗时和模型实际报告的用量。Lyra/CodeBuddy 对子任务用量按 run ID 去重合计。任何调用缺失用量时标记 `usageComplete=false`，完整用量为 null，仅保留 `knownUsage`，不以零填补。

`operator-runs` 仅保存运行标识、状态、模型身份和指标，不保存任务事实、密码、DOM、模型笔记或动作文本。已有原生截图/会话存储仍可能包含敏感内容；当前没有新建凭据保险库，也不承诺图像日志全局脱敏。CodeBuddy CLI 的工具白名单与协议拒绝不是操作系统沙箱。

## 可复现测试与评价口径

```bash
cargo test --manifest-path bench/operator-replay/Cargo.toml -- --nocapture
cargo run --release --manifest-path bench/operator-replay/Cargo.toml -- bench/operator-context.report.json
node --test scripts/operator*.test.mjs scripts/automation-schema.test.mjs scripts/nova-tools-mcp.test.mjs extensions/nova-chrome/worker.test.mjs
node scripts/browser-precision.test.mjs
npm run check
npm run build
cargo test --manifest-path src-tauri/Cargo.toml --lib operator -- --nocapture
cargo test --manifest-path src-tauri/Cargo.toml --lib jianlai -- --nocapture
```

桌面烟测仅在无人操作的可丢弃测试桌面运行（截图和移动指针，无点击/输入）：

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib jianlai::tests::desktop_smoke -- --ignored --nocapture
```

三组回放为 10 种合成观察序列 × 3 种上下文配置 × 25 次序列化重复。A 只是基于原工具定义重建的参考请求包，不是完整运行原版主 agent；B/C 调用生产 State。B/C 使用相同安全包装与决策提示，因此只构成上下文策略的消融比较，不是严格的纯行为隔离对照。

三组均保留原有批量上限 8。报告使用生产中的实际 JSON schemas 和 C 能力说明，另给出排除工具说明后的 history-only 字节数。它不调用模型、不执行业务操作、不包含图像传输/系统提示/模型协议包装；序列化字节数不是 token，组装 CPU 时间不是任务耗时，750 次回放不等于 750 次业务成功。

真实 Chromium 页面引擎回归和 Windows 原生烟测是底层回归，不是 Operator 业务成功率。真实三方案评价必须在相同模型、推理配置、权限、应用初始状态下随机交错重复，统计失败与超时，区分冷/热启动，核对业务结果、总 token、端到端时间和错误副作用；缺失这些数据时不能声称 C 比 A/B 更快或更准确。
