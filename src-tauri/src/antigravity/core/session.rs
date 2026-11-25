//! Antigravity 会话恢复三级降级链与热切模型/思考强度编排（纯 Rust，零 `tauri` 依赖）。
//!
//! 职责：
//! 1. **三级恢复链**：`session/resume` → `session/load`（load 期间静默丢弃重放消息事件，
//!    只保留 `config_option_update` / `current_mode_update`）→ `session/new`；
//! 2. **启动 2s 瞬态 Auth 重试**：子进程刚起 2s 内首次非 `invalid_grant`/`api_key` 鉴权失败，
//!    若进程仍存活则等待 250ms 重试 1 次；
//! 3. **热切模型/思考强度**：优先 `session/set_config_option`（`model` 与 `thought_level`），
//!    失败回退 `session/set_model`；两步设置中第二步失败返回 `HalfSwitched` 提示上层丢弃并重建 binding。

use super::connection::{AcpConnection, InboundEvent};
use super::protocol::{
    find_model_config_id, find_thought_level_config_id, resolve_model_and_effort_for_config,
    AcpError, AcpErrorKind, AgentCapabilities, SessionNewParams, SessionOpenResult,
    SessionResumeParams,
};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;

/// 启动后瞬态凭据加载窗口（覆盖 `agy` 冷启动 `keyringAuth` 10s 超时后刷新完成的时间窗）。
pub const STARTUP_TRANSIENT_AUTH_WINDOW: Duration = Duration::from_secs(15);
/// 启动瞬态鉴权失败重试间隔（ms）。
pub const STARTUP_TRANSIENT_AUTH_RETRY_DELAY: Duration = Duration::from_millis(750);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionOpenMode {
    Resumed,
    Loaded,
    Created,
    CreatedAfterFallback,
}

#[derive(Clone, Debug)]
pub struct OpenedSession {
    pub result: SessionOpenResult,
    pub mode: SessionOpenMode,
    /// `session/load` 窗口内被静默丢弃的历史重放事件数量。
    pub discarded_replay_events: usize,
    /// `session/load` 期间捕获到的最新 `configOptions`（若服务端通过通知下发）。
    pub replayed_config_options: Option<Vec<Value>>,
    /// `session/load` 期间捕获到的最新 `currentModeId`。
    pub replayed_wire_mode: Option<String>,
}

/// 判断是否属于启动窗口内可重试的瞬态 Auth 错误。
pub fn is_transient_startup_auth_error(conn: &AcpConnection, err: &AcpError) -> bool {
    is_transient_startup_auth_error_at(conn.is_closed(), conn.spawned_at().elapsed(), err)
}

fn is_transient_startup_auth_error_at(
    is_closed: bool,
    elapsed: Duration,
    err: &AcpError,
) -> bool {
    if is_closed || err.kind != AcpErrorKind::Auth {
        return false;
    }
    if elapsed > STARTUP_TRANSIENT_AUTH_WINDOW {
        return false;
    }
    let lower = err.message.to_ascii_lowercase();
    !lower.contains("invalid_grant")
        && !lower.contains("api_key")
        && !lower.contains("api key")
        && !lower.contains("not logged into")
}

/// 按 `session/resume` → `session/load`（静默抑制重放）→ `session/new` 三级打开或恢复 ACP 会话。
pub async fn open_or_recover_session(
    conn: &AcpConnection,
    inbound_rx: &mut mpsc::UnboundedReceiver<InboundEvent>,
    caps: &AgentCapabilities,
    cwd: &str,
    mcp_servers: Vec<Value>,
    meta: Option<Value>,
    saved_session_id: Option<&str>,
    force_new: bool,
) -> Result<OpenedSession, AcpError> {
    if let Some(sid) = saved_session_id.filter(|s| !s.is_empty() && !force_new) {
        // 1. 优先尝试 session/resume（无历史重放）
        if caps.session.resume {
            let resume_params = SessionResumeParams {
                session_id: sid.to_string(),
                cwd: cwd.to_string(),
                mcp_servers: mcp_servers.clone(),
                meta: meta.clone(),
            };
            let first = conn.session_resume(resume_params.clone()).await;
            let res = match first {
                Err(ref err) if is_transient_startup_auth_error(conn, err) => {
                    tokio::time::sleep(STARTUP_TRANSIENT_AUTH_RETRY_DELAY).await;
                    conn.session_resume(resume_params).await
                }
                other => other,
            };
            if let Ok(result) = res {
                return Ok(OpenedSession {
                    result,
                    mode: SessionOpenMode::Resumed,
                    discarded_replay_events: 0,
                    replayed_config_options: None,
                    replayed_wire_mode: None,
                });
            }
        }

        // 2. 降级尝试 session/load（load 期间静默丢弃历史重放事件，防止 UI 重复气泡）
        if caps.load_session && !conn.is_closed() {
            let load_params = SessionResumeParams {
                session_id: sid.to_string(),
                cwd: cwd.to_string(),
                mcp_servers: mcp_servers.clone(),
                meta: meta.clone(),
            };
            let (res, discarded, cfg_opts, wire_mode) =
                run_session_load_suppressing_replay(conn, inbound_rx, load_params.clone()).await;
            let (res, discarded, cfg_opts, wire_mode) = match res {
                Err(ref err) if is_transient_startup_auth_error(conn, err) => {
                    tokio::time::sleep(STARTUP_TRANSIENT_AUTH_RETRY_DELAY).await;
                    let (r2, d2, c2, m2) =
                        run_session_load_suppressing_replay(conn, inbound_rx, load_params).await;
                    (r2, discarded + d2, c2.or(cfg_opts), m2.or(wire_mode))
                }
                other => (other, discarded, cfg_opts, wire_mode),
            };
            if let Ok(result) = res {
                return Ok(OpenedSession {
                    result,
                    mode: SessionOpenMode::Loaded,
                    discarded_replay_events: discarded,
                    replayed_config_options: cfg_opts,
                    replayed_wire_mode: wire_mode,
                });
            }
        }

        // 3. 均失败 → 降级 session/new
        let new_params = SessionNewParams {
            cwd: cwd.to_string(),
            mcp_servers,
            meta,
        };
        let result = session_new_with_startup_retry(conn, new_params).await?;
        return Ok(OpenedSession {
            result,
            mode: SessionOpenMode::CreatedAfterFallback,
            discarded_replay_events: 0,
            replayed_config_options: None,
            replayed_wire_mode: None,
        });
    }

    let new_params = SessionNewParams {
        cwd: cwd.to_string(),
        mcp_servers,
        meta,
    };
    let result = session_new_with_startup_retry(conn, new_params).await?;
    Ok(OpenedSession {
        result,
        mode: SessionOpenMode::Created,
        discarded_replay_events: 0,
        replayed_config_options: None,
        replayed_wire_mode: None,
    })
}

