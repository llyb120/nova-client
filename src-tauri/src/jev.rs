//! jev (TypeSafe SystemOne): screenshot-grounded choice questions for guided browser runs.
//! jev only answers "which of these locally built options fits"; it never produces an action.
//! The run executes, verifies and hands back to the main model exactly as before.
use base64::Engine;
use serde_json::{json, Map, Value};
use std::{collections::HashMap, path::Path, time::Duration};

/// Answers below this confidence count as "unsure" and fall back to the main model.
// ponytail: one fixed threshold from live calibration (correct grounded picks scored 0.54–1 with a
// "none" option present); make it per question kind if logs drift.
pub(crate) const MIN_CONFIDENCE: f64 = 0.5;

struct Endpoint { url: String, key: String, model: String, proxy: String, relay: bool }

/// Enabled: relay → local credentials (settings, env, then Command Code) → unavailable.
/// The settings switch and NOVA_JEV=off disable every source; request errors never change sources.
fn endpoint() -> Result<Endpoint, String> {
    resolve_endpoint(&crate::native_browser::settings().unwrap_or_default(), &crate::lyra::config::process_env(),
        || crate::lyra::config::Roots::global().load_config(None))
}

fn resolve_endpoint(settings: &crate::settings::Settings, env: &HashMap<String, String>, load_config: impl FnOnce() -> Result<Value, String>) -> Result<Endpoint, String> {
    let var = |k: &str| env.get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    if !settings.jev_vision_enabled { return Err("jev 已在设置中关闭".into()); }
    if var("NOVA_JEV").as_deref() == Some("off") { return Err("jev 已关闭（NOVA_JEV=off）".into()); }
    let server = crate::relay::resolve_relay_server(&settings.relay_server);
    let token = settings.relay_token.trim();
    if !server.is_empty() && !token.is_empty() {
        // Relay owns the upstream credentials; local URL/model/proxy settings must not leak into it.
        return Ok(Endpoint { url: format!("{server}/cc/systemone"), key: token.into(),
            model: "typesafe/jev-1.14.0".into(), proxy: String::new(), relay: true });
    }
    let set = |v: &str| Some(v.trim().to_string()).filter(|v| !v.is_empty());
    let url = set(&settings.jev_api_url).or_else(|| var("NOVA_JEV_API_URL"));
    let model = set(&settings.jev_model).or_else(|| var("NOVA_JEV_MODEL"));
    let proxy = settings.lyra_proxy.clone();
    if let Some(key) = set(&settings.jev_api_key).or_else(|| var("NOVA_JEV_API_KEY")) {
        return Ok(Endpoint { url: url.unwrap_or_else(|| "https://api.typesafe.ai/v1/systemone".into()), key,
            model: model.unwrap_or_else(|| "jev-1.14.0".into()), proxy, relay: false });
    }
    let config = load_config()?;
    let provider = config["provider"].as_object().into_iter().flat_map(|p| p.values())
        .find(|p| p["preset"] == "commandcode" || p["options"]["baseURL"].as_str().is_some_and(|u| u.contains("commandcode.ai")))
        .ok_or("未配置 jev：在 Lyra 添加 Command Code 提供方，或设置 NOVA_JEV_API_KEY")?;
    let key = crate::lyra_complete::resolve_env_string(provider["options"]["apiKey"].as_str().unwrap_or_default(), &env)?;
    if key.trim().is_empty() { return Err("Command Code 提供方缺少 apiKey".into()); }
    let base = provider["options"]["baseURL"].as_str().unwrap_or("https://api.commandcode.ai/provider/v1");
    let proxy = crate::lyra::config::resolve_proxy(&Value::Null, provider, &env)?.unwrap_or(proxy);
    Ok(Endpoint { url: url.unwrap_or_else(|| crate::lyra_complete::join_url(base, "systemone")), key: key.trim().into(),
        model: model.unwrap_or_else(|| "typesafe/jev-1.14.0".into()), proxy, relay: false })
}

/// jev is configured and not switched off (run can bind steps from screenshots).
pub(crate) fn available() -> bool { endpoint().is_ok() }

/// What jev sees: "marks" (default, numbered boxes on the screenshot), "plain" or "none".
/// Set NOVA_JEV_IMAGE to compare them.
pub(crate) fn image_mode() -> &'static str {
    match std::env::var("NOVA_JEV_IMAGE").as_deref() { Ok("plain") => "plain", Ok("none") => "none", _ => "marks" }
}

