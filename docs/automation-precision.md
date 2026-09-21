# 剑来 / Chrome 精准交互与 Canvas 观察

本补丁的基线是 `pre-release` 提交 `9798b86d66b8b7b8822c3d26a92cf838d6efb03d`。Chrome 与内置 WebView 共用 `native_browser` 引擎；剑来仍是纯系统输入和截图工具。没有引入生产环境 Playwright、OCR、辅助模型、任意页面脚本执行接口，也没有调整扩展权限或删除会话历史。

## 定位与输入的执行条件

DOM 操作优先使用当前观察中的 `frame/ref`，不按相同文本重新寻找替代元素。引用检查同时覆盖原节点、可访问名称、关键属性及所属行身份，拦截虚拟列表复用同一个“删除”按钮却已切换记录的情况。表单的临时校验文字不会让其他字段无故失效；记录标题或稳定身份改变仍会拒绝旧引用。

可操作点从元素的实际 client rects 中寻找，避开遮挡及不相关的嵌套按钮。准备阶段在最多 1,200 毫秒内采样几何稳定性，检查禁用、只读及动画；真实鼠标移入后再校验同一节点、尺寸和命中对象。正向轴对齐缩放的 iframe 按边框和各级缩放映射；旋转、镜像、透视及被遮挡的框架保守拒绝，不猜坐标。闭合 Shadow DOM、不可访问框架及尚未加载的数据不被宣称已覆盖。

`fill` 执行顺序为：准备目标 → 真实点击 → 核对原字段焦点 → 全选 → 再核对焦点 → 插入文本 → 核对实际字段值。密码字段要求人工输入。页面重定向焦点、只读或 `maxlength` 等限制导致实际值不一致时返回待核对状态，而不是自动重复写入。

`type` 仅允许向观察时已经确认、现在仍未改变的可编辑焦点输入。一次点击改变了焦点后，必须使用反馈的新快照再 `type`；已明确字段的操作直接使用 `fill` 更省调用。此规则也检查 iframe 的实际焦点祖先链，避免后台框架保存的 `activeElement` 误授权当前页面输入。

## 快路径及连续操作

已知目标在屏内时可请求：

```json
{"operation":"inspect","tabTag":"<tabs 返回的 tag>","scope":"viewport","query":"保存"}
```

`scope=viewport` 仅枚举当前各框架视口内的目标，返回文本仍可能包含已加载文档。需要屏外内容时使用 `scope=all`，或滚动后重新观察。默认仍为 `all`，不会悄悄缩小原有覆盖范围。摘要被截短与采集本身缺失分别报告。

重复观察只缓存语义信息；DOM 变更会使缓存失效，同步调用前也会排空 MutationObserver 记录。坐标、命中测试、焦点、字段值和禁用状态不跨观察缓存。点击前重新读取真实身份。主页面观察失败时不能把子框架的坐标误当作主页面坐标。

一次 `act` 可提供 `action` 或 `actions`，两者互斥；每批 1–8 项。先验证整批静态参数和快照归属，再逐项校验真实目标。已确认、无中间分支判断的同一表单可合批：

```json
{
  "operation":"act",
  "tabTag":"<当前 tag>",
  "snapshotId":"<最新 snapshotId>",
  "scope":"viewport",
  "actions":[
    {"action":"fill","frame":0,"ref":"<姓名字段 ref>","text":"测试姓名"},
    {"action":"fill","frame":0,"ref":"<备注字段 ref>","text":"测试备注"}
  ]
}
```

示例中的占位 ID 必须替换成最新返回值，不能复用演示坐标或旧快照。提交、删除、导航及弹窗变化后需要新的观察与必要的用户授权，不把可能改变页面的动作盲目合批。工具可返回 `completedActions`、`inputAttempted`、`actionTimingsMs`、`basedOnSnapshotId`，区分“未发送输入”和“可能已执行部分动作”。

`feedback=none` 省略正常动作后的观察，但不会产生新的操作授权。默认复用一次动作反馈；原观察含图时继续附视口图，不必再单独截图。显式 `feedback=inspect` 不自动附图。观察失败不改变原输入的执行状态。超时、取消及释放失败均不自动重放点击；只做已按下输入的尽力释放。

## Canvas：图片不是 DOM，坐标必须来自返回的图片

观察会列出 Canvas 的引用、CSS 显示矩形和 backing-store 尺寸，不把 Canvas 内绘制的按钮谎称为 DOM 按钮。可见、可命中的 Canvas 区域达到视口面积约 15% 时，`inspect` 默认自动附可操作的视口截图，减少“DOM 没内容 → 再截全页”的往返。小 Canvas 可主动截图；`visual=none` 关闭自动附图。

较小的绘制目标可先对已观察到的 Canvas 做局部截图：

```json
{
  "operation":"screenshot",
  "tabTag":"<当前 tag>",
  "snapshotId":"<最新 snapshotId>",
  "frame":0,
  "ref":"<Canvas ref>",
  "maxEdge":0
}
```

