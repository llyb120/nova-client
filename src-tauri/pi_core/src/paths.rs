//! Path normalization and resolution, ported from
//! `pi-coding-agent/dist/utils/paths.js` (`normalizePath`, `resolvePath`) and
//! `tools/path-utils.js` (`resolveToCwd`).
//!
//! Resolution is purely lexical (no symlink following), reproducing node
//! `path.resolve` for POSIX paths: `.`/`..`/duplicate/trailing slashes are
//! normalized and `..` cannot escape the root. `~` expansion and `file://`
//! handling cover the common cases; percent-decoding of `file://` URLs and
//! Windows drive/UNC forms are out of scope for the Linux runtime.

#[derive(Debug, Clone)]
pub struct NormalizeOptions {
    pub trim: bool,
    pub normalize_unicode_spaces: bool,
    pub strip_at_prefix: bool,
    pub expand_tilde: bool,
    pub home_dir: Option<String>,
}

impl Default for NormalizeOptions {
    fn default() -> Self {
        NormalizeOptions {
            trim: false,
            normalize_unicode_spaces: false,
            strip_at_prefix: false,
            expand_tilde: true,
            home_dir: None,
        }
    }
}

/// The Unicode space code points folded to ASCII space by `normalizePath`
/// (`/[\u00A0\u2000-\u200A\u202F\u205F\u3000]/g`).
fn is_unicode_space(c: char) -> bool {
    matches!(c,
        '\u{00A0}'
        | '\u{2000}'..='\u{200A}'
        | '\u{202F}'
        | '\u{205F}'
        | '\u{3000}'
    )
}

fn default_home_dir() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
}

/// Collapse `.`/`..`/empty components of an absolute POSIX path. `..` at the
/// root is dropped, matching node `path.resolve`.
fn normalize_components(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(component),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Minimal `fileURLToPath`: strip the `file://` prefix. Percent-decoding is not
/// performed (edge case for this runtime).
fn file_url_to_path(url: &str) -> String {
    url.strip_prefix("file://").unwrap_or(url).to_string()
}

/// Port of `normalizePath(input, options)`.
pub fn normalize_path(input: &str, options: &NormalizeOptions) -> String {
    let mut normalized = if options.trim {
        input.trim().to_string()
    } else {
        input.to_string()
    };
    if options.normalize_unicode_spaces {
        normalized = normalized
            .chars()
            .map(|c| if is_unicode_space(c) { ' ' } else { c })
            .collect();
    }
    if options.strip_at_prefix && normalized.starts_with('@') {
        normalized = normalized[1..].to_string();
    }
    if options.expand_tilde {
        let home = options
            .home_dir
            .clone()
            .unwrap_or_else(default_home_dir);
        if normalized == "~" {
            return home;
        }
        if let Some(rest) = normalized.strip_prefix("~/") {
            let joined = format!("{}/{}", home.trim_end_matches('/'), rest);
            return if home.starts_with('/') {
                normalize_components(&joined)
            } else {
                joined
            };
        }
    }
    if normalized.starts_with("file://") {
        return file_url_to_path(&normalized);
    }
    normalized
}

fn is_absolute(path: &str) -> bool {
    path.starts_with('/')
}

/// Port of `resolvePath(input, baseDir, options)`: normalize both, then
/// lexically resolve `input` against `baseDir` (or treat it as absolute).
pub fn resolve_path(input: &str, base_dir: &str, options: &NormalizeOptions) -> String {
    let normalized = normalize_path(input, options);
    let normalized_base = normalize_path(base_dir, &NormalizeOptions::default());
    if is_absolute(&normalized) {
        return normalize_components(&normalized);
    }
    let combined = format!("{}/{}", normalized_base.trim_end_matches('/'), normalized);
    normalize_components(&combined)
}

/// Port of `resolveToCwd(filePath, cwd)`: resolve with Unicode-space folding and
/// `@`-prefix stripping enabled (the tool default).
pub fn resolve_to_cwd(file_path: &str, cwd: &str) -> String {
    resolve_path(
        file_path,
        cwd,
        &NormalizeOptions {
            normalize_unicode_spaces: true,
            strip_at_prefix: true,
            ..Default::default()
        },
    )
}