// 3x5 digit glyphs, one row per 3 bits.
const DIGITS: [[u8; 5]; 10] = [[7,5,5,5,7],[2,6,2,2,7],[7,1,7,4,7],[7,1,7,1,7],[5,5,7,1,1],[7,4,7,1,7],[7,4,7,5,7],[7,1,1,1,1],[7,5,7,5,7],[7,5,7,1,7]];
const PALETTE: [[u8; 3]; 6] = [[230,25,75],[0,130,200],[245,130,48],[145,30,180],[60,180,75],[240,50,230]];

/// Set-of-Mark: numbered boxes over the controls so jev matches options to pixels instead of prose
/// positions. Rects are CSS viewport px; `meta` is the screenshot's x/y/width/height/pixelWidth/pixelHeight.
pub(crate) fn mark(path: &Path, meta: &Value, boxes: &[(usize, [f64; 4])]) -> Result<std::path::PathBuf, String> {
    let mut img = image::open(path).map_err(|e| format!("读取截图失败：{e}"))?.into_rgba8();
    let num = |k: &str| meta[k].as_f64().filter(|v| v.is_finite()).ok_or("截图缺少坐标映射");
    let (ox, oy, sx, sy) = (num("x")?, num("y")?, num("pixelWidth")? / num("width")?, num("pixelHeight")? / num("height")?);
    let (iw, ih) = (img.width() as i64, img.height() as i64);
    let mut fill = |img: &mut image::RgbaImage, x0: i64, y0: i64, x1: i64, y1: i64, c: [u8; 3]| {
        for y in y0.max(0)..y1.min(ih) { for x in x0.max(0)..x1.min(iw) { img.put_pixel(x as u32, y as u32, image::Rgba([c[0], c[1], c[2], 255])); } }
    };
    for &(n, [x, y, w, h]) in boxes {
        let (x0, y0) = (((x - ox) * sx) as i64, ((y - oy) * sy) as i64);
        let (x1, y1) = (((x + w - ox) * sx) as i64, ((y + h - oy) * sy) as i64);
        if x1 <= x0 || y1 <= y0 || x1 <= 0 || y1 <= 0 || x0 >= iw || y0 >= ih { continue; }
        let c = PALETTE[n % PALETTE.len()];
        for (a, b, cc, d) in [(x0, y0, x1, y0 + 2), (x0, y1 - 2, x1, y1), (x0, y0, x0 + 2, y1), (x1 - 2, y0, x1, y1)] { fill(&mut img, a, b, cc, d, c); }
        // Label: scale-2 glyphs (6x10) on a filled tab above the box, inside it at the top edge.
        let digits: Vec<usize> = n.to_string().bytes().map(|b| (b - b'0') as usize).collect();
        let (lw, lh) = (digits.len() as i64 * 8 + 2, 14);
        let ly = if y0 >= lh { y0 - lh } else { y0 };
        fill(&mut img, x0, ly, x0 + lw, ly + lh, c);
        for (i, d) in digits.iter().enumerate() {
            for (row, bits) in DIGITS[*d].iter().enumerate() { for col in 0..3 { if bits >> (2 - col) & 1 == 1 {
                let (px, py) = (x0 + 2 + i as i64 * 8 + col * 2, ly + 2 + row as i64 * 2);
                fill(&mut img, px, py, px + 2, py + 2, [255, 255, 255]);
            } } }
        }
    }
    let out = path.with_extension("marks.jpg");
    std::fs::write(&out, crate::visual_guard::encode_jpeg(&img)?).map_err(|e| e.to_string())?;
    Ok(out)
}

fn body(model: &str, mut state: Value, instructions: &str, criteria: &Map<String, Value>, image: Option<Vec<u8>>) -> Value {
    let mut body = json!({"model":model,"questions":{"next":{"type":"choice","instructions":instructions,"criteria":criteria}}});
    if let Some(bytes) = image {
        state["screenshot"] = json!("screenshot");
        body["assets"] = json!({"screenshot":{"type":"image","format":"jpg",
            "data_base64":base64::engine::general_purpose::STANDARD.encode(bytes)}});
    }
    body["state"] = state;
    body
}

fn answer(response: &Value, criteria: &Map<String, Value>) -> Result<(String, f64), String> {
    let next = &response["answers"]["next"];
    let choice = next["choice"].as_str().filter(|c| criteria.contains_key(*c)).ok_or("jev 返回了无效选项")?;
    Ok((choice.to_string(), next["confidence"].as_f64().unwrap_or(0.).clamp(0., 1.)))
}

