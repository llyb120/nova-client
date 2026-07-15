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
    /// ACP 启动参数（空格分隔）。换成其他 ACP agent 时一并修改，
    /// 例如 Claude Agent: path=npx, args="-y @zed-industries/claude-code-acp"
    pub acp_args: String,
    /// Devin 代理地址（空 = 不覆盖环境变量；下同：注入 HTTP(S)_PROXY 等到该后端子进程）
    pub devin_proxy: String,
    /// CodeBuddy（腾讯云代码助手）ACP 可执行文件路径（默认 codebuddy，依赖 PATH）
    pub codebuddy_path: String,
    /// CodeBuddy ACP 启动参数。默认 --acp；未全局安装可改为 path=npx, args="-y @tencent-ai/codebuddy-code@latest --acp"
    pub codebuddy_args: String,
    /// CodeBuddy 代理地址
    pub codebuddy_proxy: String,
    /// Claude Code（@zed-industries/claude-code-acp）ACP 可执行文件路径（默认 npx，依赖 PATH）
    pub claudecode_path: String,
    /// Claude Code ACP 启动参数。默认用 npx 直接拉起：-y @zed-industries/claude-code-acp；
    /// 也可换成本地已安装的可执行文件。
    pub claudecode_args: String,
    /// Claude Code 代理地址
    pub claudecode_proxy: String,
    /// Cursor CLI（cursor-agent）可执行文件路径（默认 cursor-agent，依赖 PATH）
    pub cursor_path: String,
    /// Cursor ACP 启动参数（默认 acp）
    pub cursor_args: String,
    /// Cursor 代理地址
    pub cursor_proxy: String,
    /// OpenCode CLI 可执行文件路径（默认 opencode，依赖 PATH）
    pub opencode_path: String,
    /// OpenCode ACP 启动参数（默认 acp）
    pub opencode_args: String,
    /// OpenCode 代理地址
    pub opencode_proxy: String,
    /// Codex CLI 可执行文件路径（默认 codex，依赖 PATH）
    pub codex_path: String,
    /// Codex app-server 启动参数
    pub codex_args: String,
    /// Codex 代理地址（空 = 不覆盖环境变量）
    pub codex_proxy: String,
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
    /// 团队/漫游中转服务地址（空 = 关闭团队/漫游功能）
    pub relay_server: String,
    /// 团队/漫游身份 token（永久，用以区分每个人；空 = 不连接中转站）
    pub relay_token: String,
    /// 归属的群组（逗号/空格分隔，可多个）。只有相同群组的人才能在在线名单里看到彼此；
    /// 空 = 默认群组（与其他同样未配置群组的人互相可见，向后兼容）。
    pub relay_groups: String,
    /// 在团队里展示的名字（空 = 用机器名兜底）
    pub relay_name: String,
    /// 允许同团队成员借用的模型，键格式为 `<agentKind>:<modelId>`；空 = 不共享额度。
    pub quota_shared_models: Vec<String>,
    /// 是否启用各模型后端（仅影响前端可选性：关闭后不在新建/切换会话的后端列表里出现，
    /// 已存在的该后端历史会话仍可打开查看）
    pub devin_enabled: bool,
    pub codex_enabled: bool,
    pub codebuddy_enabled: bool,
    pub claudecode_enabled: bool,
    pub cursor_enabled: bool,
    pub opencode_enabled: bool,
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
            claudecode_path: "npx".into(),
            claudecode_args: "-y @zed-industries/claude-code-acp".into(),
            claudecode_proxy: String::new(),
            cursor_path: "cursor-agent".into(),
            cursor_args: "acp".into(),
            cursor_proxy: String::new(),
            opencode_path: "opencode".into(),
            opencode_args: "acp".into(),
            opencode_proxy: String::new(),
            codex_path: "codex".into(),
            codex_args: "app-server --stdio".into(),
            codex_proxy: String::new(),
            default_mode: String::new(),
            title_model_agent: "devin".into(),
            title_model: "swe-1-6".into(),
            share_model_agent: "devin".into(),
            share_model: "swe-1.6".into(),
            editor: "code".into(),
            theme: String::new(),
            relay_server: DEFAULT_RELAY_SERVER.into(),
            relay_token: String::new(),
            relay_groups: String::new(),
            relay_name: String::new(),
            quota_shared_models: Vec::new(),
            devin_enabled: true,
            codex_enabled: true,
            codebuddy_enabled: true,
            claudecode_enabled: true,
            cursor_enabled: true,
            opencode_enabled: true,
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
        if settings.claudecode_path.trim() == "npx"
            && settings.claudecode_args.trim() == "@zed-industries/claude-code-acp"
        {
            settings.claudecode_args = "-y @zed-industries/claude-code-acp".into();
        }
        if settings.session_auto_cleanup_hours == 0 {
            settings.session_auto_cleanup_hours = legacy_days
                .map(|days| days.saturating_mul(24))
                .unwrap_or(24 * 30);
        }
        settings
    }

    pub fn save(&self, dir: &PathBuf) {
        let _ = fs::create_dir_all(dir);
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(dir.join("settings.json"), json);
        }
    }
}
