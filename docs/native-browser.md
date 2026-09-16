# 右侧原生浏览器

主模型直接通过 Nova 通用工具 webview 操作 Windows WebView2。无辅助模型、无 Playwright。
测试版：bench/webview-probe/target/debug/nova.exe，同目录保留 WebView2Loader.dll。旧实例不会自动更新。

## 使用

右侧「浏览器」只有标签栏和地址栏。＋新建标签，×关闭标签。新窗口使用原生 WebView2 接管为标签页，保留 opener、空白窗口后续跳转关系。会话内直接告诉主模型要操作什么，无需配置额外模型。

切换会话或标签会停止动作，原页面的表单、滚动位置、DOM 和 JS 状态保留。保留限于本次应用运行，退出不恢复未提交表单。后台页不自动淘汰，需要时手动关闭标签释放内存。

## webview 工具

- open：打开网址，返回 browserId。
- tabs / new_tab / select_tab / close_tab：列出、新建、切换、关闭标签。
- inspect：读取整页已加载 DOM、完整文本、标题及元素，包括屏幕下方和内部滚动区域。回复默认60个元素/3000字符，优先可见未遮挡控件，完整数据保存在 documentPath；可用 query 直接搜索整页目标，不需要逐屏滚动。inlineTruncated 表示回复摘要截短，truncated/coverageGaps 表示采集本身有缺口。
- screenshot：默认整页截图，不滚动用户当前视口，也不修改滚动条外观。Node（CodeBuddy/Devin）和原生 MCP（Codex）直接返回 image 内容块，无需额外 Read 图片；同时返回 images 图片分片、文档尺寸、坐标映射及完整性标记；单次最多4片，超长页按 nextTile 继续。fullPage=false 切换为当前视口截图。主文档截图不会展开内部滚动面板/iframe，其屏幕外文字和元素可通过整页 DOM 读取。
- act：使用最新 snapshotId 执行一个动作，无模型调用。DOM click/fill 使用 frame/ref；click_at/move/drag/scroll_at 使用截图坐标；press/type 操作键盘与文本。默认整页截图使用 CSS 文档坐标（加上图片分片的 x/y 偏移），click_at/move/scroll_at 会自动滚动到该位置。视口截图使用 CSS 视口坐标；drag 需先 fullPage=false。图片有缩放时按返回的 width/height 换算。动作默认返回最新 DOM 和 snapshotId，直接用反馈验证并继续；feedback=screenshot 返回视口截图，feedback=none 关闭反馈。DOM 引用不再因思考超过30秒过期，执行前实时校验元素；坐标/键盘观察180秒有效，坐标另检查视口。旧 snapshotId 仍不能重复执行。
- stop：停止进行中的操作。界面执行时地址栏显示「停止」。

已加载内容可以一次从接口读取，不受浏览器窗口大小限制；尚未请求的懒加载数据或被虚拟列表移出 DOM 的内容无法凭空读取。coverage 返回懒加载图片、ARIA 总行数等线索，主模型据此定向滚动补取。

DOM 定位会检查多个可见点，避免仅中心被遮挡就失败。完全遮挡会返回 not_executed 和遮挡信息，主模型用截图/DOM 判断关闭弹窗、滚动或换目标。执行结果不确定则返回 needs_review，不应直接重放。坐标模式支持画布、悬停及拖动，无需 DOM 包含可点击元素。

单次动作/采集预算 15 秒，结束时恢复截图临时布局；底层网页响应超时 3 秒，避免长时间挂起；超时后先观察实际状态再决定下一步。密码、验证码手动处理。旧 Agent 会话可能需要重新启动以刷新工具定义。

## 验证

运行 node scripts/native-browser-smoke.mjs。独立临时配置中的真实 Nova 使用测试专用 CDP，无辅助模型接口。覆盖 DOM/截图操作、遮挡提示及部分遮挡、中文原生输入、跨域 iframe、原生弹窗、多标签、会话切换保留状态、整页文本、超过摘要上限的屏幕外目标、长图分片续读、文档坐标点击及停止。

报告及截图：src-tauri/target/native-browser-smoke/。

## 延迟优化验证

act 反馈失败仍保留 executed / needs_review 状态，不能据此重放动作。返回 actionMs 和 durationMs 区分操作与反馈时间。反馈是即时观察，不保证异步网络加载结束。整页 DOM 含菜单/选项/可聚焦控件，提供 href、expanded、haspopup、selected；query 使用单个关键词/短语，不是 OR 检索。

集成检查包含等待31秒后使用 DOM 引用、连续操作复用返回状态、菜单展开反馈和 href 检索。MCP 图片内容块另有 Node 与 Rust 检查。安装新构建后需重启应用并刷新 Agent 会话工具定义。