fn request(ep: &Endpoint, body: &Value) -> Result<reqwest::RequestBuilder, String> {
    let request = crate::lyra::provider::client_for_proxy(ep.proxy.trim()).post(&ep.url)
        .bearer_auth(&ep.key).header("TypeSafe-Danger-Zone-Images", "true");
    Ok(if ep.relay {
        request.header("Content-Type", "application/json").header("Content-Encoding", "gzip")
            .body(crate::relay::gzip_json(body)?)
    } else { request.json(body) })
}

/// One choice question: `criteria` maps option id → description. Returns (choice, confidence).
/// Read-only, so overload (429/503/529) is retried twice with backoff.
pub(crate) async fn choose(state: Value, instructions: &str, criteria: &Map<String, Value>, image: Option<&Path>) -> Result<(String, f64), String> {
    let ep = endpoint()?;
    let image = match image { Some(path) => Some(tokio::fs::read(path).await.map_err(|e| format!("读取截图失败：{e}"))?), None => None };
    let body = body(&ep.model, state, instructions, criteria, image);
    let request = request(&ep, &body)?;
    let mut attempt = 0;
    let response = loop {
        let request = request.try_clone().ok_or("jev 请求无法重试")?.send();
        // Connection drops are retried too (read-only request); timeouts are not, the run moves on.
        let response = match tokio::time::timeout(Duration::from_secs(15), request).await.map_err(|_| "jev 请求超时".to_string())? {
            Err(_) if attempt < 1 => { attempt += 1; continue; }
            sent => sent.map_err(|e| format!("jev 请求失败：{}", if e.is_connect() { "连接失败" } else { "网络错误" }))?,
        };
        if !matches!(response.status().as_u16(), 429 | 503 | 529) || attempt >= 2 { break response; }
        tokio::time::sleep(Duration::from_millis(500 << attempt)).await;
        attempt += 1;
    };
    let status = response.status().as_u16();
    let text = tokio::time::timeout(Duration::from_secs(10), response.text()).await
        .map_err(|_| "jev 响应超时".to_string())?.map_err(|_| "jev 响应读取失败".to_string())?;
    if !(200..300).contains(&status) { return Err(format!("jev HTTP {status}：{}", text.chars().take(200).collect::<String>())); }
    answer(&serde_json::from_str(&text).map_err(|_| "jev 响应不是有效 JSON")?, criteria)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_priority_is_relay_local_then_disabled() {
        let mut settings = crate::settings::Settings {
            relay_server: " https://relay.example/team/// ".into(), relay_token: " relay-token ".into(),
            jev_api_url: "https://local.example/systemone".into(), jev_api_key: "local-key".into(),
            jev_model: "local-model".into(), lyra_proxy: "http://127.0.0.1:7890".into(),
            ..Default::default()
        };
        let mut env = HashMap::from([
            ("NOVA_JEV_API_KEY".into(), "env-key".into()),
            ("NOVA_JEV_API_URL".into(), "https://env.example/systemone".into()),
            ("NOVA_JEV_MODEL".into(), "env-model".into()),
        ]);
        let unused = || -> Result<Value, String> { panic!("must not load local providers") };
        let ep = resolve_endpoint(&settings, &env, unused).unwrap();
        assert!(ep.relay);
        assert_eq!((&*ep.url, &*ep.key, &*ep.model, &*ep.proxy),
            ("https://relay.example/team/cc/systemone", "relay-token", "typesafe/jev-1.14.0", ""));
        settings.jev_vision_enabled = false;
        assert!(resolve_endpoint(&settings, &env, unused).is_err());
        settings.jev_vision_enabled = true;
        env.insert("NOVA_JEV".into(), " off ".into());
        assert!(resolve_endpoint(&settings, &env, unused).is_err());
        env.remove("NOVA_JEV");
        settings.relay_token = " ".into();
        let ep = resolve_endpoint(&settings, &env, unused).unwrap();
        assert!(!ep.relay);
        assert_eq!((&*ep.url, &*ep.key, &*ep.model, &*ep.proxy),
            ("https://local.example/systemone", "local-key", "local-model", "http://127.0.0.1:7890"));
        settings.relay_server.clear();
        settings.relay_token = "relay-token".into();
        assert!(!resolve_endpoint(&settings, &env, unused).unwrap().relay);
        settings.jev_api_key.clear();
        settings.jev_api_url.clear();
        settings.jev_model.clear();
        let ep = resolve_endpoint(&settings, &env, unused).unwrap();
        assert_eq!((&*ep.url, &*ep.key, &*ep.model), ("https://env.example/systemone", "env-key", "env-model"));
        env.clear();
        let ep = resolve_endpoint(&settings, &env, || Ok(json!({"provider":{"cc":{
            "preset":"commandcode","options":{"apiKey":"cc-key"}
        }}}))).unwrap();
        assert!(!ep.relay);
        assert_eq!((&*ep.url, &*ep.key, &*ep.model),
            ("https://api.commandcode.ai/provider/v1/systemone", "cc-key", "typesafe/jev-1.14.0"));
        assert!(resolve_endpoint(&settings, &env, || Ok(json!({}))).is_err());
    }

    #[test]
    fn only_relay_requests_are_gzipped_and_retries_keep_the_body() {
        use std::io::Read;
        let criteria = serde_json::from_value(json!({"c0":"选择此项","none":"都不是"})).unwrap();
        let payload = body("typesafe/jev-1.14.0", json!({"goal":"test"}), "pick", &criteria, Some(vec![1, 2, 3]));
        for relay in [true, false] {
            let ep = Endpoint { url: "https://example.test/cc/systemone".into(), key: "test-key".into(),
                model: "typesafe/jev-1.14.0".into(), proxy: String::new(), relay };
            let builder = request(&ep, &payload).unwrap();
            let retry = builder.try_clone().unwrap().build().unwrap();
            let req = builder.build().unwrap();
            assert_eq!(req.headers()["Authorization"], "Bearer test-key");
            assert_eq!(req.headers()["Content-Type"], "application/json");
            assert_eq!(req.headers()["TypeSafe-Danger-Zone-Images"], "true");
            let bytes = req.body().unwrap().as_bytes().unwrap();
            assert_eq!(retry.body().unwrap().as_bytes().unwrap(), bytes);
            let decoded = if relay {
                assert_eq!(req.headers()["Content-Encoding"], "gzip");
                let mut decoded = Vec::new();
                flate2::read::GzDecoder::new(bytes).read_to_end(&mut decoded).unwrap();
                decoded
            } else {
                assert!(!req.headers().contains_key("Content-Encoding"));
                bytes.to_vec()
            };
            assert_eq!(serde_json::from_slice::<Value>(&decoded).unwrap(), payload);
        }
    }

    #[test]
    fn marks_map_css_rects_to_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        image::RgbaImage::from_pixel(200, 100, image::Rgba([255, 255, 255, 255])).save(&path).unwrap();
        // CSS viewport 100x50 rendered at 2x; box at CSS (10,20) 30x10 → pixels (20,40)-(80,60).
        let meta = json!({"x":0,"y":0,"width":100,"height":50,"pixelWidth":200,"pixelHeight":100});
        let out = mark(&path, &meta, &[(0, [10., 20., 30., 10.])]).unwrap();
        let img = image::open(out).unwrap().into_rgb8();
        let red = |p: &image::Rgb<u8>| p[0] > 180 && p[1] < 90;
        assert!(red(img.get_pixel(50, 59)), "bottom edge");
        assert!(!red(img.get_pixel(50, 50)), "box interior stays clear");
        assert!(red(img.get_pixel(21, 27)), "label tab above the box");
    }

    #[test]
    fn request_carries_image_and_answer_rejects_unknown_choices() {
        let criteria: Map<String, Value> = serde_json::from_value(json!({"c0":"点击 United States","none":"都不是"})).unwrap();
        let sent = body("m", json!({"goal":"g"}), "pick", &criteria, Some(vec![1, 2, 3]));
        assert_eq!(sent["state"]["screenshot"], "screenshot");
        assert_eq!(sent["assets"]["screenshot"]["data_base64"], "AQID");
        assert_eq!(sent["questions"]["next"]["criteria"]["c0"], "点击 United States");
        assert!(body("m", json!({}), "pick", &criteria, None).get("assets").is_none());
        let ok = json!({"answers":{"next":{"type":"choice","choice":"c0","confidence":0.9}}});
        assert_eq!(answer(&ok, &criteria).unwrap(), ("c0".into(), 0.9));
        assert!(answer(&json!({"answers":{"next":{"choice":"c9","confidence":1}}}), &criteria).is_err());
    }
}
