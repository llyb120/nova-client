use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, Read, Write};

pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    pub root: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// edit_files 只有在 definitely_not_executed=true 时才允许 JS 安全回退。
    pub definitely_not_executed: bool,
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
        definitely_not_executed: bool,
    ) -> Self {
        Self {
            id,
            result: None,
            error: Some(RpcError {
                code: code.into(),
                message: message.into(),
                data: None,
                definitely_not_executed,
            }),
        }
    }
}

pub fn read_frame(reader: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0u8; 4];
    let mut read = 0;
    while read < header.len() {
        match reader.read(&mut header[read..])? {
            0 if read == 0 => return Ok(None),
            0 => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated MessagePack frame header",
                ))
            }
            n => read += n,
        }
    }
    let len = u32::from_le_bytes(header) as usize;
    if len == 0 || len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid MessagePack frame length: {len}"),
        ));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    Ok(Some(payload))
}

pub fn write_frame(writer: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    if payload.is_empty() || payload.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "MessagePack response frame exceeds limit",
        ));
    }
    writer.write_all(&(payload.len() as u32).to_le_bytes())?;
    writer.write_all(payload)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_prefixed_frame_round_trip() {
        let payload = vec![0x83, 0xa2, b'i', b'd'];
        let mut framed = Vec::new();
        write_frame(&mut framed, &payload).unwrap();
        assert_eq!(&framed[..4], &(payload.len() as u32).to_le_bytes());
        assert_eq!(
            read_frame(&mut framed.as_slice()).unwrap().unwrap(),
            payload
        );
    }
}
