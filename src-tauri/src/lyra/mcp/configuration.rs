//! Flat user-facing configuration; validation never reads environment or opens a connection.
use super::*;
use reqwest::header::{HeaderName, HeaderValue};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FlatServer {
    enabled: bool,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    cwd: Option<PathBuf>,
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default = "startup_timeout")]
    startup_timeout_ms: u64,
    #[serde(default = "request_timeout")]
    request_timeout_ms: u64,
    #[serde(default, rename = "readOnlyTools")]
    read_only_tools: BTreeSet<String>,
}

/// Parse ONLY the top-level `mcpServers` map; unrelated root settings are left untouched.
/// Secret placeholders remain unresolved until McpSession::new. Missing map means no services.
pub fn config_from_root(value: &Value) -> std::result::Result<McpConfig, String> {
    let root = value
        .as_object()
        .ok_or("Configuration root must be an object")?;
    let Some(map) = root.get("mcpServers") else {
        return Ok(McpConfig::default());
    };
    let flat: BTreeMap<String, FlatServer> = serde_json::from_value(map.clone())
        .map_err(|_| "Invalid mcpServers structure (expected named flat server objects with explicit enabled)")?;
    let mut servers = BTreeMap::new();
    for (name, entry) in flat {
        // Same rules as the UI: objects only, no null values, field presence decides the transport
        // (even empty headers/args/env may not be mixed in).
        let fields = map[&name].as_object().ok_or("Each MCP server must be an object")?;
        if fields.values().any(Value::is_null) {
            return Err("MCP server fields cannot be null".into());
        }
        let has = |key: &str| fields.contains_key(key);
        let transport = match (entry.command, entry.url) {
            (Some(command), None) if !has("headers") => Transport::Stdio {
                command, args: entry.args, cwd: entry.cwd, env: entry.env,
            },
            (None, Some(url)) if !["args", "env", "cwd"].into_iter().any(has) =>
                Transport::StreamableHttp { url, headers: entry.headers },
            _ => return Err("Each MCP server requires either command (+args/env/cwd) or url (+headers), not both".into()),
        };
        servers.insert(
            name,
            ServerConfig {
                enabled: entry.enabled,
                transport,
                startup_timeout_ms: entry.startup_timeout_ms,
                request_timeout_ms: entry.request_timeout_ms,
                read_only_tools: entry.read_only_tools,
            },
        );
    }
    let config = McpConfig { servers };
    validate_internal(&config)?;
    Ok(config)
}

/// Save-time validation only: even an unset {env:NAME} placeholder is valid. No IO or spawning.
pub fn validate_config(value: &Value) -> std::result::Result<(), String> {
    config_from_root(value).map(|_| ())
}

