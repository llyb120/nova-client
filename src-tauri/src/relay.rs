//! 团队分享 + 漫游模式的中转客户端。
//!
//! 连接关系：v2 WebSocket 单连接双向收发。
//! 身份用永久 token 区分（设置里配置）。
//!
//! 三种能力：
//! 1. 在线名单（presence）：谁在线、各自允许漫游的目录。
//! 2. 团队分享（share）：把一段对话快照直接发给指定的人，对方点角标接收。
//! 3. 漫游（roaming）：guest 在 host 机器上新建会话并驱动对话，真正的执行只在 host，
//!    guest 只接收展示。host 复用本地 acp/codex 管理器执行，所有产生的 update/turn/
//!    permission 事件由 lib.rs 的事件监听转发回 guest。
//!
//! 断线重连：WebSocket 断开后指数退避重连；每条定向消息有服务端分配的 seq，重连时带
//! since=<最后 seq> 补发漏掉的消息，保证不丢、不影响使用。

use crate::clues::{CaptureClueResult, ClueContextSnapshot, ClueNodeGroup, EV_CLUES};
use crate::credential_roaming::CredentialBundle;
use crate::settings::Settings;
use crate::threads::{now_ms, AgentKind, Item, PromptImage, Thread, Worktree};
use crate::AppState;
use base64::Engine;
use flate2::read::{DeflateDecoder, GzDecoder};
use flate2::write::GzEncoder;
use flate2::Compression;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Manager};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, Notify};
use tokio::time::{sleep, timeout, Duration, Instant};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use x25519_dalek::StaticSecret;

use crate::acp::{EV_PERMISSION, EV_PERMISSION_RESOLVED, EV_THREADS, EV_TURN, EV_UPDATE};

type RelayWsWriter = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

pub const EV_RELAY_STATUS: &str = "relay:status";
pub const EV_RELAY_PEERS: &str = "relay:peers";
pub const EV_RELAY_INBOX: &str = "relay:inbox";
/// guest 侧：收到对端（host）回传的可选模型/模式列表，前端据此缓存并在漫游时选用对方的模型
pub const EV_RELAY_PEER_MODELS: &str = "relay:peer-models";
/// guest 侧：收到对端某目录的本地分支列表，worktree「基于分支」下拉据此填充
pub const EV_RELAY_PEER_BRANCHES: &str = "relay:peer-branches";
/// host 侧：收到漫游请求，前端弹确认框
pub const EV_RELAY_ROAM_REQUEST: &str = "relay:roam-request";
/// 额度借用方：请求、安装 CLI、准备隔离凭证的进度。
pub const EV_RELAY_QUOTA_PROGRESS: &str = "relay:quota-progress";
/// 整段刷新某线程（漫游快照重同步后用），前端据此重新拉取 transcript
pub const EV_RELAY_RELOAD: &str = "acp:reload";

const QUOTA_CANCELLED_ERROR: &str = "额度漫游已取消";
const QUOTA_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const QUOTA_OPERATION_ACTIVE: u8 = 0;
const QUOTA_OPERATION_CANCELLED: u8 = 1;
const QUOTA_OPERATION_COMMITTED: u8 = 2;

/// host 侧：本机某线程正被哪个 guest 漫游驱动
#[derive(Clone)]
struct RoamGuest {
    token: String,
    guest_thread_id: String,
    approved_until: Instant,
}

/// host 侧：一条等待本机用户确认的漫游请求
#[derive(Clone)]
struct PendingRoam {
    from: String,
    from_name: String,
    guest_thread_id: String,
    folder: String,
    agent_kind: AgentKind,
    model: Option<String>,
    mode: Option<String>,
    clue_context: Option<ClueContextSnapshot>,
    /// guest 是否要求在 worktree 中执行（host 侧在自己仓库代建）
    worktree: bool,
    /// worktree 分支名（guest 手填）
    worktree_branch: Option<String>,
    /// worktree 基于哪个分支/提交创建（空 = host 仓库当前 HEAD）
    worktree_base: Option<String>,
    /// 审批时展示并允许 host 修改的提示词。
    prompt: Option<String>,
    /// 续期审批时已有的 host 会话；首次创建时为空。
    host_thread_id: Option<String>,
    images: Vec<PromptImage>,
}

const ROAM_AUTHORIZATION_DURATION: Duration = Duration::from_secs(30 * 60);

struct PendingQuotaClient {
    peer: String,
    agent_kind: AgentKind,
    secret: StaticSecret,
    reply: oneshot::Sender<Result<CredentialBundle, String>>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct QuotaLeaseKey {
    peer: String,
    agent_kind: AgentKind,
    auth_scope: String,
}

impl QuotaLeaseKey {
    fn new(peer: String, agent_kind: AgentKind, model: &str) -> Result<Self, String> {
        let peer = peer.trim().to_string();
        if peer.is_empty() {
            return Err("额度租约必须指定队友".into());
        }
        let auth_scope = if matches!(agent_kind, AgentKind::OpenCode | AgentKind::OpenCodePlus) {
            model.split('/').next().unwrap_or_default().to_string()
        } else {
            String::new()
        };
        if matches!(agent_kind, AgentKind::OpenCode | AgentKind::OpenCodePlus)
            && auth_scope.is_empty()
        {
            return Err("OpenCode 额度租约缺少 Provider 标识".into());
        }
        Ok(Self {
            peer,
            agent_kind,
            auth_scope,
        })
    }
}

type QuotaLeaseResult = Result<CredentialBundle, String>;
type QuotaLeaseWaiter = oneshot::Sender<QuotaLeaseResult>;

struct QuotaOperation {
    state: AtomicU8,
    notify: Notify,
}

impl QuotaOperation {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(QUOTA_OPERATION_ACTIVE),
            notify: Notify::new(),
        }
    }

    fn cancel(&self) -> bool {
        if self
            .state
            .compare_exchange(
                QUOTA_OPERATION_ACTIVE,
                QUOTA_OPERATION_CANCELLED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            return false;
        }
        self.notify.notify_one();
        true
    }

    fn commit(&self) -> bool {
        self.state
            .compare_exchange(
                QUOTA_OPERATION_ACTIVE,
                QUOTA_OPERATION_COMMITTED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    fn ensure_active(&self) -> Result<(), String> {
        if self.state.load(Ordering::SeqCst) == QUOTA_OPERATION_ACTIVE {
            Ok(())
        } else {
            Err(QUOTA_CANCELLED_ERROR.into())
        }
    }

    async fn cancelled(&self) {
        if self.state.load(Ordering::SeqCst) != QUOTA_OPERATION_ACTIVE {
            return;
        }
        self.notify.notified().await;
    }
}

fn quota_model_key(kind: &AgentKind, model: &str) -> String {
    format!("{}:{model}", kind.as_str())
}

fn ensure_quota_backend_supported(kind: &AgentKind) -> Result<(), String> {
    match kind {
        AgentKind::Devin
        | AgentKind::Codex
        | AgentKind::CodexPlus
        | AgentKind::CodeBuddy
        | AgentKind::CodeBuddyPlus
        | AgentKind::ClaudeCode
        | AgentKind::Cursor
        | AgentKind::OpenCode
        | AgentKind::OpenCodePlus => Ok(()),
    }
}

fn shared_quota_model_keys(shared_options: &Value) -> HashSet<String> {
    let mut shared = HashSet::new();
    let Some(backends) = shared_options.as_object() else {
        return shared;
    };
    for (kind, options) in backends {
        let model_options = options["configOptions"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item["id"].as_str() == Some("model"))
            })
            .and_then(|item| item["options"].as_array());
        let Some(model_options) = model_options else {
            continue;
        };
        for model in model_options {
            if let Some(value) = model["value"].as_str().filter(|value| !value.is_empty()) {
                shared.insert(format!("{kind}:{value}"));
            }
        }
    }
    shared
}

fn quota_model_is_shared(settings: &Settings, kind: &AgentKind, model: &str) -> bool {
    let enabled = match kind {
        AgentKind::Devin => settings.devin_enabled,
        AgentKind::Codex => settings.codex_enabled,
        AgentKind::CodexPlus => settings.codex_enabled,
        AgentKind::CodeBuddy => settings.codebuddy_enabled,
        AgentKind::CodeBuddyPlus => settings.codebuddy_enabled,
        AgentKind::ClaudeCode => settings.claudecode_enabled,
        AgentKind::Cursor => settings.cursor_enabled,
        AgentKind::OpenCode => settings.opencode_enabled,
        AgentKind::OpenCodePlus => settings.opencode_enabled,
    };
    enabled
        && !model.is_empty()
        && settings
            .quota_shared_models
            .contains(&quota_model_key(kind, model))
}

fn shared_model_options(
    kind: &AgentKind,
    value: &Value,
    shared: &HashSet<String>,
) -> Option<Value> {
    let mut filtered = value.clone();
    let config_options = filtered.get_mut("configOptions")?.as_array_mut()?;
    let model_option = config_options
        .iter_mut()
        .find(|option| option.get("id").and_then(Value::as_str) == Some("model"))?;
    let current = model_option
        .get("currentValue")
        .and_then(Value::as_str)
        .map(str::to_string);
    let options = model_option.get_mut("options")?.as_array_mut()?;
    options.retain(|option| {
        option
            .get("value")
            .and_then(Value::as_str)
            .is_some_and(|model| shared.contains(&quota_model_key(kind, model)))
    });
    let first = options
        .first()
        .and_then(|option| option.get("value"))
        .and_then(Value::as_str)
        .map(str::to_string)?;
    if !current
        .as_deref()
        .is_some_and(|model| shared.contains(&quota_model_key(kind, model)))
    {
        model_option["currentValue"] = json!(first);
    }
    Some(filtered)
}

fn is_publishable_roaming_path(path: &str) -> bool {
    !path.contains(crate::SCRATCH_MARK)
}

fn is_allowed_roaming_path(allowed: &[String], path: &str) -> bool {
    is_publishable_roaming_path(path)
        && std::path::Path::new(path).is_dir()
        && allowed.iter().any(|folder| folder == path)
}

fn host_prompt_is_current(prompt_epoch: &(Arc<AtomicU64>, u64)) -> bool {
    prompt_epoch.0.load(Ordering::SeqCst) == prompt_epoch.1
}

/// 一条待接收的分享
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Share {
    pub id: String,
    pub from: String,
    pub from_name: String,
    pub title: String,
    pub agent_kind: AgentKind,
    pub items: Value,
    #[serde(default)]
    pub plan: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_clue_card_id: Option<String>,
    /// 漫游召回自动回传的快照（前端标注「召回」并自动弹出收件箱）
    #[serde(default)]
    pub recall: bool,
    pub ts: i64,
}

fn accepted_share_mode(recall: bool, default_mode: &str) -> Option<String> {
    recall
        .then(|| default_mode.to_string())
        .filter(|mode| !mode.is_empty())
}

#[derive(Deserialize)]
struct InEnvelope {
    #[serde(default)]
    seq: i64,
    #[serde(default)]
    from: String,
    #[serde(default, rename = "fromName")]
    from_name: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    data: Value,
}

#[derive(Deserialize)]
struct RelayClueList {
    groups: Vec<ClueNodeGroup>,
}

#[derive(Deserialize)]
struct RelayClueAssociate {
    group: ClueNodeGroup,
}

pub struct RelayManager {
    pub app: AppHandle,
    config_dir: PathBuf,
    http: reqwest::Client,
    device_id: String,
    connected: AtomicBool,
    /// v2 WebSocket 是否已经就绪。
    protocol_v2: AtomicBool,
    /// 本次客户端进程的协议纪元，配合 messageId 识别重试和进程重启。
    epoch: String,
    /// 最近一次 v2 服务端纪元；服务端重启后游标自动归零重放。
    server_epoch: StdMutex<String>,
    /// v2 WebSocket 写半边；读循环独立运行并完成 ACK waiter。
    ws_writer: tokio::sync::Mutex<Option<RelayWsWriter>>,
    pending_ws_acks: StdMutex<HashMap<String, oneshot::Sender<Result<Value, String>>>>,
    v2_ready: Notify,
    last_seq: AtomicI64,
    last_persist: StdMutex<Instant>,
    peers: StdMutex<Value>,
    inbox: StdMutex<Vec<Share>>,
    /// host 侧：hostThreadId -> guest
    hosted: StdMutex<HashMap<String, RoamGuest>>,
    /// host 侧漫游提示词的取消代次。收到 cancel 时递增；尚未真正进入 manager 的旧任务
    /// 会在启动前发现代次变化并退出，避免 prompt/cancel 连续到达时 cancel 抢先判定未运行。
    host_prompt_epochs: StdMutex<HashMap<String, Arc<AtomicU64>>>,
    /// guest 侧：requestKey -> host token（权限响应回传路由）
    guest_perms: StdMutex<HashMap<String, String>>,
    /// guest 侧正在运行的线程（host 不在本机，managers 不知道运行态）
    guest_running: StdMutex<HashSet<String>>,
    /// guest 侧：reqId -> 对应的本地 guest 线程 id（等 host 回执后回填 remote id）
    pending_creates: StdMutex<HashMap<String, String>>,
    /// guest 侧：host 还没建好会话前用户就发的提示词，建好后按序补发
    pending_prompts: StdMutex<HashMap<String, Vec<(String, Vec<PromptImage>)>>>,
    /// host 侧：reqId -> 待本机用户确认的漫游请求
    incoming_roams: StdMutex<HashMap<String, PendingRoam>>,
    /// 借用方：reqId -> 一次性私钥与等待中的 Tauri 调用。
    pending_quota: StdMutex<HashMap<String, PendingQuotaClient>>,
    /// 借用方：一次额度漫游从安装、请求凭证到落库的取消状态。
    quota_operations: StdMutex<HashMap<String, Arc<QuotaOperation>>>,
    /// 借用方：共享模型的应用级内存租约；每个会话仍各自创建隔离运行目录。
    quota_leases: StdMutex<HashMap<QuotaLeaseKey, CredentialBundle>>,
    /// 同一租约同时预热/创建时只向对端请求一次，其余调用等待同一结果。
    quota_lease_flights: StdMutex<HashMap<QuotaLeaseKey, Vec<QuotaLeaseWaiter>>>,
    /// 高级分享：本机处理线程 id -> 处理完成后要分享给谁
    advanced: StdMutex<HashMap<String, String>>,
    /// guest 侧落盘节流：流式期间不要每条增量都写整个 store（会拖慢 WebSocket 消费）
    last_store_save: StdMutex<Instant>,
    /// guest 侧每个漫游会话最近一次收到 host 事件的时间，看门狗据此判断是否卡住
    guest_activity: StdMutex<HashMap<String, Instant>>,
    /// host 侧：每个漫游会话(按 guestThreadId)的出站 roaming.update 单调序号。
    /// guest 据此去重(重传幂等)与检测缺口(丢消息立即重同步)。
    out_seq: StdMutex<HashMap<String, i64>>,
    /// guest 侧：每个漫游会话已应用到的最大 update 序号，用于去重/缺口检测。
    in_seq: StdMutex<HashMap<String, i64>>,
    /// guest 侧：每个漫游会话最近一次主动请求重同步的时间，做节流避免缺口风暴。
    last_resync: StdMutex<HashMap<String, Instant>>,
    /// 顺序发送队列：所有出站消息走单一 worker，保证 host->guest 的流式增量按序到达
    out_tx: StdMutex<Option<mpsc::UnboundedSender<(String, String, Value)>>>,
    loop_handle: StdMutex<Option<JoinHandle<()>>>,
}

