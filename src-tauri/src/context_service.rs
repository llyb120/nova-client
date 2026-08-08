use crate::acp::resolve_program_on_path;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Clone)]
struct GlobalServiceConfig {
    endpoint: String,
    token: String,
}

static GLOBAL_SERVICE: OnceLock<GlobalServiceConfig> = OnceLock::new();

const SERVICE_JS: &[u8] = include_bytes!("../resources/nova-context-service.mjs");

pub(crate) struct ContextService {
    endpoint: String,
    token: String,
    ready_file: PathBuf,
    child: Mutex<Option<Child>>,
}

impl ContextService {
    pub(crate) fn start(data_dir: &Path, learning_enabled: bool) -> Result<Self, String> {
        let runtime_dir = data_dir.join("runtime");
        std::fs::create_dir_all(&runtime_dir)
            .map_err(|e| format!("创建 context service 运行目录失败：{e}"))?;
        crate::nova_tools_napi_asset::materialize(&runtime_dir)?;
        let script = runtime_dir.join("nova-context-service.mjs");
        if std::fs::read(&script).ok().as_deref() != Some(SERVICE_JS) {
            std::fs::write(&script, SERVICE_JS)
                .map_err(|e| format!("释放 context service 失败：{e}"))?;
        }

        let pid = std::process::id();
        let endpoint = if cfg!(windows) {
            format!(r"\\.\pipe\nova-context-{pid}")
        } else {
            std::env::temp_dir()
                .join(format!("nova-context-{pid}.sock"))
                .to_string_lossy()
                .into_owned()
        };
        let ready_file = runtime_dir.join(format!("context-service-{pid}.ready"));
        let _ = std::fs::remove_file(&ready_file);
        let token = uuid::Uuid::new_v4().to_string();
        let node = resolve_program_on_path("node")
            .ok_or_else(|| "未找到 Node.js，无法启动全局 fast_context 服务".to_string())?;
        let mut command = Command::new(node);
        command
            .arg(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .env("NOVA_CONTEXT_SERVICE_ENDPOINT", &endpoint)
            .env("NOVA_CONTEXT_SERVICE_TOKEN", &token)
            .env("NOVA_CONTEXT_READY_FILE", &ready_file)
            .env("NOVA_CONTEXT_PARENT_PID", pid.to_string())
            .env("NOVA_CONTEXT_LEARNING_OWNER", "1")
            // 设置项控制学习开关（默认开）；service 是唯一学习 owner。
            .env(
                "NOVA_CONTEXT_LEARNING",
                if learning_enabled { "1" } else { "0" },
            )
            .env("NOVA_DATA_DIR", data_dir);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("启动全局 fast_context 服务失败：{e}"))?;

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if ready_file.is_file() {
                break;
            }
            if let Some(status) = child
                .try_wait()
                .map_err(|e| format!("检查全局 fast_context 服务失败：{e}"))?
            {
                return Err(format!("全局 fast_context 服务提前退出：{status}"));
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err("等待全局 fast_context 服务就绪超时".into());
            }
            std::thread::sleep(Duration::from_millis(25));
        }

        let _ = GLOBAL_SERVICE.set(GlobalServiceConfig {
            endpoint: endpoint.clone(),
            token: token.clone(),
        });

        Ok(Self {
            endpoint,
            token,
            ready_file,
            child: Mutex::new(Some(child)),
        })
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }
}

async fn exchange<S>(mut stream: S, request: &[u8]) -> Result<Value, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stream
        .write_all(request)
        .await
        .map_err(|e| format!("写入全局 context service 失败：{e}"))?;
    stream
        .shutdown()
        .await
        .map_err(|e| format!("结束 context service 请求失败：{e}"))?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .map_err(|e| format!("读取全局 context service 失败：{e}"))?;
    let envelope: Value = serde_json::from_slice(&response)
        .map_err(|e| format!("解析全局 context service 响应失败：{e}"))?;
    if envelope.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(envelope
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("全局 context service 调用失败")
            .to_string());
    }
    Ok(envelope.get("result").cloned().unwrap_or(Value::Null))
}

/// Lyra 与 Vega 共用同一个服务进程；本地 native fallback 不参与学习，避免双模型和丢更新。
pub(crate) async fn call_global(method: &str, root: &Path, params: Value) -> Result<Value, String> {
    let config = GLOBAL_SERVICE
        .get()
        .ok_or_else(|| "全局 context service 尚未启动".to_string())?;
    let request = serde_json::to_vec(&serde_json::json!({
        "token": config.token,
        "method": method,
        "root": root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
        "params": params,
    }))
    .map_err(|e| format!("编码全局 context service 请求失败：{e}"))?;
    let mut request = request;
    request.push(b'\n');

    let call = async {
        #[cfg(unix)]
        {
            let stream = tokio::net::UnixStream::connect(&config.endpoint)
                .await
                .map_err(|e| format!("连接全局 context service 失败：{e}"))?;
            exchange(stream, &request).await
        }
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ClientOptions;
            let stream = ClientOptions::new()
                .open(&config.endpoint)
                .map_err(|e| format!("连接全局 context service 失败：{e}"))?;
            exchange(stream, &request).await
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err("当前平台不支持全局 context service IPC".to_string())
        }
    };
    tokio::time::timeout(Duration::from_secs(120), call)
        .await
        .map_err(|_| format!("全局 context service 调用超时：{method}"))?
}

impl Drop for ContextService {
    fn drop(&mut self) {
        if let Ok(child) = self.child.get_mut() {
            if let Some(mut child) = child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        let _ = std::fs::remove_file(&self.ready_file);
        if !cfg!(windows) {
            let _ = std::fs::remove_file(&self.endpoint);
        }
    }
}