async fn session_new_with_startup_retry(
    conn: &AcpConnection,
    params: SessionNewParams,
) -> Result<SessionOpenResult, AcpError> {
    match conn.session_new(params.clone()).await {
        Err(ref err) if is_transient_startup_auth_error(conn, err) => {
            tokio::time::sleep(STARTUP_TRANSIENT_AUTH_RETRY_DELAY).await;
            conn.session_new(params).await
        }
        other => other,
    }
}

async fn run_session_load_suppressing_replay(
    conn: &AcpConnection,
    inbound_rx: &mut mpsc::UnboundedReceiver<InboundEvent>,
    params: SessionResumeParams,
) -> (
    Result<SessionOpenResult, AcpError>,
    usize,
    Option<Vec<Value>>,
    Option<String>,
) {
    let mut discarded = 0usize;
    let mut latest_config_options: Option<Vec<Value>> = None;
    let mut latest_wire_mode: Option<String> = None;

    let mut handle_ev = |ev: InboundEvent| {
        if let InboundEvent::SessionUpdate { update, .. } = ev {
            match update.get("sessionUpdate").and_then(Value::as_str) {
                Some("config_option_update") => {
                    if let Some(arr) = update.get("configOptions").and_then(Value::as_array) {
                        if !arr.is_empty() {
                            latest_config_options = Some(arr.clone());
                        }
                    }
                }
                Some("current_mode_update") => {
                    if let Some(m) = update
                        .get("currentModeId")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        latest_wire_mode = Some(m.to_string());
                    }
                }
                _ => {
                    // 静默丢弃历史消息、思考块、工具调用等重放事件
                    discarded += 1;
                }
            }
        }
    };

    let load_fut = conn.session_load(params);
    tokio::pin!(load_fut);
    let result = loop {
        tokio::select! {
            biased;
            res = &mut load_fut => {
                while let Ok(ev) = inbound_rx.try_recv() {
                    handle_ev(ev);
                }
                break res;
            }
            ev = inbound_rx.recv() => {
                match ev {
                    Some(ev) => handle_ev(ev),
                    None => break load_fut.await,
                }
            }
        }
    };
    (result, discarded, latest_config_options, latest_wire_mode)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelSwitchError {
    /// 切换失败且未产生半切换态（如单步 `set_model` 失败）。
    Failed(AcpError),
    /// 两步切换（先切模型、再切思考强度）中第二步失败，连接处于半切换态，必须丢弃 binding 重建。
    HalfSwitched { error: AcpError },
}

