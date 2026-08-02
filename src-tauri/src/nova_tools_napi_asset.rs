use std::path::{Path, PathBuf};

const NAPI_ADDON: &[u8] = include_bytes!("../resources/nova-tools-napi.node");
const NAPI_ADDON_NAME: &str = "nova-tools-napi.node";

pub(crate) fn materialize(runtime_dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(runtime_dir)
        .map_err(|e| format!("创建 native tools 运行目录失败：{e}"))?;
    let path = runtime_dir.join(NAPI_ADDON_NAME);
    if std::fs::read(&path).ok().as_deref() != Some(NAPI_ADDON) {
        std::fs::write(&path, NAPI_ADDON)
            .map_err(|e| format!("释放 native tools N-API addon 失败：{e}"))?;
    }
    Ok(path)
}
