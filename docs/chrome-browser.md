# Nova 浏览器引擎与 Chrome 扩展（v0.2.6 / Runtime v7）

Nova 使用**统一虚拟鼠标指针驱动引擎**（[src-tauri/src/browser/](file:///D:/code/nova-client-master/src-tauri/src/browser/mod.rs)），同时驱动**无头 Chrome（原生 CDP WebSocket 直连）**和**真实 Chrome（持久化 WebSocket 扩展）**。

## 核心特性

1. **页面内 Top-Layer 智能避让虚拟指针（Option A：零闪烁 + 不挡字）**
   - 通过 CDP `Page.addScriptToEvaluateOnNewDocument` 在隔离世界 `nova` 常驻注入 [page_runtime.js](file:///D:/code/nova-client-master/src-tauri/src/browser/page_runtime.js)（v7）。
   - 使用原生 `popover="manual"`（Top Layer）+ Closed Shadow DOM + `pointer-events: none`，压在原生 `<dialog>.showModal()` 之上且绝不阻挡点击或 `elementFromPoint`。
   - **按需极速滑行**：无头模式下若侧栏未打开实时预览流（无活跃 `Page.startScreencast`），指针自动切换 `0ms` 瞬移，每批节省 `0.5s–2.5s`；侧栏实时预览或真实 Chrome 下保持平滑最小加加速度贝塞尔滑行。
   - **输入微偏避让**：在窄输入框内打字后，指针自动微移至输入框右下内沿并降为轻透态（`.resting`），确保新输入的文字在模型截图中 100% 清晰无遮挡。
2. **双模极速传输与帧级去重**
   - **无头模式**：[cdp_direct.rs](file:///D:/code/nova-client-master/src-tauri/src/browser/cdp_direct.rs) 直连 `--remote-debugging-port=0` 原生 CDP WebSocket（`1280×800` 1:1 DPR），侧栏通过 `Page.startScreencast` 实时推送流畅画面。
   - **真实浏览器模式**：[ext_ws.rs](file:///D:/code/nova-client-master/src-tauri/src/browser/ext_ws.rs) + [worker.js](file:///D:/code/nova-client-master/extensions/nova-chrome/worker.js) 使用持久化本地 WebSocket（`127.0.0.1:47653–47662`）与事件驱动的 `tabMutationSeq` 标签变更追踪，一次批操作 + 智能等待 + DOM 观察 + JPEG q85 截图仅需 1 次往返。
   - **`unchanged` 帧去重**：[annotate.rs](file:///D:/code/nova-client-master/src-tauri/src/browser/annotate.rs) 与 [engine.rs](file:///D:/code/nova-client-master/src-tauri/src/browser/engine.rs) 接入 `visual_guard`，当视口像素与 SoM 控件签名均未变化时直接标记 `unchanged: true` 复用上一帧文件（零重绘、零重编码，Lyra 与 Antigravity MCP 均不重复发送图像 Token）。
3. **高密度防遮挡 `[1][2]` SoM 标注 + 强校验定位与表单增强**
   - 高密度表格/工具栏自动避开微小相邻控件中心，极端拥挤时仅保留 1px 靛蓝描边与文本 `@(x,y)` 坐标，避免黑底角标堆叠成墙。
   - `resolve(id, actionKind)` 内置 **9 点命中探测 + 上层浮层遮挡报错拦截 + `disabled` 拦截 + 虚拟列表行复用漂移拦截**，彻底杜绝穿透遮罩盲点。
   - 原生 `<select>` 在 `elements` 中直接展示 `options=[...]` 预览，并支持直接用 `op="type"` 按选项名精准选值触发 `input`/`change`；受控 React/Vue/AntD 输入框支持原型链清空兜底与输入值实时回读（`val="..."`）。
4. **6 个正交 `browser_*` 工具**（[scripts/browser-tools.json](file:///D:/code/nova-client-master/scripts/browser-tools.json)）
   - `browser_tabs`：列出/新建/跳转/切换/关闭/后退/前进/刷新，自动绑定会话活跃标签（支持 `C3` 同实例简写与无痕过滤）。
   - `browser_view`：单遍视口 DOM 清单 + 标注截图，支持 `query`、`scope: "all"` 与 `zoom` 局部特写。
   - `browser_act`：1–12 步指针驱动原子批处理（`click` / `type` / `hover` / `drag` / `scroll` / `press` / `dismiss` / `wait` / `upload`）。
   - `browser_eval`：隔离世界只读 JS 执行（默认不附图，极速数据提取）。
   - `browser_debug`：只读查询 `console` / `network`（含响应体）/ `downloads`。
   - `browser_experience`：站点操作经验（`search` / `save` / `feedback`，实时 `save` 异步不阻塞）。

## 验证

- Rust 单元与真实无头 Chrome 端到端测试：`cargo test --manifest-path src-tauri/Cargo.toml --lib -- browser::`
- 工具 Schema 与扩展测试：`node --test scripts/chrome-tools.test.mjs extensions/nova-chrome/worker.test.mjs`
