use napi::{Error, Result, Status};
use napi_derive::napi;
use serde_json::Value;
use std::path::Path;

#[path = "../../../src-tauri/src/nova_tools_native/context.rs"]
mod context;
fn workspace_root(root: &str) -> Result<&Path> {
    let path = Path::new(root);
    if path.is_dir() {
        Ok(path)
    } else {
        Err(Error::new(
            Status::InvalidArg,
            format!("workspace root is not a directory: {root}"),
        ))
    }
}

#[napi(js_name = "fastContext")]
pub fn fast_context(root: String, mut params: Value) -> Result<String> {
    if let Some(object) = params.as_object_mut() {
        object.insert("_contextMode".into(), Value::String("fast".into()));
    }
    context::fast_context(workspace_root(&root)?, params)
        .map_err(|message| Error::new(Status::GenericFailure, message))
}

#[napi(js_name = "findSymbols")]
pub fn find_symbols(root: String, params: Value) -> Result<String> {
    context::find_symbols(workspace_root(&root)?, params)
        .map_err(|message| Error::new(Status::GenericFailure, message))
}

#[napi(js_name = "codeMap")]
pub fn code_map(root: String, params: Value) -> Result<String> {
    context::code_map(workspace_root(&root)?, params)
        .map_err(|message| Error::new(Status::GenericFailure, message))
}
