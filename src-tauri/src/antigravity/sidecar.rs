//! Antigravity MCP Sidecar（纯 Rust，零 `tauri` 依赖）：
//! 1. **主进程侧每会话回环 IPC 服务端（`SessionIpcServer`）**：
//!    - Linux/macOS：Unix domain socket（路径位于 app 数据目录 `<app_data_dir>/agy-mcp/<tag>.sock`，
//!      目录 `0700`、socket `0600`），通过 `peer_cred()` 校验 UID 与对端 PID 祖先归属；
//!    - Windows：每会话独立 Named Pipe `\\.\pipe\nova-agy-mcp-<pid>-<tag>`；
//!    - 每会话独立随机 token 恒时比对 + `owner` 归属校验；Drop 时自动关停监听并删除 socket 文件。
//! 2. **Sidecar 模式（`nova --antigravity-mcp-bridge --socket <path>`）**：
//!    - 走 stdio 与 `agy acp` 讲标准 MCP JSON-RPC 2.0（`initialize` / `tools/list` / `tools/call` / `ping`）；
//!    - `tools/list` 返回经 `schema_budget` 压缩（≤ 16,500 tokens、≤ 64 工具、RE2 清洗、`_meta["antigravity/alwaysLoad"] = true`）的 24 个工具；
//!    - `tools/call` 通过本机 socket/pipe 转发给主进程执行，等待期间每 15s 发送 `notifications/progress` 保活，
//!      并将文本 `{type:"text"}` 与图片 `{type:"image", data, mimeType}` 原样回传 `agy`。

use super::core::schema_budget::{
    build_fitted_nova_mcp_tools, tool_spec_to_mcp_json, McpToolSpec,
};
use super::core::system_prompt::NOVA_MCP_SERVER_NAME;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

/// 传递每会话随机鉴权 token 的环境变量名（不放 CLI 参数，避免 `ps` 泄露）。
pub const MCP_SOCKET_ENV_TOKEN: &str = "NOVA_AGY_MCP_TOKEN";
/// sidecar 命令行触发标志。
pub const MCP_BRIDGE_CLI_FLAG: &str = "--antigravity-mcp-bridge";
/// 单条 IPC 请求上限（4 MiB）。
pub const MAX_IPC_REQUEST_BYTES: usize = 4 * 1024 * 1024;
/// 单条 IPC 响应上限（32 MiB，容纳高清截图 base64）。
pub const MAX_IPC_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
/// 默认单次工具调用超时（240s，高于 `chrome_run` 3 分钟上限）。
pub const DEFAULT_TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(240);
/// 默认长任务 MCP `notifications/progress` 保活周期（15s）。
pub const DEFAULT_PROGRESS_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpIpcRequest {
    pub token: String,
    pub owner: String,
    pub cwd: String,
    pub tool: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpIpcResponse {
    pub ok: bool,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub content: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl McpIpcResponse {
    pub fn ok_content(content: Vec<Value>, is_error: bool) -> Self {
        Self {
            ok: true,
            is_error,
            content,
            error: None,
        }
    }

    pub fn ok_text(text: impl Into<String>) -> Self {
        Self::ok_content(vec![json!({ "type": "text", "text": text.into() })], false)
    }

    pub fn tool_error(message: impl Into<String>) -> Self {
        Self::ok_content(vec![json!({ "type": "text", "text": message.into() })], true)
    }

    pub fn transport_error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            is_error: true,
            content: Vec::new(),
            error: Some(message.into()),
        }
    }
}

/// 恒时字节比对，防止 token 时序侧信道。
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 构造 `session/new` 的 stdio `mcpServers` 条目（`{name, command, args, env}`）。
pub fn build_mcp_server_spec(
    exe_path: &Path,
    socket_path: &str,
    token: &str,
    cwd: &str,
    owner: &str,
) -> Value {
    json!({
        "name": NOVA_MCP_SERVER_NAME,
        "command": exe_path.to_string_lossy().into_owned(),
        "args": [
            MCP_BRIDGE_CLI_FLAG,
            "--socket", socket_path,
            "--cwd", cwd,
            "--owner", owner,
        ],
        "env": [
            { "name": MCP_SOCKET_ENV_TOKEN, "value": token }
        ]
    })
}

