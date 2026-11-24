//! Nova 24 个自定义 MCP 工具的 Schema 目录、Go RE2 `pattern` 清洗与 16.5k Token 预算压缩。
//!
//! 关键约束：
//! - 严格不含 `tauri` 依赖，便于独立验证 crate 与 sidecar 复用；
//! - 全部 24 个工具（`polaris`、`code_map`、8 个 `jianlai_*`、14 个 `chrome_*`）的原始 schema
//!   合计约 21k tokens，超过 `agy` ~20k 常驻工具上限后会静默丢弃体积最大的工具；
//! - 按 `ceil(utf8_bytes / 3)` 估算每条工具 JSON 体积，通过「保留结构约束精简嵌套说明 +
//!   二分裁切顶层 description」将总量压到 ≤ 16,500 tokens、工具数 ≤ 64；
//! - 每条工具附加 `_meta: { "antigravity/alwaysLoad": true }`；
//! - 绝不向工具 description 注入 `jianlai_tools::RULES` / `chrome_tools::RULES` 长文本
//!   （该段规则精简后通过 `systemPrompt.append` 注入）。

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// 目标常驻工具 token 预算（低于 `agy` 20k 硬限，留足余量）。
pub const TOOL_TOKEN_BUDGET: usize = 16_500;
/// 单服务器最大暴露工具数量。
pub const MAX_TOOLS: usize = 64;
/// 常驻预加载标记键名。
pub const ALWAYS_LOAD_META_KEY: &str = "antigravity/alwaysLoad";

/// 静态嵌入的真实工具 Schema（与 `jianlai_tools.rs` / `chrome_tools.rs` 同源）。
pub const JIANLAI_TOOLS_JSON: &str = include_str!("../../../../scripts/jianlai-tools.json");
pub const CHROME_TOOLS_JSON: &str = include_str!("../../../../scripts/chrome-tools.json");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McpToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetReport {
    pub tool_count: usize,
    pub dropped_tools: usize,
    pub initial_tokens: usize,
    pub final_tokens: usize,
    pub sanitized_patterns: usize,
    pub max_description_chars: Option<usize>,
}

/// 按 `agy` 规则估算单条 JSON 值的 token 数：`ceil(utf8_bytes / 3)`。
pub fn estimate_json_tokens(value: &Value) -> usize {
    let bytes = serde_json::to_vec(value).map(|v| v.len()).unwrap_or(0);
    bytes.div_ceil(3)
}

/// 将单条工具转换为标准 MCP `tools/list` 条目（含 `_meta: {"antigravity/alwaysLoad": true}`）。
pub fn tool_spec_to_mcp_json(spec: &McpToolSpec) -> Value {
    json!({
        "name": spec.name,
        "description": spec.description,
        "inputSchema": spec.input_schema,
        "_meta": {
            ALWAYS_LOAD_META_KEY: true
        }
    })
}

/// 估算整组工具序列化后的总 token 数。
pub fn estimate_tools_tokens(tools: &[McpToolSpec]) -> usize {
    tools
        .iter()
        .map(|tool| estimate_json_tokens(&tool_spec_to_mcp_json(tool)))
        .sum()
}

