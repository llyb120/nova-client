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

/// Tauri's default manifest plus a Windows 8+ compatibility declaration: layered *child*
/// windows (the taskbar island on Windows 11) are only created for 8-aware applications.
const APP_MANIFEST: &str = r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*" />
    </dependentAssembly>
  </dependency>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{4a2f28e3-53b9-4441-ba9c-d69d4a4a6e38}" />
      <supportedOS Id="{1f676c76-80e1-4239-95bb-83d0f6d0da78}" />
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}" />
    </application>
  </compatibility>
</assembly>"#;

fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(
        tauri_build::WindowsAttributes::new().app_manifest(APP_MANIFEST),
    ))
    .expect("failed to run tauri-build");
    build_windows_shell_shim();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            // tauri-winres links its resource to application binaries, but not
            // the libtest executable. Tao/rfd also use Common Controls v6 there.
            // Without activation, Windows loads v5 and aborts before any test
            // with STATUS_ENTRYPOINT_NOT_FOUND (e.g. TaskDialogIndirect).
            // Generic link args cover libtest as well as the normal app; leave
            // the existing Tauri manifest/icon/version resource in place.
            println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
            println!("cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'");
            // Normal binaries already contain Tauri's RT_MANIFEST resource.
            // Do not generate a second resource with the same ID for them.
            println!("cargo:rustc-link-arg-bins=/MANIFEST:NO");
        } else if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
            let resource = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap())
                .join("libresource.a");
            println!("cargo:rustc-link-arg={}", resource.display());
        }
    }
}
