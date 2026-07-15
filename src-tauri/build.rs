fn build_windows_shell_shim() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let source = manifest_dir.join("windows-shell-shim.rs");
    let output =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("nova-shell-shim.exe");
    println!("cargo:rerun-if-changed={}", source.display());

    let mut command = std::process::Command::new(std::env::var_os("RUSTC").unwrap());
    command
        .arg(&source)
        .arg("--crate-name")
        .arg("nova_shell_shim")
        .arg("--crate-type=bin")
        .arg("--edition=2021")
        .arg("--target")
        .arg(std::env::var_os("TARGET").unwrap())
        .arg("-Copt-level=z")
        .arg("-Ccodegen-units=1")
        .arg("-Cpanic=abort")
        .arg("-Cdebuginfo=0")
        .arg("-Cstrip=symbols")
        .arg("-o")
        .arg(&output);
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        command.arg("-Ctarget-feature=+crt-static");
    }

    let result = command
        .output()
        .unwrap_or_else(|e| panic!("无法编译 Windows shell shim：{e}"));
    if !result.status.success() {
        panic!(
            "Windows shell shim 编译失败：\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

fn main() {
    tauri_build::build();
    build_windows_shell_shim();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu")
    {
        let resource =
            std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("libresource.a");
        println!("cargo:rustc-link-arg={}", resource.display());
    }
}