/// 判断 `pid` 是否为 `ancestor_pid` 本身或其后代进程（用于 socket 对端归属校验）。
pub fn is_pid_descendant_of(pid: u32, ancestor_pid: u32) -> bool {
    if pid == ancestor_pid {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        let mut cur = pid;
        for _ in 0..16 {
            let Some(ppid) = linux_parent_pid(cur) else {
                return false;
            };
            if ppid == ancestor_pid {
                return true;
            }
            if ppid <= 1 || ppid == cur {
                return false;
            }
            cur = ppid;
        }
        false
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (pid, ancestor_pid);
        // 非 Linux 平台依赖同 UID + 每会话随机 128-bit token 鉴权
        true
    }
}

#[cfg(target_os = "linux")]
fn linux_parent_pid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `/proc/<pid>/stat` 格式：`pid (comm) state ppid ...`，comm 中可能含空格与括号，需找最后一个 ')'
    let rparen = stat.rfind(')')?;
    let after = stat.get(rparen + 2..)?;
    let mut fields = after.split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse::<u32>().ok()
}

pub type IpcToolHandler = Arc<
    dyn Fn(McpIpcRequest) -> Pin<Box<dyn Future<Output = McpIpcResponse> + Send>> + Send + Sync,
>;

/// 每会话独占的本机回环 IPC 服务端（Drop 时自动停止监听并清理 socket 文件）。
pub struct SessionIpcServer {
    endpoint: String,
    #[cfg(unix)]
    socket_file: PathBuf,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl SessionIpcServer {
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// 在 `app_data_dir` 下创建每会话独立 IPC 端点（Unix: `<app_data_dir>/agy-mcp/<tag>.sock` chmod 0600；
    /// Windows: `\\.\pipe\nova-agy-mcp-<pid>-<tag>`）。
    pub fn bind(
        app_data_dir: &Path,
        session_tag: &str,
        token: String,
        expected_owner: String,
        expected_ancestor_pid: Option<u32>,
        handler: IpcToolHandler,
    ) -> Result<Self, String> {
        let safe_tag: String = session_tag
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(24)
            .collect();
        let safe_tag = if safe_tag.is_empty() {
            "session".to_string()
        } else {
            safe_tag
        };

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            use tokio::net::UnixListener;

            let socket_path = choose_unix_socket_path(app_data_dir, &safe_tag)?;
            if let Some(parent) = socket_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("创建 MCP socket 目录失败：{e}"))?;
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
            }
            let _ = std::fs::remove_file(&socket_path);
            let listener = UnixListener::bind(&socket_path)
                .map_err(|e| format!("绑定 MCP Unix socket 失败（{}）：{e}", socket_path.display()))?;
            std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("设置 MCP socket 0600 权限失败：{e}"))?;

            let endpoint = socket_path.to_string_lossy().into_owned();
            let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
            let cleanup_path = socket_path.clone();
            let my_uid = current_unix_uid();

            let task = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = &mut shutdown_rx => break,
                        accepted = listener.accept() => {
                            let Ok((stream, _)) = accepted else { break };
                            let auth_err = verify_unix_peer(&stream, my_uid, expected_ancestor_pid);
                            let token = token.clone();
                            let owner = expected_owner.clone();
                            let handler = handler.clone();
                            tokio::spawn(async move {
                                if let Err(reason) = auth_err {
                                    write_ipc_response(stream, &McpIpcResponse::transport_error(reason)).await;
                                    return;
                                }
                                serve_ipc_stream(stream, &token, &owner, handler).await;
                            });
                        }
                    }
                }
                let _ = std::fs::remove_file(&cleanup_path);
            });

            Ok(Self {
                endpoint,
                socket_file: socket_path,
                shutdown_tx: Some(shutdown_tx),
                task: Some(task),
            })
        }

        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ServerOptions;

            let pid = std::process::id();
            let endpoint = format!(r"\\.\pipe\nova-agy-mcp-{pid}-{safe_tag}");
            let mut first_opts = ServerOptions::new();
            first_opts.first_pipe_instance(true);
            let first_server = first_opts
                .create(&endpoint)
                .map_err(|e| format!("创建 MCP Named Pipe 失败（{endpoint}）：{e}"))?;

            let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
            let pipe_name = endpoint.clone();
            let _ = expected_ancestor_pid;

            let task = tokio::spawn(async move {
                let mut current = Some(first_server);
                loop {
                    let server = match current.take() {
                        Some(s) => s,
                        None => match ServerOptions::new().create(&pipe_name) {
                            Ok(s) => s,
                            Err(_) => break,
                        },
                    };
                    tokio::select! {
                        _ = &mut shutdown_rx => break,
                        connected = server.connect() => {
                            if connected.is_err() {
                                continue;
                            }
                            let token = token.clone();
                            let owner = expected_owner.clone();
                            let handler = handler.clone();
                            tokio::spawn(async move {
                                serve_ipc_stream(server, &token, &owner, handler).await;
                            });
                        }
                    }
                }
            });

            Ok(Self {
                endpoint,
                shutdown_tx: Some(shutdown_tx),
                task: Some(task),
            })
        }
    }
}

