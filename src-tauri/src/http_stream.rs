use base64::Engine;
use flate2::read::GzDecoder;
use serde_json::Value;
use std::io::Read;

pub const SSE_MAX_EVENT_BYTES: usize = 32 * 1024 * 1024;
pub const SSE_IDLE_TIMEOUT_SECS: u64 = 45;

/// 增量解析 SSE，允许任意 HTTP chunk 边界，并忽略 heartbeat/comment 行。
pub struct SseDecoder {
    pending: Vec<u8>,
    data: Vec<String>,
    event_bytes: usize,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            data: Vec::new(),
            event_bytes: 0,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, String> {
        self.pending.extend_from_slice(chunk);
        let bytes = std::mem::take(&mut self.pending);
        let mut events = Vec::new();
        let mut start = 0usize;
        while let Some(relative) = bytes[start..].iter().position(|byte| *byte == b'\n') {
            let end = start + relative;
            let mut line = &bytes[start..end];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            self.consume_line(line, &mut events)?;
            start = end + 1;
        }
        self.pending.extend_from_slice(&bytes[start..]);
        if self.pending.len().saturating_add(self.event_bytes) > SSE_MAX_EVENT_BYTES {
            return Err("SSE 事件过大".into());
        }
        Ok(events)
    }

    pub fn finish(&mut self) -> Result<Vec<String>, String> {
        let mut events = Vec::new();
        if !self.pending.is_empty() {
            let mut line = std::mem::take(&mut self.pending);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.consume_line(&line, &mut events)?;
        }
        self.finish_event(&mut events);
        Ok(events)
    }

    fn consume_line(&mut self, line: &[u8], events: &mut Vec<String>) -> Result<(), String> {
        if line.is_empty() {
            self.finish_event(events);
            return Ok(());
        }
        if line[0] == b':' {
            return Ok(());
        }
        let text = std::str::from_utf8(line).map_err(|_| "SSE 不是有效 UTF-8".to_string())?;
        let Some(raw) = text
            .strip_prefix("data:")
            .or_else(|| (text == "data").then_some(""))
        else {
            return Ok(());
        };
        let value = raw.strip_prefix(' ').unwrap_or(raw);
        self.event_bytes = self.event_bytes.saturating_add(value.len() + 1);
        if self.event_bytes > SSE_MAX_EVENT_BYTES {
            return Err("SSE 事件过大".into());
        }
        self.data.push(value.to_string());
        Ok(())
    }

    fn finish_event(&mut self, events: &mut Vec<String>) {
        if !self.data.is_empty() {
            events.push(self.data.join("\n"));
            self.data.clear();
        }
        self.event_bytes = 0;
    }
}

/// SSE 不能发送二进制帧。大事件使用 {"encoding":"gzip","data":"<base64>"}
/// 保留逐消息压缩，避免对整条长连接启用 gzip 后被代理缓冲。
pub fn decode_sse_json(text: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if value.get("encoding").and_then(Value::as_str) != Some("gzip") {
        return Ok(value);
    }
    let encoded = value
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| "gzip SSE 事件缺少 data".to_string())?;
    let compressed = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| e.to_string())?;
    let mut decoder = GzDecoder::new(compressed.as_slice());
    let mut decoded = Vec::new();
    decoder
        .by_ref()
        .take(SSE_MAX_EVENT_BYTES as u64 + 1)
        .read_to_end(&mut decoded)
        .map_err(|e| e.to_string())?;
    if decoded.len() > SSE_MAX_EVENT_BYTES {
        return Err("gzip SSE 事件解压后过大".into());
    }
    serde_json::from_slice(&decoded).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    #[test]
    fn parses_chunked_multiline_sse_and_heartbeats() {
        let mut decoder = SseDecoder::new();
        assert!(decoder.push(b": ping\r\nda").unwrap().is_empty());
        let events = decoder.push(b"ta: {\"a\":\r\ndata: 1}\r\n\r\n").unwrap();
        assert_eq!(events, vec!["{\"a\":\n1}"]);
    }

    #[test]
    fn preserves_utf8_split_across_chunks() {
        let source = "data: 懒加载\n\n".as_bytes();
        let split = "data: ".len() + 1;
        let mut decoder = SseDecoder::new();
        assert!(decoder.push(&source[..split]).unwrap().is_empty());
        assert_eq!(decoder.push(&source[split..]).unwrap(), vec!["懒加载"]);
    }

    #[test]
    fn decodes_per_event_gzip_envelope() {
        let source = serde_json::json!({"op": "event", "text": "x".repeat(4096)});
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(source.to_string().as_bytes()).unwrap();
        let payload = encoder.finish().unwrap();
        let envelope = serde_json::json!({
            "encoding": "gzip",
            "data": base64::engine::general_purpose::STANDARD.encode(payload),
        });
        assert_eq!(decode_sse_json(&envelope.to_string()).unwrap(), source);
    }
}
