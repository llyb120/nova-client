use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// 默认中转服务地址。relay_server 为空时回退到它；空字符串表示必须用户自填。
pub const DEFAULT_RELAY_SERVER: &str = "";

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// ACP agent 可执行文件路径（默认 devin，依赖 PATH）
    pub devin_path: String,
    /// Devin ACP 启动参数（空格分隔）。
    pub acp_args: String,
    /// Devin 代理地址（空 = 不覆盖环境变量；下同：注入 HTTP(S)_PROXY 等到该后端子进程）
    pub devin_proxy: String,
    /// CodeBuddy CLI 可执行文件路径（默认 codebuddy，依赖 PATH）
    pub codebuddy_path: String,
    /// 旧版 CodeBuddy ACP 启动参数，仅用于兼容已有设置。
    pub codebuddy_args: String,
    /// CodeBuddy 代理地址
    pub codebuddy_proxy: String,
    /// Claude Code CLI 可执行文件路径，仅用于 CLI 检测和升级。
    pub claudecode_path: String,
    /// 旧版 Claude Code ACP 启动参数，仅用于兼容已有设置。
    pub claudecode_args: String,
    /// Claude Code 代理地址
    pub claudecode_proxy: String,
    /// Claude Agent SDK API Key；空 = 使用环境/provider 凭据。
    pub claudecode_sdk_api_key: String,
    /// Cursor CLI（cursor-agent）可执行文件路径（默认 cursor-agent，依赖 PATH）
    pub cursor_path: String,
    /// 旧版 Cursor ACP 启动参数，仅用于兼容已有设置。
    pub cursor_args: String,
    /// Cursor 代理地址
    pub cursor_proxy: String,
    /// Cursor SDK API Key；空 = 使用 CURSOR_API_KEY 环境变量。
    pub cursor_sdk_api_key: String,
    /// OpenCode CLI 可执行文件路径（默认 opencode，依赖 PATH）
    pub opencode_path: String,
    /// 旧版 OpenCode ACP 启动参数，仅用于兼容已有设置。
    pub opencode_args: String,
    /// OpenCode 代理地址
    pub opencode_proxy: String,
    /// Codex CLI 可执行文件路径（默认 codex，依赖 PATH）
    pub codex_path: String,
    /// Codex app-server 启动参数
    pub codex_args: String,
    /// Codex 代理地址（空 = 不覆盖环境变量）
    pub codex_proxy: String,
    /// Windows 下为 agent shell 子进程注入无窗口 shim（保存后重启应用生效）
    pub windows_shell_shim_enabled: bool,
    /// 新会话默认模式（统一模式 build / plan，空 = 跟随 agent 默认；旧值 bypass 视同 build）
    pub default_mode: String,
    /// 自动生成会话标题所用的后端（devin/codex/codebuddy/...，空 = devin）。
    /// 与新建会话一致：可任选后端，标题统一在此后端生成（Codex/不可用时回退线程自身后端）。
    pub title_model_agent: String,
    /// 自动生成会话标题所用模型（须为 title_model_agent 后端的模型；空 = 用该后端会话默认模型）
    pub title_model: String,
    /// 高级分享「处理/总结」所用的后端（devin/codex/codebuddy/...，空 = devin）
    pub share_model_agent: String,
    /// 高级分享「处理/总结」的默认模型（须为 share_model_agent 后端的模型；空 = Devin 时兜底 swe-1.6）
    pub share_model: String,
    /// 打开文件用的编辑器命令（cursor / code / zed / windsurf 等，依赖 PATH）
    pub editor: String,
    /// 界面皮肤（ink-dark / ink-light，空 = 未设置，由前端 localStorage 迁移）
    pub theme: String,
    /// 界面风格（modern / classic）；缺省使用 modern。
    pub ui_style: String,
    /// 会话历史展示方式（project / time）。
    pub history_display_mode: String,
    /// 团队/漫游中转服务地址（空 = 关闭团队/漫游功能）
    pub relay_server: String,
    /// 团队/漫游身份 token（永久，用以区分每个人；空 = 不连接中转站）
    pub relay_token: String,
    /// 归属的群组（逗号/空格分隔，可多个）。只有相同群组的人才能在在线名单里看到彼此；
    /// 空 = 默认群组（与其他同样未配置群组的人互相可见，向后兼容）。
    pub relay_groups: String,
    /// 是否允许 server 端远程查看和控制本机会话；默认关闭。
    pub remote_control_enabled: bool,
    /// 允许同团队成员借用的模型，键格式为 `<agentKind>:<modelId>`；空 = 不共享额度。
    pub quota_shared_models: Vec<String>,
    /// 是否启用各模型后端（仅影响前端可选性：关闭后不在新建/切换会话的后端列表里出现，
    /// 已存在的该后端历史会话仍可打开查看）
    pub devin_enabled: bool,
    pub alkaid_enabled: bool,
    pub codex_enabled: bool,
    /// 旧版独立 SDK 后端开关，仅用于兼容反序列化。
    pub codexplus_enabled: bool,
    pub codebuddy_enabled: bool,
    pub codebuddyplus_enabled: bool,
    pub claudecode_enabled: bool,
    pub cursor_enabled: bool,
    pub opencode_enabled: bool,
    pub opencodeplus_enabled: bool,
    /// 各后端接入方式：sdk / acp。Devin 固定使用 ACP。
    pub codex_integration: String,
    pub codebuddy_integration: String,
    pub claudecode_integration: String,
    pub cursor_integration: String,
    pub opencode_integration: String,
    /// worktree 工作目录的根（空 = 应用数据目录下的 worktrees/）。
    /// 会话开启「在 worktree 中执行」时，在此目录下为其创建独立工作目录。
    pub worktree_dir: String,
    /// 是否自动清理长期未更新的会话。
    pub session_auto_cleanup_enabled: bool,
    /// 自动清理会话的保留时长（小时）。
    pub session_auto_cleanup_hours: u32,
    /// 语义检索开关（关 = 用内置 BM25 关键词检索；开需配置下面的 embedding 服务）
    pub semantic_enabled: bool,
    /// embedding 服务地址（OpenAI 兼容 /v1/embeddings；本地 Ollama 默认 http://localhost:11434）
    pub embed_endpoint: String,
    /// embedding 模型名（如 bge-m3 / nomic-embed-text / text-embedding-3-small）
    pub embed_model: String,
    /// embedding 服务 API key（本地服务通常留空）
    pub embed_api_key: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            devin_path: "devin".into(),
            acp_args: "acp".into(),
            devin_proxy: String::new(),
            codebuddy_path: "codebuddy".into(),
            codebuddy_args: "--acp".into(),
            codebuddy_proxy: String::new(),
            claudecode_path: "claude".into(),
            claudecode_args: "-y @zed-industries/claude-code-acp".into(),
            claudecode_proxy: String::new(),
            claudecode_sdk_api_key: String::new(),
            cursor_path: "cursor-agent".into(),
            cursor_args: "acp".into(),
            cursor_proxy: String::new(),
            cursor_sdk_api_key: String::new(),
            opencode_path: "opencode".into(),
            opencode_args: "acp".into(),
            opencode_proxy: String::new(),
            codex_path: "codex".into(),
            codex_args: "app-server --stdio".into(),
            codex_proxy: String::new(),
            windows_shell_shim_enabled: false,
            default_mode: String::new(),
            title_model_agent: "devin".into(),
            title_model: "swe-1-6".into(),
            share_model_agent: "devin".into(),
            share_model: "swe-1.6".into(),
            editor: "code".into(),
            theme: String::new(),
            ui_style: "modern".into(),
            history_display_mode: "project".into(),
            relay_server: DEFAULT_RELAY_SERVER.into(),
            relay_token: String::new(),
            relay_groups: String::new(),
            remote_control_enabled: false,
            quota_shared_models: Vec::new(),
            devin_enabled: true,
            alkaid_enabled: true,
            codex_enabled: true,
            codexplus_enabled: false,
            codebuddy_enabled: true,
            codebuddyplus_enabled: false,
            claudecode_enabled: true,
            cursor_enabled: true,
            opencode_enabled: true,
            opencodeplus_enabled: false,
            codex_integration: "sdk".into(),
            codebuddy_integration: "sdk".into(),
            claudecode_integration: "sdk".into(),
            cursor_integration: "sdk".into(),
            opencode_integration: "sdk".into(),
            worktree_dir: String::new(),
            session_auto_cleanup_enabled: false,
            session_auto_cleanup_hours: 24 * 30,
            semantic_enabled: false,
            embed_endpoint: "http://localhost:11434".into(),
            embed_model: "bge-m3".into(),
            embed_api_key: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Settings;
    use std::fs;

    #[test]
    fn windows_shell_shim_is_disabled_by_default() {
        assert!(!Settings::default().windows_shell_shim_enabled);
    }

    #[test]
    fn remote_control_is_disabled_by_default() {
        assert!(!Settings::default().remote_control_enabled);
        let settings: Settings = serde_json::from_str(r#"{"relayToken":"configured"}"#).unwrap();
        assert!(!settings.remote_control_enabled);
    }

    #[test]
    fn missing_history_display_mode_defaults_to_project() {
        let settings: Settings = serde_json::from_str(r#"{"theme":"ink-dark"}"#).unwrap();
        assert_eq!(settings.history_display_mode, "project");
        assert_eq!(settings.ui_style, "modern");
    }

    #[test]
    fn sdk_integration_defaults_match_backend_policy() {
        let settings = Settings::default();
        assert_eq!(settings.codex_integration, "sdk");
        assert_eq!(settings.codebuddy_integration, "sdk");
        assert_eq!(settings.opencode_integration, "sdk");
        assert_eq!(settings.claudecode_integration, "sdk");
        assert_eq!(settings.cursor_integration, "sdk");
    }

    #[test]
    fn load_forces_persisted_integrations_to_sdk() {
        let dir = std::env::temp_dir().join(format!("nova-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("settings.json"),
            r#"{
                "codexIntegration":"acp",
                "codebuddyIntegration":"acp",
                "claudecodeIntegration":"acp",
                "cursorIntegration":"acp",
                "opencodeIntegration":"acp"
            }"#,
        )
        .unwrap();

        let settings = Settings::load(&dir);

        assert_eq!(settings.codex_integration, "sdk");
        assert_eq!(settings.codebuddy_integration, "sdk");
        assert_eq!(settings.claudecode_integration, "sdk");
        assert_eq!(settings.cursor_integration, "sdk");
        assert_eq!(settings.opencode_integration, "sdk");
        fs::remove_dir_all(dir).unwrap();
    }
}

impl Settings {
    pub fn load(dir: &PathBuf) -> Self {
        let raw = fs::read_to_string(dir.join("settings.json")).ok();
        let legacy_days = raw.as_deref().and_then(|json| {
            serde_json::from_str::<serde_json::Value>(json)
                .ok()
                .and_then(|value| value["sessionAutoCleanupDays"].as_u64())
                .and_then(|days| u32::try_from(days).ok())
        });
        let mut settings: Self = raw
            .as_deref()
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default();
        if settings.claudecode_path.trim() == "npx" {
            settings.claudecode_path = "claude".into();
        }
        if settings.session_auto_cleanup_hours == 0 {
            settings.session_auto_cleanup_hours = legacy_days
                .map(|days| days.saturating_mul(24))
                .unwrap_or(24 * 30);
        }
        // 旧版把 SDK 暴露为独立 “+” 后端；升级后折叠为同一后端的接入方式。
        if settings.codexplus_enabled {
            settings.codex_integration = "sdk".into();
        }
        if settings.codebuddyplus_enabled {
            settings.codebuddy_integration = "sdk".into();
        }
        if settings.opencodeplus_enabled {
            settings.opencode_integration = "sdk".into();
        }
        settings.codexplus_enabled = false;
        settings.codebuddyplus_enabled = false;
        settings.opencodeplus_enabled = false;
        settings.codex_integration = "sdk".into();
        settings.codebuddy_integration = "sdk".into();
        settings.claudecode_integration = "sdk".into();
        settings.cursor_integration = "sdk".into();
        settings.opencode_integration = "sdk".into();
        settings
            .quota_shared_models
            .retain(|entry| entry.starts_with("devin:") || entry.starts_with("codex:"));
        settings
    }

    pub fn save(&self, dir: &PathBuf) {
        let _ = fs::create_dir_all(dir);
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(dir.join("settings.json"), json);
        }
    }
}