/// 构造 Nova 全量 24 个自定义 MCP 工具（2 个上下文 + 8 个 jianlai + 14 个 chrome）。
pub fn build_nova_mcp_tools(jianlai_json: &str, chrome_json: &str) -> Vec<McpToolSpec> {
    let mut tools = Vec::with_capacity(24);

    // 1. polaris（与 src-tauri/src/lyra/tools.rs:58-78 保持参数一致）
    tools.push(McpToolSpec {
        name: "polaris".into(),
        description: "跨文件快速检索代码库概览（命中行、符号签名、定位与影响面）。适合首次定位模块或跨文件查找；已知精确行段直接读文件。".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "keywords": {
                    "anyOf": [
                        { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                        { "type": "string", "minLength": 1 }
                    ],
                    "description": "关键词或符号名（字符串自动转数组，最多取前 5 项）"
                },
                "task": {
                    "type": "string",
                    "description": "一句话任务描述，用于补充检索词与排序"
                },
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "maxItems": 6,
                    "description": "已知必看文件列表（最多 6 个）"
                },
                "budget": {
                    "type": "integer",
                    "minimum": 100,
                    "maximum": 1200,
                    "description": "输出行预算（100-1200）"
                },
                "maxBytes": {
                    "type": "integer",
                    "minimum": 4096,
                    "maximum": 65536,
                    "description": "输出字节硬上限（默认 12288）"
                },
                "deep": {
                    "type": "boolean",
                    "description": "默认 false；仅确需递归依赖与别名追踪时设为 true"
                },
                "coupling": {
                    "type": "boolean",
                    "description": "是否附带 git 高频共改耦合提示"
                }
            }
        }),
    });

    // 2. code_map（与 src-tauri/src/nova_tools_native/context.rs:3841-3908 保持参数一致）
    tools.push(McpToolSpec {
        name: "code_map".into(),
        description: "返回仓库文件与符号地图：scope 留空返回全仓文件行数与符号数，指定目录前缀返回该目录下各文件的顶层符号大纲。".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "scope": {
                    "type": "string",
                    "description": "可选目录前缀（如 src-tauri/src）；留空返回全仓概览"
                }
            }
        }),
    });

    // 3. 8 个 jianlai_* 工具（来自 scripts/jianlai-tools.json）
    append_json_tool_defs(&mut tools, jianlai_json);

    // 4. 14 个 chrome_* 工具（来自 scripts/chrome-tools.json）
    append_json_tool_defs(&mut tools, chrome_json);

    tools
}

fn append_json_tool_defs(out: &mut Vec<McpToolSpec>, raw_json: &str) {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(raw_json) else {
        return;
    };
    for item in items {
        let Some(name) = item
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let description = item
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let input_schema = item
            .get("parameters")
            .or_else(|| item.get("inputSchema"))
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
        out.push(McpToolSpec {
            name: name.to_string(),
            description,
            input_schema,
        });
    }
}

/// 使用仓库内置真实 JSON 资产构造全部 24 个工具（未裁剪原样）。
pub fn default_nova_mcp_tools() -> Vec<McpToolSpec> {
    build_nova_mcp_tools(JIANLAI_TOOLS_JSON, CHROME_TOOLS_JSON)
}

/// 返回全部 24 个已注册 Nova MCP 工具名。
pub fn known_nova_tool_names() -> Vec<String> {
    default_nova_mcp_tools()
        .into_iter()
        .map(|t| t.name)
        .collect()
}

/// 构造并按 `TOOL_TOKEN_BUDGET`（16,500 tokens）完成 RE2 清洗与预算压缩的 24 个工具。
pub fn build_fitted_nova_mcp_tools() -> (Vec<McpToolSpec>, BudgetReport) {
    let mut tools = default_nova_mcp_tools();
    let report = fit_tool_budget(&mut tools, TOOL_TOKEN_BUDGET);
    (tools, report)
}