impl Drop for SessionIpcServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(&self.socket_file);
        }
    }
}

#[cfg(unix)]
fn choose_unix_socket_path(app_data_dir: &Path, safe_tag: &str) -> Result<PathBuf, String> {
    // Unix domain socket `sun_path` 上限在 macOS 为 104 字节、Linux 为 108 字节，预留余量选 96 字节。
    const MAX_SUN_PATH: usize = 96;
    let file_name = format!("{safe_tag}.sock");
    let preferred = app_data_dir.join("agy-mcp").join(&file_name);
    if preferred.to_string_lossy().len() <= MAX_SUN_PATH {
        return Ok(preferred);
    }
    let fallback_dir = std::env::temp_dir().join(format!("nova-agy-{}", std::process::id()));
    Ok(fallback_dir.join(file_name))
}

#[cfg(unix)]
fn current_unix_uid() -> u32 {
    extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}

#[cfg(unix)]
fn verify_unix_peer(
    stream: &tokio::net::UnixStream,
    expected_uid: u32,
    expected_ancestor_pid: Option<u32>,
) -> Result<(), String> {
    let cred = stream
        .peer_cred()
        .map_err(|e| format!("无法校验对端凭证：{e}"))?;
    if cred.uid() != expected_uid {
        return Err("拒绝来自其他本地用户的 MCP 连接".into());
    }
    if let Some(ancestor) = expected_ancestor_pid {
        if let Some(peer_pid) = cred.pid().and_then(|p| u32::try_from(p).ok()) {
            let my_pid = std::process::id();
            let allowed = peer_pid == my_pid
                || is_pid_descendant_of(peer_pid, ancestor)
                || is_pid_descendant_of(peer_pid, my_pid);
            if !allowed {
                return Err(format!(
                    "拒绝非本会话子进程的 MCP 连接（peer_pid={peer_pid}, expected_ancestor={ancestor}）"
                ));
            }
        }
    }
    Ok(())
}