pub(super) fn validate_internal(config: &McpConfig) -> std::result::Result<(), String> {
    if config.servers.len() > 64 {
        return Err("At most 64 MCP services are supported".into());
    }
    for (name, c) in &config.servers {
        if !valid_name(name)
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err("Invalid MCP service name".into());
        }
        if !(1..=600_000).contains(&c.startup_timeout_ms)
            || !(1..=600_000).contains(&c.request_timeout_ms)
        {
            return Err("MCP timeouts must be between 1 and 600000 milliseconds".into());
        }
        if c.read_only_tools.len() > MAX_DESCRIBED
            || c.read_only_tools.iter().any(|s| !valid_name(s) || s.trim().is_empty())
        {
            return Err("Invalid MCP read-only allowlist".into());
        }
        match &c.transport {
            Transport::Stdio {
                command,
                args,
                cwd,
                env,
            } => {
                if command.trim().is_empty()
                    || command.len() > 4096
                    || command.contains('\0')
                    || args.len() > 128
                    || args.iter().any(|s| s.len() > 65_536 || s.contains('\0'))
                    || env.len() > 64
                {
                    return Err("Invalid MCP process configuration".into());
                }
                // Relative cwd would follow the model's change_working_directory into any repo.
                if cwd.as_ref().is_some_and(|p| {
                    !p.is_absolute() || p.as_os_str().to_string_lossy().contains('\0')
                }) {
                    return Err("MCP cwd must be an absolute directory path".into());
                }
                for (key, value) in env {
                    if !valid_env_name(key) {
                        return Err("Invalid MCP environment name".into());
                    }
                    expand(value, false, &mut Vec::new())?;
                }
            }
            Transport::StreamableHttp { url, headers } => {
                let url = reqwest::Url::parse(url).map_err(|_| "Invalid MCP HTTP URL")?;
                // Hosts are normalized/lowercased by the URL parser (127.1 -> 127.0.0.1, [::1]).
                let loopback = url.host_str().is_some_and(|h| {
                    h == "localhost"
                        || h.trim_start_matches('[').trim_end_matches(']')
                            .parse::<std::net::IpAddr>()
                            .is_ok_and(|ip| ip.is_loopback())
                });
                if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
                    || url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.fragment().is_some()
                {
                    return Err(
                        "MCP URL requires https (http only for loopback), a host, and no credentials or fragment".into(),
                    );
                }
                if headers.len() > 64 {
                    return Err("Too many MCP HTTP headers".into());
                }
                let mut names = BTreeSet::new();
                for (key, value) in headers {
                    let name = HeaderName::from_bytes(key.as_bytes())
                        .map_err(|_| "Invalid MCP HTTP header name")?;
                    if matches!(
                        name.as_str(),
                        "host"
                            | "content-length"
                            | "transfer-encoding"
                            | "connection"
                            | "content-type"
                            | "accept"
                            | "mcp-session-id"
                            | "mcp-protocol-version"
                    ) || !names.insert(name.to_string())
                    {
                        return Err("Reserved or duplicate MCP HTTP header".into());
                    }
                    let unresolved = expand(value, false, &mut Vec::new())?;
                    HeaderValue::from_str(&unresolved)
                        .map_err(|_| "Invalid MCP HTTP header value")?;
                }
            }
        }
    }
    Ok(())
}

fn reference_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Interpolate placeholders. Only when a placeholder was expanded, remember BOTH the individual
/// values and the resulting credential; literals and values under 8 bytes are never redacted.
fn expand(
    input: &str,
    resolve: bool,
    secrets: &mut Vec<String>,
) -> std::result::Result<String, String> {
    if input.len() > 65_536 || input.contains('\0') {
        return Err("Invalid MCP environment/header value".into());
    }
    let mut result = String::new();
    let mut rest = input;
    while let Some(start) = rest.find("{env:") {
        result.push_str(&rest[..start]);
        rest = &rest[start + 5..];
        let end = rest
            .find('}')
            .ok_or("Unclosed MCP environment placeholder")?;
        let name = &rest[..end];
        if !reference_name(name) {
            return Err("Invalid MCP environment placeholder".into());
        }
        let value = if resolve {
            std::env::var(name).map_err(|_| {
                format!("MCP environment variable {name} is missing or non-Unicode")
            })?
        } else {
            "placeholder".into()
        };
        if value.len() > 65_536 || value.contains('\0') {
            return Err("Invalid MCP environment value".into());
        }
        if resolve && value.len() >= 8 {
            secrets.push(value.clone());
        }
        result.push_str(&value);
        if result.len() > 65_536 {
            return Err("Expanded MCP value exceeds the size limit".into());
        }
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    if result.len() > 65_536 {
        return Err("Expanded MCP value exceeds the size limit".into());
    }
    if resolve && result.len() >= 8 && result != input {
        secrets.push(result.clone());
    }
    Ok(result)
}

/// Return resolved process env / HTTP headers without mutating the serializable configuration.
pub(super) fn resolve(
    c: &ServerConfig,
    secrets: &mut Vec<String>,
) -> std::result::Result<BTreeMap<String, String>, String> {
    let mut values = BTreeMap::new();
    let templates = match &c.transport {
        Transport::Stdio { env, .. } => env,
        Transport::StreamableHttp { headers, .. } => headers,
    };
    for (key, template) in templates {
        let value = expand(template, true, secrets)?;
        if matches!(&c.transport, Transport::StreamableHttp { .. }) {
            HeaderValue::from_str(&value).map_err(|_| "Invalid resolved MCP HTTP header value")?;
        }
        values.insert(key.clone(), value);
    }
    Ok(values)
}
