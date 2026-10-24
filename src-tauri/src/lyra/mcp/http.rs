//! Streamable HTTP POST transport, JSON or SSE response. No reconnect/replay/legacy SSE GET.
use super::*;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::collections::VecDeque;

pub(super) struct Http {
    client: reqwest::Client,
    url: String,
    session: Option<String>,
    pub(super) version: Option<String>,
    response: Option<reqwest::Response>,
    sse: bool,
    bytes: usize,
    buffer: Vec<u8>,
    events: VecDeque<Value>,
    parser: Sse,
}

impl Http {
    pub(super) fn new(url: &str, values: &BTreeMap<String, String>) -> Result<Self> {
        let mut headers = HeaderMap::new();
        for (key, value) in values {
            let name = HeaderName::from_bytes(key.as_bytes()).map_err(|_| protocol())?;
            let mut value = HeaderValue::from_str(value).map_err(|_| protocol())?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        // Do not follow redirects carrying credentials or transparently retry a side-effecting POST.
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| error("http_client", "Cannot construct MCP HTTP client"))?;
        Ok(Self {
            client,
            url: url.into(),
            session: None,
            version: None,
            response: None,
            sse: false,
            bytes: 0,
            buffer: Vec::new(),
            events: VecDeque::new(),
            parser: Sse::default(),
        })
    }

    pub(super) async fn send(&mut self, value: Value) -> Result<()> {
        let request = value.get("method").is_some() && value.get("id").is_some();
        let initialize = value["method"] == "initialize";
        let bytes = serde_json::to_vec(&value).map_err(|_| protocol())?;
        if bytes.len() > MAX_WIRE_BYTES {
            return Err(error("request_too_large", "MCP request exceeds wire limit"));
        }
        let mut builder = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(bytes);
        if let Some(id) = &self.session {
            let mut id = HeaderValue::from_str(id).map_err(|_| protocol())?;
            id.set_sensitive(true);
            builder = builder.header("mcp-session-id", id);
        }
        if let Some(version) = &self.version {
            builder = builder.header("mcp-protocol-version", version);
        }
        let response = builder.send().await.map_err(|_| {
            error(
                "http_error",
                "MCP HTTP request failed; URL and headers withheld, do not automatically retry",
            )
        })?;
        if !response.status().is_success() {
            return Err(error(
                "http_status",
                &format!(
                    "MCP HTTP status {}; session invalidated, do not automatically retry calls",
                    response.status().as_u16()
                ),
            ));
        }
        if let Some(id) = response.headers().get("mcp-session-id") {
            let id = id.to_str().map_err(|_| protocol())?;
            if id.is_empty() || id.len() > 1024 || !id.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
                return Err(protocol());
            }
            if initialize {
                self.session = Some(id.into());
            } else if self.session.as_deref() != Some(id) {
                return Err(protocol());
            }
        }
        if !request {
            // Protocol notifications and responses to server requests are acknowledged by 202.
            // In particular, do not overwrite the active POST SSE stream while replying to a server.
            if response.status().as_u16() != 202 {
                return Err(protocol());
            }
            return Ok(());
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_WIRE_BYTES as u64)
        {
            return Err(protocol());
        }
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .unwrap_or("")
            .trim();
        self.sse = if mime.eq_ignore_ascii_case("text/event-stream") {
            true
        } else if mime.eq_ignore_ascii_case("application/json") {
            false
        } else {
            return Err(error(
                "http_content_type",
                "MCP response requires application/json or text/event-stream",
            ));
        };
        self.response = Some(response);
        self.bytes = 0;
        self.buffer.clear();
        self.events.clear();
        self.parser = Sse::default();
        Ok(())
    }

    pub(super) async fn receive(&mut self) -> Result<Value> {
        loop {
            if let Some(v) = self.events.pop_front() {
                return Ok(v);
            }
            let response = self.response.as_mut().ok_or_else(protocol)?;
            let chunk = response
                .chunk()
                .await
                .map_err(|_| error("http_error", "MCP HTTP response stream failed"))?;
            match chunk {
                Some(chunk) => {
                    self.bytes += chunk.len();
                    if self.bytes > MAX_WIRE_BYTES {
                        return Err(protocol());
                    }
                    if self.sse {
                        self.parser.feed(&chunk, &mut self.events)?;
                    } else {
                        self.buffer.extend_from_slice(&chunk);
                    }
                }
                None => {
                    self.response = None;
                    if self.sse {
                        return Err(error("disconnected", "MCP SSE ended before its response"));
                    }
                    return serde_json::from_slice(&self.buffer).map_err(|_| protocol());
                }
            }
        }
    }

    pub(super) fn finish(&mut self) {
        // Drop the HTTP body immediately after the matching result, even if SSE stays open.
        self.response = None;
        self.events.clear();
        self.buffer.clear();
        self.parser = Sse::default();
    }
}

#[derive(Default)]
struct Sse {
    line: Vec<u8>,
    data: Vec<u8>,
    event: String,
    after_cr: bool,
    first_line: bool,
}

impl Sse {
    fn feed(&mut self, chunk: &[u8], events: &mut VecDeque<Value>) -> Result<()> {
        for &byte in chunk {
            if self.after_cr {
                self.after_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\r' | b'\n' => {
                    self.line(events)?;
                    self.after_cr = byte == b'\r';
                }
                _ => {
                    self.line.push(byte);
                    if self.line.len() > MAX_WIRE_BYTES {
                        return Err(protocol());
                    }
                }
            }
        }
        Ok(())
    }

    fn line(&mut self, events: &mut VecDeque<Value>) -> Result<()> {
        let bytes = std::mem::take(&mut self.line);
        let mut line = std::str::from_utf8(&bytes).map_err(|_| protocol())?;
        if !self.first_line {
            line = line.trim_start_matches('\u{feff}');
            self.first_line = true;
        }
        if line.is_empty() {
            if !self.data.is_empty() {
                if !self.event.is_empty() && self.event != "message" {
                    return Err(protocol());
                }
                // Message count is bounded by MAX_WIRE_BYTES here and by rpc's notification caps.
                events.push_back(serde_json::from_slice(&self.data).map_err(|_| protocol())?);
            }
            self.data.clear();
            self.event.clear();
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => {
                self.data.extend_from_slice(value.as_bytes());
                self.data.push(b'\n');
                if self.data.len() > MAX_WIRE_BYTES {
                    return Err(protocol());
                }
            }
            "event" => self.event = value.into(),
            _ => {} // id/retry intentionally ignored: no automatic resume or replay.
        }
        Ok(())
    }
}