/// 判断正则字符串是否可被 Go RE2 引擎安全编译且保持与 ECMA-262 一致的语义。
///
/// 显式拒绝会导致 `agy` toolbox 编译失败的语法：
/// - 前瞻/后顾：`(?=`, `(?!`, `(?<=`, `(?<!`
/// - 原子组/条件组/注释组：`(?>`, `(?(`, `(?#`
/// - 占有量词：`++`, `*+`, `?+`, `}+`
/// - Unicode 属性类：`\p{...}`, `\P{...}`（不同构建属性表不一致，直接剥离保安全）
/// - 反向引用与非法转义：`\1`-`\9`, `\k<`, `\uXXXX`, `\u{...}`, `\U`, `\c`, `\a`, `\A`, `\z`, `\Z`, `\G`, `\K`, `\Q`, `\E`, `\e`
/// - 字符类内的 `\b`/`\B`、不完整 `\x`、未闭合 `[...]` 或不匹配括号 `(...)`
pub fn is_re2_safe_pattern(source: &str) -> bool {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut in_class = false;
    let mut class_start = 0usize;
    let mut paren_depth: i32 = 0;

    while i < len {
        let b = bytes[i];
        if b == b'\\' {
            i += 1;
            if i >= len {
                return false;
            }
            let next = bytes[i];
            if in_class && matches!(next, b'b' | b'B') {
                return false;
            }
            if matches!(next, b'p' | b'P') {
                return false;
            }
            if next == b'x' {
                if i + 1 < len && bytes[i + 1] == b'{' {
                    let start = i + 2;
                    let mut end = start;
                    while end < len && bytes[end].is_ascii_hexdigit() {
                        end += 1;
                    }
                    if end == start || end >= len || bytes[end] != b'}' {
                        return false;
                    }
                    let hex_str = &source[start..end];
                    match u32::from_str_radix(hex_str, 16) {
                        Ok(cp) if cp <= 0x10_FFFF => {
                            i = end + 1;
                            continue;
                        }
                        _ => return false,
                    }
                } else {
                    if i + 2 >= len
                        || !bytes[i + 1].is_ascii_hexdigit()
                        || !bytes[i + 2].is_ascii_hexdigit()
                    {
                        return false;
                    }
                    i += 3;
                    continue;
                }
            }
            if next.is_ascii_alphanumeric() {
                // RE2 仅保留这些与 ECMA-262 含义一致的字母/数字转义；其余如 \1-\9, \k, \u, \U, \c, \Z, \K 等一律拒绝。
                let safe = matches!(
                    next,
                    b'f' | b'n' | b'r' | b't' | b'v' | b'd' | b'D' | b's' | b'S' | b'w' | b'W' | b'b' | b'B' | b'0'
                );
                if !safe {
                    return false;
                }
            }
            i += 1;
            continue;
        }

        if in_class {
            if b == b'[' && i + 1 < len && bytes[i + 1] == b':' {
                if let Some(rel) = source[i + 2..].find(":]") {
                    i = i + 2 + rel + 2;
                    continue;
                }
            }
            if b == b']' && i > class_start {
                in_class = false;
            }
            i += 1;
            continue;
        }

        match b {
            b'[' => {
                in_class = true;
                class_start = if i + 1 < len && bytes[i + 1] == b'^' {
                    i + 2
                } else {
                    i + 1
                };
            }
            b'(' => {
                paren_depth += 1;
                if i + 1 < len && bytes[i + 1] == b'?' {
                    let Some(&ahead) = bytes.get(i + 2) else {
                        return false;
                    };
                    if matches!(ahead, b'=' | b'!' | b'>' | b'(' | b'#') {
                        return false;
                    }
                    if ahead == b'<' {
                        if matches!(bytes.get(i + 3), Some(b'=' | b'!')) {
                            return false;
                        }
                    }
                }
            }
            b')' => {
                paren_depth -= 1;
                if paren_depth < 0 {
                    return false;
                }
            }
            b'+' | b'*' | b'?' | b'}' => {
                // 占有量词 `++`, `*+`, `?+`, `}+` 在 RE2 中非法
                if bytes.get(i + 1) == Some(&b'+') {
                    return false;
                }
            }
            _ => {}
        }
        i += 1;
    }

    !in_class && paren_depth == 0
}

/// 递归清洗 JSON Schema 中不兼容 Go RE2 的 `pattern` 字段，返回剥离数量。
pub fn sanitize_re2_patterns(schema: &mut Value) -> usize {
    let mut stripped = 0usize;
    match schema {
        Value::Object(map) => {
            if let Some(pat_val) = map.get("pattern") {
                let remove = match pat_val.as_str() {
                    Some(pat) => !is_re2_safe_pattern(pat),
                    None => true,
                };
                if remove {
                    map.remove("pattern");
                    stripped += 1;
                }
            }
            for (_, v) in map.iter_mut() {
                stripped += sanitize_re2_patterns(v);
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                stripped += sanitize_re2_patterns(item);
            }
        }
        _ => {}
    }
    stripped
}

