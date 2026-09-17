# 剑来（jianlai）

系统级桌面工具，和 `chrome` / `webview` 并存。通过 Enigo 注入原生鼠标键盘事件，通过 XCap 获取程序窗口和显示器截图；不使用 Playwright、DOM 或辅助模型。

- `windows`：列出程序名称、窗口标题、PID、windowId、尺寸及monitorId；1x1等辅助窗口不适合交互。
- `screenshot`：优先传 windowId 截目标窗口；省略则返回全部显示器独立图片（最多16屏）；已知monitorId时可只截一屏，不能同时传windowId。默认长边最多1600像素；`maxEdge: 0` 使用原分辨率，否则允许640–3840。
- `act`：传 snapshotId、imageId 和 actions（1–8项），默认返回实际前台应用窗口的新截图，自动跟随新窗口/弹窗；任务视图等不可独立捕获的系统界面回退桌面。feedback=desktop可保留操作屏幕以观察窗口外菜单。返回imageId/坐标范围可能变化，下一步必须使用新图坐标。`feedback: "none"` 可省略截图，但下次操作前必须重新截图。
- 点击、双击、移动、拖拽、Unicode文本输入、组合键、双轴滚动和有界等待均为真实系统输入。坐标以**实际返回的图片像素**为准，服务端同步映射缩放、负坐标屏幕和Retina坐标；act继承快照的maxEdge。
- `wait.ms` 仅允许0–2000，默认250。整批预校验：`actions[1].ms=8000，允许范围为 0–2000；本批次尚未执行` 意味着前面的点击也没执行。
- `recall(imagePath)` 回看本会话历史结果的图片，不创建可操作快照，也不改变当前快照。旧图仅用于阅读，操作必须绑定当前观察。
- `notes` 用于滚动前记录已读条目、覆盖范围和未确认项（最多12000字符），原样作为文字保留，不代表工具已核实。请求“可用/测速”应定位后先单独触发测试，再观察和必要的短等待，不能把数节点当成验证。

示例：

```json
{"operation":"windows"}
{"operation":"screenshot","windowId":123}
{"operation":"act","snapshotId":"上次返回值","imageId":"window-123","actions":[{"action":"click","x":180,"y":90},{"action":"type","text":"你好"}]}
```

窗口输入要求目标在前台；后台窗口可以截图，但不能盲目操作。窗口截图不会激活窗口。先截桌面，Windows用Win+Tab加wait 250ms显示任务视图，根据返回图点击目标，或点击可见任务栏图标；确认成功后再截窗口。不要盲目循环切窗或猜应用热键，同一路径连续失败两次应报告阻碍。用户指定剑来时，整个桌面任务只用剑来鼠标键盘与截图，禁止shell、COM、PowerShell、P/Invoke、UIAutomation及终端脚本操控应用。窗口位置/尺寸、焦点、显示器布局发生变化会拒绝旧坐标。快照180秒过期、仅消费一次，并绑定当前Nova会话。全局锁防止剑来并行操作同一桌面。批量动作只用于已确定的连续步骤；需要根据画面判断时拆分调用。批次内焦点改变且仍有后续输入时停止并返回新图，不把剩余键盘输入送进未知窗口。用户同时移动窗口/输入仍可能产生竞态，请操作时避免争用桌面。

`not_executed` 表示尚未发送输入。焦点、窗口几何或快照过期等目标校验失败会直接附带当前截图、新snapshotId、foreground与desktopBounds（即使feedback=none），不自动重放；窗口失焦、关闭或最小化时跟随实际前台窗口，无法捕获时改为桌面观察，原因保留在windowObservationError。不能根据焦点变化猜测是哪一个程序抢焦点。

`executed` 表示输入已发送，不表示应用任务成功；`needs_review` 表示可能部分执行，检查completedActions/error和随附新图，禁止重放整个批次。观察失败会单独返回observationError，不掩盖执行状态。

## 观察与结果验证

