use crate::settings::Settings;
use serde::Serialize;
use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const ANTHROPIC_UPSTREAM: &str = "https://api.anthropic.com";
const OPENAI_UPSTREAM: &str = "https://api.openai.com";
const PXPIPE_PACKAGE: &str = "pxpipe-proxy";
const DEFAULT_PXPIPE_MODELS: &[&str] = &["claude-fable-5", "gpt-5.6"];

struct ProxyServer {
    url: String,
    child: Child,
    proxy: String,
    models: String,
    upstreams: Upstreams,
}

#[derive(Clone, PartialEq, Eq)]
struct Upstreams {
    anthropic: String,
    openai: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PxpipeServiceStatus {
    enabled: bool,
    running: bool,
    message: String,
    instances: Vec<PxpipeServiceInstance>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PxpipeServiceInstance {
    url: String,
    pid: u32,
    running: bool,
    message: String,
    proxy: String,
    models: String,
    anthropic_upstream: String,
    openai_upstream: String,
}

static SERVERS: OnceLock<Mutex<HashMap<String, ProxyServer>>> = OnceLock::new();

fn servers() -> &'static Mutex<HashMap<String, ProxyServer>> {
    SERVERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn apply_pxpipe_env(
    cmd: &mut tokio::process::Command,
    settings: &Settings,
    proxy: &str,
) -> Result<(), String> {
    if !settings.pxpipe_experimental {
        return Ok(());
    }
    let models = pxpipe_models(settings);
    let url = ensure_proxy(proxy, models, Upstreams::from_app_config())?;
    apply_agent_env(cmd, &url);
    Ok(())
}

pub(crate) fn prepare_codex_pxpipe(
    args: &mut Vec<String>,
    settings: &Settings,
    proxy: &str,
) -> Result<Option<String>, String> {
    if !settings.pxpipe_experimental {
        return Ok(None);
    }

    let provider = read_codex_provider();
    let upstreams = Upstreams::from_app_config();
    let models = pxpipe_models(settings);
    let url = ensure_proxy(proxy, models, upstreams)?;
    if let Some((name, _)) = provider {
        args.push("-c".into());
        args.push(format!(
            "model_providers.{name}.base_url={}",
            serde_json::to_string(&format!("{url}/v1")).unwrap()
        ));
    }
    Ok(Some(url))
}

pub(crate) fn apply_codex_pxpipe_env(cmd: &mut tokio::process::Command, url: Option<&str>) {
    if let Some(url) = url {
        apply_agent_env(cmd, url);
    }
}

pub(crate) fn service_status(settings: &Settings) -> PxpipeServiceStatus {
    if !settings.pxpipe_experimental {
        return PxpipeServiceStatus {
            enabled: false,
            running: false,
            message: "图片化上下文未启用".into(),
            instances: Vec::new(),
        };
    }

    let mut dead = Vec::new();
    let mut instances = Vec::new();
    {
        let mut guard = servers().lock().unwrap();
        for (key, server) in guard.iter_mut() {
            let pid = server.child.id();
            match server.child.try_wait() {
                Ok(None) => instances.push(PxpipeServiceInstance {
                    url: server.url.clone(),
                    pid,
                    running: true,
                    message: "运行中".into(),
                    proxy: display_proxy(&server.proxy),
                    models: server.models.clone(),
                    anthropic_upstream: server.upstreams.anthropic.clone(),
                    openai_upstream: server.upstreams.openai.clone(),
                }),
                Ok(Some(status)) => {
                    dead.push(key.clone());
                    instances.push(PxpipeServiceInstance {
                        url: server.url.clone(),
                        pid,
                        running: false,
                        message: format!("已退出：{status}"),
                        proxy: display_proxy(&server.proxy),
                        models: server.models.clone(),
                        anthropic_upstream: server.upstreams.anthropic.clone(),
                        openai_upstream: server.upstreams.openai.clone(),
                    });
                }
                Err(e) => {
                    dead.push(key.clone());
                    instances.push(PxpipeServiceInstance {
                        url: server.url.clone(),
                        pid,
                        running: false,
                        message: format!("状态读取失败：{e}"),
                        proxy: display_proxy(&server.proxy),
                        models: server.models.clone(),
                        anthropic_upstream: server.upstreams.anthropic.clone(),
                        openai_upstream: server.upstreams.openai.clone(),
                    });
                }
            }
        }
        for key in dead {
            guard.remove(&key);
        }
    }

    let running_count = instances.iter().filter(|item| item.running).count();
    let message = if running_count == 0 {
        "pxpipe 未启动。它会在下一次 agent 请求需要图片化上下文时启动。".into()
    } else if running_count == 1 {
        format!("pxpipe 运行中：{}", instances[0].url)
    } else {
        format!("pxpipe 运行中：{running_count} 个实例")
    };

    PxpipeServiceStatus {
        enabled: true,
        running: running_count > 0,
        message,
        instances,
    }
}

pub(crate) fn restart_service(settings: &Settings) -> Result<PxpipeServiceStatus, String> {
    if !settings.pxpipe_experimental {
        return Err("请先勾选图片化上下文".into());
    }

    let mut restart_port = None;
    {
        let mut guard = servers().lock().unwrap();
        for (_, mut server) in guard.drain() {
            restart_port = restart_port.or_else(|| port_from_url(&server.url));
            terminate_child_tree(&mut server.child);
        }
    }

    let proxy = first_configured_proxy(settings);
    let models = pxpipe_models(settings);
    let upstreams = Upstreams::from_app_config();
    let key = server_key();
    let (url, child) = start_pxpipe_server(restart_port, &proxy, &models, &upstreams)?;
    servers().lock().unwrap().insert(
        key,
        ProxyServer {
            url,
            child,
            proxy,
            models,
            upstreams,
        },
    );

    Ok(service_status(settings))
}

pub(crate) fn shutdown() {
    let mut guard = servers().lock().unwrap();
    for (_, mut server) in guard.drain() {
        terminate_child_tree(&mut server.child);
    }
}

fn apply_agent_env(cmd: &mut tokio::process::Command, url: &str) {
    cmd.env("ANTHROPIC_BASE_URL", url);
    cmd.env("OPENAI_BASE_URL", format!("{url}/v1"));
    cmd.env("HTTP_PROXY", "");
    cmd.env("HTTPS_PROXY", "");
    cmd.env("ALL_PROXY", "");
    cmd.env("http_proxy", "");
    cmd.env("https_proxy", "");
    cmd.env("all_proxy", "");
    cmd.env("NO_PROXY", "127.0.0.1,localhost");
    cmd.env("no_proxy", "127.0.0.1,localhost");
}

fn ensure_proxy(proxy: &str, models: String, upstreams: Upstreams) -> Result<String, String> {
    let proxy = normalize_proxy(proxy);
    let key = server_key();
    let mut stale: Option<ProxyServer> = None;
    {
        let mut guard = servers().lock().unwrap();
        if let Some(server) = guard.get_mut(&key) {
            if server.child.try_wait().ok().flatten().is_none() {
                if server.proxy == proxy && server.models == models && server.upstreams == upstreams
                {
                    return Ok(server.url.clone());
                }
                stale = guard.remove(&key);
            } else {
                guard.remove(&key);
            }
        }
    }
    let restart_port = stale.as_ref().and_then(|server| port_from_url(&server.url));
    if let Some(mut server) = stale {
        terminate_child_tree(&mut server.child);
    }

    let (url, child) = start_pxpipe_server(restart_port, &proxy, &models, &upstreams)?;

    let mut guard = servers().lock().unwrap();
    if let Some(server) = guard.get_mut(&key) {
        if server.child.try_wait().ok().flatten().is_none() {
            if server.proxy == proxy && server.models == models && server.upstreams == upstreams {
                let mut duplicate = child;
                terminate_child_tree(&mut duplicate);
                return Ok(server.url.clone());
            }
            let mut old = guard.remove(&key).unwrap();
            terminate_child_tree(&mut old.child);
        }
    }
    guard.insert(
        key,
        ProxyServer {
            url: url.clone(),
            child,
            proxy,
            models,
            upstreams,
        },
    );
    Ok(url)
}

fn server_key() -> String {
    "global".into()
}

fn start_pxpipe_server(
    port: Option<u16>,
    proxy: &str,
    models: &str,
    upstreams: &Upstreams,
) -> Result<(String, Child), String> {
    let port = match port {
        Some(port) => port,
        None => reserve_loopback_port()?,
    };
    let url = format!("http://127.0.0.1:{port}");
    let mut child = spawn_pxpipe(port, proxy, models, upstreams)?;
    if let Err(e) = wait_until_ready(&mut child, port) {
        terminate_child_tree(&mut child);
        return Err(e);
    }
    Ok((url, child))
}

fn reserve_loopback_port() -> Result<u16, String> {
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("无法分配 pxpipe 端口：{e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("无法读取 pxpipe 端口：{e}"))?
        .port();
    drop(listener);
    Ok(port)
}

fn spawn_pxpipe(
    port: u16,
    proxy: &str,
    models: &str,
    upstreams: &Upstreams,
) -> Result<Child, String> {
    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = Command::new("cmd");
        cmd.arg("/D").arg("/S").arg("/C").arg("npx");
        cmd
    };
    #[cfg(not(windows))]
    let mut cmd = Command::new("npx");
    cmd.arg("-y")
        .arg(PXPIPE_PACKAGE)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("PORT", port.to_string())
        .env("HOST", "127.0.0.1")
        .env("ANTHROPIC_UPSTREAM", &upstreams.anthropic)
        .env("OPENAI_UPSTREAM", &upstreams.openai)
        .env("PXPIPE_MODELS", models)
        .env("NODE_USE_ENV_PROXY", "1");
    if !proxy.is_empty() {
        cmd.env("HTTP_PROXY", proxy)
            .env("HTTPS_PROXY", proxy)
            .env("ALL_PROXY", proxy)
            .env("http_proxy", proxy)
            .env("https_proxy", proxy)
            .env("all_proxy", proxy);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.spawn()
        .map_err(|e| format!("无法启动 pxpipe（npx -y {PXPIPE_PACKAGE}）：{e}"))
}

fn wait_until_ready(child: &mut Child, port: u16) -> Result<(), String> {
    let addr = format!("127.0.0.1:{port}");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(20) {
        if TcpStream::connect(&addr).is_ok() {
            return Ok(());
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("检查 pxpipe 进程失败：{e}"))?
        {
            return Err(format!("pxpipe 提前退出：{status}"));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    Err("pxpipe 启动超时".into())
}

fn port_from_url(url: &str) -> Option<u16> {
    url.rsplit(':').next()?.parse().ok()
}

fn terminate_child_tree(child: &mut Child) {
    crate::acp::kill_process_tree(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

fn first_configured_proxy(settings: &Settings) -> String {
    [
        &settings.codex_proxy,
        &settings.devin_proxy,
        &settings.codebuddy_proxy,
        &settings.claudecode_proxy,
        &settings.cursor_proxy,
        &settings.opencode_proxy,
    ]
    .iter()
    .map(|value| value.trim())
    .find(|value| !value.is_empty())
    .map(normalize_proxy)
    .unwrap_or_default()
}

fn display_proxy(proxy: &str) -> String {
    if proxy.is_empty() {
        "未配置".into()
    } else {
        proxy.into()
    }
}

fn pxpipe_models(settings: &Settings) -> String {
    let mut models: Vec<String> = DEFAULT_PXPIPE_MODELS
        .iter()
        .map(|model| (*model).to_string())
        .collect();
    for model in settings
        .pxpipe_models
        .split(|ch: char| ch == ',' || ch == ';' || ch.is_whitespace())
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        if !models.iter().any(|existing| existing == model) {
            models.push(model.to_string());
        }
    }
    models.join(",")
}

fn normalize_proxy(proxy: &str) -> String {
    let proxy = proxy.trim();
    if proxy.is_empty() {
        String::new()
    } else if proxy.contains("://") {
        proxy.to_string()
    } else {
        format!("http://{proxy}")
    }
}

impl Upstreams {
    fn from_env() -> Self {
        Self {
            anthropic: std::env::var("ANTHROPIC_BASE_URL")
                .or_else(|_| std::env::var("ANTHROPIC_UPSTREAM"))
                .unwrap_or_else(|_| ANTHROPIC_UPSTREAM.into())
                .trim_end_matches('/')
                .to_string(),
            openai: std::env::var("OPENAI_BASE_URL")
                .or_else(|_| std::env::var("OPENAI_UPSTREAM"))
                .unwrap_or_else(|_| OPENAI_UPSTREAM.into())
                .trim_end_matches('/')
                .trim_end_matches("/v1")
                .to_string(),
        }
    }

    fn from_app_config() -> Self {
        let mut upstreams = Self::from_env();
        if let Some((_, base_url)) = read_codex_provider() {
            upstreams.openai = normalize_openai_upstream(&base_url);
        }
        upstreams
    }
}

fn read_codex_provider() -> Option<(String, String)> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)?;
    let config = std::fs::read_to_string(home.join(".codex").join("config.toml")).ok()?;
    parse_codex_provider(&config)
}

fn parse_codex_provider(config: &str) -> Option<(String, String)> {
    let provider = toml_value(config, "model_provider")?;
    let marker = format!("[model_providers.{provider}]");
    let mut in_section = false;
    for raw in config.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_section = line == marker;
            continue;
        }
        if in_section {
            if let Some(base) = toml_line_value(line, "base_url") {
                return Some((provider, normalize_openai_upstream(&base)));
            }
        }
    }
    None
}

fn normalize_openai_upstream(base_url: &str) -> String {
    base_url
        .trim()
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .to_string()
}

fn toml_value(config: &str, key: &str) -> Option<String> {
    for raw in config.lines() {
        let line = raw.trim();
        if let Some(value) = toml_line_value(line, key) {
            return Some(value);
        }
    }
    None
}

fn toml_line_value(line: &str, key: &str) -> Option<String> {
    let (k, v) = line.split_once('=')?;
    if k.trim() != key {
        return None;
    }
    Some(v.trim().trim_matches('"').to_string())
}

#[cfg(test)]
mod tests {
    use super::{normalize_openai_upstream, normalize_proxy, parse_codex_provider, pxpipe_models};
    use crate::settings::Settings;

    #[test]
    fn reads_custom_codex_provider() {
        let config = r#"
model_provider = "custom"
[model_providers.custom]
base_url = "http://example.test:8317/v1"
"#;
        assert_eq!(
            parse_codex_provider(config),
            Some(("custom".into(), "http://example.test:8317".into()))
        );
    }

    #[test]
    fn normalizes_proxy_without_scheme() {
        assert_eq!(normalize_proxy("127.0.0.1:7890"), "http://127.0.0.1:7890");
    }

    #[test]
    fn appends_extra_models_to_pxpipe_defaults() {
        let mut settings = Settings::default();
        settings.pxpipe_models = "gpt-5.5, claude-fable-5 gpt-5.6;gpt-6".into();

        assert_eq!(
            pxpipe_models(&settings),
            "claude-fable-5,gpt-5.6,gpt-5.5,gpt-6"
        );
    }

    #[test]
    fn strips_openai_v1_suffix_for_pxpipe_upstream() {
        assert_eq!(
            normalize_openai_upstream("http://127.0.0.1:8317/v1/"),
            "http://127.0.0.1:8317"
        );
    }
}
