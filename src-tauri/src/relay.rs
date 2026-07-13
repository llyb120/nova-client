//! 团队分享 + 漫游模式的中转客户端。
//!
//! 连接关系：HTTP 负责发送（POST /v1/send），SSE 负责接收（GET /v1/stream）。
//! 身份用永久 token 区分（设置里配置）。
//!
//! 三种能力：
//! 1. 在线名单（presence）：谁在线、各自允许漫游的目录。
//! 2. 团队分享（share）：把一段对话快照直接发给指定的人，对方点角标接收。
//! 3. 漫游（roaming）：guest 在 host 机器上新建会话并驱动对话，真正的执行只在 host，
//!    guest 只接收展示。host 复用本地 acp/codex 管理器执行，所有产生的 update/turn/
//!    permission 事件由 lib.rs 的事件监听转发回 guest。
//!
//! 断线重连：SSE 断开后指数退避重连；每条定向消息有服务端分配的 seq，重连时带
//! since=<最后 seq> 补发漏掉的消息，保证不丢、不影响使用。

use crate::settings::Settings;
use crate::threads::{now_ms, AgentKind, Item, PromptImage, Thread, Worktree};
use crate::AppState;
use base64::Engine;
use flate2::read::DeflateDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep, timeout, Duration, Instant};
use x25519_dalek::StaticSecret;

use crate::acp::{EV_PERMISSION, EV_PERMISSION_RESOLVED, EV_THREADS, EV_TURN, EV_UPDATE};

pub const EV_RELAY_STATUS: &str = "relay:status";
pub const EV_RELAY_PEERS: &str = "relay:peers";
pub const EV_RELAY_INBOX: &str = "relay:inbox";
/// guest 侧：收到对端（host）回传的可选模型/模式列表，前端据此缓存并在漫游时选用对方的模型
pub const EV_RELAY_PEER_MODELS: &str = "relay:peer-models";
/// guest 侧：收到对端某目录的本地分支列表，worktree「基于分支」下拉据此填充
pub const EV_RELAY_PEER_BRANCHES: &str = "relay:peer-branches";
/// host 侧：收到漫游请求，前端弹确认框
pub const EV_RELAY_ROAM_REQUEST: &str = "relay:roam-request";
/// 额度提供方：收到凭证租借请求，前端弹确认框。
pub const EV_RELAY_QUOTA_REQUEST: &str = "relay:quota-request";
/// 额度借用方：请求、安装 CLI、准备隔离凭证的进度。
pub const EV_RELAY_QUOTA_PROGRESS: &str = "relay:quota-progress";
/// 整段刷新某线程（漫游快照重同步后用），前端据此重新拉取 transcript
pub const EV_RELAY_RELOAD: &str = "acp:reload";

/// host 侧：本机某线程正被哪个 guest 漫游驱动
#[derive(Clone)]
struct RoamGuest {
    token: String,
    guest_thread_id: String,
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
    /// guest 是否要求在 worktree 中执行（host 侧在自己仓库代建）
    worktree: bool,
    /// worktree 分支名（guest 手填）
    worktree_branch: Option<String>,
    /// worktree 基于哪个分支/提交创建（空 = host 仓库当前 HEAD）
    worktree_base: Option<String>,
}

struct PendingQuotaClient {
    peer: String,
    agent_kind: AgentKind,
    secret: StaticSecret,
    reply: oneshot::Sender<Result<crate::credential_roaming::CredentialBundle, String>>,
}