也可使用 `region:{x,y,width,height}`（主页面视口 CSS 坐标），与 `ref` 裁剪互斥。区域会限制在可见视口。返回的新 `snapshotId`、`imageId`、`pixelWidth`、`pixelHeight` 是后续操作依据。`maxEdge` 默认 1600，允许 0 或 320–3840；0 表示不追加工具侧缩放，不保证浏览器原生输出比例必然等于系统 DPR。PNG 实际尺寸是最终依据。

```json
{
  "operation":"act",
  "tabTag":"<当前 tag>",
  "snapshotId":"<局部图的新 snapshotId>",
  "imageId":"<局部图的新 imageId>",
  "action":{"action":"click_at","x":120,"y":80}
}
```

这里的 `x/y` 必须是该张图片内实际目标的位置，仅为参数形状示例。工具自动应用裁剪偏移、图片缩放与分片偏移；不要手动乘 DPR，也不要把 `canvas.width/height` 当作页面屏幕尺寸。未传 `imageId` 时兼容旧的 CSS 坐标契约；新调用建议总是提供。

Canvas 支持连续鼠标拖动、右键、双击和横向滚动：

```json
{"action":"drag","x":80,"y":90,"to_x":260,"to_y":160,"duration_ms":160}
{"action":"click_at","x":120,"y":80,"button":"right","click_count":1}
{"action":"scroll_at","x":150,"y":120,"delta":0,"delta_x":300}
```

拖动需要视口或局部图（`fullPage=false`），不能用跨页自动滚动拼接拖动。每个动作依旧受当前快照、坐标边界及目标画面校验约束。截图坐标点击前比较落点附近的像素，鼠标移入后再次比较；页面滚动或区域显著改变时要求重新定位。该守卫不是语义识别：相似外观替换、小于阈值的变化及输入时刻的竞态仍可能漏检；动态画布、悬停高亮也可能触发保守拒绝。无需改变页面的绘图实现。

WebGL 页面可以沿同样的截图/坐标路径处理，但本次执行环境不能创建 WebGL 上下文，对应用例明确跳过。不能据普通 Canvas 通过就宣称 WebGL 或所有 Canvas 应用已通过端到端验收。

## 剑来：局部细看与实际落点校验

`regionSpace=image` 接受上一张返回图片中的裁剪矩形，由工具反算到原始截图坐标并重新采集；支持已经裁剪和缩放过的图片，不要求模型自行计算 DPI、负屏幕坐标及二次偏移。

```json
{
  "operation":"screenshot",
  "snapshotId":"<最新 snapshotId>",
  "imageId":"<最新 imageId>",
  "regionSpace":"image",
  "region":{"x":100,"y":60,"width":400,"height":200},
  "maxEdge":0
}
```

原有 `regionSpace=source`（省略时默认）继续接受原图像素，需要 `windowId` 或 `monitorId`。任何局部截图都会返回新的图片和快照；后续按新图操作，不沿用旧坐标。

点击、双击、拖动及滚动前在原始、未标记鼠标圆圈的截图上校验落点邻域；不因无关区域动画而直接否定整屏。移动鼠标后读取操作系统实际落点，偏离请求超过 1 像素则不继续按键；前台变化也会中止后续输入。没有偷偷切换为 UIAutomation，也不进行盲目坐标补偿或重试。该变化增加了原生抓图/校验成本，尚未测得 Windows 整体任务提速。

只移除最后一项动作之后、正式反馈取样之前重复叠加的 80ms 等待，仍保留原有反馈至少 600ms、连续 450ms 近似稳定、最多 2 秒的检查。`stable` 不是任务完成的证据。执行状态和业务结果必须分开核对。

## 验证与边界

快速测试：

```text
node --test scripts/automation-schema.test.mjs scripts/nova-tools-mcp.test.mjs extensions/nova-chrome/worker.test.mjs
node scripts/browser-precision.test.mjs
```

浏览器测试使用真实 Chromium/CDP 和生产页面脚本，并从 Rust 源码提取实际 iframe 映射脚本执行。测试浏览器不是生产依赖；脚本支持 `TEST_BROWSER`、`TEST_REPORT`、`TEST_SCREENSHOT` 和 `REQUIRE_WEBGL=1`。它**不代替编译后的 Rust/Tauri 执行链、真实扩展和系统桌面验收**。

Windows 完整基础验证脚本：

```powershell
powershell -NoProfile -File .\scripts\verify-automation.ps1 -InstallDependencies -RequireWebGL
```

`-InstallDependencies` 显式运行 `npm ci`，只重建本项目依赖；已经安装时可以省略。该脚本需要 Node、npm、Rust/MSVC、项目原有的 Tauri 构建依赖；没有编译器会失败而非假装跳过成功。它不自动执行 `--ignored` 的桌面鼠标测试，不替代在可操作测试桌面上的人工验收。没有 WebGL 能力而指定 `-RequireWebGL` 时也必须失败。

验收还应覆盖 Windows 100%/125%/150%/200% 缩放、多屏负坐标、真实 Chrome 扩展、跨进程 iframe、目标 Canvas 应用（拖动/缩放/文本编辑/右键菜单）和中途用户抢焦点。生产输入仍可能和页面动画、外部用户或其他实例竞争，不能承诺所有页面零误点。