impl std::fmt::Display for ModelSwitchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelSwitchError::Failed(e) => write!(f, "{e}"),
            ModelSwitchError::HalfSwitched { error } => {
                write!(f, "模型与思考强度切换部分失败（半切换态）：{error}")
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct ModelSwitchResult {
    pub applied_model: Option<String>,
    pub applied_effort: Option<String>,
    pub used_legacy_set_model: bool,
    pub updated_config_options: Option<Vec<Value>>,
}

/// 在现有活跃会话上原地热切模型与思考强度（不新建 session）：
/// 1. 优先通过 `session/set_config_option` 分别设置 `model` 与 `thought_level`；
/// 2. 若 `session/set_config_option(model)` 失败则回退 `session/set_model`；
/// 3. 若第 1 步（模型）成功而第 2 步（思考强度）失败，返回 `ModelSwitchError::HalfSwitched`，
///    由调用方丢弃当前 binding 并重建，防止状态漂移。
pub async fn switch_model_and_effort(
    conn: &AcpConnection,
    session_id: &str,
    config_options: &[Value],
    requested_model: Option<&str>,
    requested_effort: Option<&str>,
) -> Result<ModelSwitchResult, ModelSwitchError> {
    let raw_model = requested_model
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let raw_effort = requested_effort
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let (resolved_base_model, resolved_effort) =
        resolve_model_and_effort_for_config(config_options, raw_model, raw_effort);

    let model_cfg_id = find_model_config_id(config_options);
    let mut thought_cfg_id = find_thought_level_config_id(config_options);
    let mut updated_config_options: Option<Vec<Value>> = None;
    let mut used_legacy = false;
    let mut model_changed_step1 = false;

    // Step 1: 切换模型
    if let Some(raw_target) = raw_model {
        let base_target = resolved_base_model.as_deref().unwrap_or(raw_target);
        if let Some(cfg_id) = model_cfg_id.as_deref() {
            match conn
                .session_set_config_option(session_id, cfg_id, base_target)
                .await
            {
                Ok(resp) => {
                    model_changed_step1 = true;
                    if let Some(arr) = resp.get("configOptions").and_then(Value::as_array) {
                        if !arr.is_empty() {
                            thought_cfg_id =
                                find_thought_level_config_id(arr).or(thought_cfg_id);
                            updated_config_options = Some(arr.clone());
                        }
                    }
                }
                Err(_) => {
                    // 回退旧协议接口 session/set_model（传原始完整 modelId）
                    conn.session_set_model(session_id, raw_target)
                        .await
                        .map_err(ModelSwitchError::Failed)?;
                    used_legacy = true;
                    model_changed_step1 = true;
                }
            }
        } else {
            conn.session_set_model(session_id, raw_target)
                .await
                .map_err(ModelSwitchError::Failed)?;
            used_legacy = true;
            model_changed_step1 = true;
        }
    }

    // Step 2: 切换思考强度（若未走旧接口内置或用户显式指定了思考强度）
    let need_effort = if used_legacy {
        // 旧接口 `session/set_model(gemini-4-neon-xhigh)` 已隐含强度；仅在用户独立传了 raw_effort 时才再调
        raw_effort.map(str::to_string)
    } else {
        resolved_effort.clone()
    };

    if let Some(effort_val) = need_effort.as_deref() {
        if let Some(cfg_id) = thought_cfg_id.as_deref() {
            match conn
                .session_set_config_option(session_id, cfg_id, effort_val)
                .await
            {
                Ok(resp) => {
                    if let Some(arr) = resp.get("configOptions").and_then(Value::as_array) {
                        if !arr.is_empty() {
                            updated_config_options = Some(arr.clone());
                        }
                    }
                }
                Err(error) => {
                    if model_changed_step1 {
                        return Err(ModelSwitchError::HalfSwitched { error });
                    }
                    return Err(ModelSwitchError::Failed(error));
                }
            }
        }
    }

    Ok(ModelSwitchResult {
        applied_model: raw_model.map(str::to_string),
        applied_effort: resolved_effort,
        used_legacy_set_model: used_legacy,
        updated_config_options,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_startup_auth_window_accepts_keyring_timeout_and_rejects_permanent_failures() {
        let auth_required = AcpError::new(
            AcpErrorKind::Auth,
            "Authentication required (auth_required)",
        );
        let unauthenticated = AcpError::new(
            AcpErrorKind::Auth,
            "UNAUTHENTICATED (code 401): Request had invalid authentication credentials.",
        );
        let invalid_grant = AcpError::new(AcpErrorKind::Auth, "oauth2: invalid_grant");
        let api_key = AcpError::new(AcpErrorKind::Auth, "invalid api_key provided");
        let not_logged_in =
            AcpError::new(AcpErrorKind::Auth, "You are not logged into Antigravity.");

        assert!(is_transient_startup_auth_error_at(
            false,
            Duration::from_secs(11),
            &auth_required
        ));
        assert!(is_transient_startup_auth_error_at(
            false,
            Duration::from_secs(14),
            &unauthenticated
        ));
        assert!(!is_transient_startup_auth_error_at(
            false,
            Duration::from_secs(16),
            &auth_required
        ));
        assert!(!is_transient_startup_auth_error_at(
            true,
            Duration::from_secs(1),
            &auth_required
        ));
        assert!(!is_transient_startup_auth_error_at(
            false,
            Duration::from_secs(1),
            &invalid_grant
        ));
        assert!(!is_transient_startup_auth_error_at(
            false,
            Duration::from_secs(1),
            &api_key
        ));
        assert!(!is_transient_startup_auth_error_at(
            false,
            Duration::from_secs(1),
            &not_logged_in
        ));
    }
}
