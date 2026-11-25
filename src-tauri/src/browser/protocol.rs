//! Shared browser protocol types, URL/upload guards, and CDP key mapping.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

pub(crate) const PAGE_RUNTIME: &str = include_str!("page_runtime.js");

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TabInfo {
    pub tag: String,
    pub title: String,
    pub url: String,
    pub active: bool,
    #[serde(default)]
    pub incognito: bool,
    #[serde(default)]
    pub loading: bool,
    #[serde(default = "default_true")]
    pub controllable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opener_tag: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ViewOptions {
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub max_items: Option<u32>,
    #[serde(default)]
    pub max_text_chars: Option<u32>,
    #[serde(default = "default_true")]
    pub screenshot: bool,
    #[serde(default = "default_true")]
    pub annotate: bool,
    /// Optional crop in CSS coordinates `(x, y, width, height)` or element `id` to zoom.
    #[serde(default)]
    pub zoom_id: Option<u32>,
    #[serde(default)]
    pub zoom_rect: Option<[f64; 4]>,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            scope: None,
            query: None,
            max_items: None,
            max_text_chars: None,
            screenshot: true,
            annotate: true,
            zoom_id: None,
            zoom_rect: None,
        }
    }
}

/// Raw result returned by either Headless Direct CDP or Real Chrome Extension WebSocket.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawView {
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub ready_state: String,
    /// `[cssWidth, cssHeight, devicePixelRatio]`
    pub viewport: [f64; 3],
    /// `[scrollX, scrollY, scrollWidth, scrollHeight]`
    #[serde(default)]
    pub scroll: [f64; 4],
    #[serde(default)]
    pub scroll_info: Option<Value>,
    #[serde(default)]
    pub cursor: Option<[f64; 2]>,
    #[serde(default)]
    pub focused: Option<Value>,
    #[serde(default)]
    pub overlay: Option<Value>,
    #[serde(default)]
    pub items: Vec<Value>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub observe_ms: u64,
    #[serde(default)]
    pub screenshot_b64: Option<String>,
    #[serde(default)]
    pub screenshot_error: Option<String>,
    /// If zoom_id was requested, the resolved CSS rect `[x, y, w, h]`.
    #[serde(default)]
    pub zoom_css: Option<[f64; 4]>,
    /// Present when pinch-zoom (`visualViewport.scale > 1`) shifts/scales the captured CSS viewport.
    #[serde(default)]
    pub visual_viewport: Option<[f64; 4]>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawActResult {
    pub ok: bool,
    #[serde(default)]
    pub completed: usize,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub actions: Vec<String>,
    #[serde(default)]
    pub opened_tabs: Vec<TabInfo>,
    #[serde(default)]
    pub view: Option<RawView>,
    #[serde(default)]
    pub action_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum Step {
    Click {
        #[serde(default)]
        id: Option<u32>,
        #[serde(default)]
        x: Option<f64>,
        #[serde(default)]
        y: Option<f64>,
        #[serde(default)]
        button: Option<String>,
        #[serde(default)]
        clicks: Option<u8>,
    },
    Type {
        text: String,
        #[serde(default)]
        id: Option<u32>,
        #[serde(default)]
        x: Option<f64>,
        #[serde(default)]
        y: Option<f64>,
        #[serde(default)]
        clear: Option<bool>,
        #[serde(default)]
        submit: Option<bool>,
    },
    Hover {
        #[serde(default)]
        id: Option<u32>,
        #[serde(default)]
        x: Option<f64>,
        #[serde(default)]
        y: Option<f64>,
    },
    Drag {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        #[serde(default)]
        duration_ms: Option<u64>,
    },
    Scroll {
        #[serde(default)]
        dy: i32,
        #[serde(default)]
        dx: i32,
        #[serde(default)]
        id: Option<u32>,
        #[serde(default)]
        x: Option<f64>,
        #[serde(default)]
        y: Option<f64>,
    },
    Press {
        key: String,
    },
    Wait {
        ms: u64,
    },
    Upload {
        selector: String,
        files: Vec<String>,
    },
    Dismiss,
}

pub(crate) fn normalized_url(raw: &str) -> Result<tauri::Url, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("请输入网址".into());
    }
    let value = if raw.contains("://") {
        raw.to_string()
    } else if raw.starts_with("localhost")
        || raw.starts_with("127.0.0.1")
        || raw.starts_with("[::1]")
    {
        format!("http://{raw}")
    } else {
        format!("https://{raw}")
    };
    let url = tauri::Url::parse(&value).map_err(|_| "网址无效")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("仅支持不含凭据的 HTTP(S) 网址".into());
    }
    Ok(url)
}

