# JEV DOM dev 实测（2026-09-22）

## 统一后端执行规则验证

修复前的 Lyra 会话 `913c790c-4b76-48eb-85d6-466e25b85f85` 显示 JEV 已开启，但 17 次浏览器调用中 `act=10`、`run=0`、JEV 请求为 0。根因是共用执行层允许直接 DOM `act`，工具描述仅作提示。

修复后使用同一 dev 构建、独立配置和本地六步报表页面验证，未替换桌面安装版：

| 后端 | 主模型 | 结果 |
| --- | --- | --- |
| Lyra | `commandcode/qwen/qwen3.8-flash/variant/medium` | 52.3 秒；4 次 `run`、10 次真实 JEV 请求、6 次 JEV 动作；路径、全部 5 行及收入降序独立核对通过；`cachedActions=0` |
| CodeBuddy | `deepseek-v4.1-flash:high` | 3.2 秒后 `stopReason=refusal`，未调用任何工具；没有完成该后端的端到端验证，不算 JEV 失败或通过 |

确定性 dev 调用验证了单步 DOM 和含 `wait` 的混合批次都会在输入前返回 `jev_run_required`，原快照仍有效。只含 3 行的 Top5 页面触发真实 JEV `defer`，返回新观察和一次兜底许可；单步滚动执行成功，下一次直接 DOM 动作再次被拦截。Lyra 测试中的 8 个独立文字判断也通过。

Rust JEV 检查 14 项通过、1 项外部回放忽略；共享浏览器检查 5 项通过（与 JEV 过滤检查重叠 1 项）；schema/传输/审计 8 项和 MCP 适配检查 9 项通过。MCP 适配检查及调用链确认覆盖共享入口，但不替代 CodeBuddy 的真实模型验证。

复现：`node scripts/jev-delegation-live.mjs --run <label> --lyra`；去掉 `--lyra` 使用 CodeBuddy。原始报告保存在 `src-tauri/target/jev-delegation-shared-policy-lyra-20260922-report.json` 和 `src-tauri/target/jev-delegation-shared-policy-codebuddy-20260922-report.json`，对应的 `-thread.json` 保留完整会话。这些是本地测试页面结果，不替代下面的 DataBrain 站点任务核验。

## DataBrain 站点任务

任务：在 `http://databrain-test.intlgame.com/` 进入 Intelligence → PC & Console Games 榜单，查询美国近半年数据，按 Units 降序。

运行环境：本地编译的 Nova dev、独立 Nova 测试配置及 Chrome 标签、主模型 `deepseek-v4.1-flash:high`、线上 `jev-1.13.0`。每轮从首页开始；保留完整会话与最终 DOM，并在结束后关闭本轮标签及 dev 进程。没有修改桌面版 Nova。

## 逐轮记录

| 版本 / 记录目录 | 耗时 | 浏览器工具调用 | JEV 请求 | JEV 执行动作 | 复用路径动作 | 独立核验 |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| 初版 `dom-plan-live-20260922-v4` | 440.2 秒 | 41 | 2 | 0 | 0 | 筛选标签与排序符合；数据与后续轮不一致，需复核 |
| 路径容错、标签激活 `dom-plan-fixed-20260922` | 230.1 秒 | 25 | 9 | 3 | 0 | 页面条件、已加载表格及前十行降序通过 |
| 当前一步独立判断 `dom-plan-next-step-20260922` | 312.6 秒 | 36 | 8 | 4 | 0 | 未通过：结束日期变成未来的 09-26 |
| 可见节点扫描 `dom-plan-visible-hints-20260922` | 253.6 秒 | 26 | 8 | 3 | 0 | 未通过：结束日期 09-26，数据仍有 201983 条 |
| 控件候选 v11 `dom-plan-control-hints-20260922` | 55.7 秒 | 9 | 6 | 3 | 0 | 未完成：主模型 stopReason=refusal；停在 Console Games |
| 叶子菜单 v12 `dom-plan-control-leaves-20260922` | 10.1 秒 | 1 | 0 | 0 | 0 | 未完成：主模型首次观察后 stopReason=refusal |

以上是同一任务的逐次调试记录，每版本仅一轮，不能用来推断稳定提速比例。第一版 JEV 两次请求均因后续路径重复而拒绝，实际执行为零。第二轮 JEV 只执行进入 Intelligence、进入 PC & Console、展开 Region；搜索及勾选美国由主模型完成，不能把整个 Region 设置算作 JEV 成功。

## 实测驱动的修复

