# CodeBuddy / WorkBuddy → OpenAI 兼容接口

Lyra 的 Rust provider 原生直连 CodeBuddy / WorkBuddy 的 `/v2/chat/completions` 模型接口，使用 Nova 内登录和现有 Rust SSE 处理，工具交给 Lyra 执行，没有新增依赖。无论本机是否安装 CLI 或桌面端，都使用同一路径。下方可选 Node.js 兼容服务供其他客户端使用和协议验证。

两者共用传输，但登录站点、会话和账号模型列表独立，不混用凭据。不再读取 CLI/桌面端登录、设置或安装包，也不会启动桌面程序解密 token。

## 在 Lyra 使用

更新并启动 Nova，在「设置 → Lyra → 添加 Provider」选择唯一的 **CodeBuddy / WorkBuddy** 入口，再选择登录通道（CodeBuddy 国内站、CodeBuddy 国际站或 WorkBuddy），在浏览器完成登录后保存并获取模型。无需安装 CLI、填写 API Key 或启动命令；Nova 使用 `codebuddy` 原生传输直连上游，不启动 Node 代理或监听本地端口。输入补全和标题生成也使用同一 Rust provider。

模型及能力均取自当前账号的云端配置，仍遵守服务端 `agents.cli.models` 可用范围；这是云端通道限制，不依赖本机 CLI。`codebuddy` / `workbuddy` 别名默认使用列表首项，可通过对应的 `CODEBUDDY_MODEL` / `WORKBUDDY_MODEL` 环境变量指定；指定模型已失效时回退到首项，不再令整份列表获取失败。显式选择不可用的模型 ID 仍会报错，不静默替换。

原生回归检查：`cargo test --manifest-path src-tauri/Cargo.toml --lib lyra`。覆盖 Nova 登录隔离、旧 CLI 配置无效化、默认模型回退、云端模型筛选、交错并行工具、流截断和补全提前结束。必须思考的模型为补全预留最多 2048 tokens 的总输出预算，收到足够正文后提前结束。

### 从旧版升级

以前仅在 CLI 或 WorkBuddy 桌面版登录的用户，需要在 Nova 的对应登录通道重新登录一次；已有 Nova 登录可继续使用。不会删除或修改原 CLI/桌面端的配置与凭据。旧 `workbuddy` 预设仍可加载，设置页保存时归一为 `preset: "codebuddy"` 和 `options.codebuddySource: "workbuddy"`，保留原 Provider ID、模型和会话引用；不会合并或删除用户已有的多个配置实例。

旧的 `CODEBUDDY_CONFIG_DIR` / `WORKBUDDY_CONFIG_DIR`、`NOVA_CODEBUDDY_AUTH_FILE` / `NOVA_WORKBUDDY_AUTH_FILE`、`NOVA_CODEBUDDY_PACKAGE` / `NOVA_WORKBUDDY_PACKAGE` 和 `WORKBUDDY_EXE` 不再影响这些 provider。模型能力不再由本机安装包补齐；云端缺失的能力采用保守默认值。

WorkBuddy 独立服务启动：`npm run workbuddy:api`。Base URL 为 `http://127.0.0.1:8789/v1`，模型选 `workbuddy` 或 `/v1/models` 中的 ID；本地服务密钥保存在 `~/.nova/alkaid/workbuddy-api.json`。启动配置与 CodeBuddy 相同，将 `NOVA_CODEBUDDY_` 前缀换成 `NOVA_WORKBUDDY_`。

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

不包含音视频、嵌入、文件上传、OpenAI 托管工具及后台任务。各模型支持的能力不同，不能视为完整 OpenAI API。回归检查使用本机模拟上游，覆盖文本、图片、工具往返、Responses SSE、并行参数、结构化输出、截断、登录隔离和取消；不代表当前账号和线上服务的实测结果。

## 认证与诊断

会话分别保存在 Nova 数据目录的 `codebuddy-login.json` / `workbuddy-login.json`。Rust provider 在请求时读取，快过期时通过 refresh token 续期；独立 Node 服务只读取该会话，过期后需要回到 Nova 刷新登录。Node 服务默认读取 `~/.nova`，可用 `NOVA_DATA_DIR` 指定 Nova 数据目录。

显式认证仍支持各自的 `CODEBUDDY_AUTH_TOKEN` / `CODEBUDDY_API_KEY` 或 `WORKBUDDY_AUTH_TOKEN` / `WORKBUDDY_API_KEY` 环境变量，不读取 CLI 设置里的 `env`，也不跨产品回退。显式 token 的更新由提供者负责。

`NOVA_CODEBUDDY_ENDPOINT` / `NOVA_WORKBUDDY_ENDPOINT` 仅支持 HTTPS origin（本机测试允许 HTTP 回环地址）。请求头使用固定已验证的 CodeBuddy 2.169.0 / WorkBuddy 5.8.2 协议版本，与本机安装版本无关。

```powershell
npm run test:codebuddy-api
npm run test:devin-api
```
