use std::collections::HashMap;

// agy reads Go's HTTP(S)_PROXY, not Windows Internet Settings. PAC is not a proxy URL.
pub(super) fn proxy_env(server: &str, bypass: &str, existing_no_proxy: String) -> HashMap<String, String> {
    let mut env = HashMap::new();
    let entries: Vec<_> = server.split(';').map(str::trim).filter(|s| !s.is_empty()).collect();
    for (scheme, key) in [("http", "HTTP_PROXY"), ("https", "HTTPS_PROXY")] {
        let value = entries.iter().find_map(|s| {
            let (protocol, value) = s.split_once('=')?;
            protocol.trim().eq_ignore_ascii_case(scheme).then_some(value.trim())
        }).or_else(|| entries.iter().copied().find(|s| !s.contains('=')));
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            let url = if value.contains("://") { value.to_owned() } else { format!("http://{value}") };
            env.insert(key.into(), url);
        } else if !entries.is_empty() {
            // A per-scheme setting with no route for this scheme means direct, not the inherited proxy.
            env.insert(key.into(), String::new());
        }
    }
    if !env.is_empty() {
        let mut rules = vec![existing_no_proxy];
        for host in bypass.split(';').map(str::trim).filter(|s| !s.is_empty() && !s.starts_with('<')) {
            // Go accepts CIDRs but not WinINET's trailing IPv4 wildcard (e.g. 192.168.*).
            if let Some(prefix) = host.strip_suffix(".*") {
                let mut octets: Vec<_> = prefix.split('.').collect();
                if octets.len() <= 3 && octets.iter().all(|n| n.parse::<u8>().is_ok()) {
                    let bits = octets.len() * 8;
                    octets.resize(4, "0");
                    rules.push(format!("{}/{bits}", octets.join(".")));
                    continue;
                }
            }
            rules.push(host.to_owned());
        }
        env.insert("NO_PROXY".into(), rules.join(","));
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exports_windows_proxy_to_agy() {
        let p = proxy_env("127.0.0.1:8080", "<local>;*.internal;10.*", "example.test".into());
        assert_eq!(p["HTTP_PROXY"], "http://127.0.0.1:8080");
        assert_eq!(p["HTTPS_PROXY"], p["HTTP_PROXY"]);
        assert_eq!(p["NO_PROXY"], "example.test,*.internal,10.0.0.0/8");
        let p = proxy_env("https=https://localhost:8443", "", String::new());
        assert_eq!(p["HTTP_PROXY"], "");
        assert_eq!(p["HTTPS_PROXY"], "https://localhost:8443");
        assert!(proxy_env("", "", String::new()).is_empty());
    }
}
