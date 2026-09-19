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
fn strings(value: &Value, max: usize) -> Vec<String> {
    let raw: Vec<&str> = match value {Value::String(s)=>vec![s],Value::Array(v)=>v.iter().filter_map(Value::as_str).collect(),_=>vec![]};
    let mut seen=HashSet::new();
    raw.into_iter().map(str::trim).filter(|s|!s.is_empty()&&s.len()<=4096&&seen.insert(s.to_lowercase())).take(max).map(str::to_owned).collect()
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
    "加载|读取|打开|load|read|open", "切换|跳转|switch|navigate|select", "终端|命令行|terminal|shell|pty",
    "首页|新建|home|new|create", "主题|亮色|暗色|theme|light|dark|palette", "快捷键|热键|shortcut|hotkey|keybinding",
    "未读|标记|unread|badge|mark", "重试|超时|retry|timeout|deadline", "重复|幂等|去重|duplicate|dedup|idempotent",
    "并发|排队|队列|concurrent|queue|semaphore", "锁|阻塞|lock|mutex|blocking", "滚动|视口|懒加载|scroll|viewport|lazy",
    "分页|分块|page|chunk|cursor", "附件|上传|attachment|upload", "缩略图|解码|thumbnail|decode",
    "权限|授权|permission|authorization|allow", "认证|登录|凭证|auth|login|credential|token",
    "后台|任务|进程|background|task|process|runtime", "断线|重连|disconnect|reconnect|connection",
    "浏览器|页面|网页|browser|webview|chrome", "定位|点击|坐标|click|coordinate|target|pointer",
    "窗口|焦点|window|focus|foreground", "校验|过期|失效|verify|validate|stale|invalidate",
    "恢复|回滚|检查点|restore|rollback|checkpoint", "世界线|时间线|timeline|branch",
    "删除|清理|移除|delete|remove|cleanup", "索引|检索|搜索|index|search|retrieval",
    "语义|向量|semantic|embedding|vector", "复制|粘贴|剪贴板|copy|paste|clipboard",
    "配置|默认|设置|config|default|settings", "漫游|共享|远程|roaming|share|remote|relay",
    "更新|升级|下载|update|upgrade|download", "加密|签名|encrypt|signature|crypto",
    "错误|报错|失败|error|failure|exception", "模型|提供商|model|provider", "计费|用量|统计|usage|cost|stats",
];
impl Query {
    pub fn parse(mut params: Value) -> Result<Self,String> {
        if !params.is_object(){return Err("polaris 参数必须是对象".into());}
        let query=params["query"].as_str().unwrap_or("").trim().chars().take(1024).collect::<String>();
        let mut task=params["task"].as_str().unwrap_or("").trim().chars().take(1024).collect::<String>();
        let mut anchors=strings(&params["keywords"],5);
        if !query.is_empty(){if identifier(&query){if !anchors.iter().any(|s|s.eq_ignore_ascii_case(&query)){anchors.push(query.clone());}}else if task.is_empty(){task=query;}}
        if task.is_empty(){task=anchors.iter().filter(|s|!identifier(s)).cloned().collect::<Vec<_>>().join(" ");}
        anchors.retain(|s|identifier(s));anchors.truncate(5);
        let files=strings(&params["files"],6);
        for f in &files {if f.starts_with('/')||Path::new(f).is_absolute()||f.contains('\\')||f.split('/').any(|s|s==".."||s==".")||f.contains(':'){return Err("files 必须是仓库内相对路径".into());}}
        if task.is_empty()&&anchors.is_empty()&&files.is_empty(){return Err("需要 query / task / keywords / files 至少其一".into());}
        let mut terms=Vec::new();let mut seen=HashSet::new();
        for t in tokens(&format!("{} {}",task,anchors.join(" "))){if seen.insert(t.clone()){terms.push((t,1.0));}if terms.len()>=64{break;}}
        let original=terms.iter().map(|(s,_)|s.clone()).collect::<HashSet<_>>();
        for group in CONCEPTS {if group.split('|').any(|s|original.contains(s)){for s in group.split('|'){if terms.len()<112&&seen.insert(s.into()){terms.push((s.into(),0.55));}}}}
        let test_intent=["测试用例","单元测试","回归测试","unit test","regression test"].iter().any(|s|task.to_lowercase().contains(s));
        let doc_intent=["文档","使用说明","readme","documentation"].iter().any(|s|task.to_lowercase().contains(s));
        let hard=params.get("maxBytes").or_else(||params.get("maxChars")).and_then(Value::as_u64).unwrap_or(32768).clamp(8192,65536) as usize;
        let lines=params["budget"].as_u64().unwrap_or(600).clamp(100,1200) as usize;
        params["task"]=Value::String(task.clone()); params["keywords"]=serde_json::json!(anchors);params["files"]=serde_json::json!(files);
        Ok(Self{params,task,anchors,files,terms,test_intent,doc_intent,hard,lines})
    }
}
