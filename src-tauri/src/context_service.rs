use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::{mpsc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

pub(crate) struct ContextService {
    endpoint: String,
    token: String,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Deserialize)]
struct ContextRequest {
    token: String,
    method: String,
    root: String,
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct ContextResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl ContextResponse {
    fn success(result: Value) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    fn failure(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(error.into()),
        }
    }
}

fn dispatch(request: ContextRequest, token: &str) -> Result<Value, String> {
    if request.token != token {
        return Err("unauthorized context service request".into());
    }
    if request.method == "ping" {
        return Ok(serde_json::json!({
            "pid": std::process::id(),
            "transport": "nova-context-jsonl-v1",
            "runtime": "rust"
        }));
    }

    let root = Path::new(&request.root);
    if !root.is_dir() {
        return Err(format!(
            "workspace root is not a directory: {}",
            request.root
        ));
    }
    let mut params = request.params;
    if request.method == "fast_context" {
        if let Some(object) = params.as_object_mut() {
            object.insert("_contextMode".into(), Value::String("fast".into()));
        }
    }
    let output = match request.method.as_str() {
        "fast_context" => crate::nova_tools_native::context::fast_context(root, params),
        "find_symbols" => crate::nova_tools_native::context::find_symbols(root, params),
        "code_map" => crate::nova_tools_native::context::code_map(root, params),
        _ => Err(format!(
            "unknown context service method: {}",
            request.method
        )),
    }?;
    Ok(Value::String(output))
}

fn response_for_line(line: &str, token: &str) -> ContextResponse {
    match serde_json::from_str::<ContextRequest>(line) {
        Ok(request) => match dispatch(request, token) {
            Ok(result) => ContextResponse::success(result),
            Err(error) => ContextResponse::failure(error),
        },
        Err(error) => ContextResponse::failure(error.to_string()),
    }
}

async fn serve_stream<S>(stream: S, token: &str)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    let response = match stream.read_line(&mut line).await {
        Ok(0) => return,
        Ok(_) if line.len() <= 2 * 1024 * 1024 => {
            let line = line.trim_end().to_string();
            let token = token.to_string();
            tokio::task::spawn_blocking(move || response_for_line(&line, &token))
                .await
                .unwrap_or_else(|error| {
                    ContextResponse::failure(format!("context worker failed: {error}"))
                })
        }
        Ok(_) => ContextResponse::failure("context request too large"),
        Err(error) => ContextResponse::failure(error.to_string()),
    };
    if let Ok(mut encoded) = serde_json::to_vec(&response) {
        encoded.push(b'\n');
        let stream = stream.get_mut();
        let _ = stream.write_all(&encoded).await;
        let _ = stream.shutdown().await;
    }
}

#[cfg(windows)]
async fn run_server(
    endpoint: String,
    token: String,
    ready: mpsc::Sender<Result<(), String>>,
    mut shutdown: oneshot::Receiver<()>,
) {
    use tokio::net::windows::named_pipe::ServerOptions;

    let mut first = true;
    loop {
        let mut options = ServerOptions::new();
        if first {
            options.first_pipe_instance(true);
        }
        let server = match options.create(&endpoint) {
            Ok(server) => server,
            Err(error) => {
                if first {
                    let _ = ready.send(Err(format!("创建 context service 管道失败：{error}")));
                }
                return;
            }
        };
        if first {
            first = false;
            let _ = ready.send(Ok(()));
        }
        tokio::select! {
            _ = &mut shutdown => return,
            connected = server.connect() => {
                if connected.is_err() { continue; }
                let token = token.clone();
                tokio::spawn(async move { serve_stream(server, &token).await });
            }
        }
    }
}

#[cfg(unix)]
async fn run_server(
    endpoint: String,
    token: String,
    ready: mpsc::Sender<Result<(), String>>,
    mut shutdown: oneshot::Receiver<()>,
) {
    use tokio::net::UnixListener;

    let _ = std::fs::remove_file(&endpoint);
    let listener = match UnixListener::bind(&endpoint) {
        Ok(listener) => listener,
        Err(error) => {
            let _ = ready.send(Err(format!("创建 context service socket 失败：{error}")));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let token = token.clone();
                    tokio::spawn(async move { serve_stream(stream, &token).await });
                }
                Err(_) => break,
            }
        }
    }
    let _ = std::fs::remove_file(&endpoint);
}

impl ContextService {
    pub(crate) fn disabled() -> Self {
        std::env::remove_var("NOVA_CONTEXT_SERVICE_ENDPOINT");
        std::env::remove_var("NOVA_CONTEXT_SERVICE_TOKEN");
        Self {
            endpoint: String::new(),
            token: String::new(),
            shutdown: Mutex::new(None),
            worker: Mutex::new(None),
        }
    }

    pub(crate) fn start(data_dir: &Path) -> Result<Self, String> {
        let runtime_dir = data_dir.join("runtime");
        std::fs::create_dir_all(&runtime_dir)
            .map_err(|e| format!("创建 context service 运行目录失败：{e}"))?;

        let pid = std::process::id();
        let endpoint = if cfg!(windows) {
            format!(r"\\.\pipe\nova-context-{pid}")
        } else {
            std::env::temp_dir()
                .join(format!("nova-context-{pid}.sock"))
                .to_string_lossy()
                .into_owned()
        };
        let token = uuid::Uuid::new_v4().to_string();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker_endpoint = endpoint.clone();
        let worker_token = token.clone();
        let worker = std::thread::Builder::new()
            .name("nova-context-service".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(2)
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready_tx.send(Err(format!(
                            "创建原生 context service runtime 失败：{error}"
                        )));
                        return;
                    }
                };
                runtime.block_on(run_server(
                    worker_endpoint,
                    worker_token,
                    ready_tx,
                    shutdown_rx,
                ));
            })
            .map_err(|e| format!("启动原生 context service 失败：{e}"))?;

        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {
                std::env::set_var("NOVA_CONTEXT_SERVICE_ENDPOINT", &endpoint);
                std::env::set_var("NOVA_CONTEXT_SERVICE_TOKEN", &token);
                Ok(Self {
                    endpoint,
                    token,
                    shutdown: Mutex::new(Some(shutdown_tx)),
                    worker: Mutex::new(Some(worker)),
                })
            }
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = shutdown_tx.send(());
                let _ = worker.join();
                Err("等待原生 context service 就绪超时".into())
            }
        }
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
        if let Ok(shutdown) = self.shutdown.get_mut() {
            if let Some(shutdown) = shutdown.take() {
                let _ = shutdown.send(());
            }
        }
        if let Ok(worker) = self.worker.get_mut() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
        if !cfg!(windows) && !self.endpoint.is_empty() {
            let _ = std::fs::remove_file(&self.endpoint);
        }
        std::env::remove_var("NOVA_CONTEXT_SERVICE_ENDPOINT");
        std::env::remove_var("NOVA_CONTEXT_SERVICE_TOKEN");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_reports_native_runtime() {
        let response = response_for_line(
            r#"{"token":"secret","method":"ping","root":"","params":{}}"#,
            "secret",
        );
        assert!(response.ok);
        assert_eq!(response.result.unwrap()["runtime"], "rust");
    }

    #[test]
    fn rejects_invalid_token() {
        let response = response_for_line(
            r#"{"token":"wrong","method":"ping","root":"","params":{}}"#,
            "secret",
        );
        assert!(!response.ok);
    }
}
