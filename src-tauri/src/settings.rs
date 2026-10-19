use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// 默认中转服务地址。relay_server 为空时回退到它；空字符串表示必须用户自填。
pub const DEFAULT_RELAY_SERVER: &str = "";

/// 新建会话 / 会话页快捷键：一键切到指定项目或模型、快速新会话、插入文本。
/// Esc 终止回合为内置行为，不作为可配置项。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionShortcut {
    pub id: String,
    /// 规范化按键，如 Ctrl+1 / Alt+P。
    pub keys: String,
    /// selectProject | selectModel | newSession | openUnread | insertText | selectWorkflow
    pub action: String,
    /// 项目绝对路径、`<agentKind>:<modelId>` / roam 编码，或 insertText 文本；newSession 可为空。
    pub target: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelTarget {
    pub agent_kind: String,
    /// 空 = 该后端默认模型。
    pub model: String,
}

/// 预设场景：自动模式、Stage、Butterfly/Meteor/员工等入口按场景取模型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scene {
    Lightweight,
    Coding,
    Computer,
}

impl Scene {
    pub const ALL: [Scene; 3] = [Scene::Lightweight, Scene::Coding, Scene::Computer];

    pub fn key(self) -> &'static str {
        match self {
            Scene::Lightweight => "lightweight",
            Scene::Coding => "coding",
            Scene::Computer => "computer",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Scene::Lightweight => "轻量化",
            Scene::Coding => "编码",
            Scene::Computer => "剑来",
        }
    }

    /// 适用条件，给模型判断是否转交。
    pub fn when(self) -> &'static str {
        match self {
            Scene::Lightweight => "简单问答、整理总结、标题摘要等轻量事务",
            Scene::Coding => "需要写代码、改仓库、调试或运行构建测试",
            Scene::Computer => "需要操作桌面应用或浏览器（截图、点击、输入）",
        }
    }

    /// 接受 key 或中文名。
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        Self::ALL.into_iter().find(|s| s.key().eq_ignore_ascii_case(value) || s.label() == value || (s == &Scene::Computer && value == "电脑操控"))
    }
}

/// 每个场景一个有序模型列表：按顺序取第一个可用的，失效时切换下一个。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SceneModels {
    pub lightweight: Vec<ModelTarget>,
    pub coding: Vec<ModelTarget>,
    pub computer: Vec<ModelTarget>,
}

impl SceneModels {
    pub fn get(&self, scene: Scene) -> &[ModelTarget] {
        match scene {
            Scene::Lightweight => &self.lightweight,
            Scene::Coding => &self.coding,
            Scene::Computer => &self.computer,
        }
    }

    /// 先决条件：三个场景都至少配置一个模型。
    pub fn ready(&self) -> bool {
        Scene::ALL.into_iter().all(|scene| !self.get(scene).is_empty())
    }
}

/// 上下文检索模式：none = 不注入工具，fast = 旧 FastContext，super = 无索引单遍 SuperContext。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ContextRetrievalMode {
    None,
    #[default]
    #[serde(other)]
    Fast,
}