/// 按目标预算执行工具数截断（≤ 64）、RE2 清洗与两级压缩：
/// - 第 1 级：在不破坏任何结构约束（`type`/`properties`/`required`/`enum`/`anyOf`/`minimum`/`maximum`/`minItems`/`maxItems`/`additionalProperties`）的前提下，
///   渐进精简 schema 内部深层重复的 `description`；
/// - 第 2 级：若仍超预算，对所有工具顶层 `description` 执行二分裁切直至总量 ≤ `max_tokens`；
/// - 兜底：若极端合成输入在清零全部 `description` 后仍超限，从体积最大的工具起裁剪。
pub fn fit_tool_budget(tools: &mut Vec<McpToolSpec>, max_tokens: usize) -> BudgetReport {
    let mut dropped_tools = 0usize;
    if tools.len() > MAX_TOOLS {
        dropped_tools += tools.len() - MAX_TOOLS;
        tools.truncate(MAX_TOOLS);
    }

    let mut sanitized_patterns = 0usize;
    for tool in tools.iter_mut() {
        sanitized_patterns += sanitize_re2_patterns(&mut tool.input_schema);
    }

    let initial_tokens = estimate_tools_tokens(tools);
    if initial_tokens <= max_tokens {
        return BudgetReport {
            tool_count: tools.len(),
            dropped_tools,
            initial_tokens,
            final_tokens: initial_tokens,
            sanitized_patterns,
            max_description_chars: None,
        };
    }

    // 第 1 级：保留全部 JSON Schema 结构字段，渐进精简嵌套 description（先 anyOf/深层，再一般字段）
    let stage1_steps: &[(usize, usize, bool)] = &[
        // (普通嵌套描述上限, anyOf/items 分支内描述上限, 是否剥离 anyOf 分支内描述)
        (120, 64, false),
        (80, 32, false),
        (64, 0, true),
        (40, 0, true),
        (24, 0, true),
    ];
    for &(prop_limit, branch_limit, strip_branch_desc) in stage1_steps {
        for tool in tools.iter_mut() {
            compact_schema_descriptions(
                &mut tool.input_schema,
                0,
                false,
                prop_limit,
                branch_limit,
                strip_branch_desc,
            );
        }
        let now = estimate_tools_tokens(tools);
        if now <= max_tokens {
            return BudgetReport {
                tool_count: tools.len(),
                dropped_tools,
                initial_tokens,
                final_tokens: now,
                sanitized_patterns,
                max_description_chars: None,
            };
        }
    }

    // 第 2 级：按二分法裁切各工具顶层 description 最大字符长度
    let orig_descriptions: Vec<String> = tools.iter().map(|t| t.description.clone()).collect();
    let max_desc_len = orig_descriptions
        .iter()
        .map(|d| d.chars().count())
        .max()
        .unwrap_or(0);

    let mut chosen_limit: Option<usize> = None;
    if max_desc_len > 0 {
        let mut lo = 0usize;
        let mut hi = max_desc_len;
        let mut best: Option<usize> = None;
        while lo <= hi {
            let mid = lo + (hi - lo) / 2;
            apply_top_description_limit(tools, &orig_descriptions, mid);
            let toks = estimate_tools_tokens(tools);
            if toks <= max_tokens {
                best = Some(mid);
                lo = mid + 1;
            } else if mid == 0 {
                break;
            } else {
                hi = mid - 1;
            }
        }
        if let Some(limit) = best {
            apply_top_description_limit(tools, &orig_descriptions, limit);
            let final_tokens = estimate_tools_tokens(tools);
            return BudgetReport {
                tool_count: tools.len(),
                dropped_tools,
                initial_tokens,
                final_tokens,
                sanitized_patterns,
                max_description_chars: Some(limit),
            };
        }
        chosen_limit = Some(0);
    }

    // 第 2b 级：剥离 inputSchema 内部全部 description，再重新对顶层 description 二分
    for tool in tools.iter_mut() {
        strip_all_schema_descriptions(&mut tool.input_schema);
    }
    if max_desc_len > 0 {
        let mut lo = 0usize;
        let mut hi = max_desc_len;
        let mut best: Option<usize> = None;
        while lo <= hi {
            let mid = lo + (hi - lo) / 2;
            apply_top_description_limit(tools, &orig_descriptions, mid);
            let toks = estimate_tools_tokens(tools);
            if toks <= max_tokens {
                best = Some(mid);
                lo = mid + 1;
            } else if mid == 0 {
                break;
            } else {
                hi = mid - 1;
            }
        }
        if let Some(limit) = best {
            apply_top_description_limit(tools, &orig_descriptions, limit);
            let final_tokens = estimate_tools_tokens(tools);
            return BudgetReport {
                tool_count: tools.len(),
                dropped_tools,
                initial_tokens,
                final_tokens,
                sanitized_patterns,
                max_description_chars: Some(limit),
            };
        }
        apply_top_description_limit(tools, &orig_descriptions, 0);
        chosen_limit = Some(0);
    }

    // 兜底：若合成测试构造了超大结构 schema，移除体积最大的工具直至 ≤ max_tokens
    while !tools.is_empty() && estimate_tools_tokens(tools) > max_tokens {
        if let Some((idx, _)) = tools
            .iter()
            .enumerate()
            .max_by_key(|(_, t)| estimate_json_tokens(&tool_spec_to_mcp_json(t)))
        {
            tools.remove(idx);
            dropped_tools += 1;
        } else {
            break;
        }
    }

    let final_tokens = estimate_tools_tokens(tools);
    BudgetReport {
        tool_count: tools.len(),
        dropped_tools,
        initial_tokens,
        final_tokens,
        sanitized_patterns,
        max_description_chars: chosen_limit,
    }
}

