//! 本地 BPE 计数。文本在所选词表下精确；服务端封装、闭源词表和多模态成本不是精确值。
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{LazyLock, Mutex};
use tiktoken_rs::tokenizer::{get_tokenizer, Tokenizer};

/// 每张图片的视觉 token 估算；本地预算与 provider 出站计数共用。
// ponytail: 视觉成本取决于分辨率/服务商切块，1600≈Claude 单图上限；接入原生 count API 后替换。
pub const IMAGE_TOKENS: u64 = 1600;

// ponytail: 进程级 (词表, 内容 hash) → 计数缓存，满 CACHE_LIMIT 整体清空；
// 仍需每次序列化+hash（O(字节)，远低于 BPE）。真成瓶颈再按历史下标增量维护。
const CACHE_LIMIT: usize = 16_384;
static MESSAGE_CACHE: LazyLock<Mutex<HashMap<u64, TokenBreakdown>>> = LazyLock::new(Default::default);
static JSON_CACHE: LazyLock<Mutex<HashMap<u64, u64>>> = LazyLock::new(Default::default);

fn memo<T: Clone>(cache: &Mutex<HashMap<u64, T>>, key: u64, count: impl FnOnce() -> T) -> T {
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) { return hit; }
    let value = count();
    if let Ok(mut c) = cache.lock() {
        if c.len() >= CACHE_LIMIT { c.clear(); }
        c.insert(key, value.clone());
    }
    value
}

#[derive(Clone, Debug)]
pub struct TokenCounter {
    model: String,
    api: String,
    o200k: bool,
    source: &'static str,
    model_tokenizer_known: bool,
}

impl Default for TokenCounter {
    fn default() -> Self {
        Self { model: String::new(), api: String::new(), o200k: true, source: "fallback", model_tokenizer_known: false }
    }
}

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
    /// options.tokenizer 覆盖选择，但不证明闭源/代理模型真的使用该词表。
    pub fn for_model(model: &str, api: &str, tokenizer_override: Option<&str>) -> Result<Self, String> {
        let name = model.to_ascii_lowercase();
        let api = api.to_ascii_lowercase();
        let openai = matches!(api.as_str(), "openai-completions" | "openai-responses" | "openai" | "responses" | "completions");
        let known = if openai {
            // tiktoken-rs 0.9 的映射早于 GPT-5.x；这些模型的普通文本仍用 o200k。
            if name.starts_with("gpt-5.") { Some(Tokenizer::O200kBase) } else { get_tokenizer(&name) }
        } else { None };
        let known = match known {
            Some(Tokenizer::O200kBase) => Some(true),
            Some(Tokenizer::Cl100kBase) => Some(false),
            _ => None,
        };
        // 前缀命中可能只是自定义别名（如 gpt-4o-private），不能宣称其词表已知。
        let exact_model = matches!(name.as_str(), "gpt-4" | "gpt-3.5-turbo" | "gpt-35-turbo"
            | "gpt-4o" | "gpt-4o-mini" | "gpt-4.1" | "gpt-4.1-mini" | "gpt-4.1-nano"
            | "gpt-5" | "gpt-5-mini" | "gpt-5-nano" | "gpt-5.1" | "gpt-5.2" | "o1" | "o3" | "o3-mini" | "o4-mini");
        let (o200k, source) = match tokenizer_override {
            Some(value) => (match value.trim().to_ascii_lowercase().as_str() {
                "o200k" | "o200k_base" => true,
                "cl100k" | "cl100k_base" => false,
                _ => return Err(format!("unsupported options.tokenizer {value:?}; use o200k_base or cl100k_base")),
            }, "override"),
            None => (known.unwrap_or(!api.starts_with("anthropic")),
                if known.is_some() && exact_model { "model" } else if known.is_some() { "inferred" } else { "fallback" }),
        };
        Ok(Self { model: model.into(), api, o200k, source, model_tokenizer_known: exact_model && known == Some(o200k) })
    }

    /// 词表是否确认为该模型所用；否则 BPE 只是近似（如 Claude 走 cl100k 回退）。
    pub fn tokenizer_known(&self) -> bool {
        self.model_tokenizer_known
    }

    fn key(&self, text: &str) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (self.o200k, text).hash(&mut h);
        h.finish()
    }

    /// 按 value 内容缓存 `count` 的结果（同词表下同内容计数相同）。
    pub fn count_cached(&self, value: &Value, count: impl FnOnce() -> u64) -> u64 {
        memo(&JSON_CACHE, self.key(&value.to_string()), count)
    }

    pub fn metadata(&self) -> Value {
        json!({
            "model": self.model, "api": self.api,
            "encoding": if self.o200k { "o200k_base" } else { "cl100k_base" },
            "source": self.source, "textBpeExact": true,
            "modelTokenizerKnown": self.model_tokenizer_known, "promptExact": false,
            "jsonCountScope": "BPE of Value::to_string() JSON text, NOT the server's model-input serialization or multimodal token cost",
            "limitations": "BPE counts are exact only for the supplied text and selected vocabulary. Model aliases/closed tokenizers, server framing, tool rendering and multimodal tokens remain estimates; provider usage is authoritative."
        })
    }

    pub fn count_text(&self, text: &str) -> u64 {
        if text.is_empty() { return 0; }
        // 库的线程安全 singleton 只加载一次内嵌词表，不为每次请求重新构造 tokenizer。
        let bpe = if self.o200k { tiktoken_rs::o200k_base_singleton() } else { tiktoken_rs::cl100k_base_singleton() };
        // 普通用户文本中的 <|...|> 不能被当作协议 special token。
        bpe.encode_ordinary(text).len() as u64
    }

    /// 精确计数 Value::to_string() 的 BPE，而非服务端 prompt tokens。
    /// provider 应传最终出站请求；base64 图片 JSON 字节不等于视觉 token。
    pub fn count_json(&self, value: &Value) -> u64 {
        self.count_text(&value.to_string())
    }

    /// Lyra 规范消息分项，不替代 provider 对最终序列化请求的计数。
    /// 按消息内容缓存：历史只追加时每次往返只对新消息做 BPE。
    pub fn count_messages(&self, messages: &[Value]) -> TokenBreakdown {
        let mut out = TokenBreakdown::default();
        for message in messages {
            out.add(&memo(&MESSAGE_CACHE, self.key(&message.to_string()), || self.count_message(message)));
        }
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