impl RelayManager {
    pub fn new(app: AppHandle, config_dir: PathBuf) -> Arc<Self> {
        let (last_seq, server_epoch) = read_relay_state(&config_dir);
        let inbox = read_inbox(&config_dir);
        let device_id = read_or_create_device_id(&config_dir);
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            // HTTP 客户端用于 v2 REST 辅助接口；长连接存活由 WebSocket Ping/Pong 检测。
            .tcp_keepalive(Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let mgr = Arc::new(RelayManager {
            app,
            config_dir,
            http,
            device_id,
            connected: AtomicBool::new(false),
            protocol_v2: AtomicBool::new(false),
            epoch: uuid::Uuid::new_v4().to_string(),
            server_epoch: StdMutex::new(server_epoch),
            ws_writer: tokio::sync::Mutex::new(None),
            pending_ws_acks: StdMutex::new(HashMap::new()),
            v2_ready: Notify::new(),
            last_seq: AtomicI64::new(last_seq),
            last_persist: StdMutex::new(Instant::now()),
            peers: StdMutex::new(json!([])),
            inbox: StdMutex::new(inbox),
            hosted: StdMutex::new(HashMap::new()),
            host_prompt_epochs: StdMutex::new(HashMap::new()),
            guest_perms: StdMutex::new(HashMap::new()),
            guest_running: StdMutex::new(HashSet::new()),
            pending_creates: StdMutex::new(HashMap::new()),
            pending_prompts: StdMutex::new(HashMap::new()),
            incoming_roams: StdMutex::new(HashMap::new()),
            pending_quota: StdMutex::new(HashMap::new()),
            quota_operations: StdMutex::new(HashMap::new()),
            quota_leases: StdMutex::new(HashMap::new()),
            quota_lease_flights: StdMutex::new(HashMap::new()),
            advanced: StdMutex::new(HashMap::new()),
            last_store_save: StdMutex::new(Instant::now()),
            guest_activity: StdMutex::new(HashMap::new()),
            out_seq: StdMutex::new(HashMap::new()),
            in_seq: StdMutex::new(HashMap::new()),
            last_resync: StdMutex::new(HashMap::new()),
            out_tx: StdMutex::new(Some(out_tx)),
            loop_handle: StdMutex::new(None),
        });
        // 出站 worker：FIFO 顺序发送保证 host->guest 增量按序；同时攒一个很短的窗口
        // 把连续的 roaming.update 合并成一条（ops 数组）批量发送，大幅减少 HTTP 往返，
        // 解决「宿主早执行完、接收方还在卡」的问题。
        let worker = mgr.clone();
        tauri::async_runtime::spawn(async move {
            let mut rx = out_rx;
            while let Some(first) = rx.recv().await {
                // 排空当前已积压的消息（非阻塞），不做人为等待——首字/低频零额外延迟；
                // 高负载时一次发送耗时约一个 RTT，期间自然积压，下一轮一次性合批发出。
                let mut batch = vec![first];
                while batch.len() < 512 {
                    match rx.try_recv() {
                        Ok(m) => batch.push(m),
                        Err(_) => break,
                    }
                }
                for (to, kind, mut data) in coalesce_outbound(batch) {
                    // 在最终合并后的出站消息上分配每会话单调序号：roaming.update 自增，
                    // roaming.snapshot 标记当前序号作为基准（guest 收到快照后从此续号）。
                    worker.assign_out_seq(&kind, &mut data);
                    worker.send_with_retry(&to, &kind, data).await;
                }
            }
        });
        // guest 看门狗：进行中的漫游会话每隔几秒重同步一次，自愈中途卡顿/丢失/卡 loading
        let snapper = mgr.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                sleep(Duration::from_secs(3)).await;
                snapper.resync_running_guests();
            }
        });
        mgr
    }

    /// 把一条出站消息放进顺序队列
    fn enqueue(&self, to: String, kind: &str, data: Value) {
        if let Some(tx) = self.out_tx.lock().unwrap().as_ref() {
            let _ = tx.send((to, kind.to_string(), data));
        }
    }

    fn cfg(&self) -> Option<(String, String, String)> {
        let state = self.app.state::<AppState>();
        let s = state.settings.lock().unwrap();
        let token = s.relay_token.trim().to_string();
        // token 是总开关：填了 token 就启用；server 留空回退到默认地址。
        if token.is_empty() {
            return None;
        }
        let server = resolve_relay_server(&s.relay_server);
        // 无默认地址时必须显式填写 server，否则不启中转。
        if server.is_empty() {
            return None;
        }
        let name = relay_display_name(&s);
        Some((server, token, name))
    }

    /// 当前归属的群组（去重后的规范化 csv）。随每次连接/发送上报服务端，
    /// 服务端据此把在线名单按群组隔离——只有同群组的人才能看到彼此。
    fn groups_csv(&self) -> String {
        let state = self.app.state::<AppState>();
        let s = state.settings.lock().unwrap();
        normalize_groups_csv(&s.relay_groups)
    }

    pub fn enabled(&self) -> bool {
        self.cfg().is_some()
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn status(&self) -> Value {
        json!({
            "enabled": self.enabled(),
            "connected": self.connected.load(Ordering::SeqCst),
        })
    }

    pub fn peers(&self) -> Value {
        self.peers.lock().unwrap().clone()
    }

    /// 主动从服务端拉取一次在线名单，更新缓存并通知前端。
    /// 前端定时器用它做「兜底刷新」——定时器平时读的是本地 presence 缓存，
    /// 一旦某次 presence 推送丢失，名单会长时间停在旧状态；这里直接查服务端 roster，
    /// 不依赖 WebSocket 单次推送，既能自愈丢失的 presence，也能在长连接尚未连上时先显示在线名单。
    pub async fn refresh_peers(&self) {
        let Some((server, token, name)) = self.cfg() else {
            *self.peers.lock().unwrap() = json!([]);
            let _ = self.app.emit(EV_RELAY_PEERS, json!([]));
            return;
        };
        let resp = self
            .http
            .get(format!("{server}/v2/peers"))
            .header("Authorization", format!("Bearer {token}"))
            .header("X-Relay-Name-Encoded", urlencode(&name))
            .header("X-Relay-Groups-Encoded", urlencode(&self.groups_csv()))
            .header("X-Relay-Device", &self.device_id)
            .timeout(Duration::from_secs(15))
            .send()
            .await;
        let Ok(resp) = resp else { return };
        if !resp.status().is_success() {
            return;
        }
        let Ok(body) = resp.json::<Value>().await else {
            return;
        };
        // 保留服务端返回的完整名单（含自己）：天线在线名单要显示自己（前端标注「我」）。
        // 漫游/分享侧各自会用自己的 token 排除自己，不受此影响。
        let peers = relay_display_peers(body.get("peers").cloned().unwrap_or(json!([])));
        self.retain_online_quota_leases(&peers);
        *self.peers.lock().unwrap() = peers.clone();
        let _ = self.app.emit(EV_RELAY_PEERS, peers);
    }

    pub fn inbox_list(&self) -> Vec<Share> {
        self.inbox.lock().unwrap().clone()
    }

    fn emit_status(&self) {
        let _ = self.app.emit(EV_RELAY_STATUS, self.status());
    }

    fn clear_quota_leases(&self) {
        self.quota_leases.lock().unwrap().clear();
    }

    fn invalidate_quota_leases_for_peer(&self, peer: &str) {
        self.quota_leases
            .lock()
            .unwrap()
            .retain(|key, _| key.peer != peer);
    }

    fn retain_online_quota_leases(&self, peers: &Value) {
        let online: HashSet<&str> = peers
            .as_array()
            .into_iter()
            .flatten()
            .filter(|peer| peer["online"].as_bool().unwrap_or(false))
            .filter_map(|peer| peer["token"].as_str())
            .collect();
        self.quota_leases
            .lock()
            .unwrap()
            .retain(|key, _| online.contains(key.peer.as_str()));
    }

    fn retain_shared_quota_leases(&self, peer: &str, shared_options: &Value) {
        let shared = shared_quota_model_keys(shared_options);
        self.quota_leases.lock().unwrap().retain(|key, _| {
            if key.peer != peer {
                return true;
            }
            let prefix = format!("{}:", key.agent_kind.as_str());
            shared.iter().any(|model| {
                let Some(model) = model.strip_prefix(&prefix) else {
                    return false;
                };
                key.auth_scope.is_empty()
                    || model == key.auth_scope
                    || model.starts_with(&format!("{}/", key.auth_scope))
            })
        });
    }

    fn set_connected(&self, on: bool) {
        let prev = self.connected.swap(on, Ordering::SeqCst);
        if prev && !on {
            self.clear_quota_leases();
        }
        if prev != on {
            self.emit_status();
        }
    }

    /// 启动/重启连接（设置变化或首启时调用）
    pub fn restart(self: &Arc<Self>) {
        if let Some(h) = self.loop_handle.lock().unwrap().take() {
            h.abort();
        }
        self.set_connected(false);
        self.emit_status();
        let me = self.clone();
        let handle = tauri::async_runtime::spawn(async move {
            me.run_loop().await;
        });
        *self.loop_handle.lock().unwrap() = Some(handle);
    }

    async fn run_loop(self: Arc<Self>) {
        let mut backoff = Duration::from_secs(1);
        loop {
            let Some((server, token, name)) = self.cfg() else {
                self.set_connected(false);
                return; // 未配置：等设置变更后由 restart 重新拉起
            };
            // 连上后先上报漫游目录
            let result = self.connect_once(&server, &token, &name).await;
            // connect_once 只有在收到成功响应头后才会置 connected=true。只要本轮曾经
            // 连上过，后续断线就应从 1s 重新退避；否则启动阶段的历史失败会让一条已稳定
            // 很久的连接断开后仍等待最多 30s，叠加读侧断线检测后看起来像没有重连。
            let was_connected = self.connected.load(Ordering::SeqCst);
            self.set_connected(false);
            self.persist_seq(true);
            if was_connected {
                backoff = Duration::from_secs(1);
            }
            // 退避重连（带抖动）
            let jitter = Duration::from_millis((now_ms() % 500) as u64);
            let delay = backoff + jitter;
            match result {
                Ok(_) => self.log(format!(
                    "[relay] 连接已关闭，{:.1}s 后重连",
                    delay.as_secs_f32()
                )),
                Err(e) => self.log(format!(
                    "[relay] 连接断开：{e}；{:.1}s 后重连",
                    delay.as_secs_f32()
                )),
            }
            sleep(delay).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }

    async fn clear_v2(&self, reason: &str) {
        self.protocol_v2.store(false, Ordering::SeqCst);
        self.ws_writer.lock().await.take();
        let pending: Vec<oneshot::Sender<Result<Value, String>>> = self
            .pending_ws_acks
            .lock()
            .unwrap()
            .drain()
            .map(|(_, tx)| tx)
            .collect();
        for tx in pending {
            let _ = tx.send(Err(reason.to_string()));
        }
    }

    async fn connect_once(&self, server: &str, token: &str, name: &str) -> Result<(), String> {
        self.clear_v2("v2 连接正在重建").await;
        let since = self.last_seq.load(Ordering::SeqCst);
        let previous_epoch = self.server_epoch.lock().unwrap().clone();
        let url = websocket_url(server, since, &previous_epoch)?;
        let mut request = url.into_client_request().map_err(|e| e.to_string())?;
        let headers = request.headers_mut();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|e| e.to_string())?,
        );
        headers.insert(
            "X-Relay-Name-Encoded",
            HeaderValue::from_str(&urlencode(name)).map_err(|e| e.to_string())?,
        );
        headers.insert(
            "X-Relay-Groups-Encoded",
            HeaderValue::from_str(&urlencode(&self.groups_csv())).map_err(|e| e.to_string())?,
        );
        headers.insert(
            "X-Relay-Device",
            HeaderValue::from_str(&self.device_id).map_err(|e| e.to_string())?,
        );
        headers.insert(
            "Sec-WebSocket-Protocol",
            HeaderValue::from_static("nova.v2"),
        );

        let (socket, _) = timeout(Duration::from_secs(20), connect_async(request))
            .await
            .map_err(|_| "建立 v2 WebSocket 超时（20s）".to_string())?
            .map_err(|e| e.to_string())?;
        let (writer, mut reader) = socket.split();
        *self.ws_writer.lock().await = Some(writer);
        self.protocol_v2.store(true, Ordering::SeqCst);
        self.v2_ready.notify_waiters();
        self.set_connected(true);
        self.log("[relay] 已通过 v2 WebSocket 连接中转站".into());
        self.publish_folders();
        self.rebuild_hosted();
        self.resync_guest_threads();

        let result = loop {
            let incoming = match timeout(Duration::from_secs(40), reader.next()).await {
                Ok(Some(Ok(message))) => message,
                Ok(Some(Err(error))) => break Err(error.to_string()),
                Ok(None) => break Ok(()),
                Err(_) => break Err("v2 接收空闲超时（40s 无心跳/数据）".into()),
            };
            match incoming {
                Message::Text(text) => self.on_v2_frame(text.as_str()),
                Message::Binary(bytes) => {
                    if let Some(text) = decode_v2_binary(bytes.as_ref()) {
                        self.on_v2_frame(&text);
                    }
                }
                Message::Ping(payload) => {
                    let mut guard = self.ws_writer.lock().await;
                    let Some(writer) = guard.as_mut() else {
                        break Err("v2 写通道已关闭".into());
                    };
                    if let Err(error) = writer.send(Message::Pong(payload)).await {
                        break Err(error.to_string());
                    }
                }
                Message::Pong(_) => {}
                Message::Close(_) => break Ok(()),
                _ => {}
            }
        };
        self.clear_v2("v2 连接已断开").await;
        result
    }

    fn on_v2_frame(&self, text: &str) {
        let Ok(frame) = serde_json::from_str::<Value>(text) else {
            return;
        };
        match frame["op"].as_str().unwrap_or_default() {
            "ready" => {
                if let Some(last_seq) = frame["lastSeq"].as_i64() {
                    // 服务端可能因纪元变化或游标越界把 since 夹回 0；必须跟随其实际基准。
                    self.last_seq.store(last_seq, Ordering::SeqCst);
                }
                if let Some(epoch) = frame["serverEpoch"].as_str() {
                    let mut current = self.server_epoch.lock().unwrap();
                    if !current.is_empty() && *current != epoch {
                        self.log("[relay] 检测到 v2 服务端新纪元，已自动重放有效缓冲".into());
                    }
                    *current = epoch.to_string();
                    drop(current);
                    self.persist_seq(true);
                }
            }
            "ack" => {
                let Some(message_id) = frame["messageId"].as_str() else {
                    return;
                };
                let waiter = self.pending_ws_acks.lock().unwrap().remove(message_id);
                if let Some(waiter) = waiter {
                    let result = if let Some(error) = frame["error"].as_str() {
                        Err(error.to_string())
                    } else {
                        Ok(frame)
                    };
                    let _ = waiter.send(result);
                }
            }
            "event" => {
                let Ok(mut env) = serde_json::from_value::<InEnvelope>(frame["event"].clone())
                else {
                    return;
                };
                env.from_name = relay_sender_name(&env.from, &env.from_name);
                maybe_decompress(&mut env.data);
                if env.seq > 0 {
                    self.last_seq.store(env.seq, Ordering::SeqCst);
                    self.persist_seq(false);
                }
                self.dispatch(env);
            }
            "error" => {
                if let Some(error) = frame["error"].as_str() {
                    self.log(format!("[relay] v2 协议错误：{error}"));
                }
            }
            _ => {}
        }
    }

    fn dispatch(&self, env: InEnvelope) {
        match env.kind.as_str() {
            "presence" => {
                // 保留服务端返回的完整名单（含自己）：天线在线名单要显示自己（前端标注「我」）。
                // 漫游/分享侧各自会用自己的 token 排除自己，不受此影响。
                let peers =
                    relay_display_peers(env.data.get("peers").cloned().unwrap_or(json!([])));
                self.retain_online_quota_leases(&peers);
                *self.peers.lock().unwrap() = peers.clone();
                let _ = self.app.emit(EV_RELAY_PEERS, peers);
            }
            "clues.changed" => {
                let _ = self.app.emit(EV_CLUES, env.data);
            }
            "clue.mentioned" => self.on_clue_mentioned(&env),
            "share" => self.on_share(&env),
            // guest -> host
            "roaming.create" => self.on_roaming_create(&env),
            "roaming.prompt" => self.on_roaming_prompt(&env),
            "roaming.cancel" => self.on_roaming_cancel(&env),
            "roaming.permission_response" => self.on_roaming_permission_response(&env),
            "roaming.config" => self.on_roaming_config(&env),
            "roaming.resync" => self.on_roaming_resync(&env),
            "roaming.recall" => self.on_roaming_recall(&env),
            "roaming.models_changed" => self.on_roaming_models_changed(&env),
            "roaming.models_request" => self.on_roaming_models_request(&env),
            "roaming.branches_request" => self.on_roaming_branches_request(&env),
            "quota.request" => self.on_quota_request(&env),
            // host -> guest
            "roaming.created" => self.on_roaming_created(&env),
            "roaming.models" => self.on_roaming_models(&env),
            "roaming.branches" => self.on_roaming_branches(&env),
            "roaming.update" => self.on_roaming_update(&env),
            "roaming.turn" => self.on_roaming_turn(&env),
            "roaming.permission" => self.on_roaming_permission(&env),
            "roaming.permission_resolved" => self.on_roaming_permission_resolved(&env),
            "roaming.snapshot" => self.on_roaming_snapshot(&env),
            "roaming.title" => self.on_roaming_title(&env),
            "roaming.error" => self.on_roaming_error(&env),
            "quota.granted" => self.on_quota_granted(&env),
            "quota.rejected" => self.on_quota_rejected(&env),
            // 数字员工跨机讨论：队友员工来咨询 / 对我方咨询的回复
            "employee.discuss" => {
                crate::employees::on_remote_discuss(&self.app, &env.from, &env.from_name, env.data)
            }
            "employee.discuss.reply" => {
                crate::employees::on_remote_discuss_reply(&self.app, env.data)
            }
            _ => {}
        }
    }

    fn on_clue_mentioned(&self, env: &InEnvelope) {
        let card_id = env.data["cardId"].as_str().unwrap_or_default();
        if card_id.is_empty() {
            return;
        }
        let _ = self
            .app
            .emit("clues:mentioned", json!({ "cardId": card_id }));
        crate::sys_notify::notify_clue_mention(
            &self.app,
            card_id,
            if env.from_name.trim().is_empty() {
                "队友"
            } else {
                &env.from_name
            },
            env.data["title"].as_str().unwrap_or("未命名线索"),
            env.data["kind"].as_str().unwrap_or("publish"),
            env.data["content"].as_str().unwrap_or_default(),
            crate::clues::EV_CLUE_MENTION_OPEN,
        );
    }

    // ===== 发送 =====

    /// 异步投递一条定向消息（进顺序队列，失败仅记日志）
    pub fn spawn_send(self: &Arc<Self>, to: String, kind: &str, data: Value) {
        self.enqueue(to, kind, data);
    }

    /// host 侧：给最终出站的 roaming.update/snapshot 打上每会话单调序号。
    /// 序号让 guest 能安全去重(重传幂等、不会把 delta 追加两次)并检测缺口(丢消息立即重同步)。
    fn assign_out_seq(&self, kind: &str, data: &mut Value) {
        let Some(tid) = data
            .get("guestThreadId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
        else {
            return;
        };
        match kind {
            "roaming.update" => {
                let mut map = self.out_seq.lock().unwrap();
                let e = map.entry(tid).or_insert(0);
                *e += 1;
                data["seq"] = json!(*e);
            }
            // 快照是「全量真相」：带上当前序号作为基准，guest 应用后从此续号，
            // 之后较低序号的迟到/重传增量都会被丢弃。
            "roaming.snapshot" => {
                let cur = self.out_seq.lock().unwrap().get(&tid).copied().unwrap_or(0);
                data["seq"] = json!(cur);
            }
            _ => {}
        }
    }

    /// 出站发送 + 瞬时失败重传。序号去重保证重传幂等，
    /// 因此丢消息(网络抖动)不再造成「思考残缺/命令卡 loading/任务卡 loading」。
    async fn send_with_retry(&self, to: &str, kind: &str, data: Value) {
        let message_id = uuid::Uuid::new_v4().to_string();
        let mut attempt = 0u32;
        loop {
            match self.send_once(&message_id, to, kind, data.clone()).await {
                Ok(_) => return,
                Err(e) => {
                    attempt += 1;
                    if attempt >= 3 {
                        self.log(format!(
                            "[relay] 发送 {kind} 失败（重试{attempt}次后放弃）：{e}"
                        ));
                        return; // 彻底失败：靠 guest 缺口检测/看门狗重同步兜底
                    }
                    sleep(Duration::from_millis(120 * attempt as u64)).await;
                }
            }
        }
    }

    pub async fn send(&self, to: &str, kind: &str, data: Value) -> Result<Value, String> {
        let message_id = uuid::Uuid::new_v4().to_string();
        self.send_once(&message_id, to, kind, data).await
    }

    async fn wait_v2_ready(&self) -> Result<(), String> {
        if self.protocol_v2.load(Ordering::SeqCst) {
            return Ok(());
        }
        let notified = self.v2_ready.notified();
        // 注册 waiter 后再次检查，避免连接恰好在两次操作之间完成导致丢通知。
        if self.protocol_v2.load(Ordering::SeqCst) {
            return Ok(());
        }
        timeout(Duration::from_secs(30), notified)
            .await
            .map_err(|_| "等待 v2 WebSocket 连接恢复超时".to_string())?;
        if self.protocol_v2.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err("v2 WebSocket 尚未连接".into())
        }
    }

    async fn send_once(
        &self,
        message_id: &str,
        to: &str,
        kind: &str,
        data: Value,
    ) -> Result<Value, String> {
        self.wait_v2_ready().await?;
        self.send_v2(message_id, to, kind, data).await
    }

    async fn send_v2(
        &self,
        message_id: &str,
        to: &str,
        kind: &str,
        data: Value,
    ) -> Result<Value, String> {
        let thread_id = relay_thread_id(&data);
        let (reply, wait) = oneshot::channel();
        let frame = json!({
            "version": 2,
            "op": "send",
            "messageId": message_id,
            "epoch": &self.epoch,
            "threadId": thread_id,
            "to": to,
            "type": kind,
            "data": data,
        });
        let encoded = serde_json::to_vec(&frame).map_err(|e| e.to_string())?;
        let outbound = if encoded.len() >= 1024 {
            Message::Binary(gzip_json(&frame)?.into())
        } else {
            let text = String::from_utf8(encoded).map_err(|e| e.to_string())?;
            Message::Text(text.into())
        };
        self.pending_ws_acks
            .lock()
            .unwrap()
            .insert(message_id.to_string(), reply);
        let send_result = {
            let mut guard = self.ws_writer.lock().await;
            let Some(writer) = guard.as_mut() else {
                self.pending_ws_acks.lock().unwrap().remove(message_id);
                return Err("v2 WebSocket 尚未连接".into());
            };
            writer.send(outbound).await
        };
        if let Err(error) = send_result {
            self.pending_ws_acks.lock().unwrap().remove(message_id);
            return Err(error.to_string());
        }
        match timeout(Duration::from_secs(30), wait).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("v2 ACK 通道已关闭".into()),
            Err(_) => {
                self.pending_ws_acks.lock().unwrap().remove(message_id);
                Err("等待 v2 ACK 超时".into())
            }
        }
    }

    // ---- 共享协作账本（跨机器去重/互斥/接力，仲裁在中转站）----

    /// 原子认领一个单子。返回 (outcome, mark)：outcome ∈ acquired|taken|done。
    pub async fn ledger_claim(
        &self,
        scope: &str,
        key: &str,
        title: &str,
        owner: &str,
        owner_name: &str,
        ttl_ms: i64,
    ) -> Result<(String, Value), String> {
        let (server, token, name) = self.cfg().ok_or("未配置中转站 token")?;
        let body = gzip_json(&json!({
            "scope": scope, "key": key, "title": title,
            "owner": owner, "ownerName": owner_name, "ttlMs": ttl_ms,
        }))?;
        let resp = self
            .http
            .post(format!("{server}/v2/ledger/claim"))
            .header("Authorization", format!("Bearer {token}"))
            .header("X-Relay-Name-Encoded", urlencode(&name))
            .header("X-Relay-Groups-Encoded", urlencode(&self.groups_csv()))
            .header("X-Relay-Device", &self.device_id)
            .header("Content-Type", "application/json")
            .header("Content-Encoding", "gzip")
            .body(body)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        let v: Value = resp.json().await.map_err(|e| e.to_string())?;
        let outcome = v
            .get("outcome")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        Ok((outcome, v.get("mark").cloned().unwrap_or(Value::Null)))
    }

    /// 更新一个单子的状态（note=None 表示不改备注）。
    pub async fn ledger_set(
        &self,
        scope: &str,
        key: &str,
        status: &str,
        note: Option<&str>,
        release: bool,
    ) -> Result<(), String> {
        let (server, token, name) = self.cfg().ok_or("未配置中转站 token")?;
        let body = gzip_json(&json!({
            "scope": scope, "key": key, "status": status,
            "note": note, "release": release,
        }))?;
        let resp = self
            .http
            .post(format!("{server}/v2/ledger/set"))
            .header("Authorization", format!("Bearer {token}"))
            .header("X-Relay-Name-Encoded", urlencode(&name))
            .header("X-Relay-Groups-Encoded", urlencode(&self.groups_csv()))
            .header("X-Relay-Device", &self.device_id)
            .header("Content-Type", "application/json")
            .header("Content-Encoding", "gzip")
            .body(body)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        Ok(())
    }

    /// 删除一个单子。
    pub async fn ledger_remove(&self, scope: &str, key: &str) -> Result<(), String> {
        let (server, token, name) = self.cfg().ok_or("未配置中转站 token")?;
        let body = gzip_json(&json!({ "scope": scope, "key": key }))?;
        let resp = self
            .http
            .post(format!("{server}/v2/ledger/remove"))
            .header("Authorization", format!("Bearer {token}"))
            .header("X-Relay-Name-Encoded", urlencode(&name))
            .header("X-Relay-Groups-Encoded", urlencode(&self.groups_csv()))
            .header("X-Relay-Device", &self.device_id)
            .header("Content-Type", "application/json")
            .header("Content-Encoding", "gzip")
            .body(body)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        Ok(())
    }

    /// 列出某 scope 下的全部单子（原始 JSON 数组）。
    pub async fn ledger_list(&self, scope: &str) -> Result<Vec<Value>, String> {
        let (server, token, name) = self.cfg().ok_or("未配置中转站 token")?;
        let resp = self
            .http
            .get(format!("{server}/v2/ledger/list"))
            .header("Authorization", format!("Bearer {token}"))
            .header("X-Relay-Name-Encoded", urlencode(&name))
            .header("X-Relay-Groups-Encoded", urlencode(&self.groups_csv()))
            .header("X-Relay-Device", &self.device_id)
            .query(&[("scope", scope)])
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        let v: Value = resp.json().await.map_err(|e| e.to_string())?;
        Ok(v.get("marks")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default())
    }

    fn clue_request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, String> {
        let (server, token, name) = self.cfg().ok_or("未配置中转站 token")?;
        let groups = self.groups_csv();
        Ok(self
            .http
            .request(method, format!("{server}{path}"))
            .header("Authorization", format!("Bearer {token}"))
            .header("X-Relay-Device", &self.device_id)
            .query(&[("name", name), ("groups", groups)])
            .timeout(Duration::from_secs(15)))
    }

    pub async fn clue_list(&self) -> Result<Vec<ClueNodeGroup>, String> {
        let response = self
            .clue_request(reqwest::Method::GET, "/v2/clues")?
            .send()
            .await
            .map_err(|error| error.to_string())?;
        Ok(decode_relay_json::<RelayClueList>(response).await?.groups)
    }

    pub async fn clue_capture(
        &self,
        thread_id: Option<&str>,
        title: &str,
        content: &str,
        placement: &str,
        target_card_id: Option<&str>,
        mention_tokens: &[String],
    ) -> Result<CaptureClueResult, String> {
        let author_name = self.cfg().map(|(_, _, name)| name).unwrap_or_default();
        let response = self
            .clue_request(reqwest::Method::POST, "/v2/clues/capture")?
            .json(&json!({
                "threadId": thread_id.unwrap_or_default(),
                "title": title,
                "content": content,
                "placement": placement,
                "targetCardId": target_card_id.unwrap_or_default(),
                "authorName": author_name,
                "mentionTokens": mention_tokens,
            }))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        decode_relay_json(response).await
    }

    pub async fn clue_comment(
        &self,
        card_id: &str,
        content: &str,
        parent_comment_id: Option<&str>,
        mention_tokens: &[String],
    ) -> Result<(), String> {
        let response = self
            .clue_request(reqwest::Method::POST, "/v2/clues/comment")?
            .json(&json!({
                "cardId": card_id,
                "content": content,
                "parentCommentId": parent_comment_id.unwrap_or_default(),
                "mentionTokens": mention_tokens,
            }))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        decode_relay_json::<Value>(response).await?;
        Ok(())
    }

    pub async fn clue_associate(
        &self,
        before_card_id: &str,
        after_card_id: &str,
    ) -> Result<ClueNodeGroup, String> {
        let response = self
            .clue_request(reqwest::Method::POST, "/v2/clues/associate")?
            .json(&json!({
                "beforeCardId": before_card_id,
                "afterCardId": after_card_id,
            }))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        Ok(decode_relay_json::<RelayClueAssociate>(response)
            .await?
            .group)
    }

    pub async fn clue_disassociate(
        &self,
        before_card_id: &str,
        after_card_id: &str,
    ) -> Result<ClueNodeGroup, String> {
        let response = self
            .clue_request(reqwest::Method::POST, "/v2/clues/disassociate")?
            .json(&json!({
                "beforeCardId": before_card_id,
                "afterCardId": after_card_id,
            }))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        Ok(decode_relay_json::<RelayClueAssociate>(response)
            .await?
            .group)
    }

    pub async fn clue_split(&self, card_id: &str) -> Result<ClueNodeGroup, String> {
        let response = self
            .clue_request(reqwest::Method::POST, "/v2/clues/split")?
            .json(&json!({ "cardId": card_id }))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        Ok(decode_relay_json::<RelayClueAssociate>(response)
            .await?
            .group)
    }

    pub async fn clue_stack(&self, card_ids: &[String]) -> Result<ClueNodeGroup, String> {
        let response = self
            .clue_request(reqwest::Method::POST, "/v2/clues/stack")?
            .json(&json!({ "cardIds": card_ids }))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        Ok(decode_relay_json::<RelayClueAssociate>(response)
            .await?
            .group)
    }

    pub async fn clue_delete(&self, card_id: &str) -> Result<(), String> {
        let response = self
            .clue_request(reqwest::Method::POST, "/v2/clues/delete")?
            .json(&json!({ "cardId": card_id }))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        decode_relay_json::<Value>(response).await?;
        Ok(())
    }

    pub async fn clue_context(&self, card_id: &str) -> Result<ClueContextSnapshot, String> {
        let response = self
            .clue_request(reqwest::Method::GET, "/v2/clues/context")?
            .query(&[("cardId", card_id)])
            .send()
            .await
            .map_err(|error| error.to_string())?;
        decode_relay_json(response).await
    }

    /// 中转站是否已配置（用于判断能否使用共享账本）。
    pub fn is_configured(&self) -> bool {
        self.cfg().is_some()
    }

    /// 上报本机「允许漫游」的目录列表
    pub fn publish_folders(&self) {
        let Some((server, token, name)) = self.cfg() else {
            return;
        };
        let folders: Vec<Value> = {
            let state = self.app.state::<AppState>();
            // worktree 目录名是随机 uuid：广播给队友时换成「仓库名 ⎇ 分支」
            let wt_names: HashMap<String, String> = {
                let worktrees = state.worktrees.lock().unwrap();
                worktrees
                    .worktrees
                    .iter()
                    .map(|w| {
                        (
                            w.path.clone(),
                            format!("{} ⎇ {}", basename(&w.repo), w.branch),
                        )
                    })
                    .collect()
            };
            let allowed = crate::current_roaming_project_folders(state.inner());
            allowed
                .iter()
                .filter(|path| is_allowed_roaming_path(&allowed, path))
                .map(|path| {
                    let name = wt_names
                        .get(path.as_str())
                        .cloned()
                        .unwrap_or_else(|| basename(&path));
                    json!({ "path": path, "name": name })
                })
                .collect()
        };
        let groups = self.groups_csv();
        let http = self.http.clone();
        let device_id = self.device_id.clone();
        let Ok(body) = gzip_json(&json!({ "folders": folders })) else {
            return;
        };
        tauri::async_runtime::spawn(async move {
            let _ = http
                .post(format!("{server}/v2/folders"))
                .header("Authorization", format!("Bearer {token}"))
                .header("X-Relay-Name-Encoded", urlencode(&name))
                .header("X-Relay-Groups-Encoded", urlencode(&groups))
                .header("X-Relay-Device", device_id)
                .header("Content-Type", "application/json")
                .header("Content-Encoding", "gzip")
                .body(body)
                .timeout(Duration::from_secs(15))
                .send()
                .await;
        });
    }

    // ===== 分享 =====

    pub fn share_thread(&self, thread_id: &str, to: &str) -> Result<(), String> {
        self.send_share(thread_id, to, false)
    }

    /// 把会话快照发给 to。recall=true 表示这是漫游召回的自动回传（对方前端标注「召回」）。
    fn send_share(&self, thread_id: &str, to: &str, recall: bool) -> Result<(), String> {
        let (title, agent_kind, items, plan, active_clue_card_id) = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            let t = store.get(thread_id).ok_or("线程不存在")?;
            // 分享跨机器：把附件内容内嵌进条目，接收方才能看到图片/文件
            let mut items = t.items.clone();
            crate::threads::embed_items_attachments(&mut items);
            (
                t.title.clone(),
                t.agent_kind.clone(),
                serde_json::to_value(&items).unwrap_or(json!([])),
                t.plan.clone().unwrap_or(json!(null)),
                t.active_clue_card_id.clone(),
            )
        };
        self.enqueue(
            to.to_string(),
            "share",
            json!({
                "title": title,
                "agentKind": agent_kind,
                "items": items,
                "plan": plan,
                "activeClueCardId": active_clue_card_id,
                "recall": recall,
            }),
        );
        Ok(())
    }

    fn on_share(&self, env: &InEnvelope) {
        let share = Share {
            id: uuid::Uuid::new_v4().to_string(),
            from: env.from.clone(),
            from_name: env.from_name.clone(),
            title: env.data["title"].as_str().unwrap_or("(分享)").to_string(),
            agent_kind: serde_json::from_value(env.data["agentKind"].clone())
                .unwrap_or(AgentKind::Devin),
            items: env.data["items"].clone(),
            plan: env.data["plan"].clone(),
            active_clue_card_id: env.data["activeClueCardId"].as_str().map(str::to_string),
            recall: env.data["recall"].as_bool().unwrap_or(false),
            ts: now_ms(),
        };
        {
            let mut inbox = self.inbox.lock().unwrap();
            inbox.push(share);
            persist_inbox(&self.config_dir, &inbox);
        }
        self.emit_inbox();
    }

    fn emit_inbox(&self) {
        let _ = self.app.emit(EV_RELAY_INBOX, self.inbox_list());
    }

    /// 接收一条分享：在指定目录新建一个本地会话
    pub fn accept_share(
        self: &Arc<Self>,
        id: &str,
        cwd: &str,
        ephemeral: bool,
    ) -> Result<String, String> {
        let share = {
            let mut inbox = self.inbox.lock().unwrap();
            let idx = inbox.iter().position(|s| s.id == id).ok_or("分享已失效")?;
            let share = inbox.remove(idx);
            persist_inbox(&self.config_dir, &inbox);
            share
        };
        self.emit_inbox();
        if !std::path::Path::new(cwd).is_dir() {
            return Err(format!("目录不存在：{cwd}"));
        }
        let items: Vec<Item> = serde_json::from_value(share.items).unwrap_or_default();
        // 召回后会在本机继续执行，模式应与本机新会话默认值一致；普通分享保持原行为。
        let mode = if share.recall {
            let state = self.app.state::<AppState>();
            let default_mode = state.settings.lock().unwrap().default_mode.clone();
            accepted_share_mode(share.recall, &default_mode)
        } else {
            None
        };
        let mut thread = Thread::new(
            cwd.to_string(),
            share.agent_kind,
            None,
            mode,
            None,
            ephemeral,
        );
        thread.title = if share.title.is_empty() {
            "收到的分享".into()
        } else {
            share.title.clone()
        };
        thread.items = items;
        thread.plan = if share.plan.is_null() {
            None
        } else {
            Some(share.plan)
        };
        thread.active_clue_card_id = share.active_clue_card_id;
        let thread_id = thread.id.clone();
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            store.threads.push(thread);
            store.save();
        }
        let _ = self.app.emit(EV_THREADS, json!({}));
        Ok(thread_id)
    }

    pub fn decline_share(&self, id: &str) {
        {
            let mut inbox = self.inbox.lock().unwrap();
            inbox.retain(|s| s.id != id);
            persist_inbox(&self.config_dir, &inbox);
        }
        self.emit_inbox();
    }

    /// 高级分享：用指定模型（默认 swe-1.6）对会话执行一段提示词，
    /// 在本机起一个会话跑出结果，跑完后自动把结果分享给目标。返回处理线程供前端打开观看。
    pub fn advanced_share(
        self: &Arc<Self>,
        thread_id: &str,
        to: String,
        prompt: String,
        agent: String,
        model: String,
    ) -> Result<Thread, String> {
        if self.cfg().is_none() {
            return Err("未配置中转站 token".into());
        }
        let (src_cwd, transcript, active_clue_card_id) = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            let t = store.get(thread_id).ok_or("线程不存在")?;
            (
                t.cwd.clone(),
                build_transcript(&t.items),
                t.active_clue_card_id.clone(),
            )
        };
        if transcript.trim().is_empty() {
            return Err("会话还没有内容可处理".into());
        }
        // 处理在本机进行：源目录是本地真实目录就用它，否则用临时目录
        let cwd = if std::path::Path::new(&src_cwd).is_dir() {
            src_cwd
        } else {
            make_scratch_dir()?
        };
        // 后端优先用调用方传入的；为空回退设置里的「分享后端」，再兜底 Devin
        let agent_kind = {
            let raw = if agent.trim().is_empty() {
                let state = self.app.state::<AppState>();
                let s = state.settings.lock().unwrap();
                s.share_model_agent.trim().to_string()
            } else {
                agent.trim().to_string()
            };
            AgentKind::from_str(&raw).unwrap_or(AgentKind::Devin)
        };
        // 模型优先用调用方传入的；为空回退设置里的「分享默认模型」；Devin 再兜底 swe-1.6
        let model = if model.trim().is_empty() {
            let state = self.app.state::<AppState>();
            let s = state
                .settings
                .lock()
                .unwrap()
                .share_model
                .trim()
                .to_string();
            if s.is_empty() && agent_kind == AgentKind::Devin {
                "swe-1.6".to_string()
            } else {
                s
            }
        } else {
            model.trim().to_string()
        };
        let model_opt = if model.is_empty() { None } else { Some(model) };
        let mut thread = Thread::new(cwd, agent_kind.clone(), model_opt, None, None, true);
        thread.active_clue_card_id = active_clue_card_id;
        thread.title = format!("高级分享 · {}", first_line(&prompt, 24));
        let new_id = thread.id.clone();
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            store.threads.push(thread);
            store.save();
        }
        let _ = self.app.emit(EV_THREADS, json!({}));
        self.advanced.lock().unwrap().insert(new_id.clone(), to);

        let seed = format!(
            "{prompt}\n\n----\n下面是需要你处理的会话记录（请直接输出处理后的结果，便于分享给同事）：\n\n{transcript}"
        );
        // 处理会话按固定后端路由执行。
        let state = self.app.state::<AppState>();
        let run_id = new_id.clone();
        match agent_kind {
            AgentKind::Devin => {
                let mgr = state.acp.clone();
                tauri::async_runtime::spawn(async move {
                    mgr.run_prompt(run_id, seed, vec![]).await;
                });
            }
            AgentKind::Codex | AgentKind::CodexPlus => {
                let mgr = state.codexplus.clone();
                tauri::async_runtime::spawn(async move {
                    mgr.run_prompt(run_id, seed, vec![]).await;
                });
            }
            AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => {
                let mgr = state.codebuddyplus.clone();
                tauri::async_runtime::spawn(async move {
                    mgr.run_prompt(run_id, seed, vec![]).await;
                });
            }
            AgentKind::ClaudeCode => {
                let mgr = state.claudeplus.clone();
                tauri::async_runtime::spawn(async move {
                    mgr.run_prompt(run_id, seed, vec![]).await;
                });
            }
            AgentKind::Cursor => {
                let mgr = state.cursorplus.clone();
                tauri::async_runtime::spawn(async move {
                    mgr.run_prompt(run_id, seed, vec![]).await;
                });
            }
            AgentKind::OpenCode | AgentKind::OpenCodePlus => {
                let mgr = state.opencodeplus.clone();
                tauri::async_runtime::spawn(async move {
                    mgr.run_prompt(run_id, seed, vec![]).await;
                });
            }
        }
        let state = self.app.state::<AppState>();
        let store = state.store.lock().unwrap();
        store
            .get(&new_id)
            .cloned()
            .ok_or_else(|| "线程创建失败".to_string())
    }

    /// 处理线程结束时调用：若它是高级分享线程，则把结果分享出去并返回目标 token
    pub fn finish_advanced_if_any(self: &Arc<Self>, thread_id: &str) -> Option<String> {
        let to = self.advanced.lock().unwrap().remove(thread_id)?;
        let _ = self.share_thread(thread_id, &to);
        Some(to)
    }

    pub fn is_advanced(&self, thread_id: &str) -> bool {
        self.advanced.lock().unwrap().contains_key(thread_id)
    }

    // ===== 额度租借：A 本机执行，临时使用 B 的加密凭证 =====

    fn emit_quota_progress(&self, operation_id: &str, stage: &str, message: impl Into<String>) {
        let _ = self.app.emit(
            EV_RELAY_QUOTA_PROGRESS,
            json!({
                "operationId": operation_id,
                "stage": stage,
                "message": message.into(),
            }),
        );
    }

    pub async fn prepare_quota_lease(
        self: &Arc<Self>,
        peer_token: String,
        agent_kind: AgentKind,
        model: String,
    ) -> Result<(), String> {
        ensure_quota_backend_supported(&agent_kind)?;
        if self.cfg().is_none() {
            return Err("未配置中转站 token".into());
        }
        let model = model.trim().to_string();
        if model.is_empty() {
            return Err("额度租约必须指定共享模型".into());
        }
        let key = QuotaLeaseKey::new(peer_token, agent_kind, &model)?;
        self.acquire_quota_lease(key, model, None).await?;
        Ok(())
    }

    fn quota_lease_ready(&self, key: &QuotaLeaseKey) -> bool {
        self.quota_leases.lock().unwrap().contains_key(key)
    }

    async fn wait_quota_lease_flight(
        wait: oneshot::Receiver<QuotaLeaseResult>,
        operation: Option<&QuotaOperation>,
    ) -> QuotaLeaseResult {
        let result = if let Some(operation) = operation {
            tokio::select! {
                _ = operation.cancelled() => return Err(QUOTA_CANCELLED_ERROR.into()),
                result = wait => result,
            }
        } else {
            wait.await
        };
        result.map_err(|_| "额度租约准备通道已关闭".to_string())?
    }

    async fn acquire_quota_lease(
        self: &Arc<Self>,
        key: QuotaLeaseKey,
        model: String,
        operation: Option<&QuotaOperation>,
    ) -> QuotaLeaseResult {
        if let Some(bundle) = self.quota_leases.lock().unwrap().get(&key).cloned() {
            return Ok(bundle);
        }

        let wait = {
            let mut flights = self.quota_lease_flights.lock().unwrap();
            if let Some(waiters) = flights.get_mut(&key) {
                let (reply, wait) = oneshot::channel();
                waiters.push(reply);
                Some(wait)
            } else {
                flights.insert(key.clone(), Vec::new());
                None
            }
        };
        if let Some(wait) = wait {
            return Self::wait_quota_lease_flight(wait, operation).await;
        }

        let result = self.request_quota_bundle(&key, &model, operation).await;
        if let Ok(bundle) = &result {
            self.quota_leases
                .lock()
                .unwrap()
                .insert(key.clone(), bundle.clone());
        }
        let waiters = self
            .quota_lease_flights
            .lock()
            .unwrap()
            .remove(&key)
            .unwrap_or_default();
        for waiter in waiters {
            let _ = waiter.send(result.clone());
        }
        result
    }

    async fn request_quota_bundle(
        self: &Arc<Self>,
        key: &QuotaLeaseKey,
        model: &str,
        operation: Option<&QuotaOperation>,
    ) -> QuotaLeaseResult {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (secret, public_key) = crate::credential_roaming::new_request_key();
        let (reply, wait) = oneshot::channel();
        self.pending_quota.lock().unwrap().insert(
            request_id.clone(),
            PendingQuotaClient {
                peer: key.peer.clone(),
                agent_kind: key.agent_kind.clone(),
                secret,
                reply,
            },
        );
        let request = self.send(
            &key.peer,
            "quota.request",
            json!({
                "reqId": request_id,
                "agentKind": key.agent_kind,
                "model": model,
                "publicKey": public_key,
            }),
        );
        let send_result = if let Some(operation) = operation {
            tokio::select! {
                _ = operation.cancelled() => {
                    self.pending_quota.lock().unwrap().remove(&request_id);
                    return Err(QUOTA_CANCELLED_ERROR.into());
                }
                result = request => result,
            }
        } else {
            request.await
        };
        if let Err(error) = send_result {
            self.pending_quota.lock().unwrap().remove(&request_id);
            return Err(format!("发送额度请求失败：{error}"));
        }

        let response = if let Some(operation) = operation {
            tokio::select! {
                _ = operation.cancelled() => {
                    self.pending_quota.lock().unwrap().remove(&request_id);
                    return Err(QUOTA_CANCELLED_ERROR.into());
                }
                result = timeout(QUOTA_REQUEST_TIMEOUT, wait) => result,
            }
        } else {
            timeout(QUOTA_REQUEST_TIMEOUT, wait).await
        };
        match response {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("额度授权通道已关闭".into()),
            Err(_) => {
                self.pending_quota.lock().unwrap().remove(&request_id);
                Err("等待对方授权超时".into())
            }
        }
    }

    pub async fn create_quota_thread(
        self: &Arc<Self>,
        peer_token: String,
        peer_name: String,
        cwd: String,
        agent_kind: AgentKind,
        model: Option<String>,
        mode: Option<String>,
        clue_context: Option<ClueContextSnapshot>,
        operation_id: String,
    ) -> Result<Thread, String> {
        if self.cfg().is_none() {
            return Err("未配置中转站 token".into());
        }
        if !std::path::Path::new(&cwd).is_dir() {
            return Err(format!("本地目录不存在：{cwd}"));
        }
        let operation_id = operation_id.trim().to_string();
        if operation_id.is_empty() {
            return Err("额度漫游操作编号不能为空".into());
        }
        let operation = Arc::new(QuotaOperation::new());
        {
            let mut operations = self.quota_operations.lock().unwrap();
            if operations.contains_key(&operation_id) {
                return Err("额度漫游操作已存在".into());
            }
            operations.insert(operation_id.clone(), operation.clone());
        }
        let result = self
            .create_quota_thread_inner(
                peer_token,
                peer_name,
                cwd,
                agent_kind,
                model,
                mode,
                clue_context,
                &operation_id,
                operation,
            )
            .await;
        self.quota_operations.lock().unwrap().remove(&operation_id);
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_quota_thread_inner(
        self: &Arc<Self>,
        peer_token: String,
        peer_name: String,
        cwd: String,
        agent_kind: AgentKind,
        model: Option<String>,
        mode: Option<String>,
        clue_context: Option<ClueContextSnapshot>,
        operation_id: &str,
        operation: Arc<QuotaOperation>,
    ) -> Result<Thread, String> {
        ensure_quota_backend_supported(&agent_kind)?;
        let settings = {
            let state = self.app.state::<AppState>();
            let settings = state.settings.lock().unwrap().clone();
            settings
        };
        operation.ensure_active()?;
        if !crate::cli_manager::is_installed(&agent_kind, &settings) {
            self.emit_quota_progress(
                operation_id,
                "installing",
                format!("本机缺少 {} CLI，正在一键安装…", agent_kind.label()),
            );
            let state = self.app.state::<AppState>();
            crate::cli_manager::ensure_installed(
                &self.app,
                state.inner(),
                agent_kind.clone(),
                &settings,
                operation_id,
            )
            .await?;
            operation.ensure_active()?;
        }

        let model = model
            .filter(|value| !value.trim().is_empty())
            .ok_or("额度漫游必须选择对方明确共享的模型")?;
        let lease_key = QuotaLeaseKey::new(peer_token.clone(), agent_kind.clone(), &model)?;
        let lease_ready = self.quota_lease_ready(&lease_key);
        if lease_ready {
            self.emit_quota_progress(operation_id, "preparing", "正在复用已预热的额度租约…");
        } else {
            self.emit_quota_progress(
                operation_id,
                "requesting",
                format!("正在向 {peer_name} 同步额度租约…"),
            );
        }
        let bundle = self
            .acquire_quota_lease(lease_key, model.clone(), Some(operation.as_ref()))
            .await?;

        operation.ensure_active()?;
        if !lease_ready {
            self.emit_quota_progress(
                operation_id,
                "preparing",
                "额度租约已就绪，正在创建隔离会话…",
            );
        }
        let default_mode = {
            let state = self.app.state::<AppState>();
            let default_mode = state.settings.lock().unwrap().default_mode.clone();
            default_mode
        };
        let mut thread = Thread::new(
            cwd.clone(),
            agent_kind.clone(),
            Some(model),
            mode.filter(|value| !value.is_empty())
                .or(Some(default_mode).filter(|value| !value.is_empty())),
            None,
            false,
        );
        thread.quota_peer = Some(peer_token);
        thread.quota_peer_name = Some(peer_name.clone());
        if let Some(context) = clue_context {
            thread.active_clue_card_id = Some(context.root_card_id.clone());
            thread.clue_context = Some(context);
        }
        thread.title = format!("额度@{peer_name} · {}", basename(&cwd));
        thread.push_system_local(
            format!(
                "🔐 本会话在你的本地目录执行，临时使用 {peer_name} 的 {} 额度；凭证已隔离，不会覆盖本机登录。",
                agent_kind.label()
            ),
            "info",
        );
        let thread_id = thread.id.clone();
        let runtime = crate::credential_roaming::materialize_runtime(
            self.app.clone(),
            &thread_id,
            &agent_kind,
            bundle,
        )?;
        if !operation.commit() {
            runtime.shutdown().await;
            return Err(QUOTA_CANCELLED_ERROR.into());
        }
        {
            let state = self.app.state::<AppState>();
            state
                .borrowed_runtimes
                .lock()
                .unwrap()
                .insert(thread_id.clone(), runtime);
            let mut store = state.store.lock().unwrap();
            store.threads.push(thread.clone());
            store.save();
            if !cwd.contains(crate::SCRATCH_MARK) {
                state.projects.lock().unwrap().touch(&cwd);
            }
        }
        self.publish_folders();
        let _ = self.app.emit(EV_THREADS, json!({}));
        self.emit_quota_progress(operation_id, "ready", "额度凭证已就绪，开始本地会话");
        Ok(thread)
    }

    pub fn cancel_quota_roaming(&self, operation_id: &str) -> bool {
        let Some(operation) = self
            .quota_operations
            .lock()
            .unwrap()
            .get(operation_id)
            .cloned()
        else {
            return false;
        };
        operation.cancel()
    }

    fn spawn_quota_send(&self, to: String, kind: &str, data: Value) {
        let relay = self.app.state::<AppState>().relay.clone();
        let kind = kind.to_string();
        tauri::async_runtime::spawn(async move {
            relay.send_with_retry(&to, &kind, data).await;
        });
    }

    fn on_quota_request(&self, env: &InEnvelope) {
        let req_id = env.data["reqId"].as_str().unwrap_or_default().to_string();
        let public_key = env.data["publicKey"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let agent_kind: AgentKind =
            serde_json::from_value(env.data["agentKind"].clone()).unwrap_or(AgentKind::Devin);
        let model = env.data["model"].as_str().unwrap_or_default().to_string();
        if req_id.is_empty() || public_key.is_empty() {
            return;
        }
        let allowed = {
            let state = self.app.state::<AppState>();
            let settings = state.settings.lock().unwrap();
            quota_model_is_shared(&settings, &agent_kind, &model)
        };
        if !allowed {
            self.spawn_quota_send(
                env.from.clone(),
                "quota.rejected",
                json!({
                    "reqId": req_id,
                    "error": format!(
                        "对方已取消共享 {} 模型 {model}，当前无法使用",
                        agent_kind.label()
                    ),
                }),
            );
            return;
        }

        let app = self.app.clone();
        let to = env.from.clone();
        std::thread::spawn(move || {
            let result = crate::credential_roaming::collect_credentials(&app, agent_kind, &model)
                .and_then(|bundle| {
                    crate::credential_roaming::encrypt_bundle(&public_key, &req_id, &bundle)
                });
            let relay = app.state::<AppState>().relay.clone();
            match result {
                Ok(grant) => relay.spawn_quota_send(
                    to,
                    "quota.granted",
                    json!({ "reqId": req_id, "grant": grant }),
                ),
                Err(error) => relay.spawn_quota_send(
                    to,
                    "quota.rejected",
                    json!({ "reqId": req_id, "error": error }),
                ),
            }
        });
    }

    fn on_quota_granted(&self, env: &InEnvelope) {
        let req_id = env.data["reqId"].as_str().unwrap_or_default().to_string();
        let Some(pending) = self.pending_quota.lock().unwrap().remove(&req_id) else {
            return;
        };
        let result = if env.from != pending.peer {
            Err("额度凭证来源与请求目标不一致".into())
        } else {
            serde_json::from_value::<crate::credential_roaming::EncryptedGrant>(
                env.data["grant"].clone(),
            )
            .map_err(|_| "加密凭证载荷无效".to_string())
            .and_then(|grant| {
                crate::credential_roaming::decrypt_bundle(pending.secret, &req_id, &grant)
            })
            .and_then(|bundle| {
                if bundle.agent_kind == pending.agent_kind {
                    Ok(bundle)
                } else {
                    Err("额度凭证后端与请求不一致".into())
                }
            })
        };
        let _ = pending.reply.send(result);
    }

    fn on_quota_rejected(&self, env: &InEnvelope) {
        let req_id = env.data["reqId"].as_str().unwrap_or_default().to_string();
        let Some(pending) = self.pending_quota.lock().unwrap().remove(&req_id) else {
            return;
        };
        let error = env.data["error"]
            .as_str()
            .unwrap_or("对方拒绝了额度租借请求")
            .to_string();
        let _ = pending.reply.send(Err(error));
    }

    // ===== 漫游：guest 侧 =====

    /// guest：在对端的目录上新建一个漫游会话（本机只接收展示）
    pub async fn create_roaming_thread(
        self: &Arc<Self>,
        peer_token: String,
        peer_name: String,
        folder: String,
        agent_kind: AgentKind,
        model: Option<String>,
        mode: Option<String>,
        first_prompt: Option<String>,
        clue_context: Option<ClueContextSnapshot>,
        worktree: bool,
        worktree_branch: Option<String>,
        worktree_base: Option<String>,
    ) -> Result<Thread, String> {
        if self.cfg().is_none() {
            return Err("未配置中转站 token".into());
        }
        let mut thread = Thread::new(
            folder.clone(),
            agent_kind.clone(),
            model.clone(),
            mode.clone(),
            None,
            false, // 漫游会话和普通会话一样长久保存
        );
        thread.roaming_role = Some("guest".into());
        thread.roaming_peer = Some(peer_token.clone());
        thread.roaming_peer_name = Some(peer_name.clone());
        if let Some(context) = clue_context.clone() {
            thread.active_clue_card_id = Some(context.root_card_id.clone());
            thread.clue_context = Some(context);
        }
        thread.title = format!("漫游 · {}", basename(&folder));
        if worktree {
            // guest 侧只是展示壳，真实 worktree 由 host 在自己仓库创建；这里仅记录分支用于界面标注
            thread.worktree = Some(Worktree {
                repo: folder.clone(),
                path: String::new(),
                branch: worktree_branch.clone().unwrap_or_default(),
            });
        }
        // 进入会话即提示等待对方确认（hold 住），确认后再开始执行
        thread.push_system_local(
            format!("⏳ 已向 {peer_name} 发起漫游请求，等待对方确认后即可开始…"),
            "info",
        );
        let thread_id = thread.id.clone();
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            store.threads.push(thread.clone());
            store.save();
        }
        let _ = self.app.emit(EV_THREADS, json!({}));

        // 不等 host 回执：先把会话建好返回，前端立刻进入；host 建好后由
        // on_roaming_created 回填 remote id 并补发期间排队的提示词。
        let req_id = uuid::Uuid::new_v4().to_string();
        self.pending_creates
            .lock()
            .unwrap()
            .insert(req_id.clone(), thread_id.clone());
        self.spawn_send(
            peer_token.clone(),
            "roaming.create",
            json!({
                "reqId": req_id,
                "guestThreadId": thread_id,
                "folder": folder,
                "agentKind": agent_kind,
                "model": model,
                "mode": mode,
                // 首条提示词随请求发给 host，仅用于审批确认框展示「谁要做什么」
                "prompt": first_prompt,
                "clueContext": clue_context,
                // worktree：让 host 在自己仓库为这次漫游创建独立工作目录 + 分支
                "worktree": worktree,
                "worktreeBranch": worktree_branch,
                "worktreeBase": worktree_base,
            }),
        );
        Ok(thread)
    }

    pub fn is_guest_running(&self, thread_id: &str) -> bool {
        self.guest_running.lock().unwrap().contains(thread_id)
    }

    fn set_guest_running(&self, thread_id: &str, running: bool) {
        self.app
            .state::<AppState>()
            .sleep_inhibitor
            .set_running(thread_id, running);
        let mut threads = self.guest_running.lock().unwrap();
        if running {
            threads.insert(thread_id.to_string());
        } else {
            threads.remove(thread_id);
        }
    }

    /// guest：发送一轮对话（路由到 host 执行）
    pub fn guest_send_prompt(
        self: &Arc<Self>,
        thread_id: &str,
        text: String,
        images: Vec<PromptImage>,
    ) -> Result<(), String> {
        // 漫游跨机器：把仅有本地路径的附件读成 base64，随消息带到 host
        let mut images = images;
        crate::threads::embed_attachment_data(&mut images);
        // 取对端 + 可能还没建好的 remote id（新建会话不再等 host 回执即可进入）
        let (peer, remote_id) = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            let t = store.get(thread_id).ok_or("线程不存在")?;
            if !t.is_roaming_guest() {
                return Err("不是漫游会话".into());
            }
            (
                t.roaming_peer.clone().ok_or("不是漫游会话")?,
                t.roaming_remote_id.clone(),
            )
        };
        // 本地乐观落用户消息（高位 id，避免与 host 条目冲突）+ 标题 + 运行态，立即反馈
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            let Some(thread) = store.get_mut(thread_id) else {
                return Err("线程不存在".into());
            };
            let item = thread.push_user_local(text.clone(), images.clone());
            if thread.title.starts_with("漫游 · ") || thread.title == "新会话" {
                let derived = derive_title(&text);
                if !derived.is_empty() {
                    thread.title = derived;
                }
            }
            store.save();
            self.emit_update(thread_id, json!({ "t": "upsert", "item": item }));
        }
        self.set_guest_running(thread_id, true);
        self.touch_guest_activity(thread_id);
        let _ = self.app.emit(
            EV_TURN,
            json!({ "threadId": thread_id, "running": true, "stopReason": null }),
        );
        let _ = self.app.emit(EV_THREADS, json!({}));
        match remote_id {
            Some(host_thread_id) => {
                self.spawn_send(
                    peer,
                    "roaming.prompt",
                    json!({
                        "hostThreadId": host_thread_id,
                        "guestThreadId": thread_id,
                        "text": text,
                        "images": images,
                    }),
                );
            }
            None => {
                // host 还没建好会话：先排队，on_roaming_created 收到回执后按序补发
                self.pending_prompts
                    .lock()
                    .unwrap()
                    .entry(thread_id.to_string())
                    .or_default()
                    .push((text, images));
            }
        }
        Ok(())
    }

    pub fn guest_cancel(self: &Arc<Self>, thread_id: &str) -> Result<(), String> {
        let (peer, host_thread_id) = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            let t = store.get(thread_id).ok_or("线程不存在")?;
            if !t.is_roaming_guest() {
                return Err("不是漫游会话".into());
            }
            (
                t.roaming_peer.clone().ok_or("不是漫游会话")?,
                t.roaming_remote_id.clone(),
            )
        };
        // host 尚未接受时，停止就是撤掉本地排队的提示词，不能让它在稍后获批后又自动开跑。
        self.pending_prompts.lock().unwrap().remove(thread_id);
        if let Some(host_thread_id) = host_thread_id {
            self.spawn_send(
                peer,
                "roaming.cancel",
                json!({ "hostThreadId": host_thread_id }),
            );
        } else {
            self.set_guest_running(thread_id, false);
            let _ = self.app.emit(
                EV_TURN,
                json!({ "threadId": thread_id, "running": false, "stopReason": "force_cancelled" }),
            );
            let _ = self.app.emit(EV_THREADS, json!({}));
        }
        Ok(())
    }

    /// guest：召回漫游会话——请求 host 把该会话的完整快照 Flow 回来
    /// （等价于对方手动分享给你，收到后照常在收件箱里选项目接收成本地会话）。
    pub fn recall_roaming_thread(self: &Arc<Self>, thread_id: &str) -> Result<(), String> {
        if self.cfg().is_none() {
            return Err("未配置中转站 token".into());
        }
        let (peer, host_thread_id) = self.roaming_route(thread_id)?;
        // 会话里落一条提示，让用户知道请求已发出、结果去收件箱里收
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            if let Some(t) = store.get_mut(thread_id) {
                let item = t.push_system_local(
                    "📥 已发送召回请求：对方回传后会出现在收件箱（Flow），届时选择本地项目即可接收。".into(),
                    "info",
                );
                store.save();
                self.emit_update(thread_id, json!({ "t": "upsert", "item": item }));
            }
        }
        self.spawn_send(
            peer,
            "roaming.recall",
            json!({ "hostThreadId": host_thread_id, "guestThreadId": thread_id }),
        );
        Ok(())
    }

    /// guest：把模型/模式变更同步给 host
    pub fn guest_sync_config(self: &Arc<Self>, thread_id: &str) {
        let Ok((peer, host_thread_id)) = self.roaming_route(thread_id) else {
            return;
        };
        let (model, mode) = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            match store.get(thread_id) {
                Some(t) => (t.model.clone(), t.mode.clone()),
                None => return,
            }
        };
        self.spawn_send(
            peer,
            "roaming.config",
            json!({ "hostThreadId": host_thread_id, "model": model, "mode": mode }),
        );
    }

    // ===== 漫游：模型选项协商（用对端的模型列表，而不是本机的） =====

    /// guest：向 host 请求它已启用后端的可选模型/模式。漫游在对端机器上执行，
    /// 本机的模型对方不一定有，所以选择器要用对方的列表。
    pub fn request_peer_models(self: &Arc<Self>, peer_token: String) {
        if self.cfg().is_none() {
            return;
        }
        self.spawn_send(peer_token, "roaming.models_request", json!({}));
    }

    /// 本机共享模型配置变化后，通知在线队友立即回拉，避免等待定时刷新或重启。
    pub fn notify_peer_models_changed(self: &Arc<Self>) {
        let Some((_, me, _)) = self.cfg() else {
            return;
        };
        let peers = self.peers();
        let Some(peers) = peers.as_array() else {
            return;
        };
        for peer in peers {
            let token = peer["token"].as_str().unwrap_or_default();
            if token.is_empty()
                || token == me.as_str()
                || !peer["online"].as_bool().unwrap_or(false)
            {
                continue;
            }
            self.spawn_send(token.to_string(), "roaming.models_changed", json!({}));
        }
    }

    /// 队友提示其共享模型已变化：立即向该队友回拉最新列表。
    fn on_roaming_models_changed(&self, env: &InEnvelope) {
        if env.from.is_empty() {
            return;
        }
        self.invalidate_quota_leases_for_peer(&env.from);
        self.spawn_send_now(env.from.clone(), "roaming.models_request", json!({}));
    }

    /// host：收到模型请求，收集本机「已启用且检测可用」后端的模型/模式列表回给对端。
    /// 顺序与前端 ALL_AGENT_KINDS 保持一致（devin → codex → codebuddy → claudecode → cursor → opencode）。
    fn on_roaming_models_request(&self, env: &InEnvelope) {
        let to = env.from.clone();
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            let (kinds, shared): (Vec<AgentKind>, HashSet<String>) = {
                let state = app.state::<AppState>();
                let s = state.settings.lock().unwrap();
                let avail = state.backend_availability.lock().unwrap();
                let kinds = [
                    (AgentKind::Devin, s.devin_enabled),
                    (AgentKind::Codex, s.codex_enabled),
                    (AgentKind::CodeBuddy, s.codebuddy_enabled),
                    (AgentKind::ClaudeCode, s.claudecode_enabled),
                    (AgentKind::Cursor, s.cursor_enabled),
                    (AgentKind::OpenCode, s.opencode_enabled),
                    (AgentKind::OpenCodePlus, s.opencodeplus_enabled),
                ]
                .into_iter()
                // 可用性未检测完（map 为空/无该键）时按可用处理，避免误伤
                .filter(|(k, en)| *en && avail.get(k.as_str()).copied().unwrap_or(true))
                .map(|(k, _)| k)
                .collect();
                (kinds, s.quota_shared_models.iter().cloned().collect())
            };
            let mut backends: Vec<&str> = Vec::new();
            let mut options = serde_json::Map::new();
            let mut shared_options = serde_json::Map::new();
            for kind in kinds {
                backends.push(kind.as_str());
                let fetched = match kind {
                    AgentKind::OpenCode | AgentKind::OpenCodePlus => {
                        app.state::<AppState>()
                            .opencodeplus
                            .ensure_model_options()
                            .await
                    }
                    AgentKind::Codex | AgentKind::CodexPlus => {
                        app.state::<AppState>()
                            .codexplus
                            .ensure_model_options()
                            .await
                    }
                    AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => {
                        app.state::<AppState>()
                            .codebuddyplus
                            .ensure_model_options()
                            .await
                    }
                    AgentKind::ClaudeCode => {
                        app.state::<AppState>()
                            .claudeplus
                            .ensure_model_options()
                            .await
                    }
                    AgentKind::Cursor => {
                        app.state::<AppState>()
                            .cursorplus
                            .ensure_model_options()
                            .await
                    }
                    AgentKind::Devin => app.state::<AppState>().acp.fetch_model_options().await,
                };
                if let Ok(v) = fetched {
                    if let Some(filtered) = shared_model_options(&kind, &v, &shared) {
                        shared_options.insert(kind.as_str().into(), filtered);
                    }
                    options.insert(kind.as_str().into(), v);
                }
            }
            let relay = app.state::<AppState>().relay.clone();
            relay.spawn_send(
                to,
                "roaming.models",
                json!({
                    "backends": backends,
                    "options": Value::Object(options),
                    "sharedOptions": Value::Object(shared_options),
                }),
            );
        });
    }

    /// guest：收到 host 回传的模型列表，转发给前端按对端 token 缓存。
    fn on_roaming_models(&self, env: &InEnvelope) {
        self.retain_shared_quota_leases(&env.from, &env.data["sharedOptions"]);
        let _ = self.app.emit(
            EV_RELAY_PEER_MODELS,
            json!({
                "peer": env.from,
                "backends": env.data["backends"].clone(),
                "options": env.data["options"].clone(),
                "sharedOptions": env.data["sharedOptions"].clone(),
            }),
        );
    }

    /// guest：向 host 请求某目录的本地分支列表（worktree「基于分支」下拉）。
    pub fn request_peer_branches(self: &Arc<Self>, peer_token: String, folder: String) {
        if self.cfg().is_none() {
            return;
        }
        self.spawn_send(
            peer_token,
            "roaming.branches_request",
            json!({ "folder": folder }),
        );
    }

    /// host：收到分支请求，列出该目录所在仓库的本地分支 + 当前分支回给对端。
    /// git 是同步阻塞命令，丢到独立线程执行，避免卡住 WebSocket 分发。
    fn on_roaming_branches_request(&self, env: &InEnvelope) {
        let to = env.from.clone();
        let folder = env.data["folder"].as_str().unwrap_or_default().to_string();
        if !self.roaming_folder_allowed(&folder) {
            self.spawn_send_now(
                to,
                "roaming.branches",
                json!({ "folder": folder, "current": null, "branches": [], "error": "该目录未开放漫游" }),
            );
            return;
        }
        let app = self.app.clone();
        std::thread::spawn(move || {
            let branches = crate::gitwt::list_branches(&folder).unwrap_or_default();
            let current = crate::gitwt::current_branch(&folder);
            let relay = app.state::<AppState>().relay.clone();
            relay.spawn_send(
                to,
                "roaming.branches",
                json!({ "folder": folder, "current": current, "branches": branches }),
            );
        });
    }

    /// guest：收到 host 回传的分支列表，转发前端按「对端 token + 目录」缓存。
    fn on_roaming_branches(&self, env: &InEnvelope) {
        let _ = self.app.emit(
            EV_RELAY_PEER_BRANCHES,
            json!({
                "peer": env.from,
                "folder": env.data["folder"].clone(),
                "current": env.data["current"].clone(),
                "branches": env.data["branches"].clone(),
            }),
        );
    }

    /// guest：响应漫游权限请求（返回 true 表示这是漫游权限、已处理）
    pub fn guest_respond_permission(self: &Arc<Self>, request_key: &str, option_id: &str) -> bool {
        let host = self.guest_perms.lock().unwrap().remove(request_key);
        let Some(host) = host else {
            return false;
        };
        self.spawn_send(
            host,
            "roaming.permission_response",
            json!({ "requestKey": request_key, "optionId": option_id }),
        );
        let _ = self
            .app
            .emit(EV_PERMISSION_RESOLVED, json!({ "requestKey": request_key }));
        true
    }

    fn roaming_route(&self, thread_id: &str) -> Result<(String, String), String> {
        let state = self.app.state::<AppState>();
        let store = state.store.lock().unwrap();
        let t = store.get(thread_id).ok_or("线程不存在")?;
        let peer = t.roaming_peer.clone().ok_or("不是漫游会话")?;
        let host_thread_id = t.roaming_remote_id.clone().ok_or("漫游会话尚未建立")?;
        Ok((peer, host_thread_id))
    }

    /// guest：检测到 update 序号缺口时立即请求一次重同步（按会话节流，避免缺口风暴）。
    /// 比看门狗(数秒)更快收敛「思考残缺 / 命令卡 loading」。
    fn request_resync(&self, thread_id: &str) {
        {
            let mut last = self.last_resync.lock().unwrap();
            if let Some(t) = last.get(thread_id) {
                if t.elapsed() < Duration::from_millis(1200) {
                    return; // 刚请求过，节流
                }
            }
            last.insert(thread_id.to_string(), Instant::now());
        }
        let Ok((peer, host_thread_id)) = self.roaming_route(thread_id) else {
            return;
        };
        self.spawn_send_now(
            peer,
            "roaming.resync",
            json!({ "hostThreadId": host_thread_id, "guestThreadId": thread_id }),
        );
    }

    fn resync_guest_threads(&self) {
        let routes: Vec<(String, String, String)> = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            store
                .threads
                .iter()
                .filter(|t| t.is_roaming_guest())
                .filter_map(|t| {
                    let peer = t.roaming_peer.clone()?;
                    let host = t.roaming_remote_id.clone()?;
                    Some((t.id.clone(), peer, host))
                })
                .collect()
        };
        for (guest_thread_id, peer, host_thread_id) in routes {
            let _ = self.send_blocking(
                &peer,
                "roaming.resync",
                json!({ "hostThreadId": host_thread_id, "guestThreadId": guest_thread_id }),
            );
        }
    }

    fn send_blocking(&self, to: &str, kind: &str, data: Value) -> Result<(), String> {
        // 进顺序队列（名字保留兼容调用处）
        self.enqueue(to.to_string(), kind, data);
        Ok(())
    }

    // ===== 漫游：host 侧 =====

    fn on_roaming_create(&self, env: &InEnvelope) {
        let req_id = env.data["reqId"].as_str().unwrap_or_default().to_string();
        let guest_thread_id = env.data["guestThreadId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let folder = env.data["folder"].as_str().unwrap_or_default().to_string();
        if folder.trim().is_empty() {
            self.spawn_send_now(
                env.from.clone(),
                "roaming.created",
                json!({ "reqId": req_id, "ok": false, "error": "未指定目录" }),
            );
            return;
        }
        if !self.roaming_folder_allowed(&folder) {
            self.spawn_send_now(
                env.from.clone(),
                "roaming.created",
                json!({ "reqId": req_id, "ok": false, "error": "该目录未开放漫游" }),
            );
            return;
        }
        let agent_kind: AgentKind =
            serde_json::from_value(env.data["agentKind"].clone()).unwrap_or(AgentKind::Devin);
        let agent_kind_str = agent_kind.as_str();
        let model = env.data["model"].as_str().map(|s| s.to_string());
        let mode = env.data["mode"].as_str().map(|s| s.to_string());
        let clue_context = match env.data.get("clueContext") {
            Some(value) if !value.is_null() => {
                match serde_json::from_value::<ClueContextSnapshot>(value.clone()) {
                    Ok(context) => Some(context),
                    Err(_) => {
                        self.spawn_send_now(
                            env.from.clone(),
                            "roaming.created",
                            json!({ "reqId": req_id, "ok": false, "error": "证据链上下文格式无效" }),
                        );
                        return;
                    }
                }
            }
            _ => None,
        };
        let worktree = env.data["worktree"].as_bool().unwrap_or(false);
        let worktree_branch = env.data["worktreeBranch"].as_str().map(|s| s.to_string());
        let worktree_base = env.data["worktreeBase"].as_str().map(|s| s.to_string());
        // 发起人首条提示词（仅展示用），去掉首尾空白
        let prompt = env.data["prompt"]
            .as_str()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        self.log(format!(
            "[relay] 收到来自 {} 的漫游请求：{}",
            env.from_name, folder
        ));

        // 登记为待确认请求，由本机用户在弹框里决定
        self.incoming_roams.lock().unwrap().insert(
            req_id.clone(),
            PendingRoam {
                from: env.from.clone(),
                from_name: env.from_name.clone(),
                guest_thread_id,
                folder: folder.clone(),
                agent_kind: agent_kind.clone(),
                model,
                mode,
                clue_context,
                worktree,
                worktree_branch: worktree_branch.clone(),
                worktree_base,
                prompt: prompt.clone(),
                host_thread_id: None,
                images: Vec::new(),
            },
        );
        let _ = self.app.emit(
            EV_RELAY_ROAM_REQUEST,
            json!({
                "reqId": req_id,
                "from": env.from,
                "fromName": env.from_name,
                "folder": folder,
                "folderName": basename(&folder),
                "agentKind": agent_kind_str,
                "folderExists": true,
                // 发起人想做什么：让本机用户能看清提示词再决定是否放行
                "prompt": prompt,
                // worktree：确认框提示「对方要求在 worktree 中执行」
                "worktree": worktree,
                "worktreeBranch": worktree_branch,
                "worktreeBase": env.data["worktreeBase"].clone(),
                "model": env.data["model"].clone(),
                "mode": env.data["mode"].clone(),
                "continuation": false,
            }),
        );
        self.notify_roam_request(&env.from_name, &basename(&folder));
    }

    /// host 侧：本机用户对漫游请求的应答（接受则建会话并回执，拒绝则回拒绝）。
    /// 建会话 +（大仓库）worktree 创建可能较慢，放后台线程执行，避免卡住本机「允许」操作；
    /// guest 侧本就异步等待 roaming.created 回执，无需改动。
    pub fn respond_roam_request(
        self: &Arc<Self>,
        req_id: &str,
        accept: bool,
        prompt: Option<String>,
        folder: Option<String>,
        model: Option<String>,
        mode: Option<String>,
        worktree: Option<bool>,
        worktree_branch: Option<String>,
        worktree_base: Option<String>,
    ) -> Result<(), String> {
        let mut pending = self
            .incoming_roams
            .lock()
            .unwrap()
            .remove(req_id)
            .ok_or("漫游请求已失效")?;
        if !accept {
            if pending.host_thread_id.is_some() {
                self.spawn_send_now(
                    pending.from.clone(),
                    "roaming.error",
                    json!({ "guestThreadId": pending.guest_thread_id, "error": "对方拒绝了本轮漫游授权" }),
                );
            } else {
                self.spawn_send_now(
                    pending.from.clone(),
                    "roaming.created",
                    json!({ "reqId": req_id, "ok": false, "error": "对方拒绝了漫游请求" }),
                );
            }
            return Ok(());
        }
        pending.prompt = prompt
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(host_thread_id) = pending.host_thread_id.clone() {
            let Some(text) = pending.prompt else {
                return Err("提示词不能为空".into());
            };
            if let Some(guest) = self.hosted.lock().unwrap().get_mut(&host_thread_id) {
                guest.approved_until = Instant::now() + ROAM_AUTHORIZATION_DURATION;
            } else {
                return Err("漫游会话已失效".into());
            }
            self.run_roaming_prompt(host_thread_id, text, pending.images);
            return Ok(());
        }
        if let Some(folder) = folder {
            pending.folder = folder.trim().to_string();
        }
        pending.model = model.filter(|value| !value.trim().is_empty());
        pending.mode = mode.filter(|value| !value.trim().is_empty());
        pending.worktree = worktree.unwrap_or(pending.worktree);
        pending.worktree_branch = worktree_branch.filter(|value| !value.trim().is_empty());
        pending.worktree_base = worktree_base.filter(|value| !value.trim().is_empty());
        let this = Arc::clone(self);
        let req_id = req_id.to_string();
        std::thread::spawn(move || this.finish_roam_accept(req_id, pending));
        Ok(())
    }

    /// host 侧：后台完成漫游会话创建（含 worktree），成败都回 roaming.created。
    fn finish_roam_accept(self: &Arc<Self>, req_id: String, pending: PendingRoam) {
        // 请求等待确认期间用户可能撤销目录授权，最终落地前必须再次校验。
        if !self.roaming_folder_allowed(&pending.folder) {
            self.spawn_send_now(
                pending.from.clone(),
                "roaming.created",
                json!({ "reqId": req_id, "ok": false, "error": "该目录已取消漫游授权" }),
            );
            return;
        }
        let mut thread = Thread::new(
            pending.folder.clone(),
            pending.agent_kind.clone(),
            pending.model.clone(),
            pending.mode.clone(),
            None,
            false,
        );
        thread.roaming_role = Some("host".into());
        thread.roaming_peer = Some(pending.from.clone());
        thread.roaming_peer_name = Some(pending.from_name.clone());
        thread.roaming_remote_id = Some(pending.guest_thread_id.clone());
        if let Some(context) = pending.clue_context {
            thread.active_clue_card_id = Some(context.root_card_id.clone());
            thread.clue_context = Some(context);
        }
        thread.title = format!("漫游@{} · {}", pending.from_name, basename(&pending.folder));
        let host_thread_id = thread.id.clone();
        // worktree：host 在自己仓库为这次漫游创建独立工作目录 + 分支，会话在其中执行。
        // 失败（非 git 仓库/分支冲突等）则回执失败，不建会话。
        if pending.worktree {
            let state = self.app.state::<AppState>();
            match crate::create_worktree_for(
                state.inner(),
                &pending.folder,
                pending.worktree_branch.as_deref(),
                pending.worktree_base.as_deref(),
                Some(host_thread_id.clone()),
                true,
            ) {
                Ok(wt) => {
                    thread.cwd = wt.path.clone();
                    thread.worktree = Some(wt);
                }
                Err(e) => {
                    self.spawn_send_now(
                        pending.from.clone(),
                        "roaming.created",
                        json!({ "reqId": req_id, "ok": false, "error": format!("创建 worktree 失败：{e}") }),
                    );
                    return;
                }
            }
        }
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            store.threads.push(thread);
            store.save();
        }
        self.hosted.lock().unwrap().insert(
            host_thread_id.clone(),
            RoamGuest {
                token: pending.from.clone(),
                guest_thread_id: pending.guest_thread_id.clone(),
                approved_until: Instant::now() + ROAM_AUTHORIZATION_DURATION,
            },
        );
        let _ = self.app.emit(EV_THREADS, json!({}));
        self.spawn_send_now(
            pending.from.clone(),
            "roaming.created",
            json!({
                "reqId": req_id,
                "ok": true,
                "hostThreadId": host_thread_id,
                "approvedPrompt": pending.prompt,
            }),
        );
    }

    /// 收到漫游请求时提醒本机用户。漫游是需要立即决策的授权事件，必须让人明确感知：
    /// 无论前台后台都把主窗口恢复可见并请求用户注意（任务栏闪烁），保证审批确认框不被
    /// 错过；仅在窗口未聚焦时再补一条系统通知（点击唤起），聚焦时确认框已直接可见。
    fn notify_roam_request(&self, from_name: &str, folder_name: &str) {
        crate::sys_notify::notify_roam_request(&self.app, from_name, folder_name);
    }

    fn roaming_folder_allowed(&self, folder: &str) -> bool {
        let state = self.app.state::<AppState>();
        let allowed = crate::current_roaming_project_folders(state.inner());
        is_allowed_roaming_path(&allowed, folder)
    }

    fn on_roaming_prompt(&self, env: &InEnvelope) {
        let host_thread_id = env.data["hostThreadId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if !self.ensure_hosted(&host_thread_id) {
            return;
        }
        let text = env.data["text"].as_str().unwrap_or_default().to_string();
        let images: Vec<PromptImage> =
            serde_json::from_value(env.data["images"].clone()).unwrap_or_default();
        let guest = self.hosted.lock().unwrap().get(&host_thread_id).cloned();
        let Some(guest) = guest.filter(|guest| guest.token == env.from) else {
            return;
        };
        if guest.approved_until <= Instant::now() {
            let req_id = uuid::Uuid::new_v4().to_string();
            let (folder, agent_kind, model, mode) = {
                let state = self.app.state::<AppState>();
                let store = state.store.lock().unwrap();
                let Some(thread) = store.get(&host_thread_id) else {
                    return;
                };
                (
                    thread.cwd.clone(),
                    thread.agent_kind.clone(),
                    thread.model.clone(),
                    thread.mode.clone(),
                )
            };
            self.incoming_roams.lock().unwrap().insert(
                req_id.clone(),
                PendingRoam {
                    from: env.from.clone(),
                    from_name: env.from_name.clone(),
                    guest_thread_id: guest.guest_thread_id,
                    folder: folder.clone(),
                    agent_kind: agent_kind.clone(),
                    model: model.clone(),
                    mode: mode.clone(),
                    clue_context: None,
                    worktree: false,
                    worktree_branch: None,
                    worktree_base: None,
                    prompt: Some(text.clone()),
                    host_thread_id: Some(host_thread_id),
                    images,
                },
            );
            let _ = self.app.emit(
                EV_RELAY_ROAM_REQUEST,
                json!({
                    "reqId": req_id,
                    "from": env.from,
                    "fromName": env.from_name,
                    "folder": folder,
                    "folderName": basename(&folder),
                    "agentKind": agent_kind.as_str(),
                    "prompt": text,
                    "model": model,
                    "mode": mode,
                    "continuation": true,
                }),
            );
            self.notify_roam_request(&env.from_name, &basename(&folder));
            return;
        }
        self.run_roaming_prompt(host_thread_id, text, images);
    }

    fn run_roaming_prompt(&self, host_thread_id: String, text: String, images: Vec<PromptImage>) {
        let agent_kind = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            match store.get(&host_thread_id) {
                Some(t) => t.agent_kind.clone(),
                None => return,
            }
        };
        let state = self.app.state::<AppState>();
        let prompt_epoch = {
            let mut epochs = self.host_prompt_epochs.lock().unwrap();
            let epoch = epochs
                .entry(host_thread_id.clone())
                .or_insert_with(|| Arc::new(AtomicU64::new(0)))
                .clone();
            let value = epoch.load(Ordering::SeqCst);
            (epoch, value)
        };
        match agent_kind {
            AgentKind::OpenCode | AgentKind::OpenCodePlus => {
                let mgr = state.opencodeplus.clone();
                tauri::async_runtime::spawn(async move {
                    if host_prompt_is_current(&prompt_epoch) {
                        mgr.run_prompt(host_thread_id, text, images).await;
                    }
                });
            }
            AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => {
                let mgr = state.codebuddyplus.clone();
                tauri::async_runtime::spawn(async move {
                    if !host_prompt_is_current(&prompt_epoch) {
                        return;
                    }
                    mgr.run_prompt(host_thread_id, text, images).await;
                });
            }
            AgentKind::Codex | AgentKind::CodexPlus => {
                let mgr = state.codexplus.clone();
                tauri::async_runtime::spawn(async move {
                    if host_prompt_is_current(&prompt_epoch) {
                        mgr.run_prompt(host_thread_id, text, images).await;
                    }
                });
            }
            AgentKind::Devin => {
                let mgr = state.acp.clone();
                if mgr.is_running(&host_thread_id) {
                    tauri::async_runtime::spawn(async move {
                        if !host_prompt_is_current(&prompt_epoch) {
                            return;
                        }
                        mgr.steer_prompt(host_thread_id, text, images).await;
                    });
                } else {
                    tauri::async_runtime::spawn(async move {
                        if !host_prompt_is_current(&prompt_epoch) {
                            return;
                        }
                        mgr.run_prompt(host_thread_id, text, images).await;
                    });
                }
            }
            AgentKind::ClaudeCode => {
                let mgr = state.claudeplus.clone();
                tauri::async_runtime::spawn(async move {
                    if host_prompt_is_current(&prompt_epoch) {
                        mgr.run_prompt(host_thread_id, text, images).await;
                    }
                });
            }
            AgentKind::Cursor => {
                let mgr = state.cursorplus.clone();
                tauri::async_runtime::spawn(async move {
                    if host_prompt_is_current(&prompt_epoch) {
                        mgr.run_prompt(host_thread_id, text, images).await;
                    }
                });
            }
        }
    }

    fn on_roaming_cancel(&self, env: &InEnvelope) {
        let host_thread_id = env.data["hostThreadId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let agent_kind = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            store.get(&host_thread_id).map(|t| t.agent_kind.clone())
        };
        let Some(agent_kind) = agent_kind else {
            return;
        };
        {
            let mut epochs = self.host_prompt_epochs.lock().unwrap();
            epochs
                .entry(host_thread_id.clone())
                .or_insert_with(|| Arc::new(AtomicU64::new(0)))
                .fetch_add(1, Ordering::SeqCst);
        }
        let state = self.app.state::<AppState>();
        if !crate::is_running(
            &state,
            state.store.lock().unwrap().get(&host_thread_id).unwrap(),
        ) {
            self.forward_local_turn(&host_thread_id, false, &json!("force_cancelled"));
            return;
        }
        match agent_kind {
            AgentKind::Devin => {
                let mgr = state.acp.clone();
                tauri::async_runtime::spawn(async move { mgr.cancel(&host_thread_id).await });
            }
            AgentKind::Codex | AgentKind::CodexPlus => {
                let mgr = state.codexplus.clone();
                tauri::async_runtime::spawn(async move { mgr.cancel(&host_thread_id).await });
            }
            AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => {
                let mgr = state.codebuddyplus.clone();
                tauri::async_runtime::spawn(async move { mgr.cancel(&host_thread_id).await });
            }
            AgentKind::ClaudeCode => {
                let mgr = state.claudeplus.clone();
                tauri::async_runtime::spawn(async move { mgr.cancel(&host_thread_id).await });
            }
            AgentKind::Cursor => {
                let mgr = state.cursorplus.clone();
                tauri::async_runtime::spawn(async move { mgr.cancel(&host_thread_id).await });
            }
            AgentKind::OpenCode | AgentKind::OpenCodePlus => {
                let mgr = state.opencodeplus.clone();
                tauri::async_runtime::spawn(async move { mgr.cancel(&host_thread_id).await });
            }
        };
    }

    /// host：guest 主动召回漫游会话。校验请求者确实是该会话的漫游对端后，
    /// 自动把完整会话快照 Flow 回去（等价于本机用户手动分享给对方）。
    fn on_roaming_recall(&self, env: &InEnvelope) {
        let host_thread_id = env.data["hostThreadId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let guest_thread_id = env.data["guestThreadId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let valid = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            store.get(&host_thread_id).is_some_and(|t| {
                t.roaming_role.as_deref() == Some("host")
                    && t.roaming_peer.as_deref() == Some(env.from.as_str())
            })
        };
        if !valid {
            self.spawn_send_now(
                env.from.clone(),
                "roaming.error",
                json!({
                    "guestThreadId": guest_thread_id,
                    "error": "召回失败：对方机器上找不到该漫游会话（可能已被删除）",
                }),
            );
            return;
        }
        if let Err(e) = self.send_share(&host_thread_id, &env.from, true) {
            self.spawn_send_now(
                env.from.clone(),
                "roaming.error",
                json!({ "guestThreadId": guest_thread_id, "error": format!("召回失败：{e}") }),
            );
            return;
        }
        self.log(format!(
            "[relay] {} 召回了漫游会话，已自动回传快照",
            env.from_name
        ));
        // host 侧会话里留痕，让本机用户知道对方已把会话拿回去了
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            if let Some(t) = store.get_mut(&host_thread_id) {
                let item = t.push_system(
                    format!(
                        "📤 {} 已召回该会话（完整快照已自动 Flow 回对方）",
                        env.from_name
                    ),
                    "info",
                );
                store.save();
                self.emit_update(&host_thread_id, json!({ "t": "upsert", "item": item }));
            }
        }
    }

    fn on_roaming_permission_response(&self, env: &InEnvelope) {
        let request_key = env.data["requestKey"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let option_id = env.data["optionId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let state = self.app.state::<AppState>();
        if request_key.starts_with("cbp-") {
            let mgr = state.codebuddyplus.clone();
            tauri::async_runtime::spawn(async move {
                let _ = mgr.respond_permission(&request_key, &option_id).await;
            });
        } else if request_key.starts_with("clp-") {
            let mgr = state.claudeplus.clone();
            tauri::async_runtime::spawn(async move {
                let _ = mgr.respond_permission(&request_key, &option_id).await;
            });
        } else if request_key.starts_with("ocp-") {
            let mgr = state.opencodeplus.clone();
            tauri::async_runtime::spawn(async move {
                let _ = mgr.respond_permission(&request_key, &option_id).await;
            });
        } else if request_key.starts_with("codex-") {
            let mgr = state.codex.clone();
            tauri::async_runtime::spawn(async move {
                let _ = mgr.respond_permission(&request_key, &option_id).await;
            });
        } else {
            let mgr = state.acp.clone();
            tauri::async_runtime::spawn(async move {
                let _ = mgr.respond_permission(&request_key, &option_id).await;
            });
        }
    }

    fn on_roaming_config(&self, env: &InEnvelope) {
        let host_thread_id = env.data["hostThreadId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if !self.ensure_hosted(&host_thread_id) {
            return;
        }
        let model = env.data["model"].as_str().map(|s| s.to_string());
        let mode = env.data["mode"].as_str().map(|s| s.to_string());
        let agent_kind = {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            let Some(t) = store.get_mut(&host_thread_id) else {
                return;
            };
            if t.model != model {
                t.clear_auto_route();
            }
            t.model = model;
            t.mode = mode;
            let ak = t.agent_kind.clone();
            store.save();
            ak
        };
        let state = self.app.state::<AppState>();
        if agent_kind == AgentKind::Devin {
            let mgr = state.acp.clone();
            tauri::async_runtime::spawn(async move {
                mgr.sync_thread_config(&host_thread_id).await;
            });
        }
    }

    fn on_roaming_resync(&self, env: &InEnvelope) {
        let host_thread_id = env.data["hostThreadId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        self.ensure_hosted(&host_thread_id);
        let guest = self.hosted.lock().unwrap().get(&host_thread_id).cloned();
        let Some(guest) = guest else {
            // 同一 token 可能同时登录正式版、开发版等多个 Nova 实例，中转站会把
            // 重同步请求广播给全部实例。没有该会话的实例必须保持沉默，否则会把
            // 另一个仍在正常执行的实例误报成「会话已结束」。
            return;
        };
        self.send_snapshot(&host_thread_id, &guest);
    }

    fn send_snapshot(&self, host_thread_id: &str, guest: &RoamGuest) {
        let (items, plan, running) = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            let Some(t) = store.get(host_thread_id) else {
                return;
            };
            let running = crate::is_running(&state, t);
            // 用户消息的图片/文件内容 guest 本地已有，快照里剥掉 data 省带宽：
            // guest 会按顺序用自己保留的用户条目回填（含附件内容）。
            let mut items = t.items.clone();
            for it in items.iter_mut() {
                if let Item::User { images, .. } = it {
                    for img in images.iter_mut() {
                        img.data = None;
                    }
                }
            }
            (
                serde_json::to_value(&items).unwrap_or(json!([])),
                t.plan.clone().unwrap_or(json!(null)),
                running,
            )
        };
        self.spawn_send_now(
            guest.token.clone(),
            "roaming.snapshot",
            json!({
                "guestThreadId": guest.guest_thread_id,
                "items": items,
                "plan": plan,
                "running": running,
            }),
        );
    }

    /// 记录某漫游会话刚收到一次 host 事件（喂看门狗）
    fn touch_guest_activity(&self, thread_id: &str) {
        self.guest_activity
            .lock()
            .unwrap()
            .insert(thread_id.to_string(), Instant::now());
    }

    /// guest 看门狗：周期性对「自认为还在运行」的漫游会话请求一次重同步。
    /// host 会回一份当前快照（含真实运行态），用于自愈：
    /// 1) 长轮次中途丢失/卡住的增量（命令卡 loading、思考残缺）；
    /// 2) 轮次结束事件丢失导致的「任务永远 loading」——host 已不在跑会回 running=false。
    /// 由 guest 自身运行态驱动，turn 正常结束后不再触发，无多余流量。
    fn resync_running_guests(&self) {
        if !self.connected.load(Ordering::SeqCst) {
            return;
        }
        let running: Vec<String> = {
            let set = self.guest_running.lock().unwrap();
            if set.is_empty() {
                return;
            }
            set.iter().cloned().collect()
        };
        // 只补救「卡住」的会话：运行中但已数秒没收到任何 host 事件。健康轮次里
        // 增量持续到达，不会触发，避免多余的快照流量与重渲染。
        let now = Instant::now();
        let stale: Vec<String> = {
            let act = self.guest_activity.lock().unwrap();
            running
                .into_iter()
                .filter(|id| {
                    act.get(id)
                        .map(|t| now.duration_since(*t) >= Duration::from_secs(4))
                        .unwrap_or(true)
                })
                .collect()
        };
        if stale.is_empty() {
            return;
        }
        let routes: Vec<(String, String, String)> = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            stale
                .iter()
                .filter_map(|id| {
                    let t = store.get(id)?;
                    if !t.is_roaming_guest() {
                        return None;
                    }
                    Some((
                        id.clone(),
                        t.roaming_peer.clone()?,
                        t.roaming_remote_id.clone()?,
                    ))
                })
                .collect()
        };
        for (guest_thread_id, peer, host_thread_id) in routes {
            self.spawn_send_now(
                peer,
                "roaming.resync",
                json!({ "hostThreadId": host_thread_id, "guestThreadId": guest_thread_id }),
            );
        }
    }

    /// lib.rs 的事件监听回调：本机 thread 的 update 若属于被漫游的会话，转发给 guest
    pub fn forward_local_update(&self, thread_id: &str, op: &Value) {
        // user 消息由 guest 本地已显示，不再回传（避免重复 + id 冲突）
        if op["t"].as_str() == Some("upsert") && op["item"]["type"].as_str() == Some("user") {
            // 但 host 自己发起的（理论上不会）仍跳过即可
            return;
        }
        let guest = self.hosted.lock().unwrap().get(thread_id).cloned();
        let Some(guest) = guest else {
            return;
        };
        self.spawn_send_now(
            guest.token,
            "roaming.update",
            json!({ "guestThreadId": guest.guest_thread_id, "op": op }),
        );
    }

    /// host 侧生成了 AI 标题，同步给 guest，让漫游会话标题和本机一致（带打字机动画）
    pub fn forward_local_title(&self, thread_id: &str) {
        let guest = self.hosted.lock().unwrap().get(thread_id).cloned();
        let Some(guest) = guest else {
            return;
        };
        let title = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            match store.get(thread_id) {
                Some(t) => t.title.clone(),
                None => return,
            }
        };
        self.spawn_send_now(
            guest.token,
            "roaming.title",
            json!({ "guestThreadId": guest.guest_thread_id, "title": title }),
        );
    }

    fn on_roaming_title(&self, env: &InEnvelope) {
        let thread_id = env.data["guestThreadId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let title = env.data["title"].as_str().unwrap_or_default().to_string();
        if thread_id.is_empty() || title.is_empty() {
            return;
        }
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            let Some(t) = store.get_mut(&thread_id) else {
                return;
            };
            t.title = title;
            store.save();
        }
        let _ = self.app.emit(crate::acp::EV_THREADS, json!({}));
        let _ = self.app.emit(
            crate::acp::EV_TITLE_GENERATED,
            json!({ "threadId": thread_id }),
        );
    }

    pub fn forward_local_turn(&self, thread_id: &str, running: bool, stop_reason: &Value) {
        let guest = self.hosted.lock().unwrap().get(thread_id).cloned();
        let Some(guest) = guest else {
            return;
        };
        // 轮次结束时附带一份完整快照，让 guest 收敛到与 host 完全一致的最终状态，
        // 自愈流式期间可能丢失/乱序的增量（思考残缺、命令/任务卡 loading 等）。
        // 快照在 turn 之前入队（FIFO），guest 先重建条目再收尾运行态。
        if !running {
            self.send_snapshot(thread_id, &guest);
        }
        self.spawn_send_now(
            guest.token,
            "roaming.turn",
            json!({ "guestThreadId": guest.guest_thread_id, "running": running, "stopReason": stop_reason }),
        );
    }

    pub fn forward_local_permission(&self, thread_id: &str, payload: &Value) {
        let guest = self.hosted.lock().unwrap().get(thread_id).cloned();
        let Some(guest) = guest else {
            return;
        };
        // 把 threadId 改成 guest 侧 id，requestKey 原样回传
        let mut p = payload.clone();
        p["threadId"] = json!(guest.guest_thread_id);
        self.spawn_send_now(guest.token, "roaming.permission", p);
    }

    pub fn forward_local_permission_resolved(&self, request_key: &str) {
        // 不知道属于哪个线程：广播给所有 hosted guest（requestKey 唯一，guest 侧自行匹配）
        let guests: Vec<RoamGuest> = self.hosted.lock().unwrap().values().cloned().collect();
        for g in guests {
            self.spawn_send_now(
                g.token,
                "roaming.permission_resolved",
                json!({ "requestKey": request_key }),
            );
        }
    }

    pub fn is_hosted(&self, thread_id: &str) -> bool {
        self.hosted.lock().unwrap().contains_key(thread_id)
    }

    /// host 明确删除漫游会话时通知 guest。只有持有该会话映射的实例会发送，
    /// 避免同 token 的其他实例对重同步请求作出错误终止判断。
    pub fn notify_host_thread_deleted(&self, thread_id: &str) {
        self.ensure_hosted(thread_id);
        self.host_prompt_epochs.lock().unwrap().remove(thread_id);
        let guest = self.hosted.lock().unwrap().remove(thread_id);
        let Some(guest) = guest else {
            return;
        };
        self.spawn_send_now(
            guest.token,
            "roaming.error",
            json!({
                "guestThreadId": guest.guest_thread_id,
                "error": "对端会话已删除，请重新发起漫游",
            }),
        );
    }

    /// 确保 host 端 guest 映射存在。漫游会话现在会持久化，重启/重连后内存里的
    /// hosted 映射会丢，这里按需从持久化的 host 线程恢复，保证能继续接收/转发。
    fn ensure_hosted(&self, host_thread_id: &str) -> bool {
        if self.hosted.lock().unwrap().contains_key(host_thread_id) {
            return true;
        }
        let (token, guest_thread_id) = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            let Some(t) = store.get(host_thread_id) else {
                return false;
            };
            if t.roaming_role.as_deref() != Some("host") {
                return false;
            }
            match (t.roaming_peer.clone(), t.roaming_remote_id.clone()) {
                (Some(tok), Some(gid)) => (tok, gid),
                _ => return false,
            }
        };
        self.hosted.lock().unwrap().insert(
            host_thread_id.to_string(),
            RoamGuest {
                token,
                guest_thread_id,
                // 重启后不沿用旧授权，下一条提示词必须重新审批。
                approved_until: Instant::now(),
            },
        );
        true
    }

    /// 启动/重连时，从持久化的 host 线程批量恢复 hosted 映射。
    pub fn rebuild_hosted(&self) {
        let ids: Vec<String> = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            store
                .threads
                .iter()
                .filter(|t| t.roaming_role.as_deref() == Some("host"))
                .map(|t| t.id.clone())
                .collect()
        };
        for id in ids {
            self.ensure_hosted(&id);
        }
    }

    fn spawn_send_now(&self, to: String, kind: &str, data: Value) {
        self.enqueue(to, kind, data);
    }

    // ===== 漫游：guest 侧收到的事件 =====

    fn on_roaming_created(&self, env: &InEnvelope) {
        let req_id = env.data["reqId"].as_str().unwrap_or_default().to_string();
        let Some(guest_thread_id) = self.pending_creates.lock().unwrap().remove(&req_id) else {
            return;
        };
        if !env.data["ok"].as_bool().unwrap_or(false) {
            // host 拒绝/失败：在会话里提示，并清掉排队的提示词（不再把会话整个删掉，
            // 因为用户可能已经进入并输入了内容）
            let msg = env.data["error"]
                .as_str()
                .unwrap_or("对方拒绝了漫游请求")
                .to_string();
            self.pending_prompts
                .lock()
                .unwrap()
                .remove(&guest_thread_id);
            self.set_guest_running(&guest_thread_id, false);
            {
                let state = self.app.state::<AppState>();
                let mut store = state.store.lock().unwrap();
                if let Some(t) = store.get_mut(&guest_thread_id) {
                    // host 侧没建成（含 worktree 创建失败）：清掉本地 worktree 标记，避免误导
                    t.worktree = None;
                    let item = t.push_system_local(msg, "error");
                    store.save();
                    self.emit_update(&guest_thread_id, json!({ "t": "upsert", "item": item }));
                }
            }
            let _ = self.app.emit(
                EV_TURN,
                json!({ "threadId": guest_thread_id, "running": false, "stopReason": "error" }),
            );
            return;
        }
        let host_thread_id = env.data["hostThreadId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        self.touch_guest_activity(&guest_thread_id);
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            if let Some(t) = store.get_mut(&guest_thread_id) {
                t.roaming_remote_id = Some(host_thread_id.clone());
                let item = t.push_system_local("✓ 对方已接受，开始漫游".into(), "info");
                store.save();
                self.emit_update(&guest_thread_id, json!({ "t": "upsert", "item": item }));
            }
        }
        // 补发 host 建好之前用户排队的提示词
        let peer = env.from.clone();
        let queued = self
            .pending_prompts
            .lock()
            .unwrap()
            .remove(&guest_thread_id)
            .unwrap_or_default();
        let approved_prompt = env.data["approvedPrompt"].as_str();
        for (index, (text, images)) in queued.into_iter().enumerate() {
            let text = if index == 0 {
                approved_prompt.unwrap_or(&text).to_string()
            } else {
                text
            };
            self.spawn_send_now(
                peer.clone(),
                "roaming.prompt",
                json!({
                    "hostThreadId": host_thread_id,
                    "guestThreadId": guest_thread_id,
                    "text": text,
                    "images": images,
                }),
            );
        }
    }

    fn on_roaming_update(&self, env: &InEnvelope) {
        let thread_id = env.data["guestThreadId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if thread_id.is_empty() {
            return;
        }
        // 支持批量：data.ops 是数组（合并发送），或单条 data.op
        let ops: Vec<Value> = if let Some(arr) = env.data["ops"].as_array() {
            arr.clone()
        } else if !env.data["op"].is_null() {
            vec![env.data["op"].clone()]
        } else {
            return;
        };
        if ops.is_empty() {
            return;
        }
        // 序号去重 / 缺口检测：
        // - seq <= 已应用：重复消息（含重传），直接丢弃，避免 delta 被追加两次（思考变乱）。
        // - seq 跳变（> 已应用+1）：中间丢了消息，立即请求一次重同步收敛，无需等看门狗。
        //   仍照常应用当前批（快照会覆盖纠正），尽量减少展示延迟。
        if let Some(seq) = env.data.get("seq").and_then(|v| v.as_i64()) {
            let mut map = self.in_seq.lock().unwrap();
            let last = map.get(&thread_id).copied().unwrap_or(0);
            if seq <= last {
                // 重复（重传）直接丢弃，避免 delta 被追加两次。但若是 host 重启后序号
                // 回退（新一轮从 1 重新计数），靠节流的重同步快速对齐，无需等看门狗。
                drop(map);
                self.request_resync(&thread_id);
                return;
            }
            let gap = seq > last + 1;
            map.insert(thread_id.clone(), seq);
            drop(map);
            if gap {
                self.request_resync(&thread_id);
            }
        }
        self.touch_guest_activity(&thread_id);
        // 一次性应用整批；落盘做节流（流式期间每条增量都把整个 store 写盘会拖慢
        // WebSocket 消费，导致中转缓冲溢出丢消息、卡 loading、思考残缺）。最终一致性由
        // 轮次结束快照 + 重连重同步兜底，所以这里漏存几条流式增量是安全的。
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            {
                let Some(t) = store.get_mut(&thread_id) else {
                    return;
                };
                for op in &ops {
                    apply_op_to_thread(t, op);
                }
            }
            let mut last = self.last_store_save.lock().unwrap();
            if last.elapsed() >= Duration::from_millis(1500) {
                store.save();
                *last = Instant::now();
            }
        }
        if ops.len() == 1 {
            self.emit_update(&thread_id, ops.into_iter().next().unwrap());
        } else {
            // 同 emit_update：只给前台会话推批。后台会话已落库，切回时快照补齐。
            let active = {
                let state = self.app.state::<AppState>();
                let a = state.active_thread.lock().unwrap();
                a.as_deref() == Some(thread_id.as_str())
            };
            if active {
                let _ = self
                    .app
                    .emit(EV_UPDATE, json!({ "threadId": thread_id, "ops": ops }));
            }
        }
    }

    fn on_roaming_turn(&self, env: &InEnvelope) {
        let thread_id = env.data["guestThreadId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let running = env.data["running"].as_bool().unwrap_or(false);
        self.touch_guest_activity(&thread_id);
        self.set_guest_running(&thread_id, running);
        if !running {
            // 轮次结束：强制落盘一次，确保最终状态持久化（流式期间是节流落盘）
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            store.save();
            *self.last_store_save.lock().unwrap() = Instant::now();
        }
        let _ = self.app.emit(
            EV_TURN,
            json!({
                "threadId": thread_id,
                "running": running,
                "stopReason": env.data["stopReason"].clone(),
            }),
        );
        let _ = self.app.emit(EV_THREADS, json!({}));
    }

    fn on_roaming_permission(&self, env: &InEnvelope) {
        let request_key = env.data["requestKey"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if request_key.is_empty() {
            return;
        }
        self.guest_perms
            .lock()
            .unwrap()
            .insert(request_key, env.from.clone());
        let _ = self.app.emit(EV_PERMISSION, env.data.clone());
    }

    fn on_roaming_permission_resolved(&self, env: &InEnvelope) {
        let request_key = env.data["requestKey"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        self.guest_perms.lock().unwrap().remove(&request_key);
        let _ = self
            .app
            .emit(EV_PERMISSION_RESOLVED, json!({ "requestKey": request_key }));
    }

    fn on_roaming_snapshot(&self, env: &InEnvelope) {
        let thread_id = env.data["guestThreadId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let snap_items: Vec<Item> =
            serde_json::from_value(env.data["items"].clone()).unwrap_or_default();
        let plan = env.data["plan"].clone();
        let running = env.data["running"].as_bool().unwrap_or(false);
        self.touch_guest_activity(&thread_id);
        self.set_guest_running(&thread_id, running);
        // 快照是全量真相：把序号基准对齐到快照携带的 seq，之后只接受更高序号的增量，
        // 丢弃迟到/重复的旧增量（避免快照后又被旧 delta 二次追加）。
        if let Some(seq) = env.data.get("seq").and_then(|v| v.as_i64()) {
            self.in_seq.lock().unwrap().insert(thread_id.clone(), seq);
        }
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            if let Some(t) = store.get_mut(&thread_id) {
                // 用 host 快照重建 agent 输出（思考/工具/回答），但要正确还原用户消息：
                // host 快照里的用户消息被剥掉了附件、用的是 host 侧小 id；guest 本地乐观
                // 插入的用户消息（高位 id）才保留了附件内容与真实发送顺序。
                //
                // 关键：按「发送顺序 + 文本」把本地用户消息映射回快照里对应的用户条目，
                // 而不是单纯按位置一一对应。这样能同时正确处理两种「显示乱掉」的情况：
                //   1) guest 连续追加提示词：本机已发出、host 还没在快照体现的提示词会
                //      被追加到末尾，而不是被整段快照抹掉（之前会丢消息）。
                //   2) host 端也往该会话补发了提示词：这些条目文本对不上本地，原样保留
                //      为 host 内容，不会被本地用户消息顶替而错位（之前会串位）。
                const LOCAL_ID_BASE: u64 = 1_000_000_000;
                let local_users: Vec<Item> = t
                    .items
                    .iter()
                    .filter(|i| matches!(i, Item::User { .. }) && i.id() >= LOCAL_ID_BASE)
                    .cloned()
                    .collect();
                let mut li = 0usize;
                let mut merged: Vec<Item> =
                    Vec::with_capacity(snap_items.len() + local_users.len());
                for it in snap_items {
                    if let Some(snap_text) = item_user_text(&it) {
                        // 文本与下一条待匹配的本地用户消息一致 → 用本地版本（恢复附件、稳定 id）
                        if li < local_users.len()
                            && item_user_text(&local_users[li]) == Some(snap_text)
                        {
                            merged.push(local_users[li].clone());
                            li += 1;
                            continue;
                        }
                    }
                    merged.push(it);
                }
                // 本地已发出、host 还没在快照里体现的提示词，按序补到末尾，避免被抹掉
                for leftover in local_users.into_iter().skip(li) {
                    merged.push(leftover);
                }
                t.items = merged;
                t.plan = if plan.is_null() { None } else { Some(plan) };
                t.updated_at = now_ms();
                store.save();
                *self.last_store_save.lock().unwrap() = Instant::now();
            } else {
                return;
            }
        }
        let _ = self
            .app
            .emit(EV_RELAY_RELOAD, json!({ "threadId": thread_id }));
        let _ = self.app.emit(
            EV_TURN,
            json!({ "threadId": thread_id, "running": running, "stopReason": null }),
        );
    }

    fn on_roaming_error(&self, env: &InEnvelope) {
        let thread_id = env.data["guestThreadId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let msg = env.data["error"].as_str().unwrap_or("漫游出错").to_string();
        self.set_guest_running(&thread_id, false);
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            if let Some(t) = store.get_mut(&thread_id) {
                let item = t.push_system(msg, "error");
                store.save();
                self.emit_update(&thread_id, json!({ "t": "upsert", "item": item }));
            }
        }
        let _ = self.app.emit(
            EV_TURN,
            json!({ "threadId": thread_id, "running": false, "stopReason": "error" }),
        );
    }

    fn emit_update(&self, thread_id: &str, op: Value) {
        // 同 acp.rs：只给前台正在查看的会话推流。漫游 guest 多开会话时，后台会话的
        // 增量若也广播到 WebView 只会被前端丢弃却仍全量反序列化，徒增 WebView2 内存压力。
        // 增量已落库，切回时经 get_thread 快照补齐。
        {
            let state = self.app.state::<AppState>();
            let active = state.active_thread.lock().unwrap();
            if active.as_deref() != Some(thread_id) {
                return;
            }
        }
        let _ = self
            .app
            .emit(EV_UPDATE, json!({ "threadId": thread_id, "op": op }));
    }

    fn persist_seq(&self, force: bool) {
        if !force {
            let mut last = self.last_persist.lock().unwrap();
            if last.elapsed() < Duration::from_millis(1000) {
                return;
            }
            *last = Instant::now();
        }
        let seq = self.last_seq.load(Ordering::SeqCst);
        let server_epoch = self.server_epoch.lock().unwrap().clone();
        write_relay_state(&self.config_dir, seq, &server_epoch);
    }

    fn log(&self, line: String) {
        let _ = self.app.emit(crate::acp::EV_LOG, line);
    }

    pub fn log_line(&self, line: String) {
        self.log(line);
    }
}

// ===== 自由函数 =====

fn apply_op_to_thread(thread: &mut Thread, op: &Value) {
    match op["t"].as_str() {
        Some("upsert") => {
            if let Ok(item) = serde_json::from_value::<Item>(op["item"].clone()) {
                let id = item.id();
                if let Some(slot) = thread.items.iter_mut().find(|i| i.id() == id) {
                    *slot = item;
                } else {
                    thread.items.push(item);
                }
            }
        }
        Some("remove") => {
            if let Some(id) = op["itemId"].as_u64() {
                thread.items.retain(|i| i.id() != id);
            }
        }
        Some("delta") => {
            if let (Some(id), Some(text)) = (op["itemId"].as_u64(), op["text"].as_str()) {
                for it in thread.items.iter_mut().rev() {
                    if it.id() == id {
                        append_item_text(it, text);
                        break;
                    }
                }
            }
        }
        Some("plan") => {
            thread.plan = Some(op["plan"].clone());
        }
        Some("mode") => {
            thread.mode = op["mode"].as_str().map(|s| s.to_string());
        }
        _ => {}
    }
    thread.updated_at = now_ms();
}

/// 取用户消息的文本（非用户条目返回 None）。漫游快照重同步时用于按文本把
/// guest 本地用户消息映射回 host 快照里的对应条目。
fn item_user_text(item: &Item) -> Option<&str> {
    match item {
        Item::User { text, .. } => Some(text.as_str()),
        _ => None,
    }
}

fn append_item_text(item: &mut Item, text: &str) {
    match item {
        Item::User { text: t, .. }
        | Item::Assistant { text: t, .. }
        | Item::Thought { text: t, .. }
        | Item::System { text: t, .. } => t.push_str(text),
        _ => {}
    }
}

fn derive_title(text: &str) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    first.chars().take(40).collect()
}

fn first_line(s: &str, n: usize) -> String {
    s.lines()
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .take(n)
        .collect()
}

/// 兼容旧版客户端的 deflate+base64 载荷；新版 HTTP 整体统一使用 gzip。
fn maybe_decompress(data: &mut Value) {
    let Some(z) = data.get("_z").and_then(|v| v.as_str()) else {
        return;
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(z) else {
        return;
    };
    let mut dec = DeflateDecoder::new(&bytes[..]);
    let mut out = Vec::new();
    if dec.read_to_end(&mut out).is_err() {
        return;
    }
    if let Ok(v) = serde_json::from_slice::<Value>(&out) {
        *data = v;
    }
}

fn decode_v2_binary(bytes: &[u8]) -> Option<String> {
    const MAX_DECOMPRESSED: u64 = 32 * 1024 * 1024;
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let decoder = GzDecoder::new(bytes);
        let mut limited = decoder.take(MAX_DECOMPRESSED + 1);
        let mut out = Vec::new();
        limited.read_to_end(&mut out).ok()?;
        if out.len() as u64 > MAX_DECOMPRESSED {
            return None;
        }
        String::from_utf8(out).ok()
    } else {
        std::str::from_utf8(bytes).ok().map(str::to_string)
    }
}

pub(crate) fn gzip_json(value: &Value) -> Result<Vec<u8>, String> {
    let raw = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(&raw).map_err(|e| e.to_string())?;
    enc.finish().map_err(|e| e.to_string())
}

/// 把一批出站消息里「连续、同目标、同会话」的 roaming.update 合并成一条（ops 数组），
/// 其余消息原样保留。保持原始顺序，既减少往返又不破坏增量顺序。
fn coalesce_outbound(batch: Vec<(String, String, Value)>) -> Vec<(String, String, Value)> {
    let mut out: Vec<(String, String, Value)> = Vec::with_capacity(batch.len());
    for (to, kind, data) in batch {
        if kind == "roaming.update" {
            if let Some((pto, pkind, pdata)) = out.last_mut() {
                if pkind == "roaming.update"
                    && *pto == to
                    && pdata["guestThreadId"] == data["guestThreadId"]
                {
                    if let Some(arr) = pdata["ops"].as_array_mut() {
                        arr.push(data["op"].clone());
                        continue;
                    }
                }
            }
            let merged = json!({
                "guestThreadId": data["guestThreadId"].clone(),
                "ops": [data["op"].clone()],
            });
            out.push((to, kind, merged));
        } else {
            out.push((to, kind, data));
        }
    }
    out
}

/// 把会话条目拼成纯文本，用于高级分享 / 线索 AI 总结时喂给模型
pub(crate) fn build_transcript(items: &[Item]) -> String {
    let mut out = String::new();
    for it in items {
        match it {
            Item::User { text, .. } => {
                out.push_str("【用户】\n");
                out.push_str(text);
                out.push_str("\n\n");
            }
            Item::Assistant { text, .. } => {
                out.push_str("【助手】\n");
                out.push_str(text);
                out.push_str("\n\n");
            }
            _ => {}
        }
    }
    out
}

fn make_scratch_dir() -> Result<String, String> {
    let name = format!("share-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let dir = std::env::temp_dir().join(crate::SCRATCH_MARK).join(name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建临时目录失败：{e}"))?;
    Ok(dir.to_string_lossy().to_string())
}

