mod config;
mod mcp;
mod tools;

use asupersync::runtime::{reactor::create_reactor, RuntimeBuilder};
use config::{default_model, load, model_options, resolve, ResolvedModel};
use futures::FutureExt;
use pi::model::AssistantMessageEvent;
use pi::sdk::{
    create_agent_session, AgentEvent, AgentSessionHandle, ContentBlock, ImageContent, Message,
    SessionOptions, TextContent, ThinkingLevel, UserContent, UserMessage,
};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub fn maybe_run() -> bool {
    if std::env::args().nth(1).as_deref() != Some("--alkaid-native-bridge") {
        return false;
    }
    let result = (|| -> Result<(), String> {
        let reactor = create_reactor().map_err(|e| format!("创建 Rust Pi reactor 失败：{e}"))?;
        let runtime = RuntimeBuilder::current_thread().with_reactor(reactor).build()
            .map_err(|e| format!("创建 Rust Pi runtime 失败：{e}"))?;
        runtime.block_on(run_bridge())
    })();
    if let Err(error) = result {
        send(&json!({"ok":false,"error":error}));
    }
    true
}

fn send(value: &Value) {
    let mut stdout = io::stdout().lock();
    let _ = serde_json::to_writer(&mut stdout, value);
    let _ = stdout.write_all(b"\n");
    let _ = stdout.flush();
}

async fn run_bridge() -> Result<(), String> {
    let mut lines = io::BufReader::new(io::stdin()).lines();
    let first = lines.next().ok_or("Vega bridge 缺少请求")?.map_err(|e| e.to_string())?;
    let request: Value = serde_json::from_str(&first).map_err(|e| format!("Vega 请求 JSON 无效：{e}"))?;
    match request.get("action").and_then(Value::as_str) {
        Some("models") => models(&request),
        Some("title") => title(&request).await,
        Some("prompt") => prompt(request, lines).await,
        Some(action) => Err(format!("Vega bridge 不支持 action: {action}")),
        None => Err("Vega bridge 请求缺少 action".into()),
    }
}

fn models(request: &Value) -> Result<(), String> {
    let config = load(request.get("alkaidServerConfig"))?;
    send(&json!({"ok":true,"data":{"configOptions":[{"id":"model","name":"Model","currentValue":default_model(&config)?,"options":model_options(&config)?}],"modes":null}}));
    Ok(())
}

async fn title(request: &Value) -> Result<(), String> {
    let config_value = load(request.get("alkaidServerConfig"))?;
    let (model, registry) = resolve(&config_value, request.get("model").and_then(Value::as_str))?;
    let runtime_root = prepare_runtime(&registry)?;
    let cwd = request.get("cwd").and_then(Value::as_str).map(PathBuf::from).unwrap_or(std::env::current_dir().map_err(|e| e.to_string())?);
    let options = session_options(&cwd, &model, "你只负责生成简短标题。", true, Vec::new());
    let mut session = create_agent_session(options).await.map_err(|e| e.to_string())?;
    let prompt = request.get("prompt").and_then(Value::as_str).unwrap_or_default();
    let assistant = session.prompt(prompt, |_| {}).await.map_err(|e| e.to_string())?;
    let text = assistant.content.iter().filter_map(|block| match block { ContentBlock::Text(text) => Some(text.text.as_str()), _ => None }).collect::<String>();
    let _ = fs::remove_dir_all(runtime_root);
    send(&json!({"ok":true,"data":text}));
    Ok(())
}

fn prepare_runtime(registry: &Value) -> Result<PathBuf, String> {
    let root = config::data_root().join("native-runtime").join(Uuid::new_v4().to_string());
    config::install_registry(&root, registry)?;
    // Pi's SDK resolves models/auth/settings from this process-scoped directory.
    std::env::set_var("PI_CODING_AGENT_DIR", &root);
    Ok(root)
}

fn thinking(value: Option<&str>) -> Option<ThinkingLevel> {
    match value {
        Some("off") => Some(ThinkingLevel::Off),
        Some("minimal") => Some(ThinkingLevel::Minimal),
        Some("low") => Some(ThinkingLevel::Low),
        Some("medium") => Some(ThinkingLevel::Medium),
        Some("high") => Some(ThinkingLevel::High),
        Some("xhigh") => Some(ThinkingLevel::XHigh),
        Some("max") => Some(ThinkingLevel::Max),
        _ => None,
    }
}

