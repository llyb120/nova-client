# 剑来（jianlai）

系统级桌面工具，和 `chrome` / `webview` 并存。通过 Enigo 注入原生鼠标键盘事件，通过 XCap 获取程序窗口和显示器截图；不使用 Playwright、DOM 或辅助模型。

- `windows`：列出程序名称、窗口标题、PID、windowId。
- `screenshot`：优先传 windowId 截目标窗口；省略则返回全部显示器独立图片（最多16屏）。默认长边最多1600像素；`maxEdge: 0` 使用原分辨率，否则允许640–3840。
- `act`：传 snapshotId、imageId 和 actions（1–8项），默认立即返回同范围的新截图。`feedback: "none"` 可省略截图，但下次操作前必须重新截图。
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

窗口输入要求目标在前台；后台窗口可以截图，但不能盲目操作。窗口截图不会激活窗口。先截桌面，Windows用Win+Tab加wait 250ms显示任务视图，根据返回图点击目标，或点击可见任务栏图标；确认成功后再截窗口。不要盲目循环切窗或猜应用热键，同一路径连续失败两次应报告阻碍。用户指定剑来时，整个桌面任务只用剑来鼠标键盘与截图，禁止shell、COM、PowerShell、P/Invoke、UIAutomation及终端脚本操控应用。窗口位置/尺寸、焦点、显示器布局发生变化会拒绝旧坐标。快照180秒过期、仅消费一次，并绑定当前Nova会话。全局锁防止剑来并行操作同一桌面。批量动作只用于已确定的连续步骤；需要根据画面判断时拆分调用。用户同时移动窗口/输入仍可能产生竞态，请操作时避免争用桌面。

`not_executed` 表示尚未发送输入。焦点、窗口几何或快照过期等目标校验失败会直接附带当前截图、新snapshotId、foreground与desktopBounds（即使feedback=none），不自动重放；窗口失焦、关闭或最小化时改为桌面观察，原因保留在windowObservationError。不能根据焦点变化猜测是哪一个程序抢焦点。

`executed` 表示输入已发送，不表示应用任务成功；`needs_review` 表示可能部分执行，检查completedActions/error和随附新图，禁止重放整个批次。观察失败会单独返回observationError，不掩盖执行状态。

## 上下文机制与适用范围

缩放、坐标映射、校验与恢复、回看、notes以及截图耗时均在共享jianlai服务层实现，Lyra、Cursor自定义工具和MCP调用使用同一套契约与策略说明。

**本次不实施历史图片裁剪，也不要求宿主淘汰旧图。** Reasonix的历史、请求组装、缓存策略、容量统计及压缩机制保持不变；其它SDK/MCP宿主仍按各自原有机制管理上下文。优化仅从新截图的尺寸、目标范围、调用策略和失败恢复入手，因此历史截图累计问题并未由本次改动解决。

滚动阅读先写notes，未记录信息可recall回看。不会删除本地旧图或重开会话来伪装上下文优化。

## 耗时诊断

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