- 多题路径返回重复候选时，严格校验第一步并保留合法前缀；后续问题不能使有效的第一步整体失效，也不能重放重复动作。
- Chrome 执行动作前激活明确绑定的标签，避免后台渲染节流导致稳定性采样超时。保留节点身份、遮挡、焦点和执行结果检查。
- 纯点击页面只请求下一步；只有当前存在可填写字段时才请求后续路径，最多四步。第一步可判断即可推进，无需预知未出现的菜单。
- 隐藏和屏幕外 DOM 不再耗尽自定义控件的 4000 节点扫描额度。真实 Chromium 回归用例先复现可见菜单缺失，再验证修复；第四轮实测 `customTargetsTruncated=false`，但这本身不能证明任务正确。
- 用户指出应参考 Vimium 后，阅读其 `LocalHints.getLocalHintsForElement/getLocalHints` 及 `DomUtils.getVisibleClientRect`，收紧为控件层级的提示。继承 pointer 的内部文字不单独生成候选，语义 span 包裹真实控件时去重，普通 tabindex 容器不成为点击候选。无语义框架控件只保留 cursor 边界回退。v11 实测又暴露嵌套菜单继承 pointer 时合并到父级的问题；v12 保留每个叶子 li，去掉包含子菜单的父组，并增加真实 Chromium 回归检查。
- 路径缓存决策不再误计为线上请求；浏览器 `run` 的传输等待上限为 210 秒，覆盖其 180 秒执行预算。

## 核验标准和边界

不把主模型的“已完成”、`run` 返回或 JEV 请求数量当成任务成功。独立读取 `final-document.json`，检查页面路径、Region、日期、加载状态、URL `sort_name=units&order=desc`，以及第一列 Digital Units（列索引 3）的实际数值递减。

第二轮页面显示 Last 26 Weeks：`2026-03-22 ~ 2026-09-19`，United States，1691 条；前五行 Units 为 4.59M、2.43M、2.11M、1.83M、1.61M。首轮相同筛选标签却显示 205827 条、第一行 10.32M，所以已将首轮先前的通过结论改为待复核。第三轮主模型报告结束日期为 09-26，独立 DOM 也确认如此，按测试日期 09-22 判为未通过。

所有已完成轮的 `cachedActions` 都为 0，尚不能声称同屏多步复用已在该站点带来收益。主模型仍有 DOM 委托后长期接手、日期选择和排序菜单定位的问题。JEV 接管能力应按实际动作和子目标验证衡量。

## 最新版直接导航检查

为了区分主模型提前结束与 JEV 本身的问题，追加 `dom-plan-direct-navigation-20260922`：使用同一个 dev 进程启动器，直接调用生产 `chrome run`，仅授权从首页导航到指定榜单，不预选元素或动作。只有页面文字/URL 实质变化后才重新委托，最多四次；此检查不调用主模型，也不测试日期和排序。

耗时 9.1 秒，5 次真实 JEV 请求，实际执行 1 次点击（Intelligence）。首页有 49 个可操作候选，加载后的导航页有 39 个，均未使用 `controlNames` 缩小范围。JEV 后续三次均选择独立的 `PC & Console Games` 叶子项；其名称及上层菜单文字反复出现/消失 `NEW`，节点身份校验拒绝执行。未放宽校验，也没有将未执行的点击计为成功。

结论：控件级候选和叶子菜单选择得到真实站点验证，最新版本的完整任务仍未验证通过。主模型两次以 `stopReason=refusal` 提前结束，未给出具体原因；不能把直接导航检查等同于完整协作测试。

## 复现与证据

```powershell
npm run dev -- --host 127.0.0.1 --strictPort
node scripts/jev-databrain-ab.mjs --run <label> src-tauri/target/debug/nova.exe on --require-jev
# 仅检查生产 JEV run 的导航，不调用主模型：
node scripts/jev-databrain-ab.mjs --run <label> src-tauri/target/debug/nova.exe on --require-jev --dom-probe
node --test scripts/automation-schema.test.mjs scripts/jev-session-report.test.mjs
node scripts/browser-precision.test.mjs
cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features jev
```

原始证据保存在 `src-tauri/target/jev-ab/<label>/`：`report.json`、`thread.json`、`final-observation.json`、`final-document.json`。报告记录二进制 SHA-256、会话 ID、动作审计和独立核验；包含站点数据的原始文件不纳入源码提交。

本次本地检查：Rust JEV 12 项通过、1 项外部回放忽略；浏览器真实 Chromium 29 项通过；schema、传输及审计 8 项通过。dev 构建成功，存在仓库既有编译/链接警告。

参考源码（固定 revision）：[Vimium 控件检测](https://github.com/philc/vimium/blob/5aa29614bf1dce05e0d316f8c38722e17f9b38c3/content_scripts/link_hints.js#L1124)、[去重及遮挡检查](https://github.com/philc/vimium/blob/5aa29614bf1dce05e0d316f8c38722e17f9b38c3/content_scripts/link_hints.js#L1375)、[可见区域](https://github.com/philc/vimium/blob/5aa29614bf1dce05e0d316f8c38722e17f9b38c3/lib/dom_utils.js#L96)。只参考筛选规则，没有引入其源码或依赖。