pub(crate) fn upload_files(root: &Path, files: &[String]) -> Result<Vec<String>, String> {
    if files.is_empty() || files.len() > 16 {
        return Err("upload 需要 1–16 个文件".into());
    }
    let root = dunce::canonicalize(root).map_err(|e| format!("上传目录不可用：{e}"))?;
    files
        .iter()
        .map(|file| {
            if file.trim().is_empty() {
                return Err("上传路径不能为空".into());
            }
            let path = dunce::canonicalize(root.join(file))
                .map_err(|e| format!("上传文件不可用：{e}"))?;
            if !path.starts_with(&root) || !path.is_file() {
                return Err("只能上传当前工作目录内的普通文件".into());
            }
            std::fs::File::open(&path).map_err(|e| format!("上传文件不可读：{e}"))?;
            path.to_str()
                .map(str::to_owned)
                .ok_or_else(|| "上传路径必须为有效 Unicode".into())
        })
        .collect()
}

/// `"Ctrl+Shift+ArrowDown"` -> `(key, DOM code, virtualKeyCode, CDP modifiers, editing command)`.
pub(crate) fn key_spec(name: &str) -> Option<(String, String, i32, i32, Option<&'static str>)> {
    let mut parts: Vec<&str> = name.split('+').collect();
    let base = parts.pop()?.trim();
    let mut modifiers = 0;
    for m in parts {
        modifiers |= match m.trim() {
            "Alt" | "Option" => 1,
            "Control" | "Ctrl" => 2,
            "Meta" | "Cmd" | "Command" => 4,
            "Shift" => 8,
            _ => return None,
        };
    }
    let named = |k: &str, vk| Some((k.to_string(), k.to_string(), vk));
    let (key, code, vk) = match base {
        "Enter" | "Return" => named("Enter", 13),
        "Tab" => named("Tab", 9),
        "Escape" | "Esc" => named("Escape", 27),
        "Backspace" => named("Backspace", 8),
        "Delete" | "Del" => named("Delete", 46),
        "Home" => named("Home", 36),
        "End" => named("End", 35),
        "PageUp" => named("PageUp", 33),
        "PageDown" => named("PageDown", 34),
        "ArrowLeft" | "Left" => named("ArrowLeft", 37),
        "ArrowUp" | "Up" => named("ArrowUp", 38),
        "ArrowRight" | "Right" => named("ArrowRight", 39),
        "ArrowDown" | "Down" => named("ArrowDown", 40),
        "Space" | " " => Some((" ".into(), "Space".into(), 32)),
        f if f.len() <= 3 && f.starts_with('F') => {
            let n: i32 = f[1..].parse().ok().filter(|n| (1..=12).contains(n))?;
            named(f, 111 + n)
        }
        c if c.len() == 1 && c.as_bytes()[0].is_ascii_alphanumeric() && modifiers & 7 != 0 => {
            let upper = c.to_ascii_uppercase();
            let key = if modifiers & 8 != 0 {
                upper.clone()
            } else {
                c.to_ascii_lowercase()
            };
            let code = if upper.as_bytes()[0].is_ascii_digit() {
                format!("Digit{upper}")
            } else {
                format!("Key{upper}")
            };
            Some((key, code, upper.as_bytes()[0] as i32))
        }
        _ => None,
    }?;
    // CDP synthetic key events need explicit editing commands for clipboard & selectAll.
    let command = if modifiers == 2 || modifiers == 4 {
        match vk {
            65 => Some("selectAll"),
            67 => Some("copy"),
            88 => Some("cut"),
            86 => Some("paste"),
            _ => None,
        }
    } else {
        None
    };
    Some((key, code, vk, modifiers, command))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_specs_cover_shortcuts_and_editing_commands() {
        assert_eq!(key_spec("Ctrl+A").unwrap().4, Some("selectAll"));
        assert_eq!(key_spec("Ctrl+C").unwrap().4, Some("copy"));
        assert_eq!(key_spec("Cmd+V").unwrap().4, Some("paste"));
        assert_eq!(key_spec("Enter").unwrap().2, 13);
        assert_eq!(key_spec("Shift+ArrowDown").unwrap().3, 8);
        assert!(key_spec("a").is_none());
    }

    #[test]
    fn normalized_url_blocks_unsafe_schemes() {
        assert_eq!(normalized_url("localhost:5173").unwrap().scheme(), "http");
        assert_eq!(normalized_url("example.com").unwrap().scheme(), "https");
        assert!(normalized_url("file:///C:/a").is_err());
        assert!(normalized_url("javascript:alert(1)").is_err());
        assert!(normalized_url("https://u:p@example.com").is_err());
    }
}