fn basename(p: &str) -> String {
    p.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(p)
        .to_string()
}

/// 规范化群组字符串：按逗号/空白/分号拆分，去空、去重，重新用逗号连接。
pub fn normalize_groups_csv(raw: &str) -> String {
    let mut seen = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for part in raw.split(|c: char| matches!(c, ',' | ';' | ' ' | '\t' | '\n' | '\r')) {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        if seen.insert(p.to_string()) {
            out.push(p.to_string());
        }
    }
    out.join(",")
}

/// 解析中转地址：空则回退默认，去掉尾部斜杠。
pub fn resolve_relay_server(raw: &str) -> String {
    let s = raw.trim().trim_end_matches('/');
    if s.is_empty() {
        crate::settings::DEFAULT_RELAY_SERVER.to_string()
    } else {
        s.to_string()
    }
}

/// 验证中转站连通性：用给定 server+token(+groups) 拉一次在线名单，返回同群组在线人数。
pub async fn probe_relay(server: &str, token: &str, groups: &str) -> Result<i64, String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("请先填写 token".into());
    }
    let server = resolve_relay_server(server);
    if server.is_empty() {
        return Err("请先填写中转站地址".into());
    }
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(format!("{server}/v2/peers"))
        .header("Authorization", format!("Bearer {token}"))
        .header(
            "X-Relay-Groups-Encoded",
            urlencode(&normalize_groups_csv(groups)),
        )
        .send()
        .await
        .map_err(|e| format!("连不上中转站：{e}"))?;
    let status = resp.status();
    if status.as_u16() == 401 {
        return Err("token 未授权（服务端开了白名单且未登记该 token）".into());
    }
    if !status.is_success() {
        return Err(format!("中转站返回 HTTP {status}"));
    }
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    let online = body["peers"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|p| p["online"].as_bool().unwrap_or(false))
                .count() as i64
        })
        .unwrap_or(0);
    Ok(online)
}

