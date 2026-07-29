//! The `write` tool, ported from `pi-coding-agent/dist/core/tools/write.js`.
//!
//! Parity note: the success message reports `content.length` — the UTF-16
//! code-unit count (JS `String.length`) — even though it says "bytes". For
//! non-ASCII content this differs from the on-disk UTF-8 size, and the port
//! reproduces that exactly.

use crate::paths::{dirname, resolve_to_cwd};

/// Port of the `write` tool execute: resolve against `cwd`, create parent
/// directories, write `content`, and return the node-identical status message.
/// Errors carry the platform `io::Error` text (not parity-checked against
/// libuv). Abortion/queueing are runtime concerns handled by the caller.
pub fn write_tool(cwd: &str, path: &str, content: &str) -> Result<String, String> {
    let absolute = resolve_to_cwd(path, cwd);
    let dir = dirname(&absolute);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(&absolute, content).map_err(|e| e.to_string())?;
    let utf16_len = content.encode_utf16().count();
    Ok(format!("Successfully wrote {utf16_len} bytes to {path}"))
}
