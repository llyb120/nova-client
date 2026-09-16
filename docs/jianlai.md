# 剑来（jianlai）

系统级桌面工具，和 `chrome` / `webview` 并存。通过 Enigo 注入原生鼠标键盘事件，通过 XCap 获取程序窗口和显示器截图；不使用 Playwright、DOM 或辅助模型。

- `windows`：列出程序名称、窗口标题、PID、windowId。
- `screenshot`：传 windowId 截该程序窗口；省略则一次返回全部显示器的独立原始图片（最多16屏）。
- `act`：传 snapshotId、imageId 和 actions（1–8项），默认立即返回同范围的新截图。`feedback: "none"` 可省略截图，但下次操作前必须重新截图。
- 点击、双击、移动、拖拽、Unicode文本输入、组合键、双轴滚动和有界等待均为真实系统输入。坐标以所选图片原始像素为准，服务端映射负坐标屏幕、Retina/缩放坐标。

示例：

```json
{"operation":"windows"}
{"operation":"screenshot","windowId":123}
{"operation":"act","snapshotId":"上次返回值","imageId":"window-123","actions":[{"action":"click","x":180,"y":90},{"action":"type","text":"你好"}]}
```

窗口输入要求目标在前台；后台窗口可以截图，但不能盲目操作。先用全屏截图点击目标使其激活，再重新截窗口。窗口位置/尺寸、焦点、显示器布局发生变化会拒绝旧坐标。快照180秒过期、仅消费一次，并绑定当前Nova会话。全局锁防止剑来并行操作同一桌面。批量动作只用于已确定的连续步骤；需要根据画面判断时拆分调用。用户同时移动窗口/输入仍可能产生竞态，请操作时避免争用桌面。

`executed` 表示输入已发送，不表示应用任务成功；`needs_review` 表示可能部分执行，检查 completedActions/error，重新截图，禁止重放整个批次。观察失败会单独返回 observationError，不掩盖执行状态。

## 平台与构建

- Windows：普通桌面；系统安全桌面/UAC及高权限程序可能阻止输入或截图，不自动提权。
- macOS：需授予 Nova 屏幕录制与辅助功能权限。
- Linux：输入仅支持 X11，需支持 EWMH 的窗口管理器；Wayland 输入明确拒绝，不回退到只能控制部分窗口的 XWayland。
- Linux 新增原生构建依赖（Debian/Ubuntu）：`libpipewire-0.3-dev libspa-0.2-dev libgbm-dev libclang-dev`，以及原有 Tauri GTK/WebKit 构建依赖。

截图保存到 Nova 配置目录的 `desktop-shots`，可能含敏感屏幕内容；目前保留供会话图像回放，按需手动清理。模型必须支持图像输入才能闭环操作。

## 验证

`cargo test --manifest-path src-tauri/Cargo.toml --lib jianlai` 验证坐标、按键与参数边界。

`cargo test --manifest-path src-tauri/Cargo.toml --lib jianlai::tests::desktop_smoke -- --ignored` 在真实桌面验证截图→鼠标移动→截图及拒绝重放（只移动指针，不点击或输入）。请在可被操作的测试桌面执行。
