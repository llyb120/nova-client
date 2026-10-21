//! Shared tool-restricted one-shot API. Callers choose a backend/model, not a transport.
//! Native generation/print is preferred; otherwise a private backend session is created
//! and disposed after one turn. User conversations are never reused or added to the UI.
use crate::{threads::AgentKind, AppState};
use serde_json::Value;
use std::{sync::{atomic::{AtomicBool, Ordering}, Arc}, time::Duration};

/// Limits apply to the entire call, including backend startup. Native token limits
/// are forwarded where supported; `max_output_bytes` is enforced for every backend.
pub(crate) struct GenerateTextOptions<'a> {
    pub system: &'a str,
    /// Inline images: {type:"image", mimeType:"image/png", data:"base64..."}.
    pub images: &'a [Value],
    pub max_output_tokens: Option<u64>,
    pub max_output_bytes: usize,
    pub timeout: Duration,
}

impl Default for GenerateTextOptions<'_> {
    fn default() -> Self {
        Self { system: "", images: &[], max_output_tokens: None,
            max_output_bytes: 180 * 1024, timeout: Duration::from_secs(60) }
    }
}

impl AppState {
    /// No tools or retained user conversation. This layer never retries or silently
    /// substitutes another backend; native SDK transport retry policies still apply.
    /// A blank model uses the chosen backend's default. Resolve scene_model first
    /// when the caller wants a configured scene rather than an explicit target.
    pub(crate) async fn generate_text_once(
        &self, kind: &AgentKind, model: &str, prompt: &str, options: GenerateTextOptions<'_>,
    ) -> Result<String, String> {
        validate(prompt, &options)?;
        let reply = async {
            if *kind == AgentKind::Lyra {
                return lyra_text(self, model, prompt, &options).await;
            }
            let prompt = if options.system.is_empty() { prompt.to_owned() }
                else { format!("{}\n\n{prompt}", options.system) };
            match kind {
                AgentKind::Lyra => unreachable!(),
                AgentKind::Codex | AgentKind::CodexPlus => self.codexplus.generate_text(model, prompt, options.images, options.timeout).await,
                AgentKind::Unsupported => Err("该后端已移除，请选择其他后端".into()),
                AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => self.codebuddy.generate_text(model, prompt, options.images, options.timeout).await,
                AgentKind::WorkBuddy => self.workbuddy.generate_text(model, prompt, options.images, options.timeout).await,
                AgentKind::Claude => self.claude.generate_text(model, prompt, options.images, options.timeout).await,
                AgentKind::Kimi => self.kimi.generate_text(model, prompt, options.images, options.timeout).await,
                AgentKind::Devin => self.acp.generate_text(model, prompt, options.images, options.timeout).await,
            }
        };
        let text = tokio::time::timeout(options.timeout, reply).await
            .map_err(|_| format!("{} 一次性生成超时，已取消，未自动重试", kind.label()))??;
        validate_output(&text, options.max_output_bytes)?;
        Ok(text)
    }
}

pub(crate) fn validate(prompt: &str, options: &GenerateTextOptions<'_>) -> Result<(), String> {
    use base64::Engine;
    if prompt.trim().is_empty() { return Err("一次性生成的 prompt 不能为空".into()); }
    if options.timeout.is_zero() || options.max_output_bytes == 0 || options.max_output_tokens == Some(0) {
        return Err("生成超时与输出上限必须大于零".into());
    }
    if std::time::Instant::now().checked_add(options.timeout).is_none() {
        return Err("生成超时设置超出支持范围".into());
    }
    let mut size = prompt.len().saturating_add(options.system.len());
    for image in options.images {
        if image["type"] != "image" || !matches!(image["mimeType"].as_str(), Some("image/png" | "image/jpeg" | "image/gif" | "image/webp")) {
            return Err("一次性生成仅接受内联 PNG/JPEG/GIF/WebP 图片".into());
        }
        let data = image["data"].as_str().filter(|s| !s.is_empty()).ok_or("图片缺少内联 base64 数据")?;
        size = size.saturating_add(data.len());
        if size > 32 * 1024 * 1024 { return Err("一次性生成输入超过 32 MiB".into()); }
        base64::engine::general_purpose::STANDARD.decode(data).map_err(|_| "图片 base64 数据无效")?;
    }
    if size > 32 * 1024 * 1024 { return Err("一次性生成输入超过 32 MiB".into()); }
    Ok(())
}