fn apply_top_description_limit(tools: &mut [McpToolSpec], originals: &[String], max_chars: usize) {
    for (tool, orig) in tools.iter_mut().zip(originals.iter()) {
        tool.description = truncate_chars(orig, max_chars);
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    if max_chars == 1 {
        return "…".into();
    }
    let mut out: String = text.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

fn compact_schema_descriptions(
    node: &mut Value,
    depth: usize,
    in_branch: bool,
    prop_limit: usize,
    branch_limit: usize,
    strip_branch_desc: bool,
) {
    match node {
        Value::Object(map) => {
            compact_object_descriptions(
                map,
                depth,
                in_branch,
                prop_limit,
                branch_limit,
                strip_branch_desc,
            );
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                compact_schema_descriptions(
                    item,
                    depth + 1,
                    in_branch,
                    prop_limit,
                    branch_limit,
                    strip_branch_desc,
                );
            }
        }
        _ => {}
    }
}

fn compact_object_descriptions(
    map: &mut Map<String, Value>,
    depth: usize,
    in_branch: bool,
    prop_limit: usize,
    branch_limit: usize,
    strip_branch_desc: bool,
) {
    if let Some(desc) = map.get("description").and_then(Value::as_str) {
        if in_branch && strip_branch_desc {
            map.remove("description");
        } else {
            let limit = if in_branch {
                branch_limit
            } else if depth >= 3 {
                prop_limit.min(48)
            } else {
                prop_limit
            };
            if limit == 0 {
                map.remove("description");
            } else if desc.chars().count() > limit {
                let compacted = truncate_chars(desc, limit);
                map.insert("description".into(), Value::String(compacted));
            }
        }
    }

    for (key, value) in map.iter_mut() {
        let child_in_branch = in_branch || matches!(key.as_str(), "anyOf" | "oneOf" | "allOf");
        compact_schema_descriptions(
            value,
            depth + 1,
            child_in_branch,
            prop_limit,
            branch_limit,
            strip_branch_desc,
        );
    }
}

fn strip_all_schema_descriptions(node: &mut Value) {
    match node {
        Value::Object(map) => {
            map.remove("description");
            for (_, v) in map.iter_mut() {
                strip_all_schema_descriptions(v);
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                strip_all_schema_descriptions(item);
            }
        }
        _ => {}
    }
}
