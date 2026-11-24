//! 手写异步 JSON-RPC 2.0 NDJSON 复用连接（零第三方 ACP/RPC crate）：
//! - 出站 request（带超时与 pending map 关联）与 notification；
//! - 入站 `session/update` 通知经 `mpsc::UnboundedSender<InboundEvent>` 传出；
//! - 入站反向 RPC `session/request_permission` 按优先级自动放行（或外发供 UI 响应），
//!   并经 `PermissionIdentityRegistry` 做身份一致性防护；
//! - 子进程退出或协议错误时立即 fail 所有挂起请求，绝不自动死循环重启。

use super::process::{AcpProcess, LaunchConfig, StderrRing};
use super::protocol::{
    encode_ndjson_frame, parse_initialize_result, parse_ndjson_line, parse_permission_request,
    parse_prompt_result, parse_session_open_result, read_ndjson_frame, select_auto_allow_option,
    AcpError, AcpErrorKind, IncomingMessage, InitializeResult, PermissionIdentityRegistry,
    PermissionRequestParams, PromptResult, SessionNewParams, SessionOpenResult,
    SessionPromptParams, SessionResumeParams, ACP_PROTOCOL_VERSION, MAX_FRAME_BYTES,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot};

pub const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(30);
pub const SESSION_OP_TIMEOUT: Duration = Duration::from_secs(120);
pub const CONFIG_OP_TIMEOUT: Duration = Duration::from_secs(30);
pub const CANCEL_GRACE_MS: u64 = 1_500;

#[derive(Clone, Debug)]
pub struct ConnectionOptions {
    /// 默认自动放行 `session/request_permission`（优先级：`allow_once` > `allow_always` > `allow` > `allow*`）。
    pub auto_approve_permissions: bool,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            auto_approve_permissions: true,
        }
    }
}

#[derive(Clone, Debug)]
pub enum InboundEvent {
    SessionUpdate {
        session_id: String,
        update: Value,
    },
    /// 子进程实时 stderr 单行（供 `ResourceExhaustionTracker` 解析 429 重试风暴）。
    StderrLine(String),
    PermissionAutoApproved {
        session_id: String,
        tool_call_id: String,
        title: String,
        option_id: String,
    },
    PermissionRejected {
        session_id: String,
        tool_call_id: String,
        reason: String,
    },
    PermissionRequest {
        rpc_id: Value,
        params: PermissionRequestParams,
    },
    ProtocolError(AcpError),
    ProcessExited {
        exit_code: Option<i32>,
        stderr_tail: Option<String>,
    },
}

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, AcpError>>>>>;

