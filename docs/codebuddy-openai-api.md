# CodeBuddy / WorkBuddy → OpenAI 兼容接口

Lyra 的 Rust provider 直接调用 CodeBuddy CLI 使用的 `/v2/chat/completions` 模型接口，复用本机登录和现有 Rust SSE 处理，工具交给 Lyra 执行，没有新增依赖。供其他客户端使用的独立 Node.js 服务仍保留在下方。

WorkBuddy 复用同一传输，但使用 WorkBuddy 内置 CLI 的产品配置、独立登录和账号模型列表，不混用全局 CodeBuddy 凭据。

## 在 Lyra 使用

更新并启动 Nova，在「设置 → Lyra → 添加 Provider」选择 **CodeBuddy（本地 API）**，保存并获取模型。无需填写 API Key、地址或启动命令。Nova 自动使用 `codebuddy` 原生协议，不启动 Node 代理或监听本地端口。输入补全和标题生成也使用同一 Rust provider。

模型列表取自当前账号的云端配置，并按 CLI 的可用模型筛选；安装包只补充模型能力，不增加云端列表外的模型。`codebuddy` 别名使用 CLI 设置中的默认模型，未设置时使用列表首项。登录变更会在后续请求自动读取。

原生回归检查：`cargo test --manifest-path src-tauri/Cargo.toml --lib lyra`。覆盖登录切换/注销、云端模型筛选、交错并行工具、流截断和补全提前结束。必须思考的模型为补全预留最多 2048 tokens 的总输出预算，收到足够正文后提前结束。

### WorkBuddy

先安装并登录 WorkBuddy 桌面版，在 Lyra 中选择 **WorkBuddy（本地 API）**，保存并获取模型，无需填写密钥。模型别名为 `workbuddy`，跟随 WorkBuddy 设置中的默认模型。Nova 使用对应的 WorkBuddy 代理设置，输入补全和标题也走 Rust provider。

已适配 Windows WorkBuddy 5.8.2 的加密登录。读取时短暂调用 WorkBuddy 自带运行时解密凭据；推理和 SSE 仍由 Rust 直接处理，无需另装 Node、启动本地代理或保持 WorkBuddy 窗口打开。不修改登录文件，不保存解密后的 token；注销、登录切换和过期都会在后续请求检查。其它平台和新的加密格式尚未验证。

独立服务启动：`npm run workbuddy:api`。Base URL 为 `http://127.0.0.1:8789/v1`，模型选 `workbuddy` 或 `/v1/models` 中的 ID；本地服务密钥自动生成并保存在 `~/.nova/alkaid/workbuddy-api.json`。启动配置与 CodeBuddy 相同，将 `NOVA_CODEBUDDY_` 前缀换成 `NOVA_WORKBUDDY_`。

默认设置目录是 `~/.workbuddy`，Windows 登录文件是 `%USERPROFILE%/AppData/Local/CodeBuddyExtension/Data/Public/auth/workbuddy-desktop.info`。可用 `WORKBUDDY_EXE`、`WORKBUDDY_CONFIG_DIR`、`NOVA_WORKBUDDY_PACKAGE`、`NOVA_WORKBUDDY_AUTH_FILE` 和 `NOVA_WORKBUDDY_ENDPOINT` 指定安装、设置与认证位置；默认模型可用 `WORKBUDDY_MODEL` 覆盖。显式认证支持 `WORKBUDDY_AUTH_TOKEN` / `WORKBUDDY_API_KEY` 或 WorkBuddy 自身设置中的 `env.CODEBUDDY_AUTH_TOKEN` / `env.CODEBUDDY_API_KEY`，忽略继承的全局 CodeBuddy 密钥。

## 提供给其他客户端

```powershell
npm run codebuddy:api
```

- Base URL：`http://127.0.0.1:8788/v1`
- 模型：`codebuddy`，或 `/v1/models` 中的模型 ID
- 本地 API Key：自动生成并复用，保存在 `~/.nova/alkaid/codebuddy-api.json`；这是本地服务的密钥，不是 CodeBuddy 登录 token。

独立服务默认仅监听回环地址。可通过 `NOVA_CODEBUDDY_PORT`、`NOVA_CODEBUDDY_HOST`、`NOVA_CODEBUDDY_API_KEY`、`NOVA_CODEBUDDY_TIMEOUT_MS` 和 `NOVA_DATA_DIR` 调整启动设置；Lyra 的原生 provider 无需这些设置。

## 支持范围

- `/v1/models`、`/v1/chat/completions`、`/v1/responses`
- 普通 JSON、SSE、多轮历史、真实 token 用量和请求取消
- 函数工具、交错并行工具参数、工具结果与 `reasoning_content` 回传
- 图片输入、模型支持的推理强度、温度、top_p、token 限额、stop
- JSON 对象／JSON Schema（沿用适配器的结构化工具和本地校验）
- Responses 工具事件与 `previous_response_id`（进程内保存，一小时到期；重启后不保留）

不包含音视频、嵌入、文件上传、OpenAI 托管工具及后台任务。各模型支持的能力不同，不能视为完整 OpenAI API。当前验证环境：Node.js 24.19、CodeBuddy CLI 2.169.0；真实验证了文本、工具往返、Responses SSE 和图片，回归检查还覆盖并行参数、结构化输出、截断、注销和取消。

## 认证与诊断

Windows 默认读取 `%USERPROFILE%/AppData/Local/CodeBuddyExtension/Data/Public/auth/Tencent-Cloud.coding-copilot.info`；macOS 使用 `~/Library/Application Support/CodeBuddyExtension`，Linux 使用 `~/.local/share/CodeBuddyExtension` 下相同的 `Data/Public/auth` 文件。

读取 CLI 的 `~/.codebuddy/settings.json` 和现有 `CODEBUDDY_AUTH_TOKEN`／`CODEBUDDY_API_KEY` 环境变量，不修改或复制登录文件。当前支持普通 CLI 凭据；加密凭据、自定义企业认证存储、复杂产品配置覆盖尚未适配。登录过期时会提示打开 CodeBuddy CLI 刷新，不在适配器内轮换 refresh token。

诊断时可用 `NOVA_CODEBUDDY_AUTH_FILE` 指定凭据文件、`CODEBUDDY_CONFIG_DIR` 指定设置目录、`NOVA_CODEBUDDY_PACKAGE` 指定安装包目录；`NOVA_CODEBUDDY_ENDPOINT` 仅支持 HTTPS origin（本机测试允许 HTTP 回环地址）。原生安装找不到 npm 版本元数据时暂用已验证的 2.169.0 协议版本。

```powershell
npm run test:codebuddy-api
npm run test:devin-api
```
