use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::{mpsc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;
use tokio::time::timeout;

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
    let output = match request.method.as_str() {
        "fast_context" => crate::nova_tools_native::context::fast_context(root, request.params),
        "find_symbols" => crate::nova_tools_native::context::find_symbols(root, request.params),
        "code_map" => crate::nova_tools_native::context::code_map(root, request.params),
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
        Ok(_) if line.len() <= 2 * 1024 * 1024 => response_for_line(line.trim_end(), token),
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
                // Keep the same serialized execution contract as the former Node service.
                serve_stream(server, &token).await;
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
                Ok((stream, _)) => serve_stream(stream, &token).await,
                Err(_) => break,
            }
        }
    }
    let _ = std::fs::remove_file(&endpoint);
}

impl ContextService {
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
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
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
            Ok(Ok(())) => Ok(Self {
                endpoint,
                token,
                shutdown: Mutex::new(Some(shutdown_tx)),
                worker: Mutex::new(Some(worker)),
            }),
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

#[derive(Deserialize)]
struct ClientResponse {
    ok: bool,
    result: Option<Value>,
    error: Option<String>,
}

async fn exchange<S>(stream: S, request: &[u8]) -> Result<String, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut stream = BufReader::new(stream);
    stream
        .get_mut()
        .write_all(request)
        .await
        .map_err(|e| e.to_string())?;
    stream
        .get_mut()
        .write_all(b"\n")
        .await
        .map_err(|e| e.to_string())?;
    let mut line = String::new();
    stream
        .read_line(&mut line)
        .await
        .map_err(|e| e.to_string())?;
    let response: ClientResponse =
        serde_json::from_str(line.trim_end()).map_err(|e| e.to_string())?;
    if !response.ok {
        return Err(response
            .error
            .unwrap_or_else(|| "context service failed".into()));
    }
    response
        .result
        .and_then(|value| value.as_str().map(str::to_string))
        .ok_or_else(|| "context service returned invalid result".into())
}

/// Native client used by Lyra. Absence, connection failure and timeout are deliberately errors so
/// the caller can immediately execute the unchanged in-process implementation as its fallback.
pub(crate) async fn call_configured(
    method: &str,
    root: &Path,
    params: &Value,
) -> Result<String, String> {
    let endpoint = std::env::var("NOVA_CONTEXT_SERVICE_ENDPOINT")
        .map_err(|_| "context service is not configured".to_string())?;
    let token = std::env::var("NOVA_CONTEXT_SERVICE_TOKEN")
        .map_err(|_| "context service is not configured".to_string())?;
    let request = serde_json::to_vec(&serde_json::json!({
        "token": token,
        "method": method,
        "root": root,
        "params": params,
    }))
    .map_err(|e| e.to_string())?;

    let operation = async {
        #[cfg(windows)]
        {
            let stream = tokio::net::windows::named_pipe::ClientOptions::new()
                .open(&endpoint)
                .map_err(|e| e.to_string())?;
            exchange(stream, &request).await
        }
        #[cfg(unix)]
        {
            let stream = tokio::net::UnixStream::connect(&endpoint)
                .await
                .map_err(|e| e.to_string())?;
            exchange(stream, &request).await
        }
    };
    timeout(Duration::from_secs(120), operation)
        .await
        .map_err(|_| format!("context service timed out: {method}"))?
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
        if !cfg!(windows) {
            let _ = std::fs::remove_file(&self.endpoint);
        }
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