纯 move 定位反馈直接返回单帧（not_checked），不增加稳定等待；需等待悬停菜单/提示时使用 move + wait。包含其他动作或执行状态为 needs_review 的反馈仍做稳定取样。稳定取样在编码前每隔约150ms取样，至少观察600ms，并要求连续450ms画面近似稳定；2秒取样预算耗尽返回最后一帧及 images[].stability.status=timeout。单次系统抓屏耗时无法中断，失败后的桌面回退另有一次预算。中间帧不落盘、不发送；主动 screenshot 和未执行输入的恢复截图保持单帧，标记 not_checked。feedback=none 仍不取图。

比较使用原始像素、忽略透明度，允许0.05%的像素发生明显颜色变化，以容忍光标闪烁和微小噪声。此启发式可能漏掉很小的更新，也可能被持续动画拖到超时；stable 不代表网络请求结束。必须继续核对目标、选中项/筛选条件与正文一致，无法确认时只重新观察，不重放操作。

新图通过 snapshotId 标识一次观察，observationSequence 是进程内递增序号（重启重置）；每张图附 snapshotId、capturedAt（UTC）和稳定取样统计，窗口/屏幕范围沿用 windowId、imageId 与 desktopBounds。operation 标明来源，操作结果的 basedOnSnapshotId 指向输入依据。historical=false 仅表示本次新采集，并非永远有效；recall 仍标记 historical=true，不伪造原图采集时间或新快照。

所有操作结果均为 verification=unverified：工具能确认输入执行状态，业务目标需模型依据最新图验证。结论须绑定截图及可见证据；未检查完整范围不能声称“全部”或“不存在”。notes 仍原样返回，须记录来源snapshotId和页面范围，不能跨页面混用；工具不验证笔记内容。本改动不删除、裁剪或压缩任何宿主的历史上下文。

## 上下文机制与适用范围

缩放、坐标映射、校验与恢复、回看、notes以及截图耗时均在共享jianlai服务层实现，Lyra、Cursor自定义工具和MCP调用使用同一套契约与策略说明。

**本次不实施历史图片裁剪，也不要求宿主淘汰旧图。** Reasonix的历史、请求组装、缓存策略、容量统计及压缩机制保持不变；其它SDK/MCP宿主仍按各自原有机制管理上下文。优化仅从新截图的尺寸、目标范围、调用策略和失败恢复入手，因此历史截图累计问题并未由本次改动解决。

滚动阅读先写notes，未记录信息可recall回看。不会删除本地旧图或重开会话来伪装上下文优化。

## 耗时诊断

NovaDev对image、png及压缩依赖启用优化编译；缩放通过DynamicImage进入依赖内的优化实现，保留Triangle算法和相同像素尺寸。目标校验复用同一次窗口枚举完成几何与遮挡检查，不缓存到下一动作。纯图像基准可运行 `cargo test --manifest-path src-tauri/Cargo.toml --lib jianlai::tests::screenshot_pipeline_benchmark -- --ignored --nocapture`，输出缩放/编码耗时，并验证与原缩放实现及PNG解码结果逐像素一致；此测试不操作桌面。

截图返回timingsMs：capture（系统捕获）、resize、encodeAndSave（PNG编码及落盘）、other（窗口枚举、焦点校验等）和total。MCP传输适配另返回deliveryTimingsMs（read/base64/total）及deliveredImageBytes；不包含网络及模型耗时。Windows前台身份检查直接读取窗口句柄与PID，避免每个动作多次枚举所有窗口及其元数据；这是只读状态检查，不负责激活。先比较这些分段数据，再决定是否换捕获后端/编码器，不据单次总耗时猜测瓶颈。

## 平台与构建

