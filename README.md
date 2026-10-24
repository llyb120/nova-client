# Nova

一个仿 Codex App 的本地桌面客户端，由 Lyra 原生代理引擎执行任务；Codex、Claude Code、Devin、CodeBuddy、WorkBuddy 的本地登录与 API 配置通过 Lyra provider 复用，不再提供独立 agent 后端。
你选择项目目录、下达任务，agent 在本地 shell 中干活，所有过程（思考、工具调用、文件 diff、终端输出、权限审批）实时呈现在界面里。

技术栈：**Rust (Tauri 2) + SolidJS + TypeScript**。

## 工作原理

SolidJS 前端通过 Tauri 命令与事件连接 Rust 后端：

- **Lyra** 直接请求配置的模型 provider，由 Nova 执行工具并管理会话上下文。
- 每个会话（线程）绑定你选择的工作目录，流式文字、思考、工具调用与计划实时推送给前端。
- 敏感操作通过权限卡片审批；会话上下文由 Lyra 管理。
- 会话历史持久化在本地 `~/.nova/`。旧版本的数据目录标识是 `com.fuckdevin.desktop` / `com.nova.desktop`；升级到 Nova 后**首次启动会自动把旧目录数据拷贝到 `~/.nova`**，保留旧目录不删；旧后端的模型迁移另按以下规则处理。

### 旧后端自动迁移

启动时会将能够确认等价 provider 的旧会话历史、场景模型、模型收藏、选模型快捷键，以及前端 local storage 中的模型选择自动迁移到 Lyra，保留原模型与思考档位（effort），而不是统一换成默认模型。迁移成功后可由 Lyra 继续会话，不再启动旧后端。

不迁移 Kimi / Cursor。其他支持迁移的旧后端若模型来源、别名、Auto 实际模型或 effort 无法确认，会保留原会话和选择并提示待处理，不猜测映射或静默换模型。请在 Lyra 配置对应 provider，然后在该会话的模型选择器中选择一个 Lyra 模型完成改绑（历史保留）；用户的 CLI 配置和凭据不会被删除。详见 [迁移边界](docs/lyra-provider-models.md#旧后端选择迁移)。

## 功能

- codex 风格首页：选择项目（最近项目可搜索 / 浏览文件夹 / **临时会话**——不关联项目，自动在系统临时目录建空工作区）+ 输入任务直接开干
- 多会话管理：按项目目录分组的会话列表，新建 / 删除 / 重命名（双击标题）
- 每个会话独立的**模型**（可搜索，列表来自 Lyra provider）与**模式**，新会话默认沿用上次选择
- 流式 transcript：Markdown 渲染、可折叠思考过程、工具调用卡片（含状态、文件位置、终端输出、原始入参/出参）
- 文件改动以 diff 形式展示（增删行高亮）
- 计划（Plan）卡片实时展示 agent 的任务进度
- 权限审批：允许一次 / 总是允许 / 拒绝
- codex 式轮次折叠：每轮完成后，思考/工具调用过程自动折叠为「已处理 Xs · N tokens」行，与结论区分开，点击可展开回看
- 用量展示：每轮 token 消耗（折叠行，悬停看输入/输出明细）+ 会话累计 tokens（标题栏右侧）
- **任务完成系统通知**：窗口不在前台时，任务结束（完成/出错/停止）会弹系统通知；Windows 点击通知可唤起窗口并跳转到对应会话，macOS 显示通知并可通过 Dock 唤起应用
- **运行时防休眠**：只要仍有会话在运行，就阻止系统因空闲自动休眠；所有会话结束后自动恢复，不强制亮屏，也不拦截手动睡眠
- 随时停止当前轮次，由 Lyra 处理取消
- 设置：Lyra provider、本地 provider 的路径（网络跟随系统代理）、新会话默认模式、日志查看
- 工具输出自动剥离 ANSI 转义码，终端输出不再乱码
- **跨平台**：Windows 与 macOS 共享同一套体验（Finder/资源管理器、终端、通知、自更新通道按平台分离）

## Lyra 本地 API provider

在「设置 → Lyra → 添加 Provider」选择 Devin / CodeBuddy / WorkBuddy（本地 API），复用对应 CLI 或桌面版的本机登录。推理由 Rust 直接请求上游，工具由 Lyra 执行；WorkBuddy 的加密登录仍通过其自带运行时读取，不依赖独立 ACP 桥。

接入说明见 [Lyra provider](docs/lyra-provider-models.md)、[Devin](docs/devin-openai-api.md) 和 [CodeBuddy / WorkBuddy](docs/codebuddy-openai-api.md)。面向其他客户端的可选 OpenAI 兼容服务及协议测试仍保留；运行 `npm run test:lyra-providers` 可检查原生 provider 连接点、兼容接口与 WorkBuddy 登录解密，避免清理独立后端时误删。

### 本地 Codex / Claude Code

在 Lyra 中添加对应本地 provider，优先读取 CLI 用户级自定义 API 配置；未配置 API 时读取本机订阅登录，由 Lyra 直接请求官方上游。不需要启动 `codex app-server` 或安装 Claude ACP adapter。

配置来源、模型别名解析及认证边界见 [Lyra provider](docs/lyra-provider-models.md)。CLI 登录配置与凭据保留在原位置，不随独立后端清理而删除。

## 运行前提

- 使用 Lyra 普通 API provider 时配置模型服务凭据即可；仅使用本地 provider 时需要对应 CLI/桌面版的配置或登录
- 仅使用 Devin 本地 API provider 时需要已登录的 [Devin CLI](https://docs.devin.ai/)（`devin auth login`）
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

构建只需前端打包，不再生成 Node 版 MCP 桥；WorkBuddy 登录辅助脚本由 Rust 内嵌。本地 provider 不使用已移除的 agent SDK/ACP 桥。Tauri 未配置整目录资源打包，更新 zip 只收录 Nova 二进制；清理旧脚本或未引用的生成资源不等于安装包会按其文件大小缩减。

**Linux 无界面服务器：**

Nova 可作为常驻后端运行，并通过 Nova Web 远程控制。服务器无需桌面环境，只需安装
`xvfb`；启动时不会显示窗口：

```bash
Nova server --relay-server https://relay.example.com --token "$NOVA_TOKEN" --project /srv/project
```

可用 `Nova server help` 查看项目管理、配置更新和自更新等无头管理命令。

完整参数、安全建议和 systemd 配置见 [Nova Headless Server](docs/headless-server.md)。

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

先点输入框旁的**停止**按钮，再检查 Lyra provider 的网络、系统代理和登录状态。Devin / CodeBuddy / WorkBuddy 本地 API provider 仍需访问各自的云端推理服务；不要尝试重启已移除的独立 agent 后端。

## 目录结构

```
src/                  # SolidJS 前端
  components/         # 侧边栏、聊天视图、工具卡片、权限卡片、设置等
  store.ts            # 全局状态 + Tauri 事件桥
  ipc.ts              # invoke 封装
src-tauri/src/
  lyra/               # 原生代理引擎、模型 provider 与工具执行
  threads.rs          # 会话数据模型与本地持久化
  settings.rs         # 设置持久化
  lib.rs              # Tauri 命令与应用状态
```

## 注意

- 想全自动不弹审批：在**设置 → 新会话默认模式**选 `Bypass Permissions`（或在会话里手动切换模式）。该模式会自动批准包括写文件、执行命令在内的所有工具调用，请只在可信目录使用。
- 修改模型连接或认证配置前，先停止进行中的轮次，避免请求被中断。
