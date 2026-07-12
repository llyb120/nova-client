# Nova

一个仿 Codex App 的本地桌面客户端，用图形界面驱动 [Devin CLI](https://devin.ai)（或任何 ACP 兼容 agent）。
你选择项目目录、下达任务，agent 在本地 shell 中干活，所有过程（思考、工具调用、文件 diff、终端输出、权限审批）实时呈现在界面里。

技术栈：**Rust (Tauri 2) + SolidJS + TypeScript**。

## 工作原理

应用不调用任何 HTTP API，而是把本机的 `devin` CLI 作为子进程拉起：

```
┌──────────────┐  Tauri 命令/事件   ┌──────────────┐  JSON-RPC (stdio)  ┌─────────────┐
│ SolidJS 前端 │ ◄───────────────► │  Rust 后端    │ ◄────────────────► │  devin acp  │
└──────────────┘                   └──────────────┘                     └─────────────┘
```

- 后端通过 `devin acp` 启动一个 **ACP（Agent Client Protocol）服务进程**，用 stdio 上的 JSON-RPC 通信。
- 每个会话（线程）对应一个 ACP session，绑定你选择的工作目录。
- Devin 的流式输出（`session/update` 通知）被解析为消息块、思考块、工具调用、计划等，实时推送给前端。
- Devin 请求敏感操作权限时（`session/request_permission`），界面弹出审批卡片，由你点击放行或拒绝。
- 应用进程重启后，旧会话通过 `session/load` 恢复上下文，可以继续对话。
- 会话历史持久化在本地（`~/.nova/threads.json`）。旧版本的数据目录标识是 `com.fuckdevin.desktop` / `com.nova.desktop`；升级到 Nova 后**首次启动会自动把旧目录数据拷贝到 `~/.nova`**（保留旧目录不删，可安全回退），会话/设置无缝延续。

## 功能

- codex 风格首页：选择项目（最近项目可搜索 / 浏览文件夹 / **临时会话**——不关联项目，自动在系统临时目录建空工作区）+ 输入任务直接开干
- 多会话管理：按项目目录分组的会话列表，新建 / 删除 / 重命名（双击标题）
- 每个会话独立的**模型**（可搜索，列表来自 devin 实时返回）与**模式**（Code / Ask / Plan / Bypass），聊天中途可切换、即时生效；新会话默认沿用上次选择
- 流式 transcript：Markdown 渲染、可折叠思考过程、工具调用卡片（含状态、文件位置、终端输出、原始入参/出参）
- 文件改动以 diff 形式展示（增删行高亮）
- 计划（Plan）卡片实时展示 Devin 的 todo 进度
- 权限审批：允许一次 / 总是允许 / 拒绝
- codex 式轮次折叠：每轮完成后，思考/工具调用过程自动折叠为「已处理 Xs · N tokens」行，与结论区分开，点击可展开回看
- 用量展示：每轮 token 消耗（折叠行，悬停看输入/输出明细）+ 会话累计 tokens（标题栏右侧）
- **剩余额度**：侧栏左下角实时显示日/周限额剩余百分比（直接调用 windsurf 后端 GetUserStatus，复用 devin 登录凭证；悬停看重置时间与按量积分，点击刷新，每 10 分钟自动更新，低于 20% 标红）
- **任务完成系统通知**：窗口不在前台时，任务结束（完成/出错/停止）会弹系统通知；Windows 点击通知可唤起窗口并跳转到对应会话，macOS 显示通知并可通过 Dock 唤起应用
- 随时停止当前轮次（`session/cancel`，devin 不响应时 10 秒后强制本地结束）
- 性能：应用启动即预启动 devin 进程；选定项目即预热 session，首条消息无建会话延迟
- 设置：agent 可执行文件与 ACP 启动参数（可换成任何 ACP agent）、新会话默认模式、一键重启 agent 进程、日志查看
- 工具输出自动剥离 ANSI 转义码，终端输出不再乱码
- **跨平台**：Windows 与 macOS 共享同一套体验（Finder/资源管理器、终端、通知、自更新通道按平台分离）

## 关于 Windsurf Cascade

Windsurf 已更名为 **Devin Desktop**（2026-06），其本地 agent Cascade 将于 **2026-07-01 EOL**，由 Devin Local 取代，且 Cascade 从未提供独立 CLI——所以无法也没必要接入。
本应用使用的 `devin` CLI 正是 Cognition（Windsurf 母公司）官方的 agent 入口，与 Devin Desktop 共用同一后端与模型池（Claude / Gemini / GPT / SWE 系列）。

如果想换其他 agent：任何实现 ACP 协议的 agent 都可以接入，在设置里改两项即可，例如 Claude Agent：

- 可执行文件：`npx`
- ACP 启动参数：`-y @zed-industries/claude-code-acp`

## 运行前提

- 已安装 [Devin CLI](https://docs.devin.ai/) 且 `devin` 在 PATH 中（或在设置里指定完整路径）
- 已完成 `devin` 登录认证（先在终端跑一次 `devin` 确认可用）
- Node.js ≥ 20、Rust ≥ 1.80
  - **Windows**：需 MSVC 工具链
  - **macOS**：需 Xcode Command Line Tools（`xcode-select --install`）

## 开发

```bash
npm install
npm run tauri dev
```

推送到 GitHub 后会自动跑 Windows + macOS CI（含 macOS DMG）。发版与自更新（GitHub Releases）见 [docs/github-actions.md](docs/github-actions.md)。

## 构建发布版

**Windows：**

```bat
build.bat
```

产出 `src-tauri\target\release\Nova.exe`（单文件，免安装）。

**macOS：**

```bash
chmod +x build.sh
./build.sh
```

产出 `src-tauri/target/release/Nova`。

如需安装包（Windows msi/nsis，macOS dmg/app）：

```bash
npm run tauri build
```

产物在 `src-tauri/target/release/bundle/` 下。

打包并上传自更新通道：

```bash
# Windows → nova 通道；macOS → nova-macos-{arch} 通道
python scripts/package.py
# 或
./package.sh          # macOS / Linux
package.bat           # Windows
```

会话与设置持久化在 `~/.nova/`（各平台一致）。
## 任务卡住怎么办

devin 依赖其云端推理服务（windsurf.com），网络不通时 devin 会在内部反复重试，表现为任务一直转圈：

1. 点输入框旁的**停止**按钮——若 devin 10 秒内不响应取消，应用会强制结束本轮
2. 仍不行则在**设置 → 重启 devin 进程**，所有卡住的轮次立即结束，会话上下文下次发消息时自动恢复
3. 根治需保证 devin 能访问其后端（必要时配置代理）

## 目录结构

```
src/                  # SolidJS 前端
  components/         # 侧边栏、聊天视图、工具卡片、权限卡片、设置等
  store.ts            # 全局状态 + Tauri 事件桥
  ipc.ts              # invoke 封装
src-tauri/src/
  acp.rs              # ACP 客户端：进程管理、JSON-RPC、会话路由、权限/fs 代理
  threads.rs          # 会话数据模型与本地持久化
  settings.rs         # 设置持久化
  lib.rs              # Tauri 命令与应用状态
```

## 注意

- 想全自动不弹审批：在**设置 → 新会话默认模式**选 `Bypass Permissions`（或在会话里手动切换模式）。该模式会自动批准包括写文件、执行命令在内的所有工具调用，请只在可信目录使用。
- 看到 agent 报 `Get-ChildItem: A parameter cannot be found that matches parameter name 'la'` 之类错误，是 devin 在 Windows 上尝试了 unix 命令（如 `ls -la`）后自行重试纠正，属 agent 行为，非应用问题。
- 修改设置会重启 agent 进程；进行中的轮次会被打断，历史上下文在下次发消息时自动恢复。
