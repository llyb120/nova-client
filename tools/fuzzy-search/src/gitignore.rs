//! 轻量 .gitignore 匹配器：支持注释、否定(!)、行首锚定(/)、仅目录模式(尾/)、
//! ** 跨层通配、* 单段通配、? 单字符。嵌套 .gitignore 以最近目录规则优先。
//! 不实现 git 的全部边角（如行尾空格转义、\.git/info/exclude），覆盖主流用法。

#[derive(Clone, Debug)]
struct Pattern {
    /// 段模式（按 / 切分）
    segs: Vec<String>,
    negate: bool,
    dir_only: bool,
    anchored: bool, // 含非末尾的 / → 相对 .gitignore 所在目录锚定
}

#[derive(Clone, Debug, Default)]
pub struct IgnoreFile {
    /// .gitignore 所在目录（相对仓库根，"" 表示根；片段以 / 分隔）
    dir: String,
    patterns: Vec<Pattern>,
}

impl IgnoreFile {
    pub fn parse(dir: &str, text: &str) -> Self {
        let mut patterns = Vec::new();
        for raw in text.lines() {
            let mut line = raw.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut negate = false;
            if let Some(rest) = line.strip_prefix('!') {
                negate = true;
                line = rest;
            }
            let mut dir_only = false;
            let mut line = line;
            if let Some(rest) = line.strip_suffix('/') {
                dir_only = true;
                line = rest;
            }
            let mut anchored = false;
            if let Some(rest) = line.strip_prefix('/') {
                anchored = true;
                line = rest;
            } else if line.matches('/').count() > 0 {
                // 中间含 / 的模式按 git 规则锚定
                anchored = true;
            }
            if line.is_empty() {
                continue;
            }
            patterns.push(Pattern {
                segs: line.split('/').map(|s| s.to_string()).collect(),
                negate,
                dir_only,
                anchored,
            });
        }
        IgnoreFile { dir: dir.to_string(), patterns }
    }

    /// 判断 path（仓库相对、/ 分隔）是否被本文件命中；返回 Some(true)=忽略, Some(false)=白名单, None=不相关。
    fn matches(&self, path: &str, is_dir: bool) -> Option<bool> {
        // 必须位于本 .gitignore 目录之下
        let rel = if self.dir.is_empty() {
            path
        } else if let Some(r) = path.strip_prefix(&format!("{}/", self.dir)) {
            r
        } else {
            return None;
        };
        let mut result = None;
        for p in &self.patterns {
            if p.dir_only && !is_dir {
                continue;
            }
            if pattern_matches(p, rel) {
                result = Some(!p.negate); // 命中且非否定 → 忽略；否定 → 白名单
            }
        }
        result
    }
}

/// 单段 glob：* 匹配任意非 / 序列，? 匹配单字符。
fn seg_match(pat: &str, text: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = text.chars().collect();
    // 带回溯的简单通配匹配
    fn rec(p: &[char], t: &[char]) -> bool {
        if p.is_empty() {
            return t.is_empty();
        }
        match p[0] {
            '*' => {
                // * 匹配 0..n 个字符
                for skip in 0..=t.len() {
                    if rec(&p[1..], &t[skip..]) {
                        return true;
                    }
                }
                false
            }
            '?' => !t.is_empty() && rec(&p[1..], &t[1..]),
            '[' => {
                // 简化：不支持字符类，按字面处理
                !t.is_empty() && p[0] == t[0] && rec(&p[1..], &t[1..])
            }
            c => !t.is_empty() && c == t[0] && rec(&p[1..], &t[1..]),
        }
    }
    rec(&p, &t)
}

/// 多段模式匹配：** 匹配 0+ 层，其余逐段。
fn pattern_matches(p: &Pattern, rel: &str) -> bool {
    let segs: Vec<&str> = rel.split('/').collect();
    if p.anchored {
        seg_seq_match(&p.segs, &segs)
    } else {
        // 非锚定：可匹配任意后缀（basename 或路径尾部）
        for start in 0..segs.len() {
            if seg_seq_match(&p.segs, &segs[start..]) {
                return true;
            }
        }
        false
    }
}

