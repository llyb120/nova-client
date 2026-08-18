//! 独立符号扫描器：花括号配对（C 系 / Rust / TS / Go…）+ Python 缩进。
//! 不依赖 regex：声明识别全部手写词法解析。

#[derive(Clone, Debug)]
pub struct Sym {
    pub ln: u32,
    pub end: u32,
    pub depth: u32,
    pub kind: &'static str,
    pub name: String,
    pub sig: String,
    pub exp: bool,
}

const BRACE_EXT: &[&str] = &[
    "rs", "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "go", "java", "kt", "kts",
    "swift", "c", "cc", "cpp", "h", "hpp", "cs", "php", "dart", "zig", "vue", "svelte",
];
const INDENT_EXT: &[&str] = &["py", "pyi"];

pub fn is_code_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    let Some(pos) = lower.rfind('.') else { return false };
    let ext = &lower[pos + 1..];
    BRACE_EXT.contains(&ext) || INDENT_EXT.contains(&ext)
}

// ---------------------------------------------------------------- 词法工具

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '$'
}
fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// 取行首单词，返回 (word, rest)。
fn take_word(s: &str) -> Option<(&str, &str)> {
    let t = s.trim_start();
    let mut chars = t.char_indices();
    match chars.next() {
        Some((_, c)) if is_ident_start(c) => {}
        _ => return None,
    }
    let mut end = t.len();
    for (i, c) in chars {
        if !is_ident_char(c) {
            end = i;
            break;
        }
    }
    Some((&t[..end], &t[end..]))
}

const DECL_MODS: &[&str] = &[
    "pub", "async", "unsafe", "extern", "static", "abstract", "final", "inline", "readonly",
    "private", "public", "protected", "override", "synchronized", "sealed", "virtual", "export",
    "default",
];

/// 剥离声明前缀修饰符（含 pub(crate) 形式）。const/let/var 不在其中，由调用方特判。
fn strip_decl_mods(mut s: &str) -> &str {
    loop {
        let t = s.trim_start();
        if let Some(rest) = t.strip_prefix("pub") {
            let r = rest.trim_start();
            if r.starts_with('(') {
                if let Some(close) = r.find(')') {
                    s = &r[close + 1..];
                    continue;
                }
            }
        }
        if let Some((w, rest)) = take_word(t) {
            if DECL_MODS.contains(&w) && (rest.is_empty() || rest.starts_with(char::is_whitespace)) {
                s = rest;
                continue;
            }
        }
        return t;
    }
}

const CTRL: &[&str] = &[
    "if", "for", "while", "switch", "catch", "return", "function", "fn", "else", "do", "new",
    "typeof", "await", "case", "match", "impl",
];