async fn decode_relay_json<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, String> {
    let status = response.status();
    let body = response.text().await.map_err(|error| error.to_string())?;
    if !status.is_success() {
        let message = body.trim();
        return Err(if message.is_empty() {
            format!("HTTP {status}")
        } else {
            message.to_string()
        });
    }
    serde_json::from_str(&body).map_err(|error| error.to_string())
}

fn relay_display_name(s: &Settings) -> String {
    if let Some(name) = relay_token_username(&s.relay_token) {
        return name.to_string();
    }
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .unwrap_or_else(|| "Nova".to_string())
}

fn relay_token_username(token: &str) -> Option<&str> {
    token.trim().split_once('/').map(|(username, _)| username)
}

fn relay_sender_name(token: &str, fallback: &str) -> String {
    relay_token_username(token).unwrap_or(fallback).to_string()
}

fn relay_display_peers(mut peers: Value) -> Value {
    if let Some(items) = peers.as_array_mut() {
        for peer in items {
            let Some(name) = peer
                .get("token")
                .and_then(Value::as_str)
                .and_then(relay_token_username)
                .map(str::to_string)
            else {
                continue;
            };
            peer["name"] = json!(name);
        }
    }
    peers
}

pub(crate) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn websocket_url(server: &str, since: i64, server_epoch: &str) -> Result<String, String> {
    let base = if let Some(rest) = server.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = server.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        return Err("中转站地址必须以 http:// 或 https:// 开头".into());
    };
    Ok(format!(
        "{}/v2/ws?since={since}&serverEpoch={}",
        base.trim_end_matches('/'),
        urlencode(server_epoch),
    ))
}

