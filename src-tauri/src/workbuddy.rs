//! Locate the bundled CLI metadata used by Lyra's WorkBuddy provider.
use std::path::{Path, PathBuf};

// WorkBuddy 5.7.7 ships the CLI outside app.asar; its adjacent product.json selects
// workbuddy-desktop authentication, so never substitute the global CodeBuddy CLI.
fn bundled_cli(root: &Path) -> PathBuf {
    root.join("resources/app.asar.unpacked/cli/bin/codebuddy")
}

pub(crate) fn program(configured: &str) -> String {
    let configured = configured.trim();
    if !configured.is_empty() {
        let path = Path::new(configured);
        if path.is_dir() {
            return bundled_cli(path).to_string_lossy().into_owned();
        }
        if path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.eq_ignore_ascii_case("WorkBuddy.exe")) {
            return bundled_cli(path.parent().unwrap_or(Path::new("."))).to_string_lossy().into_owned();
        }
        return configured.into();
    }
    #[cfg(windows)]
    for (env, relative) in [("LOCALAPPDATA", "Programs/WorkBuddy"), ("ProgramFiles", "WorkBuddy"), ("ProgramFiles(x86)", "WorkBuddy")] {
        if let Some(root) = std::env::var_os(env) {
            let cli = bundled_cli(&PathBuf::from(root).join(relative));
            if cli.is_file() {
                return cli.to_string_lossy().into_owned();
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workbuddy_maps_install_directory_and_desktop_exe_to_bundled_cli() {
        let root = std::env::temp_dir().join(format!("nova-workbuddy-path-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let cli = bundled_cli(&root);
        assert_eq!(program(root.to_str().unwrap()), cli.to_str().unwrap());
        for exe in ["WorkBuddy.exe", "WORKBUDDY.EXE"] {
            assert_eq!(program(root.join(exe).to_str().unwrap()), cli.to_str().unwrap());
        }
        assert_eq!(program(&format!("  {}  ", cli.display())), cli.to_str().unwrap());
        let missing = root.join("missing-cli");
        assert_eq!(program(missing.to_str().unwrap()), missing.to_str().unwrap());
        std::fs::remove_dir(root).unwrap();
    }
}