fn session_options(cwd: &Path, model: &ResolvedModel, system_prompt: &str, read_only: bool, mcp_tools: Vec<mcp::McpToolSpec>) -> SessionOptions {
    SessionOptions {
        provider: Some(model.provider.clone()),
        model: Some(model.model.clone()),
        api_key: model.api_key.clone(),
        thinking: thinking(model.thinking.as_deref()),
        system_prompt: Some(system_prompt.into()),
        enabled_tools: Some(if read_only {
            vec!["read","grep","find","ls"]
        } else {
            vec!["read","bash","edit","write","grep","find","ls","hashline_edit"]
        }.into_iter().map(str::to_string).collect()),
        working_directory: Some(cwd.to_path_buf()),
        no_session: true,
        include_cwd_in_prompt: false,
        tool_factory: Some(Arc::new(tools::VegaToolFactory { mcp_tools })),
        ..SessionOptions::default()
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all="camelCase", default)]
struct NativeMemory {
    version: u32,
    digests: Vec<String>,
    turns: Vec<MemoryTurn>,
    pending_prompt: Option<String>,
    system_prompt_snapshot: String,
    rewrite_version: u64,
    context_tokens: u64,
    context_tier: String,
    full_messages: Vec<Message>,
    context_stage: String,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all="camelCase", default)]
struct MemoryTurn { user_prompts: Vec<String>, conclusion: String }

fn memory_path(session_id: &str) -> Result<PathBuf, String> {
    if !session_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') { return Err("非法 Vega session id".into()); }
    let suffix = if std::env::var("NOVA_CONTEXT_MODE").as_deref() == Ok("super") { "native-super" } else { "native" };
    Ok(config::data_root().join("sessions").join(format!("{session_id}.{suffix}.json")))
}

fn load_memory(session_id: &str) -> NativeMemory {
    fs::read_to_string(memory_path(session_id).unwrap_or_default()).ok()
        .and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

fn save_memory(session_id: &str, memory: &NativeMemory) -> Result<(), String> {
    let path = memory_path(session_id)?;
    fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).map_err(|e| e.to_string())?;
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&temp, serde_json::to_vec(memory).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    fs::rename(temp, path).map_err(|e| e.to_string())
}

fn memory_prompt(memory: &NativeMemory, current: &str) -> String {
    let mut sections = vec!["请使用下面的只追加会话记录继续工作。不要要求用户重复之前的要求。".to_string(), "\n## Conversation".into()];
    if !memory.digests.is_empty() {
        sections.push("\n### Frozen digests".into());
        for (index, digest) in memory.digests.iter().enumerate() { sections.push(format!("Digest {}:\n{digest}", index + 1)); }
    }
    for turn in &memory.turns {
        for prompt in &turn.user_prompts { sections.push(format!("User:\n{prompt}")); }
        if !turn.conclusion.is_empty() { sections.push(format!("Assistant:\n{}", turn.conclusion)); }
    }
    sections.push(format!("User:\n{current}"));
    sections.join("\n\n")
}

fn estimate_tokens(text: &str) -> u64 {
    let (ascii, non_ascii) = text.chars().fold((0u64,0u64), |(a,n), c| if c.is_ascii(){(a+1,n)}else{(a,n+1)});
    ascii.div_ceil(4) + non_ascii
}

fn context_tier(tokens: u64, window: u32) -> &'static str {
    let ratio = tokens as f64 / window.max(1) as f64;
    if ratio >= 0.9 { "force" } else if ratio >= 0.8 { "elide" } else if ratio >= 0.6 { "snip" } else if ratio >= 0.5 { "warn" } else { "normal" }
}

fn build_system_prompt(cwd: &Path, read_only: bool) -> String {
    let agents = fs::read_to_string(config::data_root().join("AGENTS.md")).unwrap_or_default();
    format!("你是 Vega：高效、简单、面向软件工程结果。\n\n回复默认简洁专业，先给结论，再给行动所需信息。先理解再修改，保持改动聚焦；完成后根据 diff 执行成本最低且有效的验证。\n\nAvailable tools:\n- read_files: 并行读取多个 UTF-8 文本文件\n{}- read / grep / find / ls: 读取和搜索\n{}\n\n同一读取阶段已有多个独立 UTF-8 文本目标时必须使用 read_files；修改多个独立已有文件时必须使用 edit_files。搜索成本必须有界，Git 仓库优先 git grep。\n\n当前模式：{}。\nCurrent working directory: {}\n{}",
        if read_only { "" } else { "- edit_files: 事务式并行智能编辑多个文件\n" },
        if read_only { "" } else { "- bash / edit / write / hashline_edit: 执行和修改" },
        if read_only { "plan（只读）" } else { "build" }, cwd.display(), agents)
}