impl ContextRetrievalMode {
    // 兼容持久化的旧值（如 super）：#[serde(other)] 会把无法识别的变体反序列化为 Fast。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Fast => "fast",
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_powershell_utf8() -> bool {
    true
}
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Kimi Code CLI，使用固定的 `acp` 子命令。
    pub kimi_path: String,
    /// Claude Code 的 ACP adapter（不是 claude CLI 本身）。
    pub claude_path: String,
    pub claude_proxy: String,
    pub claude_enabled: bool,
    pub kimi_proxy: String,
    pub kimi_enabled: bool,
    /// ACP agent 可执行文件路径（默认 devin，依赖 PATH）
    pub devin_path: String,
    /// Devin ACP 启动参数（空格分隔）。
    pub acp_args: String,
    /// Devin 代理地址（空 = 直连，不继承系统代理；下同：注入 HTTP(S)_PROXY 等到该后端子进程）
    pub devin_proxy: String,
    /// CodeBuddy CLI 可执行文件路径（默认 codebuddy，依赖 PATH）
    pub codebuddy_path: String,
    /// 旧版 CodeBuddy ACP 启动参数（`--acp` 时代遗留）；当前走官方 HTTP 传输
    /// （`codebuddy --serve` + /api/v1/acp），不再使用该字段，仅兼容反序列化。
    pub codebuddy_args: String,
    /// CodeBuddy 代理地址
    pub codebuddy_proxy: String,
    /// WorkBuddy 安装目录或内置 CodeBuddy CLI；空 = 自动发现，不回退到普通 CodeBuddy。
    pub workbuddy_path: String,
    pub workbuddy_proxy: String,
    /// Codex CLI 可执行文件路径（默认 codex，依赖 PATH）
    pub codex_path: String,
    /// Codex app-server 启动参数
    pub codex_args: String,
    /// Codex 代理地址（空 = 直连，不继承系统代理）
    pub codex_proxy: String,
    /// Lyra provider 代理地址（空 = 直连，不继承系统代理）
    #[serde(alias = "vegaProxy")]
    pub lyra_proxy: String,
    /// Windows 下为 agent shell 子进程注入无窗口 shim（保存后重启应用生效）
    pub windows_shell_shim_enabled: bool,
    /// PowerShell 输出按 UTF-8 捕获（Lyra 的 bash 工具注入一次性前缀，把控制台
    /// 输出切到 UTF-8），避免 GBK(cp936) 与 UTF-8 解码错配产生乱码。默认开启。
    #[serde(default = "default_powershell_utf8")]
    pub powershell_utf8_enabled: bool,
    /// 是否允许 Lyra 自动切换当前项目和工具工作目录；默认开启。
    pub auto_change_project_enabled: bool,
    /// ponytail 极简模式：在 system prompt 注入「最小实现/最少改动」规则。默认开启。
    #[serde(default = "default_true")]
    pub ponytail_enabled: bool,
    /// 穿越世界线时间线时是否还原 checkpoint 中的工作目录文件。
    pub checkpoint_enabled: bool,
    /// 新会话默认模式（统一模式 build / plan，空 = 跟随 agent 默认；旧值 bypass 视同 build）
    pub default_mode: String,
    /// 场景模型：轻量化（标题/摘要/Stage/Butterfly/Meteor/员工）、编码、剑来。
    pub scene_models: SceneModels,
    /// 打开文件用的编辑器命令（cursor / code / zed / windsurf 等，依赖 PATH）
    pub editor: String,
    pub terminal_shell: String,
    pub terminal_args: Vec<String>,
    /// 界面皮肤（ink-dark / ink-light，空 = 未设置，由前端 localStorage 迁移）
    pub theme: String,
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
    /// 新建会话模型选择器中收藏的模型，键格式为 `<agentKind>:<modelId>`。
    pub model_favorites: Vec<String>,
    /// 会话快捷键：按键一键切换项目或模型。
    pub session_shortcuts: Vec<SessionShortcut>,
    /// Meteor 屏幕标注入口；开发实例运行时正式版自动让出触发权。
    pub meteor_enabled: bool,
    /// Alt+Alt 表示连续两次纯 Alt；其它值为全局组合键。
    pub meteor_shortcut: String,
    /// Meteor 提交时附加触发时的最近剪贴板（文字/图片/文件路径）；默认关闭。
    pub meteor_clipboard: bool,
    /// Meteor 星移（StartShift）的全局组合键；空值关闭。
    pub meteor_paste_shortcut: String,
    /// 剑谱录制开关的全局组合键；空值关闭。
    pub jianpu_shortcut: String,
    /// 是否启用各模型后端（仅影响前端可选性：关闭后不在新建/切换会话的后端列表里出现，
    /// 已存在的该后端历史会话仍可打开查看）
    pub devin_enabled: bool,
    /// 旧 Vega 开关，仅兼容反序列化（迁移到 lyra_enabled 后忽略）。
    pub vega_enabled: bool,
    /// Lyra Rust 原生 agent（进程内运行，不经 Node bridge）。
    pub lyra_enabled: bool,
    pub codex_enabled: bool,
    /// 旧版独立 SDK 后端开关，仅用于兼容反序列化。
    pub codexplus_enabled: bool,
    pub codebuddy_enabled: bool,
    pub codebuddyplus_enabled: bool,
    pub workbuddy_enabled: bool,
    /// 各后端接入方式：app-server / sdk / acp。Codex 固定使用 app-server。
    pub codex_integration: String,
    pub codebuddy_integration: String,
    /// worktree 工作目录的根（空 = 应用数据目录下的 worktrees/）。
    /// 会话开启「在 worktree 中执行」时，在此目录下为其创建独立工作目录。
    pub worktree_dir: String,
    /// 更新通道：release / pre-release。
    pub update_channel: String,
    /// 是否自动清理长期未更新的会话。
    pub session_auto_cleanup_enabled: bool,
    /// 自动清理会话的保留时长（小时）。
    pub session_auto_cleanup_hours: u32,
    /// 在侧栏显示操作知识图谱入口，默认关闭。
    pub knowledge_graph_enabled: bool,
    /// 上下文检索：none / fast。旧配置中的 super 会在加载时迁移为 fast。
    pub context_retrieval_mode: ContextRetrievalMode,