- Windows：普通桌面；系统安全桌面/UAC及高权限程序可能阻止输入或截图，不自动提权。
- macOS：需授予 Nova 屏幕录制与辅助功能权限。
- Linux：输入仅支持 X11，需支持 EWMH 的窗口管理器；Wayland 输入明确拒绝，不回退到只能控制部分窗口的 XWayland。
- Linux 官方构建基线为 Ubuntu 24.04（glibc 2.39）；产物不保证兼容 Ubuntu 22.04。XCap 使用的 libspa 0.10 与 Ubuntu 22.04 自带的 PipeWire 0.3.48 头文件不兼容，不能只安装同名开发包解决。
- Linux 新增原生构建依赖（Debian/Ubuntu）：`libpipewire-0.3-dev libspa-0.2-dev libgbm-dev libclang-dev`，以及原有 Tauri GTK/WebKit 构建依赖。

截图保存到 Nova 配置目录的 `desktop-shots` 下的会话隔离目录，可能含敏感屏幕内容；旧图仍保留供按需回看，按需手动清理。升级前平铺保存的图片不支持新的recall入口，但原文件不删除。模型必须支持图像输入才能闭环操作。

## 验证

`cargo test --manifest-path src-tauri/Cargo.toml --lib jianlai` 验证坐标、按键与参数边界。

`cargo test --manifest-path src-tauri/Cargo.toml --lib jianlai::tests::desktop_smoke -- --ignored` 在真实桌面验证截图→鼠标移动→截图及拒绝重放（只移动指针，不点击或输入）。请在可被操作的测试桌面执行。

## 操作调度与定位

普通点击、输入和滚动保留80ms基础处理时间；显式wait前后不叠加80ms，move后不额外等待。无需每个动作都追加400–900ms等待；只有已观察到加载或动画时再加短等待。scroll必须直接提供x/y/delta，自带鼠标定位，无需先move。相同已确认输入区的点击与输入可合批，跨页面、弹窗、对象确认仍拆分观察。输入框点击编辑区内部，避开工具栏；不清楚时先用窗口图定位，不盲点。窗口切换后的windowId仅选择反馈范围，不会通过系统API激活窗口。

## 局部定位与失效快照恢复

Windows截图会用品红空心圆标出系统读取的实际鼠标位置，`images[].cursor`给出当前图片内的圆心坐标。标记只绘制在反馈图中，不修改系统光标；圆心内部保留原图。截取前后鼠标位置不同、鼠标在图外或截图为后台窗口时不标记。小目标或定位不确定时先单独move，看新图确认落点后再click；不要把待确认的move与click合批，也无需给每次点击增加往返。标记只证明鼠标落点，不证明识别的目标正确。

Windows前台窗口从所在显示器的实际画面裁剪，保留窗口范围内独立弹出的候选框，避免PrintWindow截图遗漏浮层。窗口外菜单仍用feedback=desktop；跨屏、部分离屏或像素比例不一致时窗口裁剪拒绝执行，自动反馈回退桌面，显式screenshot可改用monitorId。后台窗口仍可独立截图，不能据此操作被遮挡内容。

Windows的press字母/数字按虚拟键发送，Ctrl+A与Ctrl+a等价；大小写文本使用type，组合键的大写由显式Shift控制，避免Enigo将大写字符的修饰位误当成键码。

`screenshot`指定windowId时可加`region:{x,y,width,height}`，区域以原始窗口图片originalWidth/originalHeight像素为准，先裁剪再按maxEdge缩放。返回图可直接用局部图片坐标act，工具负责偏移、缩放与负屏幕坐标映射；窗口几何校验仍检查完整窗口。act后的反馈恢复完整前台窗口，必须按新图重新定位。局部截图不读取DOM、UIAutomation或其它语义控件接口。

同会话旧snapshotId失效或当前没有快照时，act返回not_executed并尽量附当前观察；不执行旧输入，也不自动重试。另一会话持有的快照仍拒绝访问，不覆盖它。只允许最新截图操作，查看其它窗口会替换当前快照；旧图回看使用recall。

窄输入栏或地址看不清时先局部截图；一次误点后不要继续猜坐标。联系人以完整地址和已提交标签核实，红色错误标签不算成功；下拉列表变化后重新定位。正文在确认焦点后整段输入，误输入先确定内容和选区，不靠盲目Backspace试错。用户要求只准备草稿时，禁止发送和发送快捷键。
