# 剑来与 Chrome 精准操作

## 定位策略

Chrome 与右侧 webview 共用页面定位引擎，但会话、标签和传输保持独立。Chrome 必须显式传 tabs 返回的 tabTag，不能把主界面焦点当作授权目标。

普通网页优先 inspect(query) → frame/ref → click 或 fill。query 是名称、区域或 href 的一个关键词/短语，采集阶段先过滤，再测量和做命中测试；有 query 时 documentPath 只保存匹配元素，无 query 时保留全部已采集 DOM。滚动容器不是容器内部的按钮，同名控件要同时核对区域和框架。

执行前检查身份、所在数据行、可用性和可见性，等待目标几何稳定，鼠标悬停后再次检查位置及遮挡。虚拟列表复用原来的按钮节点但数据行变了，也必须重新观察。fill 在点击后及全选后复核真实焦点，避免将文字送入另一个输入框。只读字段、滑块等不作为普通文本输入。

目标位于 iframe 时会检查父框架命中并转换边框和轴向缩放，不能安全转换的旋转、倾斜或透视变换会明确拒绝。开放 Shadow DOM 会参与定位；封闭 Shadow DOM 不假装具有内部语义。

## Canvas：看图 + 图片坐标，不伪造内部控件

inspect 默认在发现足够大的可见 Canvas 时附带当前视口图，包括 Canvas 承载的 WebGL 页面。返回 visualRequired、Canvas 的 viewportRect、位图尺寸和视觉定位提示。Canvas 内的图形/表格单元格没有自动生成的 DOM ref，仍需模型根据图片识别。

小目标先局部截图，例如：

```json
{"operation":"screenshot","tabTag":"<tabs返回的tag>","fullPage":false,"region":{"x":100,"y":120,"width":500,"height":300}}
```

region 使用当前主页面 CSS 视口坐标，不兼容 fullPage=true。对截图执行坐标动作时，使用该图自己的 imageId 和实际图片像素：

```json
{"operation":"act","tabTag":"<同一tag>","snapshotId":"<最新snapshotId>","action":{"action":"click_at","imageId":"<images中的imageId>","x":125,"y":80},"feedback":"screenshot"}
```

工具根据图片实际尺寸、裁剪偏移及整页分片位置完成换算，不要求模型手动乘 DPR。省略 imageId 的旧调用仍使用 CSS 坐标。整页坐标操作允许工具滚动到目标；拖动必须使用视口截图。浏览器 pinch zoom 尚不支持，检测到后拒绝旧坐标；普通截图的像素比例通过 ImageMap 换算。

支持 double_click_at、带连续可信鼠标轨迹的 drag，以及 scroll_at 的 delta_x 水平滚动。delta 仍表示垂直 CSS 像素，二者单次绝对值不超过 1200。拖动起点和终点使用同一张图的像素坐标。

坐标输入前，会比较落点附近的旧图和当前画面，因此 Canvas 重绘即使没有改变 DOM 属性，也可以使旧坐标失效。此检查是局部像素见证，不是 OCR、目标识别或业务成功证明。目标附近持续动画/光标闪动可能导致保守拒绝；应重新观察或使用语义目标，不应盲目重放。

仅需 DOM 时设置 includeVisual=false，避免无意义图片传输。默认 act 反馈会提供新的观察与 snapshotId；直接基于该结果继续，不必再做一次相同 inspect。

## 等待与执行结果

wait_for 支持 visible、hidden、enabled、text，要求当前 frame/ref；text 条件需要 text，ms 上限 2000。条件满足立即返回，超时则重新观察，不靠扩大固定 sleep 掩盖定位问题。

snapshotId 一次性使用。状态意义：

- not_executed：尚未尝试有副作用的输入；按返回的新观察重新判断。
- executed：输入已发送，不等于业务成功。必须观察结果。
- needs_review：可能已经点击/输入，或操作中断/释放输入失败。禁止自动重放，先检查当前页面。

输入发送情况依据实际 CDP 调用记录，而不是根据报错文字猜测。操作取消时，尽力向本次绑定的标签释放尚未释放的鼠标/按键。返回 actionMs、durationMs、cdpCalls 便于区分定位、反馈与传输成本。浏览器/扩展进程断开时不能保证释放成功，releaseErrors 会报告该情况。

## 剑来

窗口/显示器截图保留未画鼠标标记的受限分辨率像素见证。点击、双击、拖动和滚动前，校验窗口/屏幕范围、前台身份、目标附近画面；再校验实际系统鼠标位置，坐标异常则停止。截图上的可视鼠标标记不参与像素比较。

region 可与明确的 windowId 或 monitorId 配合使用；区域参数以 originalWidth/originalHeight 的原图像素表示。对局部返回图操作时仍使用该图的像素，不要再加屏幕/窗口偏移。可用 maxEdge=0 请求不缩小的局部图。

批处理不会在失效后自动重放，后续落点也检查最新画面。界面已经变化的后续动作可能被提前停止，这是避免点到另一行数据的安全策略。删去的是末尾已有截图稳定等待时重复的固定延迟，保留延迟加载/动画所需的有界稳定检测。没有新增后台识别模型、远程服务或原生 UI Automation 依赖。

## 回归与测量

```sh
TEST_BROWSER=/path/to/chrome node scripts/browser-targeting.test.mjs
node --test extensions/nova-chrome/worker.test.mjs scripts/nova-tools-mcp.test.mjs
cargo test --manifest-path src-tauri/Cargo.toml --lib native_browser::
cargo test --manifest-path src-tauri/Cargo.toml --lib jianlai::
cargo test --manifest-path src-tauri/Cargo.toml --lib chrome_browser::
```

浏览器回归使用真实 Chromium/CDP 和页面脚本，包含多种 DPR、标签命名、嵌套滚动、Shadow DOM、虚拟行复用、悬停位移、焦点转移、缩放 iframe、遮挡、Canvas 双击/轨迹/滚轮。原生测试覆盖坐标映射、裁剪、局部像素变化、输入格式及连接归属。

Windows 的 scripts/native-browser-smoke.mjs 驱动真实 Nova/WebView2。Chrome 桥接部分用本机测试轮询器模拟扩展传输，执行的仍是真实 Rust 工具入口和 CDP 输入；不能将其描述为安装版 Chrome 扩展的端到端验收。独立 WebView2 profile 的测试驱动可使用 TEST_CDP_PORT 与 TEST_CHILD_CDP_PORT；调试端口只用于隔离测试副本，不能写入发布配置。

剑来 desktop_smoke 是显式 opt-in 测试，仅在隔离桌面使用：默认只截图、移动鼠标和检查像素见证，不向用户应用点击/输入。它也模拟改变保存的见证，验证旧目标被拒绝；不是所有业务应用的点击成功率测量。

性能必须使用同机同页面测量。BROWSER_BASELINE 可指向旧 native_browser_page.js，测试报告保留每次样本。2026-09-18 的 10,000 按钮固定页面对照中，旧全量采集中位 212.1ms，新全量 199.9ms，新目标 query 61.6ms；query 路径相对旧全量约 3.44 倍，但全量仅约 5.8% 改善。这是页面采集耗时，不包含模型推理、网络加载或截图开销，也不能外推为所有任务的速度或准确率。
