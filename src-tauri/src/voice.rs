use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{process::Stdio, time::Duration};
use tauri::{AppHandle, WebviewWindow};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
    sync::{Mutex, Notify},
};

const RUNTIME: &[u8] = include_bytes!("../../scripts/voice-worker.cjs");
// ponytail: one microphone session per app; use per-window workers if simultaneous dictation is needed.
static WORKER: Mutex<Option<Worker>> = Mutex::const_new(None);
static STOP: Notify = Notify::const_new();

struct Worker {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    owner: String,
    session: Option<String>,
}

fn spawn(app: &AppHandle, owner: &str) -> Result<Worker, String> {
    let node = crate::acp::resolve_program_on_path("node")
        .ok_or("腾讯云语音需要 Node.js 22 或更新版本，请安装后重启 Nova")?;
    let digest = format!("{:x}", Sha256::digest(RUNTIME));
    let root = crate::nova_data_dir(app)
        .join("voice/runtime")
        .join(&digest[..16]);
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    std::fs::write(root.join("voice-worker.cjs"), RUNTIME).map_err(|e| e.to_string())?;
    let mut command = tokio::process::Command::new(node);
    command
        .arg(root.join("voice-worker.cjs"))
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let mut child = command
        .spawn()
        .map_err(|e| format!("启动腾讯云语音失败：{e}"))?;
    crate::acp::assign_to_agent_job(&child);
    let input = child.stdin.take().ok_or("语音输入管道不可用")?;
    let output = BufReader::new(child.stdout.take().ok_or("语音输出管道不可用")?);
    Ok(Worker {
        child,
        input,
        output,
        owner: owner.into(),
        session: None,
    })
}

#[tauri::command]
pub async fn voice_request(
    app: AppHandle,
    window: WebviewWindow,
    mut request: Value,
) -> Result<Value, String> {
    let action_owned = request["action"]
        .as_str()
        .ok_or("缺少语音操作")?
        .to_string();
    let action = action_owned.as_str();
    if !matches!(action, "start" | "audio" | "finish" | "cancel") {
        return Err("未知语音操作".into());
    }
    if action == "start" {
        let settings = crate::settings::Settings::load(&crate::nova_data_dir(&app));
        if !settings.voice_input_enabled {
            return Err("请先在设置 → 辅助启用语音输入".into());
        }
        request["config"] = json!({
            "appId": settings.tencent_asr_app_id.trim(),
            "secretId": settings.tencent_asr_secret_id.trim(),
            "secretKey": settings.tencent_asr_secret_key.trim(),
            "engineModelType": settings.tencent_asr_engine_model_type.trim(),
        });
    }
    let id = request["id"].as_str().unwrap_or("");
    if id.is_empty() || id.len() > 80 {
        return Err("无效语音会话".into());
    }
    if action == "audio" {
        let samples = request["samples"].as_array().ok_or("无效音频")?;
        if samples.is_empty()
            || samples.len() > 16384
            || samples
                .iter()
                .any(|v| !v.as_f64().is_some_and(|x| x.is_finite() && x.abs() <= 1.0))
        {
            return Err("无效音频数据".into());
        }
    }
    let mut slot = WORKER.lock().await;
    if slot
        .as_mut()
        .is_some_and(|w| w.child.try_wait().ok().flatten().is_some())
    {
        *slot = None;
    }
    if slot.is_none() {
        if action != "start" {
            return Err("语音会话已结束".into());
        }
        *slot = Some(spawn(&app, window.label())?);
    }
    let worker = slot.as_mut().unwrap();
    if action == "start" {
        if worker.session.is_some() {
            return Err("已有语音输入正在进行".into());
        }
        worker.owner = window.label().into();
    } else if worker.owner != window.label() || worker.session.as_deref() != Some(id) {
        return Err("语音会话已结束".into());
    }
    if action == "cancel" {
        *slot = None;
        return Ok(json!(""));
    }
    let operation = async {
        worker
            .input
            .write_all(format!("{request}\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        worker.input.flush().await.map_err(|e| e.to_string())?;
        loop {
            let mut line = String::new();
            if worker
                .output
                .read_line(&mut line)
                .await
                .map_err(|e| e.to_string())?
                == 0
            {
                return Err("语音进程退出，请确认已安装 Node.js 22 或更新版本".to_string());
            }
            let response: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            if let Some(error) = response["error"].as_str() {
                return Err(error.to_string());
            }
            return Ok(response["result"].clone());
        }
    };
    let result = tokio::select! {
        _ = STOP.notified() => Err("语音服务已停止".into()),
        result = tokio::time::timeout(Duration::from_secs(30), operation) =>
            result.unwrap_or_else(|_| Err("腾讯云语音处理超时，请重试".into())),
    };
    if result.is_err() {
        *slot = None; // kill_on_drop prevents a timed-out response from leaking into the next request.
    } else if action == "start" {
        worker.session = Some(id.into());
    } else if matches!(action, "finish" | "cancel") {
        worker.session = None;
    }
    result
}

pub async fn shutdown() {
    STOP.notify_waiters();
    *WORKER.lock().await = None;
}
