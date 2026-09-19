// One native normalization boundary for Lyra, MCP and SDK callers.
use super::*;

#[derive(Clone, Debug)]
pub(super) struct Query {
    pub params: Value,
    pub task: String,
    pub anchors: Vec<String>,
    pub files: Vec<String>,
    pub terms: Vec<(String, f64)>,
    pub test_intent: bool,
    pub doc_intent: bool,
    pub hard: usize,
    pub lines: usize,
}
fn strings(value: &Value, max: usize, fold: bool) -> Vec<String> {
    let raw: Vec<&str> = match value {Value::String(s)=>vec![s],Value::Array(v)=>v.iter().filter_map(Value::as_str).collect(),_=>vec![]};
    let mut seen=HashSet::new();
    raw.into_iter().map(str::trim).filter(|s|!s.is_empty()&&s.len()<=4096&&seen.insert(if fold{s.to_lowercase()}else{(*s).to_string()})).take(max).map(str::to_owned).collect()
}
pub(super) fn identifier(s: &str) -> bool {
    !s.is_empty() && s.len()<=200 && s.chars().next().is_some_and(|c|c.is_ascii_alphabetic()||c=='_'||c=='$')
        && s.chars().all(|c|c.is_ascii_alphanumeric()||matches!(c,'_'|'$'|'-'|'.'|':'))
}
fn noise(s: &str) -> bool {
    matches!(s,"the"|"and"|"with"|"from"|"this"|"that"|"what"|"why"|"where"|"how"|"does"|"when"|"which"|"into"|"return"|"let"|"const"|"pub"|"fn"|"async"|"await"|"self"|"true"|"false"|"some"|"none"|"string"|"vec"|"function"|"export"|"mut"|"use"|"可以"|"怎么"|"为何"|"为什么"|"如何"|"代码"|"实现"|"一个"|"时候"|"哪里"|"什么"|"需要"|"帮我"|"找到"|"这个"|"进行")
}
/// Split identifiers and retain CJK bigrams; this is lexical expansion, not an embedding.
pub(super) fn tokens(text: &str) -> Vec<String> {
    let mut out=Vec::new(); let mut ascii=String::new(); let mut han=Vec::new();
    let flush_ascii=|s:&mut String,out:&mut Vec<String>|{
        if s.len()>=2 {out.push(s.to_lowercase());
            let chars:Vec<char>=s.chars().collect();let mut part=String::new();
            for (i,&c) in chars.iter().enumerate(){
                if c=='_'||c=='-'||c=='.'||c==':'||c=='$' {if part.len()>=2{out.push(part.to_lowercase())}part.clear();continue;}
                if c.is_ascii_uppercase()&&i>0&&(chars[i-1].is_ascii_lowercase()||(chars[i-1].is_ascii_uppercase()&&chars.get(i+1).is_some_and(char::is_ascii_lowercase))){if part.len()>=2{out.push(part.to_lowercase())}part.clear();}
                part.push(c);
            }if part.len()>=2{out.push(part.to_lowercase());}
        }s.clear();
    };
    let flush_han=|s:&mut Vec<char>,out:&mut Vec<String>|{for size in 2..=4 {for slice in s.windows(size){out.push(slice.iter().collect());}}s.clear();};
    for c in text.chars(){if is_han(c){flush_ascii(&mut ascii,&mut out);han.push(c);}else if c.is_ascii_alphanumeric()||matches!(c,'_'|'$'|'-'|'.'|':'){flush_han(&mut han,&mut out);ascii.push(c);}else{flush_ascii(&mut ascii,&mut out);flush_han(&mut han,&mut out);}}
    flush_ascii(&mut ascii,&mut out);flush_han(&mut han,&mut out);
    out.retain(|s|!noise(s));out
}
// Generic bilingual software concepts. No repository paths, function names or eval answers.
const CONCEPTS: &[&str]=&[
    "取消|停止|中断|终止|cancel|abort|stop|interrupt", "会话|对话|聊天|session|thread|chat|conversation",
    "发送|提交|send|submit|prompt", "生成|流式|输出|generation|stream|output|delta",
    "图片|图像|截图|image|picture|screenshot|capture", "缓存|复用|cache|memo|reuse",
    "磁盘|保存|落盘|持久化|persist|save|disk|storage", "历史|记录|history|transcript|record",
    "加载|读取|打开|load|read|open", "切换|跳转|switch|navigate|select|open", "终端|命令行|terminal|shell|pty",
    "首页|新建|home|new|create", "主题|亮色|暗色|theme|light|dark|palette", "快捷键|热键|shortcut|hotkey|keybinding",
    "未读|标记|unread|badge|mark", "重试|再试|超时|retry|timeout|deadline", "重复|幂等|去重|duplicate|dedup|idempotent",
    "并发|排队|队列|concurrent|queue|semaphore", "锁|阻塞|lock|mutex|blocking", "滚动|视口|懒加载|scroll|viewport|lazy",
    "分页|分块|page|chunk|cursor", "附件|上传|attachment|upload", "缩略图|解码|thumbnail|decode",
    "权限|授权|permission|authorization|allow", "认证|登录|凭证|auth|login|credential|token",
    "后台|任务|进程|background|task|process|runtime", "断线|重连|disconnect|reconnect|connection",
    "浏览器|页面|网页|browser|webview|chrome", "定位|点击|坐标|click|coordinate|target|pointer",
    "窗口|焦点|window|focus|foreground", "校验|过期|失效|verify|validate|stale|invalidate",
    "恢复|还原|restore|recover", "回滚|rollback", "检查点|checkpoint", "世界线|时间线|timeline|branch",
    "删除|清理|移除|delete|remove|cleanup", "索引|检索|搜索|index|search|retrieval",
    "语义|向量|semantic|embedding|vector", "复制|粘贴|剪贴板|copy|paste|clipboard",
    "配置|默认|设置|config|default|settings", "漫游|共享|远程|roaming|share|remote|relay",
    "更新|升级|下载|update|upgrade|download", "加密|encrypt|encryption", "解密|decrypt|decryption", "签名|signature|sign", "绑定|bind|binding",
    "文件|file|files", "路径|path|paths", "目录|文件夹|directory|folder", "资源管理器|文件管理器|explorer|finder",
    "展开|expand|open", "收起|collapse|close", "继承|inherit", "记住|记忆|remember", "状态|state|status", "请求|request",
    "拒绝|禁止|deny|reject|forbidden", "代理|proxy", "立即|immediate", "尺寸|大小|size|dimension",
    "错误|报错|失败|error|failure|exception", "模型|提供商|model|provider", "计费|用量|统计|usage|cost|stats",
];
impl Query {
    /// Distinct concepts are coverage constraints; eight aliases of one verb
    /// must not count as eight independently satisfied parts of a request.
    pub(super) fn facets(&self) -> Vec<&'static str> {
        CONCEPTS.iter().copied().filter(|group| group.split('|').any(|word|
            self.terms.iter().any(|(term,weight)|term==word&&*weight>=1.0))).collect()
    }
    pub fn parse(mut params: Value) -> Result<Self,String> {
        if !params.is_object(){return Err("polaris 参数必须是对象".into());}
        let query=params["query"].as_str().unwrap_or("").trim().chars().take(1024).collect::<String>();
        let mut task=params["task"].as_str().unwrap_or("").trim().chars().take(1024).collect::<String>();
        let mut anchors=strings(&params["keywords"],5,true);
        if !query.is_empty(){if identifier(&query){if !anchors.iter().any(|s|s.eq_ignore_ascii_case(&query)){anchors.push(query.clone());}}else if task.is_empty(){task=query;}}
        if task.is_empty(){task=anchors.iter().filter(|s|!identifier(s)).cloned().collect::<Vec<_>>().join(" ");}
        task=task.chars().take(1024).collect();
        anchors.retain(|s|identifier(s));anchors.truncate(5);
        let files=strings(&params["files"],6,false).into_iter().map(|f|f.replace('\\',"/")).collect::<Vec<_>>();
        for f in &files {if f.starts_with('/')||Path::new(f).is_absolute()||f.split('/').any(|s|s==".."||s==".")||f.contains(':'){return Err("files 必须是仓库内相对路径".into());}}
        if task.is_empty()&&anchors.is_empty()&&files.is_empty(){return Err("需要 query / task / keywords / files 至少其一".into());}
        let mut terms=Vec::new();let mut seen=HashSet::new();
        // A repeated side effect is an idempotency intent, not a new identifier.
        static DUPLICATE_INTENT: OnceLock<Regex> = OnceLock::new();
        let repeated_effect = DUPLICATE_INTENT.get_or_init(|| Regex::new(r"(?:避免|防止|不要).{0,20}(?:又|重复|两次|多次)").unwrap()).is_match(&task)
            && ["发送", "提交", "执行", "请求", "调用"].iter().any(|s| task.contains(s));
        let search_task = format!("{}{}", task, if repeated_effect { " 去重" } else { "" });
        let raw=tokens(&format!("{} {}",search_task,anchors.join(" ")));
        let original=raw.iter().cloned().collect::<HashSet<_>>();
        // A long first CJK phrase must not consume every slot before the actual
        // predicate at the end of the question. Preserve concepts/identifiers first.
        let meaningful=|t:&String|t.is_ascii()||CONCEPTS.iter().any(|g|g.split('|').any(|s|s==t));
        for t in raw.iter().filter(|t|meaningful(t)).chain(raw.iter().filter(|t|!meaningful(t))) {
            if seen.insert(t.clone()){terms.push((t.clone(),1.0));}if terms.len()>=64{break;}
        }
        for group in CONCEPTS {if group.split('|').any(|s|original.contains(s)){for s in group.split('|'){if terms.len()<112&&seen.insert(s.into()){terms.push((s.into(),0.55));}}}}
        // Behavioral predicates must not be outvoted by generic nouns (a
        // credentials getter is not encryption; a pointer event is not cancel).
        // This changes query weights only, never fabricates a source symbol.
        let strong = ["cancel", "encrypt", "decrypt", "retry", "dedup", "delete", "restore", "persist", "copy", "paste"];
        let predicates = CONCEPTS.iter().filter(|group| {
            group.split('|').any(|s| original.contains(s)) && group.split('|').any(|s| strong.contains(&s))
        }).collect::<Vec<_>>();
        for (term, weight) in &mut terms {
            if predicates.iter().any(|group| group.split('|').any(|s| s == term.as_str())) { *weight *= 3.0; }
            else if !predicates.is_empty() && ["click", "coordinate", "pointer", "点击", "坐标"].contains(&term.as_str()) { *weight *= 0.5; }
        }
        let test_intent=["测试用例","单元测试","回归测试","unit test","regression test"].iter().any(|s|task.to_lowercase().contains(s));
        let doc_intent=["文档","使用说明","readme","documentation"].iter().any(|s|task.to_lowercase().contains(s));
        let hard=params.get("maxBytes").or_else(||params.get("maxChars")).and_then(Value::as_u64).unwrap_or(32768).clamp(8192,65536) as usize;
        let lines=params["budget"].as_u64().unwrap_or(600).clamp(100,1200) as usize;
        params["task"]=Value::String(task.clone()); params["keywords"]=serde_json::json!(anchors);params["files"]=serde_json::json!(files);
        Ok(Self{params,task,anchors,files,terms,test_intent,doc_intent,hard,lines})
    }
}