async fn serve_ipc_stream<S>(
    stream: S,
    expected_token: &str,
    expected_owner: &str,
    handler: IpcToolHandler,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(stream);
    let mut raw = Vec::new();
    let limit = (MAX_IPC_REQUEST_BYTES as u64).saturating_add(1);
    let read_res = (&mut reader).take(limit).read_until(b'\n', &mut raw).await;

    let response = match read_res {
        Ok(0) => return,
        Ok(_) if raw.len() > MAX_IPC_REQUEST_BYTES => {
            McpIpcResponse::transport_error("MCP IPC 请求超过大小上限")
        }
        Ok(_) => match serde_json::from_slice::<McpIpcRequest>(&raw) {
            Ok(req) => {
                if !constant_time_eq(req.token.as_bytes(), expected_token.as_bytes()) {
                    McpIpcResponse::transport_error("MCP IPC 鉴权失败（token 不匹配）")
                } else if !expected_owner.is_empty() && req.owner != expected_owner {
                    McpIpcResponse::transport_error("MCP IPC 会话归属不匹配")
                } else {
                    handler(req).await
                }
            }
            Err(e) => McpIpcResponse::transport_error(format!("解析 MCP IPC 请求失败：{e}")),
        },
        Err(e) => McpIpcResponse::transport_error(format!("读取 MCP IPC 请求失败：{e}")),
    };

    write_ipc_response(reader.into_inner(), &response).await;
}

async fn write_ipc_response<S>(mut stream: S, response: &McpIpcResponse)
where
    S: tokio::io::AsyncWrite + Unpin,
{
    if let Ok(mut encoded) = serde_json::to_vec(response) {
        encoded.push(b'\n');
        let _ = stream.write_all(&encoded).await;
        let _ = stream.flush().await;
        let _ = stream.shutdown().await;
    }
}

/// Sidecar 客户端：向主进程本机 socket/pipe 发起一次工具调用并等待结果。
pub async fn call_ipc_endpoint(
    endpoint: &str,
    request: &McpIpcRequest,
    timeout: Duration,
) -> Result<McpIpcResponse, String> {
    let mut payload =
        serde_json::to_vec(request).map_err(|e| format!("序列化 MCP IPC 请求失败：{e}"))?;
    payload.push(b'\n');

    tokio::time::timeout(timeout, async move {
        #[cfg(unix)]
        {
            let stream = tokio::net::UnixStream::connect(endpoint)
                .await
                .map_err(|e| format!("连接主进程 MCP socket 失败（{endpoint}）：{e}"))?;
            roundtrip_ipc_stream(stream, &payload).await
        }
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ClientOptions;
            let stream = ClientOptions::new()
                .open(endpoint)
                .map_err(|e| format!("连接主进程 MCP Named Pipe 失败（{endpoint}）：{e}"))?;
            roundtrip_ipc_stream(stream, &payload).await
        }
    })
    .await
    .map_err(|_| format!("工具调用超时（{}s）", timeout.as_secs()))?
}

async fn roundtrip_ipc_stream<S>(mut stream: S, payload: &[u8]) -> Result<McpIpcResponse, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    stream
        .write_all(payload)
        .await
        .map_err(|e| format!("写入 MCP IPC 失败：{e}"))?;
    stream
        .flush()
        .await
        .map_err(|e| format!("刷新 MCP IPC 失败：{e}"))?;

    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    let limit = (MAX_IPC_RESPONSE_BYTES as u64).saturating_add(1);
    let n = (&mut reader)
        .take(limit)
        .read_until(b'\n', &mut buf)
        .await
        .map_err(|e| format!("读取 MCP IPC 响应失败：{e}"))?;
    if n == 0 {
        return Err("主进程 MCP IPC 未返回响应即断开".into());
    }
    if buf.len() > MAX_IPC_RESPONSE_BYTES {
        return Err("主进程 MCP IPC 响应超过大小上限".into());
    }
    serde_json::from_slice::<McpIpcResponse>(&buf)
        .map_err(|e| format!("解析主进程 MCP IPC 响应失败：{e}"))
}

// ============================================================================
// Stdio MCP JSON-RPC 2.0 Sidecar 服务端
// ============================================================================

