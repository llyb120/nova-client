# Nova 浏览器引擎与 Chrome 扩展（v0.2.0）

Nova 使用全新的**统一虚拟鼠标指针驱动引擎**（[src-tauri/src/browser/](file:///D:/code/nova-client-master/src-tauri/src/browser/mod.rs)），同时驱动**无头 Chrome（原生 CDP WebSocket 直连）**和**真实 Chrome（持久化 WebSocket 扩展）**。

## 核心特性

1. **页面内 Top-Layer 虚拟鼠标指针（观赏性 + 视觉闭环）**
   - 通过 CDP `Page.addScriptToEvaluateOnNewDocument` 在隔离世界 `nova` 常驻注入 [page_runtime.js](file:///D:/code/nova-client-master/src-tauri/src/browser/page_runtime.js)。
   - 使用原生 `popover="manual"`（Top Layer）+ Closed Shadow DOM + `pointer-events: none`，压在原生 `<dialog>.showModal()` 之上且绝不阻挡点击或 `elementFromPoint`。
   - 所有空间操作（点击、点击聚焦输入、悬停、拖拽、滚动）均经由该指针平滑贝塞尔曲线滑行、目标描边高亮、点击波纹扩散与落点靶环完成。
2. **双模极速传输**
   - **无头模式**：`cdp_direct.rs` 直连 `--remote-debugging-port=0` 原生 CDP WebSocket（`1280×800` 1:1 DPR，绕过扩展与调试黄条），侧栏「浏览器」页通过 `Page.startScreencast` 实时推送流畅画面，可直接观看虚拟鼠标指针滑行与点击。
   - **真实浏览器模式**：`ext_ws.rs` + `extensions/nova-chrome/worker.js`（v0.2.0）使用持久化本地 WebSocket（`127.0.0.1:47653–47662`）与**复合原子宏指令**，一次批操作 + 智能事件等待 + DOM 观察 + JPEG 截图仅需 1 次往返。保留旧 `/pair` + `/poll` 版本升级垫片，已装 0.1.x 扩展会自动热重载升级至 0.2.0。
3. **纯净页面 + 模型侧 `[1][2]` SoM 编号叠加**
   - 真实页面与侧栏预览只显示简洁的虚拟鼠标指针。
   - [annotate.rs](file:///D:/code/nova-client-master/src-tauri/src/browser/annotate.rs) 在发给 AI 模型的 JPEG 副本上额外绘制高对比度防重叠 `[1][2]` 角标；元素列表同步附带精确截图像素 `@(x,y)` 与指针位置 `cursor: [x,y]`，实现 DOM 编号与视觉坐标 1:1 对齐。
4. **6 个极简 `browser_*` 工具**（[scripts/browser-tools.json](file:///D:/code/nova-client-master/scripts/browser-tools.json)）
   - `browser_tabs`：列出/新建/跳转/切换/关闭/后退/前进/刷新，自动绑定会话活跃标签。
   - `browser_view`：快速单遍视口 DOM 清单 + 标注截图，支持 `query`、`scope: "all"` 与 `zoom` 局部特写。
   - `browser_act`：1–12 步指针驱动原子批处理（`click` / `type`（自动点击 focus + 可选 `clear` & `submit`） / `hover` / `drag` / `scroll` / `press` / `wait` / `upload`）。
   - `browser_eval`：隔离世界 JS 执行（默认不附图，极速数据读取）。
   - `browser_debug`：只读查询 `console` / `network`（含响应体）/ `downloads`。
   - `browser_experience`：站点操作经验（`search` / `save` / `feedback`，兼容既有知识图谱与剑谱 `recordingId`）。

## 验证

- Rust 单元与真实无头 Chrome 端到端测试：`cargo test --manifest-path src-tauri/Cargo.toml --lib -- browser::`
- 工具 Schema 与扩展测试：`node --test scripts/chrome-tools.test.mjs extensions/nova-chrome/worker.test.mjs`