pub struct AcpConnection {
    stdin_tx: mpsc::UnboundedSender<Vec<u8>>,
    next_id: AtomicU64,
    pending: PendingMap,
    stderr_ring: Arc<Mutex<StderrRing>>,
    process: Arc<tokio::sync::Mutex<Option<AcpProcess>>>,
    closed: Arc<AtomicBool>,
    child_pid: Option<u32>,
    spawned_at: std::time::Instant,
    writer_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    reader_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    stderr_forward_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl AcpConnection {
    pub async fn spawn(
        cfg: LaunchConfig,
        opts: ConnectionOptions,
    ) -> Result<(Self, mpsc::UnboundedReceiver<InboundEvent>), AcpError> {
        Self::spawn_with_child_hook(cfg, opts, |_| {}).await
    }

    pub async fn spawn_with_child_hook<F>(
        cfg: LaunchConfig,
        opts: ConnectionOptions,
        on_child: F,
    ) -> Result<(Self, mpsc::UnboundedReceiver<InboundEvent>), AcpError>
    where
        F: FnOnce(&Child),
    {
        let (stderr_line_tx, mut stderr_line_rx) = mpsc::unbounded_channel::<String>();
        let spawned_at = std::time::Instant::now();
        let (proc, mut stdin, stdout) =
            AcpProcess::spawn_with_hook_and_stderr(&cfg, on_child, Some(stderr_line_tx))?;
        let child_pid = proc.pid();
        let stderr_ring = proc.stderr_ring();
        let process = Arc::new(tokio::sync::Mutex::new(Some(proc)));
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel::<InboundEvent>();

        // 实时转发子进程 stderr 行给上层 429 配额风暴检测器
        let stderr_inbound_tx = inbound_tx.clone();
        let stderr_forward_task = tokio::spawn(async move {
            while let Some(line) = stderr_line_rx.recv().await {
                if stderr_inbound_tx.send(InboundEvent::StderrLine(line)).is_err() {
                    break;
                }
            }
        });

        // 写协程：顺序刷写 NDJSON 帧，保证多协程并发请求时不交错字节。
        let writer_closed = closed.clone();
        let writer_pending = pending.clone();
        let writer_stderr = stderr_ring.clone();
        let writer_task = tokio::spawn(async move {
            while let Some(frame) = stdin_rx.recv().await {
                if stdin.write_all(&frame).await.is_err() || stdin.flush().await.is_err() {
                    writer_closed.store(true, Ordering::SeqCst);
                    let tail = stderr_tail_from(&writer_stderr);
                    fail_all_pending(
                        &writer_pending,
                        AcpError::new(AcpErrorKind::Transport, "写入 ACP stdin 失败，通道已关闭")
                            .with_stderr(tail),
                    );
                    break;
                }
            }
            let _ = stdin.shutdown().await;
        });

        // 读协程：持续消费 stdout 有界 NDJSON 帧并派发响应、通知与反向请求。
        let reader_pending = pending.clone();
        let reader_stdin = stdin_tx.clone();
        let reader_closed = closed.clone();
        let reader_process = process.clone();
        let reader_stderr = stderr_ring.clone();
        let auto_approve = opts.auto_approve_permissions;
        let reader_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut identity_registry = PermissionIdentityRegistry::default();

            loop {
                let raw = match read_ndjson_frame(&mut reader, MAX_FRAME_BYTES).await {
                    Ok(Some(raw)) => raw,
                    Ok(None) => {
                        reader_closed.store(true, Ordering::SeqCst);
                        let exit_code = {
                            let mut guard = reader_process.lock().await;
                            match guard.as_mut() {
                                Some(p) => p.wait_exit_code(Duration::from_millis(200)).await,
                                None => None,
                            }
                        };
                        let tail = stderr_tail_from(&reader_stderr);
                        let msg = match exit_code {
                            Some(code) => format!("Antigravity 子进程已退出（退出码 {code}）"),
                            None => "Antigravity 子进程意外断开（stdout EOF）".to_string(),
                        };
                        fail_all_pending(
                            &reader_pending,
                            AcpError::process_exit(msg, tail.clone()),
                        );
                        let _ = inbound_tx.send(InboundEvent::ProcessExited {
                            exit_code,
                            stderr_tail: tail,
                        });
                        break;
                    }
                    Err(err) => {
                        reader_closed.store(true, Ordering::SeqCst);
                        let tail = stderr_tail_from(&reader_stderr);
                        let err = err.with_stderr(tail);
                        fail_all_pending(&reader_pending, err.clone());
                        let _ = inbound_tx.send(InboundEvent::ProtocolError(err));
                        break;
                    }
                };

                let parsed = match parse_ndjson_line(&raw) {
                    Ok(Some(msg)) => msg,
                    Ok(None) => continue,
                    Err(err) => {
                        reader_closed.store(true, Ordering::SeqCst);
                        let tail = stderr_tail_from(&reader_stderr);
                        let err = err.with_stderr(tail);
                        fail_all_pending(&reader_pending, err.clone());
                        let _ = inbound_tx.send(InboundEvent::ProtocolError(err));
                        break;
                    }
                };

                match parsed {
                    IncomingMessage::Response { id, result, error } => {
                        let numeric_id = id
                            .as_u64()
                            .or_else(|| id.as_str().and_then(|s| s.parse::<u64>().ok()));
                        if let Some(req_id) = numeric_id {
                            let tx = reader_pending.lock().unwrap().remove(&req_id);
                            if let Some(tx) = tx {
                                if let Some(rpc_err) = error {
                                    let tail = stderr_tail_from(&reader_stderr);
                                    let _ = tx.send(Err(AcpError::from_rpc_error(
                                        rpc_err.code,
                                        rpc_err.message,
                                        tail,
                                    )));
                                } else {
                                    let _ = tx.send(Ok(result.unwrap_or(Value::Null)));
                                }
                            }
                        }
                    }
                    IncomingMessage::Notification { method, params } => {
                        if method == "session/update" {
                            let session_id = params
                                .get("sessionId")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            let update = params.get("update").cloned().unwrap_or(Value::Null);
                            if update.get("sessionUpdate").and_then(Value::as_str)
                                == Some("tool_call")
                            {
                                let tid = update
                                    .get("toolCallId")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                let name = update
                                    .get("name")
                                    .or_else(|| update.get("title"))
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                let kind = update
                                    .get("kind")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                identity_registry.record_tool_call(tid, name, kind);
                            }
                            let _ = inbound_tx.send(InboundEvent::SessionUpdate {
                                session_id,
                                update,
                            });
                        }
                    }
                    IncomingMessage::Request { id, method, params } => {
                        if method == "session/request_permission" {
                            match parse_permission_request(&params) {
                                Ok(req) if auto_approve => {
                                    let tool_call_id = req
                                        .tool_call
                                        .get("toolCallId")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string();
                                    let title = req
                                        .tool_call
                                        .get("title")
                                        .or_else(|| req.tool_call.get("name"))
                                        .and_then(Value::as_str)
                                        .unwrap_or("tool")
                                        .to_string();
                                    match identity_registry.verify_permission(&req.tool_call) {
                                        Ok(()) => {
                                            if let Some(option_id) =
                                                select_auto_allow_option(&req.options)
                                            {
                                                let reply = json!({
                                                    "jsonrpc": "2.0",
                                                    "id": id,
                                                    "result": {
                                                        "outcome": {
                                                            "outcome": "selected",
                                                            "optionId": option_id,
                                                        }
                                                    }
                                                });
                                                if let Ok(frame) = encode_ndjson_frame(&reply) {
                                                    let _ = reader_stdin.send(frame);
                                                }
                                                let _ = inbound_tx.send(
                                                    InboundEvent::PermissionAutoApproved {
                                                        session_id: req.session_id,
                                                        tool_call_id,
                                                        title,
                                                        option_id,
                                                    },
                                                );
                                            } else {
                                                let reply = json!({
                                                    "jsonrpc": "2.0",
                                                    "id": id,
                                                    "result": {
                                                        "outcome": { "outcome": "cancelled" }
                                                    }
                                                });
                                                if let Ok(frame) = encode_ndjson_frame(&reply) {
                                                    let _ = reader_stdin.send(frame);
                                                }
                                                let _ = inbound_tx.send(
                                                    InboundEvent::PermissionRejected {
                                                        session_id: req.session_id,
                                                        tool_call_id,
                                                        reason: "未找到任何 allow* 放行选项".into(),
                                                    },
                                                );
                                            }
                                        }
                                        Err(reason) => {
                                            let reply = json!({
                                                "jsonrpc": "2.0",
                                                "id": id,
                                                "result": {
                                                    "outcome": { "outcome": "cancelled" }
                                                }
                                            });
                                            if let Ok(frame) = encode_ndjson_frame(&reply) {
                                                let _ = reader_stdin.send(frame);
                                            }
                                            let _ = inbound_tx.send(
                                                InboundEvent::PermissionRejected {
                                                    session_id: req.session_id,
                                                    tool_call_id,
                                                    reason,
                                                },
                                            );
                                        }
                                    }
                                }
                                Ok(req) => {
                                    let _ = inbound_tx.send(InboundEvent::PermissionRequest {
                                        rpc_id: id,
                                        params: req,
                                    });
                                }
                                Err(err) => {
                                    let reply = json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "error": {
                                            "code": -32602,
                                            "message": err.message,
                                        }
                                    });
                                    if let Ok(frame) = encode_ndjson_frame(&reply) {
                                        let _ = reader_stdin.send(frame);
                                    }
                                }
                            }
                        } else {
                            // 声明不支持的客户端回调一律立刻返回 -32601，防止服务端挂死。
                            let reply = json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": {
                                    "code": -32601,
                                    "message": format!("Client method not supported: {method}"),
                                }
                            });
                            if let Ok(frame) = encode_ndjson_frame(&reply) {
                                let _ = reader_stdin.send(frame);
                            }
                        }
                    }
                }
            }
        });

        Ok((
            Self {
                stdin_tx,
                next_id: AtomicU64::new(1),
                pending,
                stderr_ring,
                process,
                closed,
                child_pid,
                spawned_at,
                writer_task: Mutex::new(Some(writer_task)),
                reader_task: Mutex::new(Some(reader_task)),
                stderr_forward_task: Mutex::new(Some(stderr_forward_task)),
            },
            inbound_rx,
        ))
    }

    pub fn pid(&self) -> Option<u32> {
        self.child_pid
    }

    pub fn spawned_at(&self) -> std::time::Instant {
        self.spawned_at
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn stderr_tail(&self) -> Option<String> {
        stderr_tail_from(&self.stderr_ring)
    }

    /// 发送 JSON-RPC 请求并等待响应（可选超时）。
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, AcpError> {
        if self.is_closed() {
            return Err(AcpError::process_exit(
                "ACP 连接已关闭",
                self.stderr_tail(),
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = encode_ndjson_frame(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if self.stdin_tx.send(frame).is_err() {
            self.pending.lock().unwrap().remove(&id);
            return Err(AcpError::process_exit(
                "ACP 写入通道已关闭",
                self.stderr_tail(),
            ));
        }
        let response = match timeout {
            Some(dur) => match tokio::time::timeout(dur, rx).await {
                Ok(res) => res,
                Err(_) => {
                    self.pending.lock().unwrap().remove(&id);
                    return Err(AcpError::timeout(format!(
                        "ACP 请求 {method} 超时（{}s）",
                        dur.as_secs()
                    ))
                    .with_stderr(self.stderr_tail()));
                }
            },
            None => rx.await,
        };
        match response {
            Ok(result) => result,
            Err(_) => Err(AcpError::process_exit(
                format!("ACP 请求 {method} 在收到响应前连接已断开"),
                self.stderr_tail(),
            )),
        }
    }

    /// 发送 JSON-RPC 通知（无 id，不等待响应）。
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), AcpError> {
        if self.is_closed() {
            return Err(AcpError::process_exit(
                "ACP 连接已关闭",
                self.stderr_tail(),
            ));
        }
        let frame = encode_ndjson_frame(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))?;
        self.stdin_tx
            .send(frame)
            .map_err(|_| AcpError::process_exit("ACP 写入通道已关闭", self.stderr_tail()))
    }

    pub async fn initialize(
        &self,
        client_name: &str,
        version: &str,
    ) -> Result<InitializeResult, AcpError> {
        let params = json!({
            "protocolVersion": ACP_PROTOCOL_VERSION,
            "clientCapabilities": {
                "fs": {
                    "readTextFile": false,
                    "writeTextFile": false,
                },
                "terminal": false,
            },
            "clientInfo": {
                "name": client_name,
                "version": version,
            },
        });
        let raw = self
            .request("initialize", params, Some(INITIALIZE_TIMEOUT))
            .await?;
        parse_initialize_result(raw)
    }

    pub async fn session_new(
        &self,
        params: SessionNewParams,
    ) -> Result<SessionOpenResult, AcpError> {
        let raw = self
            .request("session/new", params.to_json(), Some(SESSION_OP_TIMEOUT))
            .await?;
        parse_session_open_result(raw, None)
    }

    pub async fn session_resume(
        &self,
        params: SessionResumeParams,
    ) -> Result<SessionOpenResult, AcpError> {
        let sid = params.session_id.clone();
        let raw = self
            .request(
                "session/resume",
                params.to_json(),
                Some(SESSION_OP_TIMEOUT),
            )
            .await?;
        parse_session_open_result(raw, Some(&sid))
    }

    pub async fn session_load(
        &self,
        params: SessionResumeParams,
    ) -> Result<SessionOpenResult, AcpError> {
        let sid = params.session_id.clone();
        let raw = self
            .request("session/load", params.to_json(), Some(SESSION_OP_TIMEOUT))
            .await?;
        parse_session_open_result(raw, Some(&sid))
    }

    pub async fn session_set_config_option(
        &self,
        session_id: &str,
        config_id: &str,
        value: &str,
    ) -> Result<Value, AcpError> {
        self.request(
            "session/set_config_option",
            json!({
                "sessionId": session_id,
                "configId": config_id,
                "value": value,
            }),
            Some(CONFIG_OP_TIMEOUT),
        )
        .await
    }

    pub async fn session_set_model(
        &self,
        session_id: &str,
        model_id: &str,
    ) -> Result<(), AcpError> {
        self.request(
            "session/set_model",
            json!({
                "sessionId": session_id,
                "modelId": model_id,
            }),
            Some(CONFIG_OP_TIMEOUT),
        )
        .await
        .map(|_| ())
    }

    pub async fn session_set_mode(
        &self,
        session_id: &str,
        mode_id: &str,
    ) -> Result<(), AcpError> {
        self.request(
            "session/set_mode",
            json!({
                "sessionId": session_id,
                "modeId": mode_id,
            }),
            Some(CONFIG_OP_TIMEOUT),
        )
        .await
        .map(|_| ())
    }

    /// 发起一轮 `session/prompt`；不设固定总时长超时，由外层 `PromptWatchdog` 与取消机制管控。
    pub async fn session_prompt(
        &self,
        params: SessionPromptParams,
    ) -> Result<PromptResult, AcpError> {
        let raw = self.request("session/prompt", params.to_json(), None).await?;
        parse_prompt_result(raw)
    }

    /// 发送 JSON-RPC notification `session/cancel`。
    pub async fn session_cancel(&self, session_id: &str) -> Result<(), AcpError> {
        self.notify("session/cancel", json!({ "sessionId": session_id }))
            .await
    }

    /// 尝试优雅关闭远端 ACP 会话（若服务端支持 `sessionCapabilities.close`）。
    pub async fn session_close(&self, session_id: &str) -> Result<(), AcpError> {
        self.request(
            "session/close",
            json!({ "sessionId": session_id }),
            Some(Duration::from_secs(5)),
        )
        .await
        .map(|_| ())
    }

    /// 手动回复反向 `session/request_permission`（供日后关闭自动放行时接入审批 UI）。
    pub async fn respond_permission(
        &self,
        rpc_id: Value,
        option_id: Option<&str>,
    ) -> Result<(), AcpError> {
        let outcome = match option_id {
            Some(opt) => json!({ "outcome": "selected", "optionId": opt }),
            None => json!({ "outcome": "cancelled" }),
        };
        let frame = encode_ndjson_frame(&json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "result": { "outcome": outcome },
        }))?;
        self.stdin_tx
            .send(frame)
            .map_err(|_| AcpError::process_exit("ACP 写入通道已关闭", self.stderr_tail()))
    }

    /// 优雅终止连接与子进程树，并 fail 所有挂起 RPC。
    pub async fn terminate(&self, grace_ms: u64) {
        self.closed.store(true, Ordering::SeqCst);
        fail_all_pending(&self.pending, AcpError::aborted("ACP 连接已终止"));
        if let Some(mut proc) = self.process.lock().await.take() {
            proc.terminate(grace_ms).await;
        }
        if let Some(handle) = self.writer_task.lock().unwrap().take() {
            handle.abort();
        }
        if let Some(handle) = self.reader_task.lock().unwrap().take() {
            handle.abort();
        }
        if let Some(handle) = self.stderr_forward_task.lock().unwrap().take() {
            handle.abort();
        }
    }
}

impl Drop for AcpConnection {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        fail_all_pending(&self.pending, AcpError::aborted("ACP 连接已销毁"));
        if let Some(handle) = self.writer_task.lock().unwrap().take() {
            handle.abort();
        }
        if let Some(handle) = self.reader_task.lock().unwrap().take() {
            handle.abort();
        }
        if let Some(handle) = self.stderr_forward_task.lock().unwrap().take() {
            handle.abort();
        }
        if let Some(pid) = self.child_pid {
            super::process::kill_process_group_force(pid);
        }
    }
}

fn fail_all_pending(pending: &PendingMap, error: AcpError) {
    let entries = std::mem::take(&mut *pending.lock().unwrap());
    for (_, tx) in entries {
        let _ = tx.send(Err(error.clone()));
    }
}

fn stderr_tail_from(ring: &Arc<Mutex<StderrRing>>) -> Option<String> {
    let tail = ring.lock().ok()?.tail_redacted();
    (!tail.trim().is_empty()).then_some(tail)
}