/// 顶层/类级声明识别，返回 (name, kind)。
pub fn parse_decl(line: &str) -> Option<(String, &'static str)> {
    let s = strip_decl_mods(line);
    let (w, rest) = take_word(s)?;
    match w {
        "function" => {
            let r = rest.trim_start().trim_start_matches('*').trim_start();
            take_word(r).map(|(n, _)| (n.to_string(), "fn"))
        }
        "class" => take_word(rest).map(|(n, _)| (n.to_string(), "class")),
        "interface" => take_word(rest).map(|(n, _)| (n.to_string(), "interface")),
        "type" => take_word(rest).map(|(n, _)| (n.to_string(), "type")),
        "enum" => take_word(rest).map(|(n, _)| (n.to_string(), "enum")),
        "struct" => take_word(rest).map(|(n, _)| (n.to_string(), "struct")),
        "union" => take_word(rest).map(|(n, _)| (n.to_string(), "union")),
        "trait" => take_word(rest).map(|(n, _)| (n.to_string(), "trait")),
        "mod" => take_word(rest).map(|(n, _)| (n.to_string(), "mod")),
        "impl" => {
            let mut r = rest.trim_start();
            if r.starts_with('<') {
                if let Some(c) = r.find('>') {
                    r = r[c + 1..].trim_start();
                }
            }
            take_word(r).map(|(n, _)| (n.to_string(), "impl"))
        }
        "func" => {
            // Go: func name 或 func (recv) name
            let mut r = rest.trim_start();
            if r.starts_with('(') {
                if let Some(c) = r.find(')') {
                    r = r[c + 1..].trim_start();
                }
            }
            take_word(r).map(|(n, _)| (n.to_string(), "fn"))
        }
        "fn" => take_word(rest).map(|(n, _)| (n.to_string(), "fn")),
        "const" | "let" | "var" => {
            let (n, r2) = take_word(rest)?;
            // Rust: const fn / static fn
            if n == "fn" {
                return take_word(r2).map(|(m, _)| (m.to_string(), "fn"));
            }
            let r2t = r2.trim_start();
            // 跳过 TS 类型标注 `: Type`
            let after = if r2t.starts_with(':') {
                match r2t.find('=') {
                    Some(p) => &r2t[p..],
                    None => return None,
                }
            } else {
                r2t
            };
            let v = after.strip_prefix('=')?.trim_start();
            let is_fn = v.starts_with('(')
                || v.starts_with("function")
                || v.starts_with("async")
                || take_word(v)
                    .map(|(_, rr)| rr.trim_start().starts_with("=>"))
                    .unwrap_or(false);
            if is_fn {
                Some((n.to_string(), "fn"))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 方法识别（depth >= 1）：`name(args) {`，行内必须开块。
pub fn parse_method(line: &str) -> Option<(String, &'static str)> {
    let s = strip_decl_mods(line);
    let (name, rest) = take_word(s)?;
    if CTRL.contains(&name) {
        return None;
    }
    let mut r = rest.trim_start();
    if r.starts_with('<') {
        // 跳过泛型参数（配对尖括号）
        let mut depth = 0i32;
        let mut end = None;
        for (i, c) in r.char_indices() {
            match c {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        match end {
            Some(i) => r = r[i + 1..].trim_start(),
            None => return None,
        }
    }
    if !r.starts_with('(') || !line.contains('{') {
        return None;
    }
    Some((name.to_string(), "method"))
}

/// 属性块识别（depth >= 1）：`name = (x) => {` / `name: T = async () => {`。
pub fn parse_prop(line: &str) -> Option<(String, &'static str)> {
    let s = strip_decl_mods(line);
    let (name, rest) = take_word(s)?;
    if CTRL.contains(&name) {
        return None;
    }
    let r = rest.trim_start();
    let v = if r.starts_with(':') {
        let p = r.find('=')?;
        &r[p + 1..]
    } else if let Some(v) = r.strip_prefix('=') {
        v
    } else {
        return None;
    };
    let v = v.trim_start();
    let arrow = v.starts_with('(')
        || v.starts_with("async")
        || take_word(v)
            .map(|(_, rr)| rr.trim_start().starts_with("=>"))
            .unwrap_or(false);
    if arrow && line.trim_end().ends_with('{') {
        Some((name.to_string(), "prop"))
    } else {
        None
    }
}

// ---------------------------------------------------------------- 注释/字符串剥离 + 括号深度

enum St {
    Normal,
    BlockComment,
    Str(char),
    Template,
    /// Rust 原始字符串：r#"..."#，记录 # 个数
    RawStr(usize),
}

/// 正则字面量只能出现在这些字符之后（否则 `/` 是除号）。
/// 不含 `<`/`>`：JSX 的 `</div>` 会被误判成正则起始。
const REGEX_PREV: &[char] = &[
    '(', ',', '=', ':', '[', '!', '&', '|', '?', '{', ';', '+', '*', '%', '~', '^',
];
const REGEX_PREV_WORDS: &[&str] = &[
    "return", "case", "typeof", "instanceof", "in", "of", "do", "else", "yield", "await",
    "new",
];

/// 当前行已输出代码（空白已置空格）+ 上一非空行尾字符，判断此处的 `/` 是否为正则起始。
fn regex_allowed(out: &[char], prev_line_last: Option<char>) -> bool {
    let last = out
        .iter()
        .rev()
        .find(|c| !c.is_whitespace())
        .copied()
        .or(prev_line_last);
    let Some(last) = last else { return true };
    if REGEX_PREV.contains(&last) {
        return true;
    }
    // 行尾关键字形式：`...return /re/`
    let word: String = out
        .iter()
        .rev()
        .take_while(|c| c.is_ascii_alphanumeric() || **c == '_' || **c == '$')
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    REGEX_PREV_WORDS.contains(&word.as_str())
}

/// 单引号为字符字面量（而非字符串）的语言：Rust/Go/Java/C 系等。
/// 这些语言中 `'a` 多为生命周期/字符，若误判为字符串起始会吞掉后续括号。
const CHAR_LITERAL_EXT: &[&str] = &[
    "rs", "go", "java", "c", "cc", "cpp", "h", "hpp", "cs", "kt", "kts", "swift", "dart",
    "zig",
];

fn is_char_mode_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    let Some(pos) = lower.rfind('.') else { return false };
    CHAR_LITERAL_EXT.contains(&&lower[pos + 1..])
}

/// 字符字面量判定：'x' 或 '\n' 形式；否则视为生命周期注解。
fn is_char_literal(chars: &[char], j: usize) -> bool {
    chars.get(j + 2) == Some(&'\'')
        || (chars.get(j + 1) == Some(&'\\') && chars.get(j + 3) == Some(&'\''))
}

/// 逐字符状态机：剥离注释与字符串，统计每行行首/行尾花括号深度。
/// 模板字符串内 ${ ... } 表达式照常计括号；其余内容置空格。
/// 支持正则字面量启发式（REGEX_PREV）与 Rust 原始字符串 r#"..."#；
/// char_mode 下 `'` 仅在字符字面量形态时视为字符串起始（区分 Rust 生命周期）。
pub fn strip_and_depth(lines: &[&str], char_mode: bool) -> (Vec<String>, Vec<u32>, Vec<u32>) {
    let mut code = Vec::with_capacity(lines.len());
    let mut ds = vec![0u32; lines.len()];
    let mut da = vec![0u32; lines.len()];
    let mut depth: u32 = 0;
    let mut st = St::Normal;
    let mut tmpl: Vec<u32> = Vec::new();
    let mut prev_line_last: Option<char> = None;
    for (i, line) in lines.iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mut out = chars.clone();
        let mut started = false;
        let mut j = 0;
        while j < chars.len() {
            let c = chars[j];
            let n = chars.get(j + 1).copied().unwrap_or('\0');
            match st {
                St::BlockComment => {
                    out[j] = ' ';
                    if c == '*' && n == '/' {
                        out[j + 1] = ' ';
                        j += 1;
                        st = St::Normal;
                    }
                }
                St::Str(q) => {
                    out[j] = ' ';
                    if c == '\\' {
                        if j + 1 < chars.len() {
                            out[j + 1] = ' ';
                        }
                        j += 1;
                    } else if c == q {
                        st = St::Normal;
                    }
                }
                St::RawStr(hashes) => {
                    out[j] = ' ';
                    if c == '"'
                        && (1..=hashes).all(|k| chars.get(j + k) == Some(&'#'))
                    {
                        for k in 1..=hashes {
                            out[j + k] = ' ';
                        }
                        j += hashes;
                        st = St::Normal;
                    }
                }
                St::Template => {
                    if c == '\\' {
                        out[j] = ' ';
                        if j + 1 < chars.len() {
                            out[j + 1] = ' ';
                        }
                        j += 1;
                    } else if c == '`' {
                        out[j] = ' ';
                        st = St::Normal;
                    } else if c == '$' && n == '{' {
                        out[j] = ' ';
                        out[j + 1] = ' ';
                        j += 1;
                        tmpl.push(depth);
                        depth += 1; // ${ 视作一次真实的括号开启，} 闭合并回到模板态
                        st = St::Normal;
                    } else {
                        out[j] = ' ';
                    }
                }
                St::Normal => {
                    if c == '/' && n == '/' {
                        for k in j..chars.len() {
                            out[k] = ' ';
                        }
                        break;
                    }
                    if c == '/' && n == '*' {
                        out[j] = ' ';
                        out[j + 1] = ' ';
                        j += 1;
                        st = St::BlockComment;
                    } else if c == '/' && regex_allowed(&out[..j.min(out.len())], prev_line_last) {
                        // 正则字面量：置空格直至未转义的 `/`（方括号字符类内的 `/` 不算结束）
                        out[j] = ' ';
                        let mut in_class = false;
                        let mut k = j + 1;
                        while k < chars.len() {
                            let c2 = chars[k];
                            out[k] = ' ';
                            if c2 == '\\' {
                                if k + 1 < chars.len() {
                                    out[k + 1] = ' ';
                                }
                                k += 1;
                            } else if c2 == '[' {
                                in_class = true;
                            } else if c2 == ']' {
                                in_class = false;
                            } else if c2 == '/' && !in_class {
                                break;
                            }
                            k += 1;
                        }
                        j = k;
                    } else if c == 'r' && is_raw_string_start(&chars, j) {
                        // Rust 原始字符串 r"..." / r#"..."#
                        let mut k = j + 1;
                        while chars.get(k) == Some(&'#') {
                            k += 1;
                        }
                        let hashes = k - j - 1;
                        for m in j..=k {
                            out[m] = ' ';
                        }
                        j = k;
                        st = St::RawStr(hashes);
                    } else if c == '"' || c == '\'' {
                        if c == '\'' && char_mode && !is_char_literal(&chars, j) {
                            // Rust/Go 生命周期或借用标注，不当字符串
                        } else {
                            out[j] = ' ';
                            st = St::Str(c);
                        }
                    } else if c == '`' {
                        out[j] = ' ';
                        st = St::Template;
                    } else {
                        if !started && c != ' ' && c != '\t' {
                            ds[i] = depth;
                            started = true;
                        }
                        if c == '{' {
                            depth += 1;
                        } else if c == '}' {
                            depth = depth.saturating_sub(1);
                            // 模板 ${ expr } 的右括号：回到模板字符串态
                            if let Some(&t) = tmpl.last() {
                                if depth == t {
                                    tmpl.pop();
                                    st = St::Template;
                                }
                            }
                        }
                    }
                }
            }
            j += 1;
        }
        if !started {
            ds[i] = depth;
        }
        prev_line_last = out.iter().rev().find(|c| !c.is_whitespace()).copied().or(prev_line_last);
        code.push(out.into_iter().collect());
        da[i] = depth;
    }
    (code, ds, da)
}

/// 检测 Rust 原始字符串起始：r" 或 r#+" 。
fn is_raw_string_start(chars: &[char], j: usize) -> bool {
    let mut k = j + 1;
    while chars.get(k) == Some(&'#') {
        k += 1;
    }
    chars.get(k) == Some(&'"')
}

/// 声明所在块的真实结束行（花括号配对）。
fn block_end(i: usize, d: u32, da: &[u32], total: usize) -> usize {
    if da[i] <= d {
        return i; // 单行声明或签名跨行，保守取自身
    }
    let open = da[i];
    for j in i + 1..total {
        if da[j] < open {
            return j;
        }
    }
    total - 1
}

// ---------------------------------------------------------------- Python（缩进）

fn scan_python(lines: &[&str]) -> Vec<Sym> {
    let mut syms = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        let indent = (line.len() - t.len()) as u32;
        let (kind, rest) = if let Some(r) = t.strip_prefix("async def ") {
            ("fn", r)
        } else if let Some(r) = t.strip_prefix("def ") {
            ("fn", r)
        } else if let Some(r) = t.strip_prefix("class ") {
            ("class", r)
        } else {
            continue;
        };
        let Some((name, _)) = take_word(rest) else { continue };
        let mut end = i;
        for (j, l2) in lines.iter().enumerate().skip(i + 1) {
            if l2.trim().is_empty() {
                continue;
            }
            let lead = (l2.len() - l2.trim_start().len()) as u32;
            if lead <= indent {
                end = j - 1;
                break;
            }
            end = j;
        }
        let sig: String = line.trim().split_whitespace().collect::<Vec<_>>().join(" ");
        syms.push(Sym {
            ln: i as u32 + 1,
            end: end as u32 + 1,
            depth: indent / 4,
            kind,
            name: name.to_string(),
            sig: truncate_chars(&sig, 120),
            exp: !name.starts_with('_'),
        });
    }
    syms
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        s.chars().take(max).collect()
    } else {
        s.to_string()
    }
}

// ---------------------------------------------------------------- 主扫描入口

/// 扫描单个源文件，输出符号边界（ln/end 均为 1-indexed 闭区间）。
pub fn scan_source(text: &str, file: &str) -> (usize, Vec<Sym>) {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.len() > 1 && lines.last() == Some(&"") {
        lines.pop();
    }
    let total = lines.len();
    let lower = file.to_lowercase();
    if lower.ends_with(".py") || lower.ends_with(".pyi") {
        return (total, scan_python(&lines));
    }
    let (code, ds, da) = strip_and_depth(&lines, is_char_mode_file(file));
    let mut syms = Vec::new();
    for i in 0..total {
        let d = ds[i];
        if d > 2 {
            continue; // 嵌套过深（局部函数/闭包）不进索引
        }
        let t = code[i].trim_start();
        if t.len() < 3 {
            continue;
        }
        let mut found = parse_decl(t);
        if found.is_none() && d >= 1 {
            found = parse_method(t).or_else(|| parse_prop(t));
        }
        let Some((name, kind)) = found else { continue };
        let end = block_end(i, d, &da, total);
        if end < i {
            continue;
        }
        // 单行 prop/method 无分析价值
        let trimmed = code[i].trim_end();
        if (kind == "prop" || kind == "method")
            && end == i
            && !(trimmed.ends_with('{') || trimmed.ends_with('('))
        {
            continue;
        }
        let sig: String = lines[i].trim().split_whitespace().collect::<Vec<_>>().join(" ");
        syms.push(Sym {
            ln: i as u32 + 1,
            end: end as u32 + 1,
            depth: d,
            kind,
            name,
            sig: truncate_chars(&sig, 120),
            exp: t.starts_with("export") || t.starts_with("pub"),
        });
    }
    (total, syms)
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_ts_class_and_method() {
        let src = "export class Foo {\n  bar() {\n    return 1;\n  }\n}\n\nfunction helper() {}\n";
        let (_, syms) = scan_source(src, "a.ts");
        assert!(
            syms.iter()
                .any(|s| s.name == "Foo" && s.kind == "class" && s.ln == 1 && s.end == 5),
            "class Foo 边界: {syms:?}"
        );
        assert!(
            syms.iter()
                .any(|s| s.name == "bar" && s.kind == "method" && s.ln == 2 && s.end == 4),
            "method bar 边界: {syms:?}"
        );
        assert!(syms.iter().any(|s| s.name == "helper" && s.kind == "fn"));
    }

    #[test]
    fn scans_arrow_fn_and_skips_deep_nesting() {
        let src = "export const loadConfig = async (root) => {\n  return root;\n};\n";
        let (_, syms) = scan_source(src, "a.ts");
        assert!(syms.iter().any(|s| s.name == "loadConfig" && s.kind == "fn"));
    }

    #[test]
    fn scans_rust() {
        let src = "pub fn retry_auth() {\n    loop {}\n}\n\nstruct Handler {\n    count: u32,\n}\n\nimpl Handler {\n    fn bump(&mut self) {\n        self.count += 1;\n    }\n}\n";
        let (_, syms) = scan_source(src, "a.rs");
        assert!(syms.iter().any(|s| s.name == "retry_auth" && s.kind == "fn" && s.exp));
        assert!(syms.iter().any(|s| s.name == "Handler" && s.kind == "struct"));
        assert!(syms.iter().any(|s| s.name == "bump" && s.kind == "fn" && s.depth == 1));
    }

    #[test]
    fn braces_in_strings_and_comments_do_not_shift_depth() {
        let src = "function a() {\n  const s = \"}{{\"; // }}}{{\n  /* { } */ return 1;\n}\nfunction b() {}\n";
        let (_, syms) = scan_source(src, "a.js");
        let a = syms.iter().find(|s| s.name == "a").unwrap();
        assert_eq!((a.ln, a.end), (1, 4), "{syms:?}");
        assert!(syms.iter().any(|s| s.name == "b" && s.ln == 5));
    }

    #[test]
    fn regex_literals_do_not_shift_depth() {
        // 正则中的引号/括号不应影响深度（ctx-index.mjs 的真实场景）
        let src = "export function a() {\n  const re = /['\"{}]/;\n  return 1;\n}\nfunction b() {}\n";
        let (_, syms) = scan_source(src, "a.mjs");
        let a = syms.iter().find(|s| s.name == "a").unwrap();
        assert_eq!((a.ln, a.end), (1, 4), "{syms:?}");
        assert!(syms.iter().any(|s| s.name == "b" && s.ln == 5));
    }

    #[test]
    fn rust_lifetimes_are_not_strings() {
        let src = "impl<'a> Store<'a> {\n    fn get(&self, x: &'a str) -> Option<&'a str> {\n        Some(x)\n    }\n}\nfn after() {}\n";
        let (_, syms) = scan_source(src, "a.rs");
        assert!(syms.iter().any(|s| s.name == "get" && s.kind == "fn"));
        assert!(syms.iter().any(|s| s.name == "after"), "{syms:?}");
    }

    #[test]
    fn rust_raw_strings_do_not_shift_depth() {
        let src = "fn a() {\n    let s = r#\"}{\"#;\n}\nfn b() {}\n";
        let (_, syms) = scan_source(src, "a.rs");
        let a = syms.iter().find(|s| s.name == "a").unwrap();
        assert_eq!((a.ln, a.end), (1, 3), "{syms:?}");
        assert!(syms.iter().any(|s| s.name == "b"));
    }

    #[test]
    fn template_literals_do_not_shift_depth() {
        // 一行内多个 ${} 且含三元/对象字面量
        let src = "function a() {\n  const s = `${base}::${orig}`;\n  const t = `${ {a:1} } end`;\n}\nfunction b() {}\n";
        let (_, syms) = scan_source(src, "a.mjs");
        let a = syms.iter().find(|s| s.name == "a").unwrap();
        assert_eq!((a.ln, a.end), (1, 4), "{syms:?}");
        assert!(syms.iter().any(|s| s.name == "b" && s.ln == 5));
    }

    #[test]
    fn scans_python_indent_blocks() {
        let src = "def foo():\n    return 1\n\nclass Bar:\n    def baz(self):\n        pass\n";
        let (_, syms) = scan_source(src, "a.py");
        assert!(syms.iter().any(|s| s.name == "foo" && s.kind == "fn"));
        assert!(
            syms.iter()
                .any(|s| s.name == "Bar" && s.kind == "class" && s.ln == 4 && s.end == 6),
            "{syms:?}"
        );
        assert!(syms.iter().any(|s| s.name == "baz" && s.kind == "fn" && s.depth == 1));
    }
}
