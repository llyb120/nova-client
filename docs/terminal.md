# 右侧终端与会话快捷键

右侧工作区选择「终端」，或按 Ctrl+反引号（键帽上的 ~，也接受 Ctrl+Shift+~）；再次按下收起。可在「设置 → 通用 → 会话快捷键」添加「打开 / 收起终端」修改按键。已占用默认按键时保留原来的快捷键。

＋ 创建新终端标签，× 关闭标签并结束对应进程。切换会话、文件/终端视图或收起面板不会结束 shell；每个会话保留独立的标签、输出与运行状态，首页是独立标签组。应用退出时清理终端，不跨应用重启恢复进程。

「设置 → 通用 → 默认终端」填写 shell 可执行文件，例如 pwsh.exe、powershell.exe、cmd.exe、/bin/bash 或 /bin/zsh；启动参数每行一项，直接传递，不进行 shell 字符串拼接。留空时 Windows 使用 COMSPEC（回退 cmd.exe），Unix 使用 SHELL（回退 /bin/sh）。配置只作用于新标签。会话终端使用实际会话目录，首页终端使用已选项目目录；漫游访客不会在本机创建终端。

终端聚焦时 Esc、Ctrl+C、Ctrl+S 等交给 shell，不会误停止 AI 或保存编辑器文件。支持 Ctrl+Shift+C/V 复制粘贴（macOS 可用 Command+C/V）。最多保留 32 个标签，每个保留 5,000 行滚动历史，输入分块串行发送，输出按字节传输并限流以保留 UTF-8 和控制序列。

原「打开未读消息」快捷键仍优先未读；没有普通未读会话时循环打开进行中的普通会话，包含室女座运行会话。任务链只计一个目标并定位到最新运行阶段；训练会话和未落库占位不参与。

## 验证

`node scripts/terminal-shortcuts.test.mjs` 覆盖实际 store 动作、未读优先、运行回退、任务链及快捷键隔离。

`TEST_BROWSER=/path/to/chromium node scripts/workspace-terminal.test.mjs` 使用真实 Solid/xterm 并模拟 Tauri IPC，覆盖多标签、中文分片、控制键、切换保留、粘贴顺序、启动/关闭竞态、退出错误和尺寸变化。

`cargo test --manifest-path src-tauri/Cargo.toml --lib workspace_terminal::tests` 验证参数、边界及真实 PTY 输出/尺寸/退出。桌面验收还应测试目标平台的交互式 shell、子进程终止和应用退出清理。
