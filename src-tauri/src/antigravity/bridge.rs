//! Antigravity MCP 工具桥主进程胶水层：
//! - 早期拦截 CLI 标志 `--antigravity-mcp-bridge` 进入纯 Rust stdio sidecar 模式（绝不启动 Tauri）；
//! - 主进程为每个活跃 Antigravity 会话启动独占回环 IPC（`SessionIpcServer`），
//!   并把 `tools/call` 派发到 Nova 现有的 `polaris`、`code_map` 实现。

#[path = "polaris_delivery.rs"]
mod polaris_delivery;

use super::sidecar::{
    build_mcp_server_spec, parse_sidecar_cli_args, run_sidecar_blocking, McpIpcRequest,
    McpIpcResponse, SessionIpcServer,
};
use serde_json::{json, Value};
use crate::lyra::config;
use tauri::{AppHandle, Manager};
use std::sync::atomic::Ordering;
use std::path::Path;
use std::sync::Arc;

/// 仅用于独立的 code_map；共享工具使用 Lyra 自己的结果预算和归档。
const MCP_TEXT_MAX_BYTES: usize = 96 * 1024;

/// 若 argv 命中 `--antigravity-mcp-bridge` 则直接运行轻量 stdio MCP sidecar 并退出进程，
/// 绝不启动 Tauri/WebView 或抢占 GUI 单实例锁。
pub fn maybe_run_antigravity_mcp_bridge() -> bool {
    let args: Vec<String> = std::env::args().collect();
    let Some(parsed) = parse_sidecar_cli_args(&args) else {
        return false;
    };
    let code = match parsed {
        Ok(cfg) => run_sidecar_blocking(cfg),
        Err(err) => {
            eprintln!("[nova-mcp-bridge] 参数错误：{err}");
            2
        }
    };
    std::process::exit(code);
}

/// 为指定 Antigravity 会话启动每会话独占的回环 IPC 服务端，并生成对应的 ACP `mcpServers` 配置项。
/// 返回的 `SessionIpcServer` 应保存在 `SessionBinding` 中随会话生死。
pub fn start_session_bridge(
    app: &AppHandle,
    app_data_dir: &Path,
    thread_id: &str,
    canonical_cwd: &str,
    agy_pid: Option<u32>,
    polaris_enabled: bool,
) -> Result<(SessionIpcServer, Value), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("无法获取当前 Nova 可执行文件路径：{e}"))?;
    let exe = dunce::canonicalize(&exe).unwrap_or(exe);
    let token = uuid::Uuid::new_v4().to_string();
    let short_id = uuid::Uuid::new_v4().simple().to_string();
    let session_tag = format!("s-{}", &short_id[..12]);
    let bound_cwd = canonical_cwd.to_string();
    let bound_owner = thread_id.to_string();

    let app = app.clone();
    let archive = app_data_dir.join("tool-results").join(&session_tag);
    let handler = Arc::new(move |req: McpIpcRequest| {
        let app = app.clone();
        let archive = archive.clone();
        let cwd = bound_cwd.clone();
        let owner = bound_owner.clone();
        Box::pin(async move {
            let effective_cwd = if req.cwd.trim().is_empty() {
                cwd
            } else {
                req.cwd
            };
            let root = Path::new(&effective_cwd);
            execute_nova_mcp_tool(&app, root, &owner, &req.tool, &req.arguments, &archive).await
        }) as std::pin::Pin<Box<dyn std::future::Future<Output = McpIpcResponse> + Send>>
    });

    let server = SessionIpcServer::bind(
        app_data_dir,
        &session_tag,
        token.clone(),
        thread_id.to_string(),
        agy_pid,
        handler,
    )?;

    let spec = build_mcp_server_spec(&exe, server.endpoint(), &token, canonical_cwd, thread_id, polaris_enabled);
    Ok((server, spec))
}

/// 在 Nova 主进程内派发仓库检索工具（`code_map` / `polaris`）。
async fn execute_nova_mcp_tool(
    app: &AppHandle,
    root: &Path,
    owner: &str,
    raw_name: &str,
    args: &Value,
    archive: &Path,
) -> McpIpcResponse {
    let name = raw_name.trim().strip_prefix("mcp_nova_").unwrap_or(raw_name.trim());
    if !root.is_dir() {
        return McpIpcResponse::tool_error(format!(
            "工作区目录不存在或不可访问：{}",
            root.display()
        ));
    }

    let state = app.state::<crate::AppState>();
    let Some(cancelled) = state.antigravity.tool_cancel_flag(owner) else {
        return McpIpcResponse::tool_error("当前没有活动的 Antigravity 轮次");
    };
    if cancelled.load(Ordering::SeqCst) {
        return McpIpcResponse::tool_error("已取消");
    }
    match name {
        "code_map" => execute_code_map(root, args).await,
        "polaris" => {
            if !state.settings.lock().unwrap().context_tools_enabled() {
                return McpIpcResponse::tool_error("Polaris 未在设置中启用");
            }
            let Some(selection) = state.lyra_lightweight_model() else {
                return McpIpcResponse::tool_error("Polaris 需要在轻量化场景中配置 Lyra 模型");
            };
            let model = match config::Roots::global().load_config(None).and_then(|value|
                config::resolve_model(&value, Some(&selection), &config::process_env())) {
                Ok(model) => model,
                Err(err) => return McpIpcResponse::tool_error(err),
            };
            let http = crate::lyra::route::client(&model.model).unwrap_or_else(crate::lyra::provider::http_client);
            match crate::lyra::polaris_agent::run_with_delivery(root, args, &model, &http, &cancelled, true).await {
                Ok(text) => {
                    let path = archive.join(format!("{}-polaris.txt", uuid::Uuid::new_v4()));
                    McpIpcResponse::ok_text(polaris_delivery::deliver(text, &path))
                }
                Err(err) => McpIpcResponse::tool_error(err),
            }
        }
        other => McpIpcResponse::tool_error(format!("未知 Nova MCP 工具：{other}")),
    }
}

async fn execute_code_map(root: &Path, args: &Value) -> McpIpcResponse {
    let code_root = root.to_path_buf();
    let params = if args.is_object() {
        args.clone()
    } else {
        json!({})
    };
    match tokio::task::spawn_blocking(move || {
        crate::nova_tools_native::context::code_map(&code_root, params)
    })
    .await
    {
        Ok(Ok(text)) => McpIpcResponse::ok_text(clamp_mcp_text(&text, MCP_TEXT_MAX_BYTES)),
        Ok(Err(err)) => McpIpcResponse::tool_error(err),
        Err(err) => McpIpcResponse::tool_error(format!("code_map 执行失败：{err}")),
    }
}

fn clamp_mcp_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    const NOTICE: &str = "\n\n[工具输出超过 96KB 上限，后续内容已截断]";
    let keep = max_bytes.saturating_sub(NOTICE.len());
    let mut end = keep.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + NOTICE.len());
    out.push_str(&text[..end]);
    out.push_str(NOTICE);
    out
}