fn seg_seq_match(pat: &[String], segs: &[&str]) -> bool {
    if pat.is_empty() {
        return segs.is_empty();
    }
    if pat[0] == "**" {
        // ** 匹配 0..n 层
        for skip in 0..=segs.len() {
            if seg_seq_match(&pat[1..], &segs[skip..]) {
                return true;
            }
        }
        return false;
    }
    if segs.is_empty() {
        return false;
    }
    seg_match(&pat[0], segs[0]) && seg_seq_match(&pat[1..], &segs[1..])
}

/// 按目录层级组织的 gitignore 集合：查询时从根到叶依次判定，深目录规则覆盖浅层。
#[derive(Default)]
pub struct GitignoreStack {
    /// 每层目录的 IgnoreFile，按浅到深排列
    stack: Vec<IgnoreFile>,
}

impl GitignoreStack {
    pub fn new() -> Self {
        GitignoreStack { stack: Vec::new() }
    }

    /// 进入目录时压入该目录的 .gitignore（如有）。
    pub fn push(&mut self, dir: &str, text: Option<&str>) {
        if let Some(t) = text {
            self.stack.push(IgnoreFile::parse(dir, t));
        }
    }

    pub fn pop(&mut self, dir: &str) {
        if self.stack.last().map(|f| f.dir.as_str()) == Some(dir) {
            self.stack.pop();
        }
    }

    /// 综合判定：深层规则优先；同一文件内后写规则优先（parse 已按顺序存储，matches 返回最后命中）。
    pub fn is_ignored(&self, path: &str, is_dir: bool) -> bool {
        let mut decision = false;
        for f in &self.stack {
            if let Some(ignored) = f.matches(path, is_dir) {
                decision = ignored;
            }
        }
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_basename() {
        let g = IgnoreFile::parse("", "node_modules\n*.log\n");
        assert_eq!(g.matches("node_modules", true), Some(true));
        assert_eq!(g.matches("a/node_modules", true), Some(true));
        assert_eq!(g.matches("src/app.ts", false), None);
        assert_eq!(g.matches("error.log", false), Some(true));
        assert_eq!(g.matches("logs/error.log", false), Some(true));
    }

    #[test]
    fn anchored_and_dir_only() {
        let g = IgnoreFile::parse("", "/dist\nbuild/\n");
        assert_eq!(g.matches("dist", true), Some(true));
        assert_eq!(g.matches("sub/dist", true), None); // 锚定根
        assert_eq!(g.matches("build", true), Some(true));
        assert_eq!(g.matches("build", false), None); // 仅目录
    }

    #[test]
    fn negation() {
        let g = IgnoreFile::parse("", "*.log\n!important.log\n");
        assert_eq!(g.matches("a.log", false), Some(true));
        assert_eq!(g.matches("important.log", false), Some(false));
    }

    #[test]
    fn double_star() {
        let g = IgnoreFile::parse("", "docs/**/draft.md\n**/gen/**\n");
        assert_eq!(g.matches("docs/a/b/draft.md", false), Some(true));
        assert_eq!(g.matches("docs/draft.md", false), Some(true));
        assert_eq!(g.matches("src/gen/x/y.rs", false), Some(true));
        assert_eq!(g.matches("gen/top.rs", false), Some(true));
    }

    #[test]
    fn nested_override() {
        let mut stack = GitignoreStack::new();
        stack.push("", Some("target\n"));
        stack.push("vendor", Some("!target\n"));
        assert!(stack.is_ignored("target", true));
        assert!(!stack.is_ignored("vendor/target", true)); // 深层否定覆盖
    }

    #[test]
    fn question_mark() {
        let g = IgnoreFile::parse("", "src/?.tmp\n");
        assert_eq!(g.matches("src/a.tmp", false), Some(true));
        assert_eq!(g.matches("src/ab.tmp", false), None);
    }
}
