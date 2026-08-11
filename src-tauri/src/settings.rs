use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// 默认中转服务地址。relay_server 为空时回退到它；空字符串表示必须用户自填。
pub const DEFAULT_RELAY_SERVER: &str = "";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CursorModelContextRule {
    /// 不区分大小写的 Cursor 模型 id 匹配串（包含匹配）。
    pub prefix: String,
    /// 模型上下文窗口，单位 token。
    pub context_window: u32,
}

/// 新建会话 / 会话页快捷键：一键切到指定项目或模型、快速新会话、插入文本。
/// Esc 终止回合为内置行为，不作为可配置项。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionShortcut {
    pub id: String,
    /// 规范化按键，如 Ctrl+1 / Alt+P。
    pub keys: String,
    /// selectProject | selectModel | newSession | insertText
    pub action: String,
    /// 项目绝对路径、`<agentKind>:<modelId>` / roam/quota 编码，或 insertText 文本；newSession 可为空。
    pub target: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StageModelTarget {
    pub agent_kind: String,
    pub model: String,
}

/// 上下文检索模式：none = 不注入工具，fast = 旧 FastContext，super = 无索引单遍 SuperContext。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ContextRetrievalMode {
    None,
    Fast,
    #[default]
    #[serde(other)]
    Super,
}