fn input_parts(parts: Option<&Vec<Value>>) -> Result<(String, Vec<ContentBlock>), String> {
    let mut texts = Vec::new();
    let mut blocks = Vec::new();
    for part in parts.into_iter().flatten() {
        match part.get("type").and_then(Value::as_str) {
            Some("text") => texts.push(part.get("text").and_then(Value::as_str).unwrap_or_default().to_string()),
            Some("image_data") => blocks.push(ContentBlock::Image(ImageContent {
                data: part.get("data").and_then(Value::as_str).unwrap_or_default().into(),
                mime_type: part.get("mime").and_then(Value::as_str).unwrap_or("image/png").into(),
            })),
            Some("local_image") => {
                let path = part.get("path").and_then(Value::as_str).ok_or("图片缺少 path")?;
                let data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, fs::read(path).map_err(|e| format!("读取图片失败：{e}"))?);
                blocks.push(ContentBlock::Image(ImageContent { data, mime_type: "image/png".into() }));
            }
            _ => {}
        }
    }
    let text = texts.join("\n\n");
    blocks.insert(0, ContentBlock::Text(TextContent::new(text.clone())));
    Ok((text, blocks))
}

async fn prompt(request: Value, lines: impl Iterator<Item=io::Result<String>> + Send + 'static) -> Result<(), String> {
    let config_value = load(request.get("alkaidServerConfig"))?;
    let (model, registry) = resolve(&config_value, request.get("model").and_then(Value::as_str))?;
    let runtime_root = prepare_runtime(&registry)?;
    let cwd = PathBuf::from(request.get("cwd").and_then(Value::as_str).unwrap_or("."));
    let session_id = request.get("sessionId").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| Uuid::new_v4().simple().to_string());
    let read_only = request.get("mode").and_then(Value::as_str) == Some("plan");
    let (input_text, mut content) = input_parts(request.get("parts").and_then(Value::as_array))?;
    let mut memory = load_memory(&session_id);
    memory.version = 1;
    memory.pending_prompt = Some(input_text.clone());
    let system_prompt = if memory.system_prompt_snapshot.is_empty() { build_system_prompt(&cwd, read_only) } else { memory.system_prompt_snapshot.clone() };
    memory.system_prompt_snapshot = system_prompt.clone();
    let compact_prompt = memory_prompt(&memory, &input_text);
    let full_context_tokens = serde_json::to_string(&memory.full_messages)
        .map(|text| estimate_tokens(&text)).unwrap_or_default();
    let use_full_context = !memory.full_messages.is_empty()
        && full_context_tokens < u64::from(model.context_window) * 3 / 4;
    if !use_full_context {
        if let Some(ContentBlock::Text(text)) = content.first_mut() { text.text = compact_prompt.clone(); }
        memory.context_stage = "slim".into();
        memory.context_tokens = estimate_tokens(&compact_prompt);
    } else {
        memory.context_stage = "full".into();
        memory.context_tokens = full_context_tokens;
    }
    memory.context_tier = context_tier(memory.context_tokens, model.context_window).into();
    save_memory(&session_id, &memory)?;

    let mcp_tools = mcp::load(&cwd, &config::data_root())?;
    let mut session = create_agent_session(session_options(&cwd, &model, &system_prompt, read_only, mcp_tools)).await.map_err(|e| e.to_string())?;
    if use_full_context {
        session.session_mut().agent.replace_messages(memory.full_messages.clone());
    }
    let steering = Arc::new(Mutex::new(VecDeque::<Message>::new()));
    let fetch_queue = Arc::clone(&steering);
    session.session_mut().agent.register_message_fetchers(Some(Arc::new(move || {
        let messages = fetch_queue.lock().unwrap().drain(..).collect::<Vec<_>>();
        futures::future::ready(messages).boxed()
    })), None);
    let (abort_handle, abort_signal) = AgentSessionHandle::new_abort_handle();
    let abort = Arc::new(Mutex::new(Some(abort_handle)));
    let command_abort = Arc::clone(&abort);
    let command_steering = Arc::clone(&steering);
    std::thread::spawn(move || {
        for line in lines.flatten() {
            let Ok(command) = serde_json::from_str::<Value>(&line) else { continue };
            match command.get("action").and_then(Value::as_str) {
                Some("cancel") => { if let Some(handle) = command_abort.lock().unwrap().as_ref() { handle.abort(); } }
                Some("steer") => if let Ok((text, _)) = input_parts(command.get("parts").and_then(Value::as_array)) {
                    command_steering.lock().unwrap().push_back(Message::User(UserMessage { content: UserContent::Text(text), timestamp: chrono::Utc::now().timestamp_millis() }));
                },
                _ => {}
            }
        }
    });

    send(&json!({"type":"timing","phase":"context_shape","elapsedMs":0,"contextTier":memory.context_tier,"rewriteVersion":memory.rewrite_version,"engine":"rust-native"}));
    send(&json!({"type":"ready","sessionId":session_id}));
    let text_state = Arc::new(Mutex::new(String::new()));
    let thinking_state = Arc::new(Mutex::new(String::new()));
    let tools = Arc::new(Mutex::new(HashMap::<String,Value>::new()));
    let callback_text = Arc::clone(&text_state);
    let callback_thinking = Arc::clone(&thinking_state);
    let callback_tools = Arc::clone(&tools);
    let run = session.session_mut().agent.run_with_content_with_abort(content, Some(abort_signal), move |event| {
        map_event(event, &callback_text, &callback_thinking, &callback_tools);
    }).await;
    *abort.lock().unwrap() = None;
    let cancelled = matches!(run, Err(pi::sdk::Error::Aborted));
    match run {
        Ok(assistant) => {
            let conclusion = assistant.content.iter().filter_map(|block| match block { ContentBlock::Text(text)=>Some(text.text.as_str()), _=>None }).collect::<String>();
            memory.turns.push(MemoryTurn { user_prompts: vec![input_text], conclusion });
            memory.full_messages = session.session().agent.messages().to_vec();
            memory.pending_prompt = None;
            save_memory(&session_id, &memory)?;
            send(&json!({"type":"done","sessionId":session_id,"cancelled":false,"usage":assistant.usage}));
        }
        Err(_) if cancelled => send(&json!({"type":"done","sessionId":session_id,"cancelled":true})),
        Err(error) => return Err(error.to_string()),
    }
    let _ = fs::remove_dir_all(runtime_root);
    Ok(())
}

