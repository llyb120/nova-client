//! 本地 token 估算（字符启发式）。只用于发送前预算；provider 返回的 usage 才是实测，
//! 由 ContextWindow 以上次实测为锚点，只对其后新增的消息做估算。
use serde::Serialize;
use serde_json::{json, Value};

/// 每张图片的视觉 token 估算；本地预算与 provider 出站计数共用。
// ponytail: 视觉成本取决于分辨率/服务商切块，1600≈Claude 单图上限；接入原生 count API 后替换。
pub const IMAGE_TOKENS: u64 = 1600;

#[derive(Clone, Debug, Default)]
pub struct TokenCounter;

/// 分项均未校准，互不重复；total() 是它们的和。数量字段不计入 token 总数。
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenBreakdown {
    pub system: u64,
    pub tools: u64,
    pub messages_text: u64,
    pub tool_arguments: u64,
    pub tool_results: u64,
    pub reasoning: u64,
    pub images: u64,
    pub other: u64,
    pub overhead: u64,
    pub message_count: u64,
    pub image_count: u64,
    pub unknown_part_count: u64,
}

impl TokenBreakdown {
    pub fn total(&self) -> u64 {
        self.system + self.tools + self.messages_text + self.tool_arguments + self.tool_results
            + self.reasoning + self.images + self.other + self.overhead
    }

    fn add(&mut self, o: &Self) {
        self.system += o.system; self.tools += o.tools; self.messages_text += o.messages_text;
        self.tool_arguments += o.tool_arguments; self.tool_results += o.tool_results; self.reasoning += o.reasoning;
        self.images += o.images; self.other += o.other; self.overhead += o.overhead;
        self.message_count += o.message_count; self.image_count += o.image_count; self.unknown_part_count += o.unknown_part_count;
    }
}

impl TokenCounter {
    pub fn metadata(&self) -> Value {
        json!({
            "method": "heuristic", "promptExact": false,
            "limitations": "ASCII ~4 chars/token, other chars ~1 token each; images/unknown parts are placeholders. Provider usage is authoritative and anchors the estimate."
        })
    }

    // ponytail: 字符启发式，单条文本误差约 ±20%（CJK 偏保守）；长会话由实测 usage 锚定，只有新增部分是估算。
    pub fn count_text(&self, text: &str) -> u64 {
        let (ascii, other) = text.chars().fold((0_u64, 0_u64), |(a, o), ch| if ch.is_ascii() { (a + 1, o) } else { (a, o + 1) });
        ascii.div_ceil(4) + other
    }

    /// 估算 Value::to_string()，而非服务端 prompt tokens；base64 图片 JSON 字节不等于视觉 token。
    pub fn count_json(&self, value: &Value) -> u64 {
        self.count_text(&value.to_string())
    }

    /// Lyra 规范消息分项，不替代 provider 实测。
    pub fn count_messages(&self, messages: &[Value]) -> TokenBreakdown {
        let mut out = TokenBreakdown::default();
        for message in messages { out.add(&self.count_message(message)); }
        out
    }

    fn count_message(&self, message: &Value) -> TokenBreakdown {
        let mut out = TokenBreakdown::default();
        {
            out.message_count += 1;
            // ponytail: 每消息 4 token 封装为启发式；升级为 provider 原生 count API 后替换。
            out.overhead += 4;
            for key in ["toolCallId", "toolName", "name"] {
                if let Some(text) = message[key].as_str() { out.overhead += self.count_text(text); }
            }
            let result = message["role"] == "toolResult" || message["role"] == "tool";
            if result {
                // provider 会从 details 路径加载截图；这里只计候选图片，不读取文件/base64。
                // 是否存在、是否被最近图片策略丢弃须由 provider 最终请求计数确定。
                let images = message.pointer("/details/images").and_then(Value::as_array).into_iter().flatten()
                    .filter(|image| image["path"].is_string()).count()
                    + usize::from(message.pointer("/details/imagePath").is_some_and(Value::is_string));
                out.image_count += images.min(16) as u64;
                out.images += images.min(16) as u64 * IMAGE_TOKENS;
            }
            if let Some(text) = message["content"].as_str() {
                if result { out.tool_results += self.count_text(text); } else { out.messages_text += self.count_text(text); }
            }
            for part in message["content"].as_array().into_iter().flatten() {
                match part["type"].as_str() {
                    Some("text") => {
                        let count = self.count_text(part["text"].as_str().unwrap_or_default());
                        if result { out.tool_results += count; } else { out.messages_text += count; }
                    }
                    Some("toolCall") => {
                        out.overhead += 4 + self.count_text(part["name"].as_str().unwrap_or_default())
                            + self.count_text(part["id"].as_str().unwrap_or_default());
                        if let Some(args) = part.get("arguments") {
                            out.tool_arguments += match args.as_str() {
                                Some(text) => self.count_text(text),
                                None => self.count_json(args),
                            };
                        }
                    }
                    Some("image" | "image_url" | "input_image") => {
                        out.images += IMAGE_TOKENS;
                        out.image_count += 1;
                    }
                    Some("thinking" | "reasoning") => {
                        out.reasoning += self.count_text(part["thinking"].as_str().or_else(|| part["text"].as_str()).unwrap_or_default());
                    }
                    _ => {
                        // 未知/音频/文件块可能包含巨大 base64；不把传输载荷当作模型文本。
                        // ponytail: 未支持模态暂按 1200/block 占位估算；需 provider 原生计数才能准确预算。
                        out.other += 1200;
                        out.unknown_part_count += 1;
                    }
                }
            }
        }
        out
    }
}