#[derive(Clone)]
struct IncomingQuota {
    from: String,
    agent_kind: AgentKind,
    public_key: String,
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
    /// 漫游召回自动回传的快照（前端标注「召回」并自动弹出收件箱）
    #[serde(default)]
    pub recall: bool,
    pub ts: i64,
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

pub struct RelayManager {
    pub app: AppHandle,
    config_dir: PathBuf,
    http: reqwest::Client,
    device_id: String,
    connected: AtomicBool,
    last_seq: AtomicI64,
    last_persist: StdMutex<Instant>,
    peers: StdMutex<Value>,
    inbox: StdMutex<Vec<Share>>,
    /// host 侧：hostThreadId -> guest
    hosted: StdMutex<HashMap<String, RoamGuest>>,
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
    /// 提供方：reqId -> 等待本机用户确认的凭证租借请求。
    incoming_quota: StdMutex<HashMap<String, IncomingQuota>>,
    /// 高级分享：本机处理线程 id -> 处理完成后要分享给谁
    advanced: StdMutex<HashMap<String, String>>,
    /// guest 侧落盘节流：流式期间不要每条增量都写整个 store（会拖慢 SSE 消费）
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
        let last_seq = read_last_seq(&config_dir);
        let inbox = read_inbox(&config_dir);
        let device_id = read_or_create_device_id(&config_dir);
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            // 开启 TCP keepalive：让 OS 层也能更早发现静默死亡的长连接（SSE），
            // 配合下面 SSE 读侧的空闲超时，双重兜底「漫游过一会儿就不动了」。
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
            last_seq: AtomicI64::new(last_seq),
            last_persist: StdMutex::new(Instant::now()),
            peers: StdMutex::new(json!([])),
            inbox: StdMutex::new(inbox),
            hosted: StdMutex::new(HashMap::new()),
            guest_perms: StdMutex::new(HashMap::new()),
            guest_running: StdMutex::new(HashSet::new()),
            pending_creates: StdMutex::new(HashMap::new()),
            pending_prompts: StdMutex::new(HashMap::new()),
            incoming_roams: StdMutex::new(HashMap::new()),
            pending_quota: StdMutex::new(HashMap::new()),
            incoming_quota: StdMutex::new(HashMap::new()),
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
    /// 前端定时器用它做「兜底刷新」——此前定时器读的是本地缓存（只被 SSE presence 更新），
    /// 一旦某次 presence 推送丢失，名单会长时间停在旧状态；这里直接查服务端 roster，
    /// 不依赖 SSE，既能自愈丢失的 presence，也能在 SSE 尚未连上时就先显示在线名单（加快入网体感）。
    pub async fn refresh_peers(&self) {
        let Some((server, token, name)) = self.cfg() else {
            *self.peers.lock().unwrap() = json!([]);
            let _ = self.app.emit(EV_RELAY_PEERS, json!([]));
            return;
        };
        let resp = self
            .http
            .get(format!("{server}/v1/peers"))
            .header("Authorization", format!("Bearer {token}"))
            .header("X-Relay-Name", &name)
            .header("X-Relay-Groups", self.groups_csv())
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
        let peers = body.get("peers").cloned().unwrap_or(json!([]));
        *self.peers.lock().unwrap() = peers.clone();
        let _ = self.app.emit(EV_RELAY_PEERS, peers);
    }

    pub fn inbox_list(&self) -> Vec<Share> {
        self.inbox.lock().unwrap().clone()
    }

    fn emit_status(&self) {
        let _ = self.app.emit(EV_RELAY_STATUS, self.status());
    }

    fn set_connected(&self, on: bool) {
        let prev = self.connected.swap(on, Ordering::SeqCst);
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
            self.set_connected(false);
            self.persist_seq(true);
            match result {
                Ok(_) => backoff = Duration::from_secs(1),
                Err(e) => {
                    self.log(format!("[relay] 连接断开：{e}"));
                }
            }
            // 退避重连（带抖动）
            let jitter = Duration::from_millis((now_ms() % 500) as u64);
            sleep(backoff + jitter).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }

    async fn connect_once(&self, server: &str, token: &str, name: &str) -> Result<(), String> {
        let since = self.last_seq.load(Ordering::SeqCst);
        let url = format!(
            "{server}/v1/stream?token={}&name={}&since={}&groups={}&device={}",
            urlencode(token),
            urlencode(name),
            since,
            urlencode(&self.groups_csv()),
            urlencode(&self.device_id),
        );
        let resp = self
            .http
            .get(&url)
            .header("Accept", "text/event-stream")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        self.set_connected(true);
        self.log("[relay] 已连接中转站".into());
        // 上报漫游目录 + 恢复 host 映射 + 重同步进行中的漫游会话
        self.publish_folders();
        self.rebuild_hosted();
        self.resync_guest_threads();

        let mut resp = resp;
        let mut buf: Vec<u8> = Vec::new();
        let mut data_lines: Vec<String> = Vec::new();
        let mut event_type = String::from("message");
        loop {
            // SSE 读侧空闲超时：服务端每 15s 发一次心跳（注释行），健康连接下绝不会 40s
            // 收不到任何字节。一旦底层 TCP 静默死亡（NAT 重绑定 / 网络切换 / 中间设备重置
            // 而未被 reqwest 感知为正常关闭），resp.chunk() 会永久挂起——既不报错也不返回 None，
            // 于是永远不重连、不重同步，表现为「漫游执行过一会儿就自动停止」（host/guest 同理）。
            // 超时即判定连接已死，返回 Err 触发 run_loop 退避重连 + since 补发，自动恢复。
            let chunk = match timeout(Duration::from_secs(40), resp.chunk()).await {
                Ok(r) => r.map_err(|e| e.to_string())?,
                Err(_) => return Err("接收空闲超时（40s 无心跳/数据），判定连接已断，重连".into()),
            };
            let Some(chunk) = chunk else {
                return Ok(()); // 服务端关闭，正常重连
            };
            buf.extend_from_slice(&chunk);
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1]);
                let line = line.trim_end_matches('\r');
                if line.is_empty() {
                    if !data_lines.is_empty() {
                        let data = data_lines.join("\n");
                        self.on_sse_event(&event_type, &data);
                    }
                    data_lines.clear();
                    event_type = "message".into();
                    continue;
                }
                if line.starts_with(':') {
                    continue; // 心跳/注释
                }
                if let Some(rest) = line.strip_prefix("data:") {
                    data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
                } else if let Some(rest) = line.strip_prefix("event:") {
                    event_type = rest.trim().to_string();
                }
            }
        }
    }

    fn on_sse_event(&self, event_type: &str, data: &str) {
        if event_type == "ready" {
            return;
        }
        let Ok(mut env) = serde_json::from_str::<InEnvelope>(data) else {
            return;
        };
        // 还原对端可能压缩过的大载荷
        maybe_decompress(&mut env.data);
        if env.seq > 0 {
            // 跟随服务端分配的序号，作为断线/重连补发的基准（since）。
            // 关键是「跟随」而非「只增大」：服务端重启后每人的 seq 会从 0 重新计数（新纪元），
            // 这时收到的 seq 比旧 last_seq 小；若只取较大值，重连时仍会上报旧纪元的大 since，
            // 服务端便把新消息（低 seq）全部过滤掉 => 漫游/分享消息互相收不到。直接存最近收到
            // 的 seq（SSE 单连接内严格递增，跨重连由服务端按 since 续发），配合服务端对越界
            // since 的归零处理即可自愈。on_sse_event 由单一接收循环顺序调用，无并发写。
            self.last_seq.store(env.seq, Ordering::SeqCst);
            self.persist_seq(false);
        }
        self.dispatch(env);
    }

    fn dispatch(&self, env: InEnvelope) {
        match env.kind.as_str() {
            "presence" => {
                // 保留服务端返回的完整名单（含自己）：天线在线名单要显示自己（前端标注「我」）。
                // 漫游/分享侧各自会用自己的 token 排除自己，不受此影响。
                let peers = env.data.get("peers").cloned().unwrap_or(json!([]));
                *self.peers.lock().unwrap() = peers.clone();
                let _ = self.app.emit(EV_RELAY_PEERS, peers);
            }
            "share" => self.on_share(&env),
            // guest -> host
            "roaming.create" => self.on_roaming_create(&env),
            "roaming.prompt" => self.on_roaming_prompt(&env),
            "roaming.cancel" => self.on_roaming_cancel(&env),
            "roaming.permission_response" => self.on_roaming_permission_response(&env),
            "roaming.config" => self.on_roaming_config(&env),
            "roaming.resync" => self.on_roaming_resync(&env),
            "roaming.recall" => self.on_roaming_recall(&env),
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
        let mut attempt = 0u32;
        loop {
            match self.send(to, kind, data.clone()).await {
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
        let (server, token, name) = self.cfg().ok_or("未配置中转站 token")?;
        let body = gzip_json(&json!({ "to": to, "type": kind, "data": data }))?;
        let resp = self
            .http
            .post(format!("{server}/v1/send"))
            .header("Authorization", format!("Bearer {token}"))
            .header("X-Relay-Name", &name)
            .header("X-Relay-Groups", self.groups_csv())
            .header("X-Relay-Device", &self.device_id)
            .header("Content-Type", "application/json")
            .header("Content-Encoding", "gzip")
            .body(body)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        resp.json::<Value>().await.map_err(|e| e.to_string())
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
            .post(format!("{server}/v1/ledger/claim"))
            .header("Authorization", format!("Bearer {token}"))
            .header("X-Relay-Name", &name)
            .header("X-Relay-Groups", self.groups_csv())
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
            .post(format!("{server}/v1/ledger/set"))
            .header("Authorization", format!("Bearer {token}"))
            .header("X-Relay-Name", &name)
            .header("X-Relay-Groups", self.groups_csv())
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
            .post(format!("{server}/v1/ledger/remove"))
            .header("Authorization", format!("Bearer {token}"))
            .header("X-Relay-Name", &name)
            .header("X-Relay-Groups", self.groups_csv())
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
            .get(format!("{server}/v1/ledger/list"))
            .header("Authorization", format!("Bearer {token}"))
            .header("X-Relay-Name", &name)
            .header("X-Relay-Groups", self.groups_csv())
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

    /// 中转站是否已配置（用于判断能否使用共享账本）。
    pub fn is_configured(&self) -> bool {
        self.cfg().is_some()
    }

    /// 上报本机「允许漫游」的目录列表
    pub fn publish_folders(&self) {
        let Some((server, token, name)) = self.cfg() else {
            return;
        };
        // 不再依赖手动「允许漫游」：直接把本机的项目列表广播出去，
        // 对端可选其中任意项目发起漫游，真正放行由本机用户在确认框里决定。
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
            let projects = state.projects.lock().unwrap();
            projects
                .projects
                .iter()
                .filter(|p| std::path::Path::new(p).is_dir())
                .map(|p| {
                    let name = wt_names
                        .get(p.as_str())
                        .cloned()
                        .unwrap_or_else(|| basename(p));
                    json!({ "path": p, "name": name })
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
                .post(format!("{server}/v1/folders"))
                .header("Authorization", format!("Bearer {token}"))
                .header("X-Relay-Name", &name)
                .header("X-Relay-Groups", groups)
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
        let (title, agent_kind, items, plan) = {
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
        let mut thread = Thread::new(
            cwd.to_string(),
            share.agent_kind,
            None,
            None,
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
        let (src_cwd, transcript) = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            let t = store.get(thread_id).ok_or("线程不存在")?;
            (t.cwd.clone(), build_transcript(&t.items))
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
        // 处理会话按其后端路由：Codex 走原生管理器，其它走对应 ACP 管理器
        let state = self.app.state::<AppState>();
        let run_id = new_id.clone();
        match agent_kind {
            AgentKind::Codex => {
                let mgr = state.codex.clone();
                tauri::async_runtime::spawn(async move {
                    mgr.run_prompt(run_id, seed, vec![]).await;
                });
            }
            kind => {
                let mgr = state.acp_for(&kind).expect("ACP 后端必有 manager");
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

    fn emit_quota_progress(&self, stage: &str, message: impl Into<String>) {
        let _ = self.app.emit(
            EV_RELAY_QUOTA_PROGRESS,
            json!({ "stage": stage, "message": message.into() }),
        );
    }

    pub async fn create_quota_thread(
        self: &Arc<Self>,
        peer_token: String,
        peer_name: String,
        cwd: String,
        agent_kind: AgentKind,
        model: Option<String>,
        mode: Option<String>,
        first_prompt: Option<String>,
    ) -> Result<Thread, String> {
        if self.cfg().is_none() {
            return Err("未配置中转站 token".into());
        }
        if !std::path::Path::new(&cwd).is_dir() {
            return Err(format!("本地目录不存在：{cwd}"));
        }
        let request_id = uuid::Uuid::new_v4().to_string();
        let (secret, public_key) = crate::credential_roaming::new_request_key();
        let (reply, wait) = oneshot::channel();
        self.pending_quota.lock().unwrap().insert(
            request_id.clone(),
            PendingQuotaClient {
                peer: peer_token.clone(),
                agent_kind: agent_kind.clone(),
                secret,
                reply,
            },
        );
        self.emit_quota_progress("requesting", format!("等待 {peer_name} 授权额度…"));
        self.spawn_send(
            peer_token.clone(),
            "quota.request",
            json!({
                "reqId": request_id,
                "agentKind": agent_kind,
                "publicKey": public_key,
                "projectName": basename(&cwd),
                "prompt": first_prompt,
            }),
        );

        let bundle = match timeout(Duration::from_secs(5 * 60), wait).await {
            Ok(Ok(result)) => result?,
            Ok(Err(_)) => return Err("额度授权通道已关闭".into()),
            Err(_) => {
                self.pending_quota.lock().unwrap().remove(&request_id);
                return Err("等待对方授权超时".into());
            }
        };

        let settings = {
            let state = self.app.state::<AppState>();
            let settings = state.settings.lock().unwrap().clone();
            settings
        };
        if !crate::cli_manager::is_installed(&agent_kind, &settings) {
            self.emit_quota_progress(
                "installing",
                format!("本机缺少 {} CLI，正在一键安装…", agent_kind.label()),
            );
            let state = self.app.state::<AppState>();
            crate::cli_manager::ensure_installed(state.inner(), agent_kind.clone(), &settings)
                .await?;
        }

        self.emit_quota_progress("preparing", "正在解密并准备隔离凭证…");
        let default_mode = {
            let state = self.app.state::<AppState>();
            let default_mode = state.settings.lock().unwrap().default_mode.clone();
            default_mode
        };
        let mut thread = Thread::new(
            cwd.clone(),
            agent_kind.clone(),
            model.filter(|value| !value.is_empty()),
            mode.filter(|value| !value.is_empty())
                .or(Some(default_mode).filter(|value| !value.is_empty())),
            None,
            false,
        );
        thread.quota_peer = Some(peer_token);
        thread.quota_peer_name = Some(peer_name.clone());
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
        self.emit_quota_progress("ready", "额度凭证已就绪，开始本地会话");
        Ok(thread)
    }

    fn on_quota_request(&self, env: &InEnvelope) {
        let req_id = env.data["reqId"].as_str().unwrap_or_default().to_string();
        let public_key = env.data["publicKey"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let agent_kind: AgentKind =
            serde_json::from_value(env.data["agentKind"].clone()).unwrap_or(AgentKind::Devin);
        if req_id.is_empty() || public_key.is_empty() {
            return;
        }
        self.incoming_quota.lock().unwrap().insert(
            req_id.clone(),
            IncomingQuota {
                from: env.from.clone(),
                agent_kind: agent_kind.clone(),
                public_key,
            },
        );
        let _ = self.app.emit(
            EV_RELAY_QUOTA_REQUEST,
            json!({
                "reqId": req_id,
                "from": env.from,
                "fromName": env.from_name,
                "agentKind": agent_kind,
                "projectName": env.data["projectName"],
                "prompt": env.data["prompt"],
            }),
        );
        crate::sys_notify::notify_quota_request(&self.app, &env.from_name, agent_kind.label());
    }

    pub fn respond_quota_request(
        self: &Arc<Self>,
        req_id: &str,
        accept: bool,
    ) -> Result<(), String> {
        let pending = self
            .incoming_quota
            .lock()
            .unwrap()
            .remove(req_id)
            .ok_or("额度请求已失效")?;
        if !accept {
            self.spawn_send_now(
                pending.from,
                "quota.rejected",
                json!({ "reqId": req_id, "error": "对方拒绝了额度租借请求" }),
            );
            return Ok(());
        }
        let this = Arc::clone(self);
        let req_id = req_id.to_string();
        std::thread::spawn(move || {
            let result = crate::credential_roaming::collect_credentials(pending.agent_kind.clone())
                .and_then(|bundle| {
                    crate::credential_roaming::encrypt_bundle(
                        &pending.public_key,
                        &req_id,
                        &bundle,
                    )
                });
            match result {
                Ok(grant) => this.spawn_send_now(
                    pending.from,
                    "quota.granted",
                    json!({ "reqId": req_id, "grant": grant }),
                ),
                Err(error) => this.spawn_send_now(
                    pending.from,
                    "quota.rejected",
                    json!({ "reqId": req_id, "error": error }),
                ),
            }
        });
        Ok(())
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
        self.guest_running
            .lock()
            .unwrap()
            .insert(thread_id.to_string());
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
        let (peer, host_thread_id) = self.roaming_route(thread_id)?;
        self.spawn_send(
            peer,
            "roaming.cancel",
            json!({ "hostThreadId": host_thread_id }),
        );
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

    /// host：收到模型请求，收集本机「已启用且检测可用」后端的模型/模式列表回给对端。
    /// 顺序与前端 ALL_AGENT_KINDS 保持一致（devin → codex → codebuddy → claudecode → cursor → opencode）。
    fn on_roaming_models_request(&self, env: &InEnvelope) {
        let to = env.from.clone();
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            let kinds: Vec<AgentKind> = {
                let state = app.state::<AppState>();
                let s = state.settings.lock().unwrap();
                let avail = state.backend_availability.lock().unwrap();
                [
                    (AgentKind::Devin, s.devin_enabled),
                    (AgentKind::Codex, s.codex_enabled),
                    (AgentKind::CodeBuddy, s.codebuddy_enabled),
                    (AgentKind::ClaudeCode, s.claudecode_enabled),
                    (AgentKind::Cursor, s.cursor_enabled),
                    (AgentKind::OpenCode, s.opencode_enabled),
                ]
                .into_iter()
                // 可用性未检测完（map 为空/无该键）时按可用处理，避免误伤
                .filter(|(k, en)| *en && avail.get(k.as_str()).copied().unwrap_or(true))
                .map(|(k, _)| k)
                .collect()
            };
            let mut backends: Vec<&str> = Vec::new();
            let mut options = serde_json::Map::new();
            for kind in kinds {
                backends.push(kind.as_str());
                let fetched = match app.state::<AppState>().acp_for(&kind) {
                    Some(mgr) => mgr.fetch_model_options().await,
                    None => {
                        let mgr = app.state::<AppState>().codex.clone();
                        mgr.fetch_model_options().await
                    }
                };
                if let Ok(v) = fetched {
                    options.insert(kind.as_str().into(), v);
                }
            }
            let relay = app.state::<AppState>().relay.clone();
            relay.spawn_send(
                to,
                "roaming.models",
                json!({ "backends": backends, "options": Value::Object(options) }),
            );
        });
    }

    /// guest：收到 host 回传的模型列表，转发给前端按对端 token 缓存。
    fn on_roaming_models(&self, env: &InEnvelope) {
        let _ = self.app.emit(
            EV_RELAY_PEER_MODELS,
            json!({
                "peer": env.from,
                "backends": env.data["backends"].clone(),
                "options": env.data["options"].clone(),
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
    /// git 是同步阻塞命令，丢到独立线程执行，避免卡住 SSE 分发。
    fn on_roaming_branches_request(&self, env: &InEnvelope) {
        let to = env.from.clone();
        let folder = env.data["folder"].as_str().unwrap_or_default().to_string();
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
        // 审批制：不再预检/限制目录，一律登记为待确认请求交本机用户决定。
        // 目录是否存在等到用户点「允许」时（respond_roam_request）再校验——避免在请求
        // 阶段就静默拒绝，导致 host 完全无感知（表现为「对方漫游时本机毫无提示」）。
        if folder.trim().is_empty() {
            self.spawn_send_now(
                env.from.clone(),
                "roaming.created",
                json!({ "reqId": req_id, "ok": false, "error": "未指定目录" }),
            );
            return;
        }
        let agent_kind: AgentKind =
            serde_json::from_value(env.data["agentKind"].clone()).unwrap_or(AgentKind::Devin);
        let agent_kind_str = agent_kind.as_str();
        let model = env.data["model"].as_str().map(|s| s.to_string());
        let mode = env.data["mode"].as_str().map(|s| s.to_string());
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
                worktree,
                worktree_branch: worktree_branch.clone(),
                worktree_base,
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
                // 目录在本机是否存在：不存在时前端确认框会提示，允许后会在该路径新建
                "folderExists": std::path::Path::new(&folder).is_dir(),
                // 发起人想做什么：让本机用户能看清提示词再决定是否放行
                "prompt": prompt,
                // worktree：确认框提示「对方要求在 worktree 中执行」
                "worktree": worktree,
                "worktreeBranch": worktree_branch,
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
    ) -> Result<(), String> {
        let pending = self
            .incoming_roams
            .lock()
            .unwrap()
            .remove(req_id)
            .ok_or("漫游请求已失效")?;
        if !accept {
            self.spawn_send_now(
                pending.from.clone(),
                "roaming.created",
                json!({ "reqId": req_id, "ok": false, "error": "对方拒绝了漫游请求" }),
            );
            return Ok(());
        }
        let this = Arc::clone(self);
        let req_id = req_id.to_string();
        std::thread::spawn(move || this.finish_roam_accept(req_id, pending));
        Ok(())
    }

    /// host 侧：后台完成漫游会话创建（含 worktree），成败都回 roaming.created。
    fn finish_roam_accept(self: &Arc<Self>, req_id: String, pending: PendingRoam) {
        // 审批制：用户已点「允许」即表示认可该路径。目录不存在就按其意愿现场创建，
        // 这样漫游不再被「必须是本机已存在的项目」限制；创建失败（权限/非法路径）才回错。
        if !std::path::Path::new(&pending.folder).is_dir() {
            if let Err(e) = std::fs::create_dir_all(&pending.folder) {
                self.spawn_send_now(
                    pending.from.clone(),
                    "roaming.created",
                    json!({ "reqId": req_id, "ok": false, "error": format!("目录不存在且无法创建：{e}") }),
                );
                return;
            }
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
            },
        );
        let _ = self.app.emit(EV_THREADS, json!({}));
        self.spawn_send_now(
            pending.from.clone(),
            "roaming.created",
            json!({ "reqId": req_id, "ok": true, "hostThreadId": host_thread_id }),
        );
    }

    /// 收到漫游请求时提醒本机用户。漫游是需要立即决策的授权事件，必须让人明确感知：
    /// 无论前台后台都把主窗口恢复可见并请求用户注意（任务栏闪烁），保证审批确认框不被
    /// 错过；仅在窗口未聚焦时再补一条系统通知（点击唤起），聚焦时确认框已直接可见。
    fn notify_roam_request(&self, from_name: &str, folder_name: &str) {
        crate::sys_notify::notify_roam_request(&self.app, from_name, folder_name);
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
        let agent_kind = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            match store.get(&host_thread_id) {
                Some(t) => t.agent_kind.clone(),
                None => return,
            }
        };
        let state = self.app.state::<AppState>();
        match agent_kind {
            AgentKind::CodeBuddy => {
                // CodeBuddy 单连接不能并发：运行中再发不走「注入」，一律 run_prompt，
                // 经串行闸门排到当前轮次之后顺序执行（详见 acp.rs 的 serial_gate 说明）。
                let mgr = state.codebuddy.clone();
                tauri::async_runtime::spawn(async move {
                    mgr.run_prompt(host_thread_id, text, images).await;
                });
            }
            AgentKind::Codex => {
                let mgr = state.codex.clone();
                if mgr.is_running(&host_thread_id) {
                    tauri::async_runtime::spawn(async move {
                        mgr.steer_prompt(host_thread_id, text, images).await;
                    });
                } else {
                    tauri::async_runtime::spawn(async move {
                        mgr.run_prompt(host_thread_id, text, images).await;
                    });
                }
            }
            // Devin / ClaudeCode / Cursor / OpenCode：多路复用，运行中注入「引导」，否则新起一轮
            kind => {
                let Some(mgr) = state.acp_for(&kind) else {
                    return;
                };
                if mgr.is_running(&host_thread_id) {
                    tauri::async_runtime::spawn(async move {
                        mgr.steer_prompt(host_thread_id, text, images).await;
                    });
                } else {
                    tauri::async_runtime::spawn(async move {
                        mgr.run_prompt(host_thread_id, text, images).await;
                    });
                }
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
        let state = self.app.state::<AppState>();
        match state.acp_for(&agent_kind) {
            Some(mgr) => {
                tauri::async_runtime::spawn(async move {
                    mgr.cancel(&host_thread_id).await;
                });
            }
            None => {
                let mgr = state.codex.clone();
                tauri::async_runtime::spawn(async move {
                    mgr.cancel(&host_thread_id).await;
                });
            }
        }
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
        if request_key.starts_with("codex-") {
            let mgr = state.codex.clone();
            tauri::async_runtime::spawn(async move {
                let _ = mgr.respond_permission(&request_key, &option_id).await;
            });
        } else if request_key.starts_with("cb-") {
            let mgr = state.codebuddy.clone();
            tauri::async_runtime::spawn(async move {
                let _ = mgr.respond_permission(&request_key, &option_id).await;
            });
        } else if request_key.starts_with("cc-") {
            let mgr = state.claudecode.clone();
            tauri::async_runtime::spawn(async move {
                let _ = mgr.respond_permission(&request_key, &option_id).await;
            });
        } else if request_key.starts_with("cs-") {
            let mgr = state.cursor.clone();
            tauri::async_runtime::spawn(async move {
                let _ = mgr.respond_permission(&request_key, &option_id).await;
            });
        } else if request_key.starts_with("oc-") {
            let mgr = state.opencode.clone();
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
            t.model = model;
            t.mode = mode;
            let ak = t.agent_kind.clone();
            store.save();
            ak
        };
        let state = self.app.state::<AppState>();
        if let Some(mgr) = state.acp_for(&agent_kind) {
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
            let running = state.all_acp().iter().any(|m| m.is_running(host_thread_id))
                || state.codex.is_running(host_thread_id);
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
            self.guest_running.lock().unwrap().remove(&guest_thread_id);
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
        for (text, images) in queued {
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
        // SSE 消费，导致中转缓冲溢出丢消息、卡 loading、思考残缺）。最终一致性由
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
        if running {
            self.guest_running.lock().unwrap().insert(thread_id.clone());
        } else {
            self.guest_running.lock().unwrap().remove(&thread_id);
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
        if running {
            self.guest_running.lock().unwrap().insert(thread_id.clone());
        } else {
            self.guest_running.lock().unwrap().remove(&thread_id);
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
        self.guest_running.lock().unwrap().remove(&thread_id);
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
        write_last_seq(&self.config_dir, seq);
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

/// 把会话条目拼成纯文本，用于高级分享时喂给模型
fn build_transcript(items: &[Item]) -> String {
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
        .get(format!("{server}/v1/peers"))
        .header("Authorization", format!("Bearer {token}"))
        .header("X-Relay-Groups", normalize_groups_csv(groups))
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

fn relay_display_name(s: &Settings) -> String {
    let name = s.relay_name.trim();
    if !name.is_empty() {
        return name.to_string();
    }
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "Nova".to_string())
}

fn urlencode(s: &str) -> String {
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

fn read_last_seq(dir: &PathBuf) -> i64 {
    std::fs::read_to_string(dir.join("relay-state.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v["lastSeq"].as_i64())
        .unwrap_or(0)
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

fn write_last_seq(dir: &PathBuf, seq: i64) {
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(
        dir.join("relay-state.json"),
        json!({ "lastSeq": seq }).to_string(),
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