    /// 用户自定义追加的环境变量（覆盖同名用户/系统变量），
    /// 注入所有 agent 子进程，并作为工作流模板 {{xx}} 的替换来源。
    #[serde(default)]
    pub custom_env_vars: std::collections::HashMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            kimi_path: "kimi".into(),
            claude_path: "claude-agent-acp".into(),
            claude_proxy: String::new(),
            claude_enabled: false,
            kimi_proxy: String::new(),
            kimi_enabled: false,
            devin_path: "devin".into(),
            acp_args: "acp".into(),
            devin_proxy: String::new(),
            codebuddy_path: "codebuddy".into(),
            codebuddy_args: "--acp".into(),
            codebuddy_proxy: String::new(),
            workbuddy_path: String::new(),
            workbuddy_proxy: String::new(),
            codex_path: "codex".into(),
            codex_args: "app-server --stdio".into(),
            codex_proxy: String::new(),
            lyra_proxy: String::new(),
            windows_shell_shim_enabled: false,
            powershell_utf8_enabled: true,
            auto_change_project_enabled: true,
            ponytail_enabled: true,
            checkpoint_enabled: false,
            default_mode: String::new(),
            scene_models: SceneModels::default(),
            editor: "code".into(),
            terminal_shell: String::new(),
            terminal_args: Vec::new(),
            theme: String::new(),
            history_display_mode: "project".into(),
            relay_server: DEFAULT_RELAY_SERVER.into(),
            relay_token: String::new(),
            relay_groups: String::new(),
            remote_control_enabled: false,
            model_favorites: Vec::new(),
            session_shortcuts: Vec::new(),
            meteor_enabled: true,
            meteor_shortcut: "Alt+Alt".into(),
            meteor_clipboard: false,
            meteor_paste_shortcut: "Alt+V".into(),
            jianpu_shortcut: "Alt+R".into(),
            devin_enabled: false,
            vega_enabled: false,
            lyra_enabled: true,
            codex_enabled: false,
            codexplus_enabled: false,
            codebuddy_enabled: false,
            codebuddyplus_enabled: false,
            workbuddy_enabled: false,
            codex_integration: "app-server".into(),
            codebuddy_integration: "acp".into(),
            worktree_dir: String::new(),
            update_channel: "release".into(),
            session_auto_cleanup_enabled: false,
            session_auto_cleanup_hours: 24 * 30,
            knowledge_graph_enabled: false,
            context_retrieval_mode: ContextRetrievalMode::Fast,
            custom_env_vars: std::collections::HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ModelTarget, Scene, SessionShortcut, Settings};
    use std::fs;

    #[test]
    fn powershell_utf8_is_enabled_by_default_and_round_trips() {
        assert!(Settings::default().powershell_utf8_enabled);
        // 旧配置缺该字段时按默认开启解析。
        let legacy: Settings = serde_json::from_str(r#"{}"#).unwrap();
        assert!(legacy.powershell_utf8_enabled);
        let off: Settings = serde_json::from_str(r#"{"powershellUtf8Enabled":false}"#).unwrap();
        assert!(!off.powershell_utf8_enabled);
    }

    #[test]
    fn windows_shell_shim_is_disabled_by_default() {
        assert!(!Settings::default().windows_shell_shim_enabled);
    }

    #[test]
    fn removed_taskbar_status_toggle_still_loads() {
        // Butterfly is always on; old configs carrying the toggle must still parse.
        assert!(serde_json::from_str::<Settings>(r#"{"taskbarStatusEnabled":false}"#).is_ok());
    }

    #[test]
    fn custom_env_vars_default_empty_and_round_trips() {
        assert!(Settings::default().custom_env_vars.is_empty());
        // 旧配置缺该字段时按空表解析。
        let legacy: Settings = serde_json::from_str(r#"{}"#).unwrap();
        assert!(legacy.custom_env_vars.is_empty());
        let with_vars: Settings =
            serde_json::from_str(r#"{"customEnvVars":{"MY_KEY":"abc"}}"#).unwrap();
        assert_eq!(with_vars.custom_env_vars["MY_KEY"], "abc");
    }

    #[test]
    fn apply_custom_env_vars_sets_and_removes() {
        let unique = |suffix: &str| format!("NOVA_TEST_CUSTOM_ENV_{suffix}");
        let (keep, removed) = (unique("KEEP"), unique("REMOVED"));
        let mut previous = std::collections::HashMap::new();
        previous.insert(keep.clone(), "old".into());
        previous.insert(removed.clone(), "x".into());
        std::env::set_var(&keep, "old");
        std::env::set_var(&removed, "x");

        let mut settings = Settings::default();
        settings.custom_env_vars.insert(keep.clone(), "new".into());
        settings.apply_custom_env_vars(&previous);

        assert_eq!(std::env::var(&keep).as_deref(), Ok("new"));
        assert!(std::env::var_os(&removed).is_none());
        std::env::remove_var(&keep);
    }

    #[test]
    fn legacy_lightweight_model_migrates_to_scene() {
        let dir = std::env::temp_dir().join(format!("nova-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("settings.json"), r#"{"titleModelAgent":"alkaid","titleModel":"p/m","stageModels":[{"agentKind":"codex","model":"x"}]}"#).unwrap();
        let settings = Settings::load(&dir);
        assert_eq!(settings.scene_models.lightweight, vec![ModelTarget { agent_kind: "lyra".into(), model: "p/m".into() }]);
        assert!(settings.scene_models.coding.is_empty() && !settings.scene_models.ready());
        let saved = serde_json::to_value(&settings).unwrap();
        assert!(saved.get("stageModels").is_none() && saved.get("lightweightModel").is_none());
        // 已有场景配置时不再被旧字段覆盖。
        fs::write(dir.join("settings.json"), r#"{"lightweightModelAgent":"codex","sceneModels":{"lightweight":[{"agentKind":"lyra","model":""}]}}"#).unwrap();
        assert_eq!(Settings::load(&dir).scene_models.lightweight[0].agent_kind, "lyra");
        fs::remove_dir_all(dir).unwrap();
        assert_eq!(Scene::parse("剑来"), Some(Scene::Computer));
        assert_eq!(Scene::parse("电脑操控"), Some(Scene::Computer));
        assert_eq!(Scene::parse("coding"), Some(Scene::Coding));
    }

    #[test]
    fn checkpoint_file_restore_is_disabled_by_default() {
        assert!(!Settings::default().checkpoint_enabled);
    }

    #[test]
    fn polaris_is_enabled_by_default() {
        let settings = Settings::default();
        assert_eq!(
            settings.context_retrieval_mode,
            super::ContextRetrievalMode::Fast
        );
        assert!(settings.context_tools_enabled());
    }

    #[test]
    fn persisted_super_context_migrates_to_fast() {
        let dir = std::env::temp_dir().join(format!("nova-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("settings.json"),
            r#"{"contextRetrievalMode":"super"}"#,
        )
        .unwrap();
        let settings = Settings::load(&dir);
        assert_eq!(
            settings.context_retrieval_mode,
            super::ContextRetrievalMode::Fast
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_disabled_polaris_migrates_to_none() {
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
        ] {
            let text = serde_json::to_string(&mode).unwrap();
            let decoded: super::ContextRetrievalMode = serde_json::from_str(&text).unwrap();
            assert_eq!(decoded, mode);
        }
    }

    #[test]
    fn removed_cursor_settings_are_ignored() {
        let settings: Settings = serde_json::from_str(r#"{
            "cursorPath":"cursor-agent",
            "cursorArgs":"acp",
            "cursorProxy":"http://localhost:1234",
            "cursorSdkApiKey":"legacy-key",
            "cursorDisableSubagents":true,
            "cursorModelContexts":[{"prefix":"claude-4","contextWindow":200000}],
            "cursorContextMode":"super",
            "cursorEnabled":true,
            "cursorIntegration":"sdk",
            "codexPath":"custom-codex",
            "lyraProxy":"http://localhost:5678"
        }"#).unwrap();
        assert_eq!(settings.codex_path, "custom-codex");
        assert_eq!(settings.lyra_proxy, "http://localhost:5678");
        let saved = serde_json::to_value(settings).unwrap();
        assert!(saved.as_object().unwrap().keys().all(|key| !key.starts_with("cursor")));
    }

    #[test]
    fn meteor_defaults_and_configuration_round_trip() {
        for settings in [Settings::default(), serde_json::from_str::<Settings>(r#"{}"#).unwrap()] {
            assert!(settings.meteor_enabled);
            assert_eq!(settings.meteor_shortcut, "Alt+Alt");
            assert!(!settings.meteor_clipboard);
            assert_eq!(settings.meteor_paste_shortcut, "Alt+V");
        }
        let settings: Settings = serde_json::from_str(
            r#"{"meteorEnabled":false,"meteorShortcut":"Ctrl+Alt+M","meteorAgentKind":"codex","meteorModel":"gpt-5.6","meteorClipboard":true}"#,
        ).unwrap();
        assert!(settings.meteor_clipboard);
        let saved = serde_json::to_value(&settings).unwrap();
        assert_eq!(saved["meteorEnabled"], false);
        assert_eq!(saved["meteorShortcut"], "Ctrl+Alt+M");
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
    fn vega_and_external_backends_are_disabled_by_default() {
        let settings = Settings::default();
        assert!(!settings.vega_enabled);
        assert!(!settings.devin_enabled);
        assert!(!settings.codex_enabled);
        assert!(!settings.codebuddy_enabled);
    }

    #[test]
    fn vega_config_migrates_to_lyra() {
        let mut settings = Settings::default();
        settings.vega_enabled = true;
        settings.lyra_enabled = false;
        settings.scene_models.coding.push(ModelTarget {
            agent_kind: "alkaid".into(),
            model: "p/m".into(),
        });
        settings.model_favorites = vec!["alkaid:p/m".into()];
        settings.session_shortcuts.push(SessionShortcut {
            id: "1".into(),
            keys: "Ctrl+1".into(),
            action: "selectModel".into(),
            target: "alkaid:p/m".into(),
        });
        settings.migrate_legacy_vega();
        assert!(settings.lyra_enabled);
        assert!(!settings.vega_enabled);
        assert_eq!(settings.scene_models.coding[0].agent_kind, "lyra");
        assert_eq!(settings.model_favorites[0], "lyra:p/m");
        assert_eq!(settings.session_shortcuts[0].target, "lyra:p/m");
    }

    #[test]
    fn legacy_vega_switch_enables_lyra() {
        let settings: Settings = serde_json::from_str(r#"{"vegaEnabled":true}"#).unwrap();
        assert!(settings.vega_enabled);
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
    fn auto_change_project_is_enabled_by_default_and_round_trips() {
        let defaults = Settings::default();
        assert!(defaults.auto_change_project_enabled);
        let settings: Settings =
            serde_json::from_str(r#"{"autoChangeProjectEnabled":false}"#).unwrap();
        assert!(!settings.auto_change_project_enabled);
    }

    #[test]
    fn missing_history_display_mode_defaults_to_project() {
        let settings: Settings = serde_json::from_str(r#"{"theme":"ink-dark"}"#).unwrap();
        assert_eq!(settings.history_display_mode, "project");
    }

    #[test]
    fn legacy_chat_renderer_is_ignored() {
        let settings: Settings = serde_json::from_str(r#"{"chatViewRender":"dom"}"#).unwrap();
        let saved = serde_json::to_value(settings).unwrap();
        assert!(saved.get("chatViewRender").is_none());
    }

    #[test]
    fn sdk_integration_defaults_match_backend_policy() {
        let settings = Settings::default();
        assert_eq!(settings.codex_integration, "app-server");
        assert_eq!(settings.codebuddy_integration, "acp");
    }

    #[test]
    fn load_forces_integrations_to_backend_policy() {
        let dir = std::env::temp_dir().join(format!("nova-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("settings.json"),
            r#"{
                "codexIntegration":"sdk",
                "codebuddyIntegration":"sdk",
                "codebuddyplusEnabled":true
            }"#,
        )
        .unwrap();

        let settings = Settings::load(&dir);

        assert_eq!(settings.codex_integration, "app-server");
        assert_eq!(settings.codebuddy_integration, "acp");
        assert!(!settings.codebuddyplus_enabled);
        fs::remove_dir_all(dir).unwrap();
    }
}

/// `<agentKind>:<modelId>` / roam 编码键中，值为 alkaid 的段改写为 lyra；其余不变。
fn migrate_alkaid_key(key: &mut String) {
    if key.split(':').any(|seg| seg == "alkaid") {
        *key = key
            .split(':')
            .map(|seg| if seg == "alkaid" { "lyra" } else { seg })
            .collect::<Vec<_>>()
            .join(":");
    }
}

fn migrate_alkaid_keys(keys: &mut [String]) {
    for key in keys.iter_mut() {
        migrate_alkaid_key(key);
    }
}

fn migrate_alkaid_target(target: &mut String) {
    migrate_alkaid_key(target);
}

impl Settings {
    /// Vega 已移除：旧的开关与 alkaid 键统一迁移到 Lyra。
    fn migrate_legacy_vega(&mut self) {
        if self.vega_enabled {
            self.lyra_enabled = true;
            self.vega_enabled = false;
        }
        let scenes = &mut self.scene_models;
        for target in scenes.lightweight.iter_mut().chain(&mut scenes.coding).chain(&mut scenes.computer) {
            if target.agent_kind.trim() == "alkaid" {
                target.agent_kind = "lyra".into();
            }
        }
        migrate_alkaid_keys(&mut self.model_favorites);
        for shortcut in &mut self.session_shortcuts {
            migrate_alkaid_target(&mut shortcut.target);
        }
    }

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
        // 旧版单独的轻量级模型迁移为轻量化场景的第一项；Stage/Butterfly/Meteor 模型配置已移除。
        if settings.scene_models.lightweight.is_empty() {
            let legacy = |keys: [&str; 2]| raw_value.as_ref().and_then(|value| {
                keys.iter().find_map(|key| value.get(*key).and_then(serde_json::Value::as_str)).map(str::to_string)
            });
            if let Some(agent_kind) = legacy(["lightweightModelAgent", "titleModelAgent"]).filter(|kind| !kind.trim().is_empty()) {
                let model = legacy(["lightweightModel", "titleModel"]).unwrap_or_default();
                settings.scene_models.lightweight.push(ModelTarget { agent_kind, model });
            }
        }
        if settings.update_channel != "pre-release" {
            settings.update_channel = "release".into();
        }
        if settings.session_auto_cleanup_hours == 0 {
            settings.session_auto_cleanup_hours = legacy_days
                .map(|days| days.saturating_mul(24))
                .unwrap_or(24 * 30);
        }
        // 旧版把 SDK 暴露为独立 “+” 后端；升级后折叠为同一后端的接入方式。
        settings.codexplus_enabled = false;
        settings.codebuddyplus_enabled = false;
        // SDK bridge 已移除：CodeBuddy 固定走官方 HTTP 传输（`codebuddy --serve`
        // + /api/v1/acp）；Codex 固定走 app-server。
        settings.codex_integration = "app-server".into();
        settings.codebuddy_integration = "acp".into();
        // Vega 已移除：旧的开关与 alkaid 键统一迁移到 Lyra。
        settings.migrate_legacy_vega();
        settings
    }

    pub fn context_tools_enabled(&self) -> bool {
        self.context_retrieval_mode != ContextRetrievalMode::None
    }

    pub fn apply_context_retrieval_environment(&self) {
        std::env::set_var(
            "NOVA_CONTEXT_RETRIEVAL_MODE",
            self.context_retrieval_mode.as_str(),
        );
    }

    /// 把用户自定义环境变量写入当前进程，之后新启动的 agent 子进程继承。
    /// 先移除上一轮由 Nova 注入、本轮已被删除的变量，避免残留。
    pub fn apply_custom_env_vars(&self, previous: &std::collections::HashMap<String, String>) {
        for name in previous.keys() {
            if !self.custom_env_vars.contains_key(name) {
                std::env::remove_var(name);
            }
        }
        for (name, value) in &self.custom_env_vars {
            std::env::set_var(name, value);
        }
    }

    pub fn save(&self, dir: &PathBuf) {
        let _ = fs::create_dir_all(dir);
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(dir.join("settings.json"), json);
        }
    }
}
