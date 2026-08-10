use crate::acp::resolve_program_on_path;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SERVICE_JS: &[u8] = include_bytes!("../resources/nova-context-service.mjs");

pub(crate) struct ContextService {
    endpoint: String,
    token: String,
    ready_file: PathBuf,
    child: Mutex<Option<Child>>,
}

impl ContextService {
    pub(crate) fn start(data_dir: &Path) -> Result<Self, String> {
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