fn relay_thread_id(data: &Value) -> &str {
    data.get("guestThreadId")
        .or_else(|| data.get("hostThreadId"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn read_relay_state(dir: &PathBuf) -> (i64, String) {
    let state = std::fs::read_to_string(dir.join("relay-state.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or_else(|| json!({}));
    (
        state["lastSeq"].as_i64().unwrap_or(0),
        state["serverEpoch"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

fn read_or_create_device_id(dir: &PathBuf) -> String {
    let path = dir.join("relay-device-id");
    if let Ok(id) = std::fs::read_to_string(&path) {
        let id = id.trim();
        if !id.is_empty() {
            return id.to_string();
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(path, &id);
    id
}

fn write_relay_state(dir: &PathBuf, seq: i64, server_epoch: &str) {
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(
        dir.join("relay-state.json"),
        json!({ "lastSeq": seq, "serverEpoch": server_epoch }).to_string(),
    );
}

fn read_inbox(dir: &PathBuf) -> Vec<Share> {
    std::fs::read_to_string(dir.join("relay-inbox.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<Share>>(&s).ok())
        .unwrap_or_default()
}

fn persist_inbox(dir: &PathBuf, inbox: &[Share]) {
    let _ = std::fs::create_dir_all(dir);
    if let Ok(json) = serde_json::to_string_pretty(inbox) {
        let _ = std::fs::write(dir.join("relay-inbox.json"), json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_websocket_url_keeps_cursor_and_server_epoch() {
        assert_eq!(
            websocket_url("https://relay.example/base", 42, "epoch/a").unwrap(),
            "wss://relay.example/base/v2/ws?since=42&serverEpoch=epoch%2Fa"
        );
        assert_eq!(
            websocket_url("http://127.0.0.1:8320/", 0, "").unwrap(),
            "ws://127.0.0.1:8320/v2/ws?since=0&serverEpoch="
        );
    }

    #[test]
    fn v2_gzip_binary_round_trips_and_rejects_oversize() {
        let value = json!({ "text": "x".repeat(4096) });
        let compressed = gzip_json(&value).unwrap();
        let decoded = decode_v2_binary(&compressed).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&decoded).unwrap(), value);

        let huge = json!({ "text": "x".repeat(33 * 1024 * 1024) });
        let compressed = gzip_json(&huge).unwrap();
        assert!(decode_v2_binary(&compressed).is_none());
    }

    #[test]
    fn relay_token_username_is_backward_compatible() {
        assert_eq!(relay_token_username("alice/random-id"), Some("alice"));
        assert_eq!(relay_token_username("legacy-token"), None);
        assert_eq!(relay_sender_name("alice/random-id", "Secret"), "alice");
        assert_eq!(
            relay_sender_name("legacy-token", "Legacy Name"),
            "Legacy Name"
        );
    }

    #[test]
    fn relay_peer_names_hide_random_token_suffixes() {
        let peers = relay_display_peers(json!([
            { "token": "alice/random-id", "name": "Secret" },
            { "token": "legacy-token", "name": "Legacy Name" }
        ]));
        assert_eq!(peers[0]["token"], "alice/random-id");
        assert_eq!(peers[0]["name"], "alice");
        assert_eq!(peers[1]["name"], "Legacy Name");
    }

    #[test]
    fn recalled_share_uses_local_default_mode() {
        assert_eq!(accepted_share_mode(true, "build").as_deref(), Some("build"));
        assert_eq!(accepted_share_mode(true, ""), None);
        assert_eq!(accepted_share_mode(false, "build"), None);
    }

    #[test]
    fn filters_peer_models_to_explicit_quota_shares() {
        let source = json!({
            "configOptions": [{
                "id": "model",
                "currentValue": "cursor-large",
                "options": [
                    { "value": "cursor-small", "name": "Small" },
                    { "value": "cursor-large", "name": "Large" }
                ]
            }],
            "modes": null
        });
        let shared = HashSet::from(["cursor:cursor-small".to_string()]);

        let filtered = shared_model_options(&AgentKind::Cursor, &source, &shared).unwrap();
        let model = &filtered["configOptions"][0];
        assert_eq!(model["currentValue"], "cursor-small");
        assert_eq!(model["options"].as_array().unwrap().len(), 1);
        assert_eq!(model["options"][0]["value"], "cursor-small");
    }

    #[test]
    fn quota_request_requires_current_exact_model_share() {
        let mut settings = Settings::default();
        settings.quota_shared_models = vec!["cursor:cursor-small".into()];

        assert!(quota_model_is_shared(
            &settings,
            &AgentKind::Cursor,
            "cursor-small"
        ));
        assert!(!quota_model_is_shared(
            &settings,
            &AgentKind::Cursor,
            "cursor-large"
        ));
        assert!(!quota_model_is_shared(
            &settings,
            &AgentKind::Codex,
            "cursor-small"
        ));
    }

    #[test]
    fn quota_lease_key_is_per_peer_and_backend() {
        let cursor =
            QuotaLeaseKey::new("peer-a".into(), AgentKind::Cursor, "cursor-small").unwrap();
        let codex = QuotaLeaseKey::new("peer-a".into(), AgentKind::Codex, "gpt-5").unwrap();

        assert_ne!(cursor, codex);
        assert!(QuotaLeaseKey::new("".into(), AgentKind::Cursor, "cursor-small").is_err());
    }

    #[test]
    fn opencode_quota_lease_is_scoped_to_provider() {
        let anthropic = QuotaLeaseKey::new(
            "peer-a".into(),
            AgentKind::OpenCode,
            "anthropic/claude-sonnet-4",
        )
        .unwrap();
        let openai =
            QuotaLeaseKey::new("peer-a".into(), AgentKind::OpenCode, "openai/gpt-5").unwrap();

        assert_ne!(anthropic, openai);
        assert_eq!(anthropic.auth_scope, "anthropic");
    }

    #[test]
    fn quota_runtime_supports_every_frontend_backend() {
        for kind in [
            AgentKind::Devin,
            AgentKind::Codex,
            AgentKind::CodeBuddy,
            AgentKind::ClaudeCode,
            AgentKind::Cursor,
            AgentKind::OpenCode,
        ] {
            assert!(ensure_quota_backend_supported(&kind).is_ok());
        }
    }

    #[test]
    fn shared_quota_model_keys_follow_peer_payload() {
        let shared = shared_quota_model_keys(&json!({
            "cursor": {
                "configOptions": [{
                    "id": "model",
                    "options": [
                        { "value": "cursor-small" },
                        { "value": "cursor-large" }
                    ]
                }]
            }
        }));

        assert!(shared.contains("cursor:cursor-small"));
        assert!(shared.contains("cursor:cursor-large"));
        assert!(!shared.contains("codex:cursor-small"));
    }

    #[tokio::test]
    async fn quota_operation_cancel_wakes_waiter_and_blocks_commit() {
        let operation = Arc::new(QuotaOperation::new());
        let waiter = operation.clone();
        let task = tokio::spawn(async move { waiter.cancelled().await });

        assert!(operation.cancel());
        timeout(Duration::from_millis(100), task)
            .await
            .unwrap()
            .unwrap();
        assert!(!operation.commit());
    }

    #[test]
    fn committed_quota_operation_rejects_late_cancel() {
        let operation = QuotaOperation::new();
        assert!(operation.commit());
        assert!(!operation.cancel());
    }

    #[test]
    fn temporary_sessions_are_not_published_as_roaming_projects() {
        assert!(!is_publishable_roaming_path(
            r"C:\Users\tester\AppData\Local\Temp\Nova-scratch\0714-104825-1912"
        ));
        assert!(is_publishable_roaming_path(r"D:\project\nova-client"));
    }

    #[test]
    fn roaming_path_requires_whitelist_and_existing_directory() {
        let dir = std::env::temp_dir().join(format!("nova-roaming-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.to_string_lossy().to_string();
        assert!(!is_allowed_roaming_path(&[], &path));
        assert!(is_allowed_roaming_path(std::slice::from_ref(&path), &path));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!is_allowed_roaming_path(std::slice::from_ref(&path), &path));
    }

    #[test]
    fn roaming_cancel_invalidates_host_prompt_that_has_not_started() {
        let epoch = Arc::new(AtomicU64::new(0));
        let pending = (epoch.clone(), epoch.load(Ordering::SeqCst));

        epoch.fetch_add(1, Ordering::SeqCst);

        assert!(!host_prompt_is_current(&pending));
        let next = (epoch.clone(), epoch.load(Ordering::SeqCst));
        assert!(host_prompt_is_current(&next));
    }
}