fn validate_output(text: &str, limit: usize) -> Result<(), String> {
    if text.trim().is_empty() { return Err("一次性生成返回空结果".into()); }
    if text.len() > limit { return Err(format!("生成结果超过 {limit} UTF-8 字节，未返回截断结果")); }
    Ok(())
}

async fn lyra_text(state: &AppState, model: &str, prompt: &str, options: &GenerateTextOptions<'_>) -> Result<String, String> {
    use crate::lyra::{config, provider::{self, StreamEvent}};
    let config = config::Roots::borrowed(state.config_dir.clone()).load_config(None)?;
    let mut resolved = config::resolve_model(&config, Some(model).filter(|s| !s.is_empty()), &config::process_env())?;
    if let Some(limit) = options.max_output_tokens { resolved.model.max_output_tokens = resolved.model.max_output_tokens.min(limit); }
    // Arbitrary provider fields may override tools/messages/token limits; don't allow
    // them through the no-tools boundary. Resolved sampling/headers/proxy are retained.
    resolved.model.extra_options.clear();
    let proxy = state.settings.lock().map_err(|e| e.to_string())?.lyra_proxy.clone();
    let http = provider::client_for_proxy(proxy.trim());
    let messages = [crate::lyra::history::user_message(prompt, options.images)];
    let cancel = Arc::new(AtomicBool::new(false));
    let mut text = String::new();
    let mut oversized = false;
    let result = provider::stream_chat(&http, &resolved.model, &resolved.api_key, Some("off"),
        options.system, &messages, &[], None, &cancel, &mut |event| {
            if let StreamEvent::TextDelta(delta) = event {
                if text.len().saturating_add(delta.len()) > options.max_output_bytes {
                    oversized = true;
                    cancel.store(true, Ordering::SeqCst);
                } else if !oversized { text.push_str(&delta); }
            }
        }).await;
    if oversized { return Err(format!("生成结果超过 {} UTF-8 字节，未返回截断结果", options.max_output_bytes)); }
    let result = result?;
    if result.error_message.is_some() || matches!(result.stop_reason.as_str(), "error" | "aborted" | "length") {
        return Err(result.error_message.unwrap_or_else(|| format!("一次性生成未正常完成：{}", result.stop_reason)));
    }
    if result.content.iter().any(|part| part["type"] == "toolCall") {
        return Err("纯文本生成返回了未授权工具调用，已拒绝".into());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shared_boundary_checks_limits_and_images_without_truncating_text() {
        let options = GenerateTextOptions::default();
        assert!(validate("hello", &options).is_ok());
        assert!(validate(" \n", &options).is_err());
        assert!(validate("hello", &GenerateTextOptions { timeout: Duration::ZERO, ..Default::default() }).is_err());
        assert!(validate("hello", &GenerateTextOptions { timeout: Duration::MAX, ..Default::default() }).is_err());
        assert!(validate("hello", &GenerateTextOptions { max_output_tokens: Some(0), ..Default::default() }).is_err());
        let images = [json!({"type":"image", "mimeType":"image/png", "data":"YQ=="})];
        assert!(validate("hello", &GenerateTextOptions { images: &images, ..Default::default() }).is_ok());
        for image in [json!({"type":"local_image", "path":"secret"}), json!({"type":"image", "mimeType":"image/png", "data":"!"})] {
            assert!(validate("hello", &GenerateTextOptions { images: &[image], ..Default::default() }).is_err());
        }
        assert!(validate_output("中文", 6).is_ok());
        assert!(validate_output("中文", 5).is_err());
        assert!(validate_output(" \n", 6).is_err());
    }
}
