fn main() {
    let root = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    std::fs::create_dir_all(root.join("icons")).unwrap();
    std::fs::copy(root.join("../../app-icon.png"), root.join("icons/icon.png")).unwrap();
    tauri_build::build();
}