#[derive(Clone, Debug)]
pub struct SidecarConfig {
    pub socket_path: String,
    pub token: String,
    pub cwd: String,
    pub owner: String,
    pub tool_timeout: Duration,
    pub progress_interval: Duration,
}

impl SidecarConfig {
    pub fn new(
        socket_path: impl Into<String>,
        token: impl Into<String>,
        cwd: impl Into<String>,
        owner: impl Into<String>,
    ) -> Self {
        Self {
            socket_path: socket_path.into(),
            token: token.into(),
            cwd: cwd.into(),
            owner: owner.into(),
            tool_timeout: DEFAULT_TOOL_CALL_TIMEOUT,
            progress_interval: DEFAULT_PROGRESS_INTERVAL,
        }
    }
}

/// 解析 CLI 参数：识别 `--antigravity-mcp-bridge --socket <path> [--cwd <dir>] [--owner <id>] [--token <tok>]`。
/// 未携带 `--antigravity-mcp-bridge` 时返回 `None`。
pub fn parse_sidecar_cli_args(args: &[String]) -> Option<Result<SidecarConfig, String>> {
    if !args.iter().any(|a| a == MCP_BRIDGE_CLI_FLAG) {
        return None;
    }
    let mut socket_path = String::new();
    let mut cwd = String::new();
    let mut owner = String::new();
    let mut token = std::env::var(MCP_SOCKET_ENV_TOKEN).unwrap_or_default();

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--socket" => {
                if let Some(v) = args.get(i + 1) {
                    socket_path = v.clone();
                    i += 2;
                    continue;
                }
            }
            "--cwd" => {
                if let Some(v) = args.get(i + 1) {
                    cwd = v.clone();
                    i += 2;
                    continue;
                }
            }
            "--owner" => {
                if let Some(v) = args.get(i + 1) {
                    owner = v.clone();
                    i += 2;
                    continue;
                }
            }
            "--token" => {
                if let Some(v) = args.get(i + 1) {
                    token = v.clone();
                    i += 2;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }

    if socket_path.trim().is_empty() {
        return Some(Err("缺少必填参数 --socket <path>".into()));
    }
    if cwd.trim().is_empty() {
        cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".into());
    }
    Some(Ok(SidecarConfig::new(socket_path, token, cwd, owner)))
}

