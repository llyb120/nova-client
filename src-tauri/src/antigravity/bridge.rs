//! Antigravity MCP 工具桥主进程胶水层：
//! - 早期拦截 CLI 标志 `--antigravity-mcp-bridge` 进入纯 Rust stdio sidecar 模式（绝不启动 Tauri）；
//! - 主进程为每个活跃 Antigravity 会话启动独占回环 IPC（`SessionIpcServer`），
//!   并把 `tools/call` 派发到 Nova 现有的 `polaris`、`code_map`、`jianlai_*`、`browser_*` 实现；
//! - 屏幕/浏览器工具通过 `lyra::turn::screen_lock()` 串行互斥，大结果经 `compact_browser_result` 治理，
//!   截图文件读盘后作为标准 MCP `{ type: "image", data, mimeType }` 内容块回传。

use super::core::mapper::{build_mcp_content_blocks, infer_image_mime_type};
use super::sidecar::{
    build_mcp_server_spec, parse_sidecar_cli_args, run_sidecar_blocking, McpIpcRequest,
    McpIpcResponse, SessionIpcServer,
};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

/// MCP 工具文本回传硬上限（96 KiB，防止大输出撑爆 `agy` 上下文）。
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
    app_data_dir: &Path,
    thread_id: &str,
    canonical_cwd: &str,
    agy_pid: Option<u32>,
) -> Result<(SessionIpcServer, Value), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("无法获取当前 Nova 可执行文件路径：{e}"))?;
    let exe = dunce::canonicalize(&exe).unwrap_or(exe);
    let token = uuid::Uuid::new_v4().to_string();
    let short_id = uuid::Uuid::new_v4().simple().to_string();
    let session_tag = format!("s-{}", &short_id[..12]);
    let bound_cwd = canonical_cwd.to_string();
    let bound_owner = thread_id.to_string();

    let handler = Arc::new(move |req: McpIpcRequest| {
        let cwd = bound_cwd.clone();
        let owner = bound_owner.clone();
        Box::pin(async move {
            let effective_cwd = if req.cwd.trim().is_empty() {
                cwd
            } else {
                req.cwd
            };
            let root = Path::new(&effective_cwd);
            execute_nova_mcp_tool(root, &owner, &req.tool, &req.arguments).await
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

    let spec = build_mcp_server_spec(&exe, server.endpoint(), &token, canonical_cwd, thread_id);
    Ok((server, spec))
}

/// 在 Nova 主进程内派发上下文、桌面与浏览器工具。
pub async fn execute_nova_mcp_tool(
    root: &Path,
    owner: &str,
    raw_name: &str,
    args: &Value,
) -> McpIpcResponse {
    let name = raw_name.trim().strip_prefix("mcp_nova_").unwrap_or(raw_name.trim());
    if !root.is_dir() {
        return McpIpcResponse::tool_error(format!(
            "工作区目录不存在或不可访问：{}",
            root.display()
        ));
    }

    match name {
        "polaris" => execute_polaris(root, args).await,
        "code_map" => execute_code_map(root, args).await,
        n if n != "jianlai" && crate::jianlai_tools::is_tool(n) => {
            execute_jianlai(root, owner, n, args).await
        }
        n if n.starts_with("browser_") && crate::browser::tools::is_tool(n) => {
            execute_browser(root, owner, n, args).await
        }
        other => McpIpcResponse::tool_error(format!("未知 Nova MCP 工具：{other}")),
    }
}

async fn execute_polaris(root: &Path, args: &Value) -> McpIpcResponse {
    let code_root = root.to_path_buf();
    let mut normalized = args.clone();
    if !normalized.is_object() {
        normalized = json!({});
    }
    if let Some(object) = normalized.as_object_mut() {
        // 与 lyra/tools.rs:682-699 保持一致的 keywords 归一化（字符串转数组、去重、最多 5 项）
        let raw = object.get("keywords").cloned().unwrap_or(Value::Null);
        let values = match raw {
            Value::Array(values) => values,
            Value::String(value) => vec![Value::String(value)],
            _ => Vec::new(),
        };
        let mut seen = HashSet::new();
        let keywords: Vec<Value> = values
            .into_iter()
            .filter_map(|v| v.as_str().map(str::trim).map(str::to_string))
            .filter(|v| !v.is_empty() && seen.insert(v.to_lowercase()))
            .take(5)
            .map(Value::String)
            .collect();
        if !keywords.is_empty() {
            object.insert("keywords".into(), Value::Array(keywords));
        }
    }

    match tokio::task::spawn_blocking(move || {
        crate::nova_tools_native::context::polaris(&code_root, normalized)
    })
    .await
    {
        Ok(Ok(text)) => McpIpcResponse::ok_text(clamp_mcp_text(&text, MCP_TEXT_MAX_BYTES)),
        Ok(Err(err)) => McpIpcResponse::tool_error(err),
        Err(err) => McpIpcResponse::tool_error(format!("polaris 执行失败：{err}")),
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

async fn execute_jianlai(root: &Path, owner: &str, name: &str, args: &Value) -> McpIpcResponse {
    let mapped = match crate::jianlai_tools::arguments(name, args) {
        Ok(v) => v,
        Err(err) => return McpIpcResponse::tool_error(err),
    };
    let _screen_guard = crate::lyra::turn::screen_lock().lock().await;
    match crate::jianlai::execute(root, &mapped, owner).await {
        Ok(raw) => {
            let value = crate::jianlai_tools::result(raw, &mapped);
            build_visual_tool_response(&value)
        }
        Err(err) => McpIpcResponse::tool_error(err),
    }
}

async fn execute_browser(root: &Path, owner: &str, name: &str, args: &Value) -> McpIpcResponse {
    let _screen_guard = crate::lyra::turn::screen_lock().lock().await;
    match crate::browser::execute_tool(root, name, args, owner).await {
        Ok(value) => build_visual_tool_response(&value),
        Err(err) => McpIpcResponse::tool_error(err),
    }
}

/// 将 `browser_*` / `jianlai_*` 返回的结构化 JSON 转为 MCP 内容块：
/// - 文本部分按需经 `compact_browser_result` 压缩至 ≤ 96 KiB（保持合法 JSON 且保留 `snapshotId`/`images`）；
/// - 截图路径经读盘后转为标准 `{ "type": "image", "data": "<base64>", "mimeType": "<mime>" }` 块。
fn build_visual_tool_response(value: &Value) -> McpIpcResponse {
    let raw_text = value.to_string();
    let governed_text = if raw_text.len() > MCP_TEXT_MAX_BYTES {
        crate::lyra::tools::compact_browser_result(value, MCP_TEXT_MAX_BYTES)
    } else {
        raw_text
    };
    let content = build_mcp_content_blocks(governed_text, Some(value), |path| {
        let bytes = std::fs::read(path).ok()?;
        let mime = crate::lyra::prompt::image_media_type(path)
            .unwrap_or_else(|| infer_image_mime_type(path));
        Some((bytes, mime))
    });
    McpIpcResponse::ok_content(content, false)
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