#[cfg(test)]
mod intent_tests {
    use super::*;
    #[test] fn late_predicates_survive_uninformative_prefixes() {
        let q=Query::parse(serde_json::json!({"task":format!("{}，真正的问题是哪里取消后台任务", "已经阅读有关说明并尝试理解现有处理行为".repeat(12))})).unwrap();
        assert!(q.terms.iter().any(|(t,w)|t=="cancel"&&*w>1.0));
        assert!(q.terms.iter().any(|(t,_)|t=="取消"));assert!(q.terms.len()<=112);
    }
    #[test] fn repeated_effects_are_not_confused_with_cached_reads() {
        let q=Query::parse(serde_json::json!({"task":"如何避免它又提交同一请求"})).unwrap();
        assert!(q.terms.iter().any(|(t,w)|t=="dedup"&&*w>1.0));
        let read=Query::parse(serde_json::json!({"task":"如何复用缓存避免再次读取历史"})).unwrap();
        assert!(!read.terms.iter().any(|(t,_)|t=="dedup"));
    }
    #[test] fn encryption_direction_is_not_a_generic_credentials_getter() {
        let q=Query::parse(serde_json::json!({"task":"用户凭证如何加密"})).unwrap();
        let weight=|term:&str|q.terms.iter().find(|(s,_)|s==term).map(|(_,w)|*w).unwrap_or(0.0);
        assert!(weight("encrypt")>weight("credential"));assert_eq!(weight("decrypt"),0.0);
    }
}