/// 运行 stdio MCP JSON-RPC 2.0 服务端循环，直至 stdin EOF。
pub async fn serve_mcp_stdio<R, W>(
    mut reader: R,
    mut writer: W,
    cfg: SidecarConfig,
    tools: Vec<McpToolSpec>,
) -> Result<(), String>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let tools_json: Vec<Value> = tools.iter().map(tool_spec_to_mcp_json).collect();
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|e| format!("读取 MCP stdin 失败：{e}"))?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                write_mcp_json(
                    &mut writer,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": { "code": -32700, "message": format!("Parse error: {e}") }
                    }),
                )
                .await?;
                continue;
            }
        };

        let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        // 通知（无 id）不回响应
        if id.is_none() {
            continue;
        }
        let req_id = id.unwrap();

        match method {
            "initialize" => {
                let protocol_version = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("2025-03-26");
                write_mcp_json(
                    &mut writer,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {
                            "protocolVersion": protocol_version,
                            "capabilities": {
                                "tools": { "listChanged": false }
                            },
                            "serverInfo": {
                                "name": NOVA_MCP_SERVER_NAME,
                                "version": env!("CARGO_PKG_VERSION")
                            }
                        }
                    }),
                )
                .await?;
            }
            "ping" => {
                write_mcp_json(
                    &mut writer,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {}
                    }),
                )
                .await?;
            }
            "tools/list" => {
                write_mcp_json(
                    &mut writer,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": { "tools": tools_json }
                    }),
                )
                .await?;
            }
            "tools/call" => {
                let raw_name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim();
                let tool_name = raw_name
                    .strip_prefix("mcp_nova_")
                    .unwrap_or(raw_name)
                    .to_string();
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .filter(|v| !v.is_null())
                    .unwrap_or_else(|| json!({}));
                let progress_token = params
                    .pointer("/_meta/progressToken")
                    .cloned()
                    .filter(|v| !v.is_null());

                if tool_name.is_empty() {
                    write_mcp_json(
                        &mut writer,
                        &json!({
                            "jsonrpc": "2.0",
                            "id": req_id,
                            "result": {
                                "content": [{ "type": "text", "text": "tools/call 缺少工具名 name" }],
                                "isError": true
                            }
                        }),
                    )
                    .await?;
                    continue;
                }

                let ipc_req = McpIpcRequest {
                    token: cfg.token.clone(),
                    owner: cfg.owner.clone(),
                    cwd: cfg.cwd.clone(),
                    tool: tool_name,
                    arguments,
                };

                let call_fut = call_ipc_endpoint(&cfg.socket_path, &ipc_req, cfg.tool_timeout);
                tokio::pin!(call_fut);

                let mut progress_seq = 0u64;
                let mut ticker = tokio::time::interval(cfg.progress_interval.max(Duration::from_millis(50)));
                ticker.tick().await; // 跳过立即触发的首 tick

                let ipc_outcome = loop {
                    tokio::select! {
                        biased;
                        res = &mut call_fut => break res,
                        _ = ticker.tick() => {
                            if let Some(ref ptok) = progress_token {
                                progress_seq += 1;
                                let _ = write_mcp_json(
                                    &mut writer,
                                    &json!({
                                        "jsonrpc": "2.0",
                                        "method": "notifications/progress",
                                        "params": {
                                            "progressToken": ptok,
                                            "progress": progress_seq,
                                        }
                                    }),
                                ).await;
                            }
                        }
                    }
                };

                let call_result = match ipc_outcome {
                    Ok(resp) if resp.ok => {
                        let content = if resp.content.is_empty() {
                            vec![json!({ "type": "text", "text": "" })]
                        } else {
                            resp.content
                        };
                        json!({
                            "content": content,
                            "isError": resp.is_error,
                        })
                    }
                    Ok(resp) => {
                        let msg = resp
                            .error
                            .unwrap_or_else(|| "主进程工具桥返回未知错误".into());
                        json!({
                            "content": [{ "type": "text", "text": msg }],
                            "isError": true,
                        })
                    }
                    Err(err) => {
                        json!({
                            "content": [{ "type": "text", "text": err }],
                            "isError": true,
                        })
                    }
                };

                write_mcp_json(
                    &mut writer,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": call_result,
                    }),
                )
                .await?;
            }
            _ => {
                write_mcp_json(
                    &mut writer,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "error": {
                            "code": -32601,
                            "message": format!("Method not found: {method}")
                        }
                    }),
                )
                .await?;
            }
        }
    }

    Ok(())
}

async fn write_mcp_json<W: AsyncWrite + Unpin>(writer: &mut W, value: &Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|e| format!("序列化 MCP JSON 失败：{e}"))?;
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .await
        .map_err(|e| format!("写入 MCP stdout 失败：{e}"))?;
    writer
        .flush()
        .await
        .map_err(|e| format!("刷新 MCP stdout 失败：{e}"))?;
    Ok(())
}

/// 阻塞式启动轻量 single-thread tokio 运行时执行 stdio MCP sidecar（绝不启动 Tauri/WebView）。
pub fn run_sidecar_blocking(cfg: SidecarConfig) -> i32 {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("[nova-mcp-bridge] 创建 tokio runtime 失败：{e}");
            return 1;
        }
    };
    let (tools, _report) = build_fitted_nova_mcp_tools();
    let res = runtime.block_on(async move {
        let stdin = BufReader::new(tokio::io::stdin());
        let stdout = tokio::io::stdout();
        serve_mcp_stdio(stdin, stdout, cfg, tools).await
    });
    match res {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("[nova-mcp-bridge] 运行失败：{e}");
            1
        }
    }
}