fn output_text(output: &pi::sdk::ToolOutput) -> String {
    output.content.iter().filter_map(|block| match block { ContentBlock::Text(text)=>Some(text.text.as_str()), _=>None }).collect::<Vec<_>>().join("\n")
}

fn map_event(event: AgentEvent, text: &Arc<Mutex<String>>, thinking: &Arc<Mutex<String>>, tools: &Arc<Mutex<HashMap<String,Value>>>) {
    match event {
        AgentEvent::MessageUpdate { assistant_message_event, .. } => match assistant_message_event {
            AssistantMessageEvent::TextDelta { delta, .. } => { let mut value=text.lock().unwrap(); value.push_str(&delta); send(&json!({"type":"item","item":{"id":"assistant-native","type":"agent_message","text":*value}})); }
            AssistantMessageEvent::ThinkingDelta { delta, .. } => { let mut value=thinking.lock().unwrap(); value.push_str(&delta); send(&json!({"type":"item","item":{"id":"thinking-native","type":"reasoning","text":*value}})); }
            _ => {}
        },
        AgentEvent::ToolExecutionStart { tool_call_id, tool_name, args } => {
            let file_change = matches!(tool_name.as_str(), "edit"|"write"|"edit_files"|"hashline_edit");
            let item = if tool_name == "bash" { json!({"id":tool_call_id,"type":"command_execution","status":"in_progress","command":args.get("command"),"arguments":args}) }
                else if file_change { json!({"id":tool_call_id,"type":"file_change","status":"in_progress","tool":tool_name,"arguments":args,"changes":paths_from_args(&args)}) }
                else { json!({"id":tool_call_id,"type":"mcp_tool_call","status":"in_progress","server":"Vega","tool":tool_name,"arguments":args}) };
            tools.lock().unwrap().insert(tool_call_id, item.clone()); send(&json!({"type":"item","item":item}));
        }
        AgentEvent::ToolExecutionEnd { tool_call_id, result, is_error, .. } => if let Some(mut item)=tools.lock().unwrap().remove(&tool_call_id) {
            item["status"] = Value::String(if is_error{"failed"}else{"completed"}.into());
            let output=output_text(&result); item["aggregated_output"]=Value::String(output.clone());
            if is_error { item["error"]=json!({"message":output}); } else { item["result"]=serde_json::to_value(result).unwrap_or(Value::Null); }
            send(&json!({"type":"item","item":item}));
        },
        AgentEvent::AutoRetryStart { attempt, .. } => send(&json!({"type":"timing","phase":"provider_retry","elapsedMs":0,"attempt":attempt})),
        AgentEvent::AutoCompactionStart { reason } => send(&json!({"type":"timing","phase":"mid_turn_context_rewrite","elapsedMs":0,"reason":reason})),
        _ => {}
    }
}

fn paths_from_args(args: &Value) -> Vec<Value> {
    if let Some(path)=args.get("path").and_then(Value::as_str) { return vec![json!({"path":path,"kind":"update"})]; }
    args.get("files").and_then(Value::as_array).into_iter().flatten().filter_map(|file|file.get("path").and_then(Value::as_str)).map(|path|json!({"path":path,"kind":"update"})).collect()
}
