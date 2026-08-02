use napi::{Error, Result, Status};
use napi_derive::napi;
use serde_json::Value;
use std::path::Path;

#[path = "../../../src-tauri/src/nova_tools_native/edit.rs"]
mod edit;
#[path = "../../../src-tauri/src/nova_tools_native/read.rs"]
mod read;

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

#[napi(js_name = "readFiles")]
pub fn read_files(root: String, params: Value) -> Result<Value> {
    read::read_files(workspace_root(&root)?, params)
        .map_err(|message| Error::new(Status::GenericFailure, message))
}

#[napi(js_name = "editFiles")]
pub fn edit_files(root: String, params: Value) -> Result<Value> {
    edit::edit_files(workspace_root(&root)?, params)
        .map_err(|message| Error::new(Status::GenericFailure, message))
}
