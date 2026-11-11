//! autoRoute：provider 设了 `options.autoRoute` 时，该 provider 的请求绕开系统代理直连，
//! 并在若干 Cloudflare 边缘 IP 里实测挑最快的一条钉住（`.resolve_to_addrs`，URL/SNI 仍是域名）。
//!
//! 没测出结果时先用「直连不钉 IP」——「不钉 IP」本身也是一个候选，所以慢的候选永远不会赢，
//! 开了这个开关最坏就是退化成普通直连，不会比直连更差。
//!
//! shortcut: 选路结果只在内存里、5 分钟重测一次；跨进程重启会重新退化成直连直到第一轮测完。

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::lyra::config::ResolvedModel;

/// 各 /16 取一个地址，只是「待测清单」不是「优选名单」：同一个 IP 在不同出口网络上会落到
/// 不同机房（实测过 162.159.138.1 在一条链路上落 FRA、在另一条上落 HKG），所以必须先测。
const CANDIDATES: [&str; 12] = [
    "104.16.0.1",
    "104.17.0.1",
    "104.18.0.1",
    "104.19.0.1",
    "104.20.0.1",
    "104.21.0.1",
    "104.24.0.1",
    "104.25.0.1",
    "104.26.0.1",
    "172.66.0.1",
    "172.67.0.1",
    "188.114.96.1",
];

const TTL: Duration = Duration::from_secs(300);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy)]
struct Choice {
    ip: Option<IpAddr>,
    at: Instant,
}

#[derive(Default)]
struct State {
    choices: HashMap<String, Choice>,
    clients: HashMap<(String, Option<IpAddr>), reqwest::Client>,
    probing: HashSet<String>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::default()))
}

/// 该 provider 的客户端；`autoRoute` 关闭时返回 None，调用方保持原行为。
pub(crate) fn client(model: &ResolvedModel) -> Option<reqwest::Client> {
    if !model.auto_route {
        return None;
    }
    let (host, pinnable) = target(&model.base_url);
    let ip = if pinnable { choice(&host) } else { None };
    Some(cached_client(&host, ip))
}

/// baseURL 的 host，以及能不能钉：只有 443 端口下钉进去的地址就是下游地址。
fn target(base_url: &str) -> (String, bool) {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return (String::new(), false);
    };
    (
        url.host_str().unwrap_or_default().to_string(),
        url.port_or_known_default() == Some(443),
    )
}

/// 当前该 host 的选路结果；顺带保证过期后会有一个后台重测在跑。
fn choice(host: &str) -> Option<IpAddr> {
    // baseURL 直接写 IP 时没有可钉的域名，只绕代理。
    if host.is_empty() || host.parse::<IpAddr>().is_ok() {
        return None;
    }
    let (cached, start) = {
        let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
        let cached = state.choices.get(host).copied();
        let stale = cached.is_none_or(|choice| choice.at.elapsed() > TTL);
        (cached, stale && state.probing.insert(host.to_string()))
    };
    if start {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let host = host.to_string();
                handle.spawn(async move { probe(&host).await });
            }
            // 没有 tokio 上下文（同步单测等）：放掉探测标记，下次再试。
            Err(_) => {
                state().lock().unwrap_or_else(|e| e.into_inner()).probing.remove(host);
            }
        }
    }
    cached.and_then(|choice| choice.ip)
}

/// 并发测一遍候选 + 「不钉 IP」的基线，最快的那个赢。
async fn probe(host: &str) {
    let url = format!("https://{host}/cdn-cgi/trace");
    let mut tasks = tokio::task::JoinSet::new();
    for ip in std::iter::once(None).chain(CANDIDATES.iter().map(|ip| ip.parse().ok())) {
        let client = build(host, ip);
        let url = url.clone();
        tasks.spawn(async move { (ip, measure(&client, &url).await) });
    }
    let mut measured = Vec::new();
    while let Some(Ok(item)) = tasks.join_next().await {
        measured.push(item);
    }
    let choice = Choice {
        ip: pick(measured.into_iter().filter_map(|(ip, elapsed)| elapsed.map(|elapsed| (ip, elapsed)))),
        at: Instant::now(),
    };
    let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
    state.choices.insert(host.to_string(), choice);
    state.probing.remove(host);
}

fn pick(measured: impl IntoIterator<Item = (Option<IpAddr>, Duration)>) -> Option<IpAddr> {
    measured
        .into_iter()
        .min_by_key(|(_, elapsed)| *elapsed)
        .and_then(|(ip, _)| ip)
}

async fn measure(client: &reqwest::Client, url: &str) -> Option<Duration> {
    let started = Instant::now();
    let response = client.get(url).timeout(PROBE_TIMEOUT).send().await.ok()?;
    // 边缘拒绝为该 zone 服务时回 403/1034，响应比正常还快；只看连通性会把它选上去。
    if !response.status().is_success() {
        return None;
    }
    let text = response.text().await.ok()?;
    text.contains("colo=").then(|| started.elapsed())
}

fn cached_client(host: &str, ip: Option<IpAddr>) -> reqwest::Client {
    let key = (host.to_string(), ip);
    let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(client) = state.clients.get(&key) {
        return client.clone();
    }
    // 换 IP 会攒出旧客户端，留几个就够，多了整体丢掉重建。
    if state.clients.len() > 8 {
        state.clients.clear();
    }
    let client = build(host, ip);
    state.clients.insert(key, client.clone());
    client
}

fn build(host: &str, ip: Option<IpAddr>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(10))
        .pool_max_idle_per_host(4)
        .tcp_keepalive(Duration::from_secs(20));
    if let Some(ip) = ip.filter(|_| !host.is_empty()) {
        builder = builder.resolve_to_addrs(host, &[SocketAddr::new(ip, 443)]);
    }
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    fn ip(text: &str) -> Option<IpAddr> {
        Some(text.parse().unwrap())
    }

    #[test]
    fn fastest_candidate_wins_unless_unpinned_is_faster() {
        assert_eq!(pick([(ip("1.1.1.1"), ms(120)), (None, ms(300))]), ip("1.1.1.1"));
        // 基线最快就不钉 IP：宁可用系统解析，也不钉一条更慢的。
        assert_eq!(pick([(ip("1.1.1.1"), ms(300)), (None, ms(120))]), None);
        assert_eq!(pick(std::iter::empty()), None);
    }

    #[test]
    fn target_requires_a_pinnable_host() {
        assert_eq!(
            target("https://amr-link.open-design.ai/v1"),
            ("amr-link.open-design.ai".to_string(), true)
        );
        // 非 443 端口钉进去的是另一个地址，只绕代理不钉 IP。
        assert!(target("https://amr-link.open-design.ai:8443/v1").1 == false);
        assert_eq!(target("不是 URL"), (String::new(), false));
        // 直接写 IP 的 baseURL 没有可钉的域名。
        assert_eq!(choice("1.2.3.4"), None);
    }
}