impl ContextRetrievalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Fast => "fast",
            Self::Super => "super",
        }
    }
}

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
    /// 兼容旧配置；Cursor 后端已改为仅使用官方 SDK，不再依赖本机 CLI
    pub cursor_path: String,
    /// 旧版 Cursor ACP 启动参数，仅用于兼容已有设置。
    pub cursor_args: String,
    /// Cursor 代理地址
    pub cursor_proxy: String,
    /// Cursor SDK API Key；空 = 使用 CURSOR_API_KEY 环境变量。
    pub cursor_sdk_api_key: String,
    /// Cursor 模型 id 包含匹配到上下文窗口的映射；最长匹配串优先。
    pub cursor_model_contexts: Vec<CursorModelContextRule>,
    /// Vega 上下文机制：default = Reasonix，super = 改造前的超级上下文。
    pub vega_context_mode: String,
    /// Cursor 上下文机制：default = Reasonix，super = 改造前的超级上下文。
    pub cursor_context_mode: String,
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
    /// Vega provider 代理地址（空 = 不覆盖环境变量）
    pub vega_proxy: String,
    /// Windows 下为 agent shell 子进程注入无窗口 shim（保存后重启应用生效）
    pub windows_shell_shim_enabled: bool,
    /// 穿越世界线时间线时是否还原 checkpoint 中的工作目录文件。
    pub checkpoint_enabled: bool,
    /// 新会话默认模式（统一模式 build / plan，空 = 跟随 agent 默认；旧值 bypass 视同 build）
    pub default_mode: String,
    /// 标题、快速总结、摘要和上下文压缩等辅助任务使用的轻量级模型后端。
    #[serde(alias = "titleModelAgent")]
    pub lightweight_model_agent: String,
    /// 轻量级模型 id；失败时辅助任务回退到发起任务的原模型。
    #[serde(alias = "titleModel")]
    pub lightweight_model: String,
    /// /stage、/stage2 等命令依次使用的模型；/stage 默认取第一项。
    pub stage_models: Vec<StageModelTarget>,
    /// 输入框补全所用 Vega 模型（provider/model 格式）；空 = 关闭补全。
    /// 补全不走 agent，只对该模型 API 直接发一次 completion 请求。
    pub completion_model: String,
    /// 打开文件用的编辑器命令（cursor / code / zed / windsurf 等，依赖 PATH）
    pub editor: String,
    /// 界面皮肤（ink-dark / ink-light，空 = 未设置，由前端 localStorage 迁移）
    pub theme: String,
    /// 会话历史展示方式（project / time）。
    pub history_display_mode: String,
    /// 聊天视图渲染方式（dom / canvas；默认 canvas）。
    pub chat_view_render: String,
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
    /// 新建会话模型选择器中收藏的模型，键格式为 `<agentKind>:<modelId>`。
    pub model_favorites: Vec<String>,
    /// 会话快捷键：按键一键切换项目或模型。
    pub session_shortcuts: Vec<SessionShortcut>,
    /// 是否启用各模型后端（仅影响前端可选性：关闭后不在新建/切换会话的后端列表里出现，
    /// 已存在的该后端历史会话仍可打开查看）
    pub devin_enabled: bool,
    pub vega_enabled: bool,
    /// Lyra 原生 agent（与 Vega 共用配置）。
    pub lyra_enabled: bool,
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
    /// 上下文检索：none / fast / super。SuperContext 使用无持久化索引的单遍程序切片，默认启用。
    pub context_retrieval_mode: ContextRetrievalMode,

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
            cursor_model_contexts: Vec::new(),
            vega_context_mode: "default".into(),
            cursor_context_mode: "default".into(),
            opencode_path: "opencode".into(),
            opencode_args: "acp".into(),
            opencode_proxy: String::new(),
            codex_path: "codex".into(),
            codex_args: "app-server --stdio".into(),
            codex_proxy: String::new(),
            vega_proxy: String::new(),
            windows_shell_shim_enabled: false,
            checkpoint_enabled: false,
            default_mode: String::new(),
            lightweight_model_agent: "alkaid".into(),
            lightweight_model: String::new(),
            stage_models: Vec::new(),
            completion_model: String::new(),
            editor: "code".into(),
            theme: String::new(),
            history_display_mode: "project".into(),
            chat_view_render: "canvas".into(),
            relay_server: DEFAULT_RELAY_SERVER.into(),
            relay_token: String::new(),
            relay_groups: String::new(),
            remote_control_enabled: false,
            quota_shared_models: Vec::new(),
            model_favorites: Vec::new(),
            session_shortcuts: Vec::new(),
            devin_enabled: false,
            vega_enabled: false,
            lyra_enabled: true,
            codex_enabled: false,
            codexplus_enabled: false,
            codebuddy_enabled: false,
            codebuddyplus_enabled: false,
            claudecode_enabled: false,
            cursor_enabled: false,
            opencode_enabled: false,
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
            context_retrieval_mode: ContextRetrievalMode::Super,

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
    fn legacy_title_model_migrates_to_lightweight_model() {
        let settings: Settings =
            serde_json::from_str(r#"{"titleModelAgent":"codex","titleModel":"gpt-5-mini"}"#)
                .unwrap();
        assert_eq!(settings.lightweight_model_agent, "codex");
        assert_eq!(settings.lightweight_model, "gpt-5-mini");
    }

    #[test]
    fn checkpoint_file_restore_is_disabled_by_default() {
        assert!(!Settings::default().checkpoint_enabled);
    }

    #[test]
    fn super_context_is_enabled_by_default() {
        let settings = Settings::default();
        assert_eq!(
            settings.context_retrieval_mode,
            super::ContextRetrievalMode::Super
        );
        assert!(settings.context_tools_enabled());
        assert!(settings.super_context_enabled());
    }

    #[test]
    fn legacy_disabled_fast_context_migrates_to_none() {
        let dir = std::env::temp_dir().join(format!("nova-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("settings.json"), r#"{"fastContextEnabled":false}"#).unwrap();
        let settings = Settings::load(&dir);
        assert_eq!(
            settings.context_retrieval_mode,
            super::ContextRetrievalMode::None
        );
        assert!(!settings.context_tools_enabled());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn context_retrieval_modes_round_trip() {
        for mode in [
            super::ContextRetrievalMode::None,
            super::ContextRetrievalMode::Fast,
            super::ContextRetrievalMode::Super,
        ] {
            let text = serde_json::to_string(&mode).unwrap();
            let decoded: super::ContextRetrievalMode = serde_json::from_str(&text).unwrap();
            assert_eq!(decoded, mode);
        }
    }

    #[test]
    fn cursor_model_context_rules_round_trip() {
        let settings: Settings = serde_json::from_str(
            r#"{"cursorModelContexts":[{"prefix":"claude-4","contextWindow":200000}]}"#,
        )
        .unwrap();
        assert_eq!(settings.cursor_model_contexts.len(), 1);
        assert_eq!(settings.cursor_model_contexts[0].prefix, "claude-4");
        assert_eq!(settings.cursor_model_contexts[0].context_window, 200_000);
    }

    #[test]
    fn session_shortcuts_round_trip() {
        let settings: Settings = serde_json::from_str(
            r#"{"sessionShortcuts":[{"id":"a","keys":"Ctrl+1","action":"selectModel","target":"codex:gpt-5.6"}]}"#,
        )
        .unwrap();
        assert_eq!(settings.session_shortcuts.len(), 1);
        assert_eq!(settings.session_shortcuts[0].keys, "Ctrl+1");
        assert_eq!(settings.session_shortcuts[0].action, "selectModel");
        assert_eq!(settings.session_shortcuts[0].target, "codex:gpt-5.6");
    }

    #[test]
    fn context_modes_default_and_round_trip() {
        let defaults = Settings::default();
        assert_eq!(defaults.vega_context_mode, "default");
        assert_eq!(defaults.cursor_context_mode, "default");
        let settings: Settings =
            serde_json::from_str(r#"{"vegaContextMode":"super","cursorContextMode":"super"}"#)
                .unwrap();
        assert_eq!(settings.vega_context_mode, "super");
        assert_eq!(settings.cursor_context_mode, "super");
    }

    #[test]
    fn vega_and_external_backends_are_disabled_by_default() {
        let settings = Settings::default();
        assert!(!settings.vega_enabled);
        assert!(!settings.devin_enabled);
        assert!(!settings.codex_enabled);
        assert!(!settings.codebuddy_enabled);
        assert!(!settings.claudecode_enabled);
        assert!(!settings.cursor_enabled);
        assert!(!settings.opencode_enabled);
    }

    #[test]
    fn legacy_alkaid_enabled_does_not_enable_vega() {
        let settings: Settings = serde_json::from_str(r#"{"alkaidEnabled":true}"#).unwrap();
        assert!(!settings.vega_enabled);
    }

    #[test]
    fn remote_control_is_disabled_by_default() {
        assert!(!Settings::default().remote_control_enabled);
        let settings: Settings = serde_json::from_str(r#"{"relayToken":"configured"}"#).unwrap();
        assert!(!settings.remote_control_enabled);
    }

    #[test]
    fn model_favorites_survive_reload() {
        let dir = std::env::temp_dir().join(format!("nova-settings-{}", uuid::Uuid::new_v4()));
        let mut settings = Settings::default();
        settings.model_favorites = vec!["codex:gpt-5.6".into()];
        settings.save(&dir);

        assert_eq!(
            Settings::load(&dir).model_favorites,
            settings.model_favorites
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn quota_shared_models_survive_reload_for_all_backends() {
        let dir = std::env::temp_dir().join(format!("nova-settings-{}", uuid::Uuid::new_v4()));
        let mut settings = Settings::default();
        settings.quota_shared_models = vec![
            "alkaid:provider/model".into(),
            "devin:swe-1.6".into(),
            "codex:gpt-5.6".into(),
            "codebuddy:claude-sonnet".into(),
            "claudecode:claude-opus".into(),
            "cursor:cursor-small".into(),
            "opencode:provider/model".into(),
        ];
        settings.save(&dir);

        assert_eq!(
            Settings::load(&dir).quota_shared_models,
            settings.quota_shared_models
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_history_display_mode_defaults_to_project() {
        let settings: Settings = serde_json::from_str(r#"{"theme":"ink-dark"}"#).unwrap();
        assert_eq!(settings.history_display_mode, "project");
    }

    #[test]
    fn missing_chat_view_render_defaults_to_canvas() {
        let settings: Settings = serde_json::from_str(r#"{"theme":"ink-dark"}"#).unwrap();
        assert_eq!(settings.chat_view_render, "canvas");
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
        let raw_value = raw
            .as_deref()
            .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok());
        let legacy_days = raw_value
            .as_ref()
            .and_then(|value| value["sessionAutoCleanupDays"].as_u64())
            .and_then(|days| u32::try_from(days).ok());
        let mut settings: Self = raw
            .as_deref()
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default();
        // 三态设置上线前只有 fastContextEnabled 布尔值。显式关闭继续迁移为 none；
        // 其它旧配置采用新的默认 SuperContext。
        if raw_value
            .as_ref()
            .is_some_and(|value| value.get("contextRetrievalMode").is_none())
            && raw_value
                .as_ref()
                .and_then(|value| value.get("fastContextEnabled"))
                .and_then(serde_json::Value::as_bool)
                == Some(false)
        {
            settings.context_retrieval_mode = ContextRetrievalMode::None;
        }
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
        if settings.vega_context_mode != "super" {
            settings.vega_context_mode = "default".into();
        }
        if settings.cursor_context_mode != "super" {
            settings.cursor_context_mode = "default".into();
        }
        settings
    }

    pub fn context_tools_enabled(&self) -> bool {
        self.context_retrieval_mode != ContextRetrievalMode::None
    }

    pub fn super_context_enabled(&self) -> bool {
        self.context_retrieval_mode == ContextRetrievalMode::Super
    }

    pub fn apply_context_retrieval_environment(&self) {
        std::env::set_var(
            "NOVA_CONTEXT_RETRIEVAL_MODE",
            self.context_retrieval_mode.as_str(),
        );
        std::env::set_var(
            "NOVA_CONTEXT_NO_INDEX",
            if self.super_context_enabled() {
                "1"
            } else {
                "0"
            },
        );
        // NOVA_SUPER_FAST_CONTEXT 是已废弃的旧实验优化，不属于新的 SuperContext。
        std::env::remove_var("NOVA_SUPER_FAST_CONTEXT");
    }

    pub fn save(&self, dir: &PathBuf) {
        let _ = fs::create_dir_all(dir);
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(dir.join("settings.json"), json);
        }
    }
}
