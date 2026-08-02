mod context;
mod edit;
mod protocol;
mod read;

use protocol::{read_frame, write_frame, Request, Response};
use serde_json::Value;
use std::io::{self, BufReader, BufWriter};
use std::path::Path;

const SIDECAR_ARG: &str = "--nova-tools-native";

pub fn maybe_run_from_args() -> bool {
    if !std::env::args_os().any(|arg| arg == SIDECAR_ARG) {
        return false;
    }
    if let Err(error) = serve_stdio() {
        // stdout is reserved for framed protocol responses.
        eprintln!("nova-tools-native: {error}");
    }
    true
}

fn dispatch(request: &Request) -> Result<Value, String> {
    let root = Path::new(&request.root);
    if !root.is_dir() {
        return Err(format!(
            "workspace root is not a directory: {}",
            request.root
        ));
    }
    match request.method.as_str() {
        "ping" => Ok(serde_json::json!({"version": 1, "transport": "msgpack-le32"})),
        "read_files" => read::read_files(root, request.params.clone()),
        "edit_files" => edit::edit_files(root, request.params.clone()),
        "fast_context" => context::fast_context(root, request.params.clone()).map(Value::String),
        "find_symbols" => context::find_symbols(root, request.params.clone()).map(Value::String),
        "code_map" => context::code_map(root, request.params.clone()).map(Value::String),
        other => Err(format!("unknown native tool method: {other}")),
    }
}

fn serve_stdio() -> Result<(), String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());
    loop {
        let Some(payload) = read_frame(&mut reader).map_err(|e| e.to_string())? else {
            return Ok(());
        };
        let request: Request = match rmp_serde::from_slice(&payload) {
            Ok(request) => request,
            Err(error) => {
                let response = Response::err(0, "INVALID_REQUEST", error.to_string(), true);
                let bytes = rmp_serde::to_vec_named(&response).map_err(|e| e.to_string())?;
                write_frame(&mut writer, &bytes).map_err(|e| e.to_string())?;
                continue;
            }
        };
        let response = match dispatch(&request) {
            Ok(result) => Response::ok(request.id, result),
            Err(message) => Response::err(
                request.id,
                "TOOL_ERROR",
                message,
                request.method != "edit_files",
            ),
        };
        let bytes = rmp_serde::to_vec_named(&response).map_err(|e| e.to_string())?;
        write_frame(&mut writer, &bytes).map_err(|e| e.to_string())?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_dispatches() {
        let request = Request {
            id: 1,
            method: "ping".into(),
            root: std::env::current_dir().unwrap().display().to_string(),
            params: Value::Null,
        };
        assert_eq!(dispatch(&request).unwrap()["transport"], "msgpack-le32");
    }
}
