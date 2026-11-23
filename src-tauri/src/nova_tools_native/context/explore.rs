//! Repo-aware parallel exploration and verified verbatim source assembly for Polaris.
//!
//! All source returned to the caller is read verbatim from disk under canonical `root` confinement.
//! Queries, paths, and model proposals are untrusted data, never unvalidated filesystem paths.

use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const PROJECT_FILE_CACHE_TTL_MS: u64 = 15_000;
const MAX_PROJECT_FILES: usize = 30_000;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const FULL_TREE_FILE_THRESHOLD: usize = 800;
const IN_PROCESS_GREP_FILE_LIMIT: usize = 650;
const MAX_REPO_MAP_CHARS: usize = 12_000;
const MAX_LIST_PATTERNS: usize = 10;
const MAX_LIST_RESULTS: usize = 80;
const MAX_GREP_QUERIES: usize = 12;
const MAX_GREP_HITS_PER_QUERY: usize = 25;
const MAX_GREP_TOTAL_CHARS: usize = 14_000;
const MAX_OUTLINE_FILES: usize = 32;
const MAX_OUTLINE_TOTAL_CHARS: usize = 26_000;
const MAX_READ_SLICES: usize = 12;
const MAX_READ_LINES: usize = 150;
const MAX_READ_TOTAL_CHARS: usize = 16_000;
const MAX_GIT_QUERIES: usize = 4;
const MAX_GIT_LINES: usize = 120;
const MAX_FULL_SELECTION_LINES: usize = 350;
const OVERSIZED_FULL_PREVIEW_LINES: usize = 250;
const MAX_AUTO_DEP_LINES: usize = 80;
const MAX_AUTO_DEPS_PER_FILE: usize = 8;
const MAX_AUTO_DEPS_BYTES: usize = 32 * 1024;
const MAX_ASSEMBLED_BYTES: usize = 220 * 1024;

#[derive(Clone)]
struct CachedFileList {
    created: Instant,
    files: Arc<Vec<String>>,
}

static PROJECT_FILES_CACHE: OnceLock<Mutex<HashMap<(PathBuf, Option<PathBuf>), CachedFileList>>> =
    OnceLock::new();

pub(crate) struct Workspace {
    pub(crate) root: PathBuf,
    pub(crate) nested: Option<(PathBuf, String)>,
    pub(crate) files: Arc<Vec<String>>,
    pub(crate) allowed: HashSet<String>,
    pub(crate) explicit: Vec<String>,
    pub(crate) notes: Vec<String>,
    file_cache: Mutex<HashMap<String, Option<Arc<Vec<String>>>>>,
}

pub(crate) struct Briefing {
    pub(crate) text: String,
    pub(crate) preflight_selections: Vec<Selection>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct GrepQuery {
    pub(crate) q: String,
    #[serde(default)]
    pub(crate) path: Option<String>,
    #[serde(default)]
    pub(crate) regex: bool,
    #[serde(default)]
    pub(crate) case_sensitive: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ReadSlice {
    pub(crate) file: String,
    #[serde(default)]
    pub(crate) start: Option<usize>,
    #[serde(default)]
    pub(crate) end: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct GitQuery {
    pub(crate) rev: String,
    #[serde(default)]
    pub(crate) path: Option<String>,
    #[serde(default)]
    pub(crate) diff: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Selection {
    pub(crate) file: String,
    #[serde(default)]
    pub(crate) start: Option<usize>,
    #[serde(default)]
    pub(crate) end: Option<usize>,
    #[serde(default)]
    pub(crate) lines: Option<String>,
    #[serde(default)]
    pub(crate) symbols: Vec<String>,
    #[serde(default)]
    pub(crate) full: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct BatchRequest {
    pub(crate) list: Vec<String>,
    pub(crate) grep: Vec<GrepQuery>,
    pub(crate) outline: Vec<String>,
    pub(crate) read: Vec<ReadSlice>,
    pub(crate) git: Vec<GitQuery>,
}

impl BatchRequest {
    pub(crate) fn action_count(&self) -> usize {
        self.list.len() + self.grep.len() + self.outline.len() + self.read.len() + self.git.len()
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.action_count() == 0
    }
}

pub(crate) struct Assembled {
    pub(crate) text: String,
    pub(crate) file_count: usize,
    pub(crate) block_count: usize,
}

pub(crate) fn is_project_text_file(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    for seg in lower.split('/') {
        if matches!(
            seg,
            "node_modules"
                | "dist"
                | "target"
                | "coverage"
                | ".git"
                | "__pycache__"
                | ".venv"
                | "venv"
                | ".next"
                | ".nuxt"
                | ".output"
                | ".turbo"
                | ".cache"
        ) {
            return false;
        }
    }
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    if matches!(
        name,
        "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "cargo.lock"
            | "go.sum"
            | "poetry.lock"
            | "composer.lock"
            | "gemfile.lock"
            | "bun.lockb"
            | "bun.lock"
            | ".ds_store"
            | "thumbs.db"
    ) {
        return false;
    }
    if lower.ends_with(".min.js")
        || lower.ends_with(".min.css")
        || lower.ends_with(".map")
        || lower.ends_with(".lock")
        || lower.ends_with(".bundle.js")
        || lower.contains(".generated.")
    {
        return false;
    }
    if matches!(
        name,
        "dockerfile"
            | "makefile"
            | "justfile"
            | "procfile"
            | "version"
            | "go.mod"
            | "cargo.toml"
            | ".env.example"
            | ".gitignore"
            | ".dockerignore"
    ) || name.starts_with("dockerfile.")
    {
        return true;
    }
    let ext = Path::new(name)
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("");
    CODE_FILE_EXTENSIONS.contains(&ext)
        || matches!(
            ext,
            "json"
                | "jsonc"
                | "json5"
                | "toml"
                | "yaml"
                | "yml"
                | "html"
                | "htm"
                | "xml"
                | "plist"
                | "proto"
                | "graphql"
                | "gql"
                | "prisma"
                | "sh"
                | "bash"
                | "zsh"
                | "ps1"
                | "bat"
                | "cmd"
                | "txt"
                | "cfg"
                | "ini"
                | "conf"
                | "mod"
                | "gradle"
                | "cmake"
                | "nix"
                | "lua"
                | "rb"
                | "ex"
                | "exs"
                | "erl"
                | "r"
                | "jl"
                | "sol"
                | "move"
                | "wgsl"
                | "glsl"
                | "manifest"
                | "rc"
        )
}

fn git_project_files(repo: &Path) -> Option<Vec<String>> {
    let args = vec![
        "ls-files".into(),
        "-c".into(),
        "-o".into(),
        "--exclude-standard".into(),
        "-z".into(),
    ];
    let stdout = run_command_until_limited(
        repo,
        "git",
        &args,
        Some(Instant::now() + Duration::from_millis(1_500)),
        MAX_FILE_LIST_OUTPUT_BYTES,
    )?;
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    for raw in stdout.split(|b| *b == 0) {
        if raw.is_empty() {
            continue;
        }
        let Ok(s) = std::str::from_utf8(raw) else { continue };
        let rel = normalize_rel(s);
        if is_project_text_file(&rel) && seen.insert(rel.clone()) {
            files.push(rel);
            if files.len() >= MAX_PROJECT_FILES {
                break;
            }
        }
    }
    Some(files)
}

fn walk_project_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .ignore(true)
        .follow_links(false);
    for entry in builder.build().filter_map(Result::ok) {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else { continue };
        let rel = normalize_rel(&rel.to_string_lossy());
        if is_project_text_file(&rel) {
            files.push(rel);
            if files.len() >= MAX_PROJECT_FILES {
                break;
            }
        }
    }
    files
}

fn list_project_files(
    root: &Path,
    nested: Option<&(PathBuf, String)>,
    notes: &mut Vec<String>,
) -> Arc<Vec<String>> {
    let key = (
        root.to_path_buf(),
        nested.map(|(repo, _)| repo.to_path_buf()),
    );
    let cache = PROJECT_FILES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(entry) = guard.get(&key) {
            if entry.created.elapsed() < Duration::from_millis(PROJECT_FILE_CACHE_TTL_MS) {
                return entry.files.clone();
            }
        }
    }
    let mut files = git_project_files(root)
        .filter(|list| !list.is_empty())
        .unwrap_or_else(|| walk_project_files(root));
    if let Some((repo, prefix)) = nested {
        files.retain(|f| !f.starts_with(prefix));
        if let Some(inner) = git_project_files(repo) {
            files.extend(inner.into_iter().map(|f| format!("{prefix}{f}")));
        } else {
            notes.push("内层 Git 工作树文件清单不可用，未检索该嵌套目录。".into());
        }
    }
    files.sort();
    files.dedup();
    let files = Arc::new(files);
    if let Ok(mut guard) = cache.lock() {
        guard.retain(|_, v| {
            v.created.elapsed() < Duration::from_millis(PROJECT_FILE_CACHE_TTL_MS * 2)
        });
        guard.insert(
            key,
            CachedFileList {
                created: Instant::now(),
                files: files.clone(),
            },
        );
    }
    files
}

fn split_code_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            if cur.len() >= 2 {
                out.push(cur.to_ascii_lowercase());
            }
            cur.clear();
            continue;
        }
        if !cur.is_empty()
            && c.is_ascii_uppercase()
            && chars
                .get(i.saturating_sub(1))
                .is_some_and(|prev| prev.is_ascii_lowercase() || prev.is_ascii_digit())
        {
            if cur.len() >= 2 {
                out.push(cur.to_ascii_lowercase());
            }
            cur.clear();
        }
        cur.push(c);
    }
    if cur.len() >= 2 {
        out.push(cur.to_ascii_lowercase());
    }
    out
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_ascii_lowercase().chars().collect();
    let t: Vec<char> = text.to_ascii_lowercase().chars().collect();
    let (m, n) = (p.len(), t.len());
    let mut dp = vec![vec![false; n + 1]; m + 1];
    dp[0][0] = true;
    for i in 1..=m {
        if p[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }
    for i in 1..=m {
        let double_star = p[i - 1] == '*' && i >= 2 && p[i - 2] == '*';
        for j in 1..=n {
            if p[i - 1] == '*' {
                if double_star {
                    dp[i][j] = dp[i - 1][j] || dp[i][j - 1] || dp[i - 2][j];
                } else {
                    dp[i][j] = dp[i - 1][j] || (t[j - 1] != '/' && dp[i][j - 1]);
                }
            } else if p[i - 1] == '?' {
                dp[i][j] = t[j - 1] != '/' && dp[i - 1][j - 1];
            } else {
                dp[i][j] = p[i - 1] == t[j - 1] && dp[i - 1][j - 1];
            }
        }
    }
    dp[m][n]
}

fn read_lines_confined(root: &Path, rel: &str) -> Result<Vec<String>, String> {
    if Path::new(rel)
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!("路径越出检索目录：{rel}"));
    }
    let path = dunce::canonicalize(root.join(rel)).map_err(|e| format!("无法访问 {rel}：{e}"))?;
    if !path.starts_with(root) {
        return Err(format!("路径越出检索目录：{rel}"));
    }
    let meta = fs::metadata(&path).map_err(|e| format!("无法读取 {rel} 元信息：{e}"))?;
    if !meta.is_file() {
        return Err(format!("{rel} 不是文件"));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("{rel} 超过 2MiB，跳过整文件加载"));
    }
    let bytes = fs::read(&path).map_err(|e| format!("无法读取 {rel}：{e}"))?;
    if bytes.iter().take(4096).any(|&b| b == 0) {
        return Err(format!("{rel} 为二进制文件"));
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();
    if lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    Ok(lines)
}

fn clip_line(s: &str, max_chars: usize) -> String {
    let trimmed = s.trim_end();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        let mut out: String = trimmed.chars().take(max_chars).collect();
        out.push('…');
        out
    }
}

fn parse_line_ranges(spec: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    for part in spec.split([',', ';']) {
        let p = part.trim().trim_start_matches(['L', 'l', ':']);
        if p.is_empty() {
            continue;
        }
        if let Some((a, b)) = p.split_once('-').or_else(|| p.split_once("..")) {
            let a = a.trim().trim_start_matches(['L', 'l']);
            let b = b.trim().trim_start_matches(['L', 'l', '=']);
            if let (Ok(s), Ok(e)) = (a.parse::<usize>(), b.parse::<usize>()) {
                if s > 0 && e >= s {
                    ranges.push((s, e));
                }
            }
        } else if let Ok(ln) = p.parse::<usize>() {
            if ln > 0 {
                ranges.push((ln, ln));
            }
        }
    }
    ranges
}

impl Workspace {
    pub(crate) fn open(
        cwd: &Path,
        root_arg: Option<&str>,
        explicit_files: &[String],
    ) -> Result<Self, String> {
        let target = match root_arg {
            None => cwd.to_path_buf(),
            Some(path) if !path.trim().is_empty() => cwd.join(path.trim()),
            Some(_) => return Err("polaris.root 必须是非空目录路径".into()),
        };
        let root = dunce::canonicalize(&target)
            .map_err(|e| format!("无法访问检索目录 {}：{e}", target.display()))?;
        if !root.is_dir() {
            return Err("polaris.root 不是目录".into());
        }
        let mut explicit = Vec::new();
        for file in explicit_files {
            let trimmed = file.trim();
            if trimmed.is_empty() {
                continue;
            }
            let resolved = dunce::canonicalize(root.join(trimmed))
                .map_err(|e| format!("无法访问检索文件 {trimmed}：{e}"))?;
            let relative = resolved.strip_prefix(&root).map_err(|_| {
                format!("检索文件不属于 {}，请明确指定正确的 root", root.display())
            })?;
            explicit.push(normalize_rel(&relative.to_string_lossy()));
        }
        explicit.sort();
        explicit.dedup();

        let cwd_canon = dunce::canonicalize(cwd).ok();
        let nested = cwd_canon
            .as_deref()
            .into_iter()
            .flat_map(Path::ancestors)
            .take_while(|p| p.starts_with(&root) && *p != root)
            .find(|p| p.join(".git").exists())
            .and_then(|repo| {
                let prefix = format!(
                    "{}/",
                    repo.strip_prefix(&root).ok()?.to_str()?.replace('\\', "/")
                );
                Some((repo.to_path_buf(), prefix))
            });

        let mut notes = Vec::new();
        let files = list_project_files(&root, nested.as_ref(), &mut notes);
        let mut allowed: HashSet<String> = files.iter().cloned().collect();
        for f in &explicit {
            allowed.insert(f.clone());
        }
        Ok(Self {
            root,
            nested,
            files,
            allowed,
            explicit,
            notes,
            file_cache: Mutex::new(HashMap::new()),
        })
    }

    fn cached_lines(&self, rel: &str) -> Result<Arc<Vec<String>>, String> {
        if let Ok(guard) = self.file_cache.lock() {
            if let Some(entry) = guard.get(rel) {
                return entry
                    .clone()
                    .ok_or_else(|| format!("文件 {rel} 不可读或已跳过"));
            }
        }
        match read_lines_confined(&self.root, rel) {
            Ok(lines) => {
                let arc = Arc::new(lines);
                if let Ok(mut guard) = self.file_cache.lock() {
                    guard.insert(rel.to_string(), Some(arc.clone()));
                }
                Ok(arc)
            }
            Err(e) => {
                if let Ok(mut guard) = self.file_cache.lock() {
                    guard.insert(rel.to_string(), None);
                }
                Err(e)
            }
        }
    }

    pub(crate) fn normalize_input_path(&self, raw: &str) -> Result<String, String> {
        let trimmed = raw
            .trim()
            .trim_matches('`')
            .trim_matches('"')
            .trim_matches('\'');
        if trimmed.is_empty() {
            return Err("空路径".into());
        }
        let p = Path::new(trimmed);
        if p.is_absolute() {
            let canon = dunce::canonicalize(p).map_err(|e| format!("无法访问 {trimmed}：{e}"))?;
            let rel = canon
                .strip_prefix(&self.root)
                .map_err(|_| format!("路径越出检索目录：{trimmed}"))?;
            return Ok(normalize_rel(&rel.to_string_lossy()));
        }
        let rel = normalize_rel(trimmed);
        if Path::new(&rel)
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return Err(format!("路径越出检索目录：{trimmed}"));
        }
        Ok(rel)
    }

    /// Step 0 local pre-flight: Repo Map + Git Commit introspection + Explicit/Route/Exact-Anchor hits.
    pub(crate) fn briefing(&self, task: &str, keywords: &[String]) -> Briefing {
        let want_tests = retrieval_plan(task).tests;
        let (commits, path_matches) = std::thread::scope(|s| {
            let h_commits = s.spawn(|| self.inspect_task_commits(task));
            let h_paths = s.spawn(|| self.match_paths_for_task(task, keywords));
            (
                h_commits.join().unwrap_or_default(),
                h_paths.join().unwrap_or_default(),
            )
        });
        // Skip noisy repo-wide anchor grep if git commits already give the exact file inventory.
        let preflight_hits = if commits.is_empty() || !keywords.is_empty() {
            self.preflight_exact_anchors(task, keywords)
        } else {
            (String::new(), Vec::new())
        };

        let mut out = String::new();
        out.push_str(&format!("任务：{task}\n"));
        if !keywords.is_empty() {
            out.push_str(&format!("补充关键词：{}\n", keywords.join(", ")));
        }
        out.push_str(&format!("仓库规模：{} 个文本/代码文件\n", self.files.len()));
        if !self.notes.is_empty() {
            out.push_str(&format!("提示：{}\n", self.notes.join(" ")));
        }

        if !commits.is_empty() {
            out.push_str("\n## 任务提及的 Git 提交（已自动解析变更文件）\n");
            out.push_str(&commits);
        }

        out.push_str("\n## 仓库目录结构\n");
        out.push_str(&self.render_repo_map(task, keywords, &path_matches));

        let mut preflight_files: Vec<String> = self.explicit.clone();
        for f in self.files.iter() {
            if f.len() >= 5 && task.contains(f.as_str()) && !preflight_files.contains(f) {
                preflight_files.push(f.clone());
            }
        }
        let route_entries = self.route_entry_files(task);
        for f in &route_entries {
            if !preflight_files.contains(f) {
                preflight_files.push(f.clone());
            }
        }
        preflight_files.truncate(10);

        let mut preflight_selections = Vec::new();
        for f in &self.explicit {
            preflight_selections.push(Selection {
                file: f.clone(),
                full: true,
                ..Default::default()
            });
        }
        for f in &route_entries {
            preflight_selections.push(Selection {
                file: f.clone(),
                start: Some(1),
                end: Some(40),
                ..Default::default()
            });
        }

        if !preflight_files.is_empty() {
            out.push_str("\n## 已直接命中的入口/显式文件大纲\n");
            let outlines = self.outline_files(&preflight_files, 6_000, want_tests);
            out.push_str(&outlines);
        }

        if !preflight_hits.0.is_empty() {
            out.push_str("\n## 精确符号/路由预探测命中\n");
            out.push_str(&preflight_hits.0);
            preflight_selections.extend(preflight_hits.1);
        }

        Briefing {
            text: out,
            preflight_selections,
        }
    }

    fn render_repo_map(&self, task: &str, keywords: &[String], path_matches: &[String]) -> String {
        let want_tests = retrieval_plan(task).tests;
        let task_words: HashSet<String> = std::iter::once(task)
            .chain(keywords.iter().map(String::as_str))
            .flat_map(split_code_words)
            .filter(|w| w.len() >= 3 && !stop_word(w))
            .collect();
        let mut out = String::new();

        if self.files.len() <= FULL_TREE_FILE_THRESHOLD {
            let mut by_dir: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            for f in self.files.iter() {
                let (dir, name) = match f.rsplit_once('/') {
                    Some((d, n)) => (d, n),
                    None => (".", f.as_str()),
                };
                by_dir.entry(dir).or_default().push(name);
            }
            for (dir, names) in by_dir {
                let is_noise_dir = !want_tests
                    && (dir == "tests"
                        || dir.starts_with("tests/")
                        || dir.ends_with("/__tests__")
                        || dir.ends_with("/tests")
                        || dir.starts_with("reference/grokbot-ui")
                        || dir.starts_with("bench"));
                let line = if is_noise_dir && names.len() > 8 {
                    let preview = names.iter().take(6).copied().collect::<Vec<_>>().join(", ");
                    format!("{dir}/ ({} files, 例: {preview}, ...)\n", names.len())
                } else {
                    format!("{dir}/: {}\n", names.join(", "))
                };
                if out.len() + line.len() > MAX_REPO_MAP_CHARS {
                    out.push_str("...（其余目录可用 list 查询）\n");
                    break;
                }
                out.push_str(&line);
            }
            return out;
        }

        // Large repo (> 800 files): expand directories matching task tokens in full, summarize others.
        let mut dir_groups: BTreeMap<String, Vec<&str>> = BTreeMap::new();
        for f in self.files.iter() {
            let parts: Vec<&str> = f.split('/').collect();
            let bucket = match parts.len() {
                1 => ".".to_string(),
                2 => parts[0].to_string(),
                3 => format!("{}/{}", parts[0], parts[1]),
                _ => format!("{}/{}/{}", parts[0], parts[1], parts[2]),
            };
            dir_groups.entry(bucket).or_default().push(f.as_str());
        }
        let dir_matches_task = |dir: &str| -> bool {
            split_code_words(dir)
                .iter()
                .any(|w| task_words.contains(w))
        };
        let mut entries: Vec<(String, Vec<&str>, bool)> = dir_groups
            .into_iter()
            .map(|(dir, files)| {
                let matched = dir_matches_task(&dir);
                (dir, files, matched)
            })
            .collect();
        entries.sort_by(|a, b| {
            b.2.cmp(&a.2)
                .then_with(|| {
                    let prio = |d: &str| -> u8 {
                        if d.starts_with("src")
                            || d.starts_with("server")
                            || d.starts_with("local")
                            || d.starts_with("extensions")
                            || d.starts_with("shared")
                            || d.starts_with("packages")
                            || d.starts_with("apps")
                            || d.starts_with("pkg")
                            || d.starts_with("internal")
                            || d.starts_with("cmd")
                            || d.starts_with("lib")
                        {
                            0
                        } else if d.starts_with("scripts") || d.starts_with("docs") || d == "." {
                            1
                        } else {
                            2
                        }
                    };
                    prio(&a.0).cmp(&prio(&b.0))
                })
                .then_with(|| b.1.len().cmp(&a.1.len()))
                .then_with(|| a.0.cmp(&b.0))
        });

        let dir_budget = (MAX_REPO_MAP_CHARS * 2) / 3;
        for (dir, group, matched) in entries.into_iter().take(120) {
            let max_show = if matched { 40 } else { 6 };
            let sample: Vec<&str> = group
                .iter()
                .filter(|f| want_tests || !anchor_noise_path(f))
                .take(max_show)
                .map(|f| f.strip_prefix(&format!("{dir}/")).unwrap_or(f))
                .collect();
            let line = if sample.is_empty() {
                format!("{dir}/ ({} files)\n", group.len())
            } else if group.len() <= sample.len() {
                format!("{dir}/: {}\n", sample.join(", "))
            } else {
                format!(
                    "{dir}/ ({} files, 例: {}, ...)\n",
                    group.len(),
                    sample.join(", ")
                )
            };
            if out.len() + line.len() > dir_budget {
                out.push_str("...（其余目录可用 list 查询）\n");
                break;
            }
            out.push_str(&line);
        }

        if !path_matches.is_empty() {
            out.push_str("\n路径关键词初筛候选（大仓提示，非全集，可用 list/grep 扩查）：\n");
            for f in path_matches.iter().take(40) {
                out.push_str(&format!("- {f}\n"));
            }
        }
        out
    }

    fn inspect_task_commits(&self, task: &str) -> String {
        static SHA_RE: OnceLock<Regex> = OnceLock::new();
        let re = SHA_RE.get_or_init(|| Regex::new(r"\b([0-9a-fA-F]{7,40})\b").unwrap());
        let mut shas = Vec::new();
        for m in re.find_iter(task) {
            let s = m.as_str();
            let has_digit = s.bytes().any(|b| b.is_ascii_digit());
            let has_hex = s.bytes().any(|b| matches!(b, b'a'..=b'f' | b'A'..=b'F'));
            if has_digit && has_hex && !shas.iter().any(|x: &String| x.eq_ignore_ascii_case(s)) {
                shas.push(s.to_string());
                if shas.len() >= 8 {
                    break;
                }
            }
        }
        if shas.is_empty() {
            return String::new();
        }
        let deadline = Instant::now() + Duration::from_millis(1_500);
        // Fast path: fetch all commits in parallel so 4 commits take 1 git RTT (~50ms) instead of 4x.
        let chunks: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = shas
                .iter()
                .map(|sha| {
                    s.spawn(|| {
                        let args = vec![
                            "show".into(),
                            "--stat=100".into(),
                            "--oneline".into(),
                            "--no-color".into(),
                            sha.clone(),
                        ];
                        let bytes = run_command_until_limited(
                            &self.root,
                            "git",
                            &args,
                            Some(deadline),
                            32 * 1024,
                        )?;
                        let text = String::from_utf8_lossy(&bytes);
                        let lines: Vec<&str> =
                            text.lines().filter(|l| !l.trim().is_empty()).collect();
                        if lines.is_empty() {
                            return None;
                        }
                        let mut part = format!("- {}\n", lines[0].trim());
                        for line in lines.iter().skip(1).take(40) {
                            part.push_str(&format!("  {}\n", line.trim()));
                        }
                        if lines.len() > 41 {
                            part.push_str(&format!("  ...(+{} 项)\n", lines.len() - 41));
                        }
                        Some(part)
                    })
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|h| h.join().ok().flatten())
                .collect()
        });
        chunks.concat()
    }

    fn match_paths_for_task(&self, task: &str, keywords: &[String]) -> Vec<String> {
        let mut tokens: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        for src in std::iter::once(task).chain(keywords.iter().map(String::as_str)) {
            for w in split_code_words(src) {
                if w.len() >= 2 && !stop_word(&w) && seen.insert(w.clone()) {
                    tokens.push(w);
                }
            }
        }
        if tokens.is_empty() {
            return Vec::new();
        }
        let want_tests = retrieval_plan(task).tests;
        let mut df = vec![0usize; tokens.len()];
        let mut file_hits: Vec<(usize, Vec<usize>)> = Vec::new();
        for (idx, file) in self.files.iter().enumerate() {
            if !want_tests && (anchor_noise_path(file) || file.ends_with("_test.go")) {
                continue;
            }
            let segs = split_code_words(file);
            let mut matched = Vec::new();
            for (ti, tok) in tokens.iter().enumerate() {
                if segs.iter().any(|s| s == tok) {
                    matched.push(ti);
                    df[ti] += 1;
                }
            }
            if !matched.is_empty() {
                file_hits.push((idx, matched));
            }
        }
        let n = self.files.len().max(1) as f64;
        let mut scored: Vec<(f64, &str)> = file_hits
            .into_iter()
            .map(|(idx, matched)| {
                let score: f64 = matched
                    .into_iter()
                    .map(|ti| (1.0 + n / (df[ti].max(1) as f64)).ln())
                    .sum();
                (score, self.files[idx].as_str())
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        scored
            .into_iter()
            .take(40)
            .map(|(_, f)| f.to_string())
            .collect()
    }

    fn route_entry_files(&self, task: &str) -> Vec<String> {
        static ROUTE_RE: OnceLock<Regex> = OnceLock::new();
        let re = ROUTE_RE
            .get_or_init(|| Regex::new(r"/[A-Za-z0-9_-]+(?:/[A-Za-z0-9_-]+)+").unwrap());
        let mut suffixes = Vec::new();
        for m in re.find_iter(task) {
            let route = m.as_str().to_ascii_lowercase();
            suffixes.push(route.clone());
            if let Some((parent, last)) = route.rsplit_once('/') {
                if let Some(prev) = parent.rsplit('/').next().filter(|p| !p.is_empty()) {
                    suffixes.push(format!("/{prev}/{last}"));
                }
            }
        }
        if suffixes.is_empty() {
            return Vec::new();
        }
        let mut matches = Vec::new();
        for file in self.files.iter() {
            let path = Path::new(file);
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            if !matches!(ext, "tsx" | "jsx" | "ts" | "js" | "vue" | "svelte") {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let target = if matches!(stem, "index" | "page") {
                path.parent().unwrap_or(path).to_path_buf()
            } else {
                path.with_extension("")
            };
            let normalized = format!(
                "/{}",
                target
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_ascii_lowercase()
            );
            if suffixes.iter().any(|suf| normalized.ends_with(suf)) {
                matches.push(file.clone());
                if matches.len() >= 4 {
                    break;
                }
            }
        }
        matches
    }

    fn preflight_exact_anchors(&self, task: &str, keywords: &[String]) -> (String, Vec<Selection>) {
        static TOKEN_RE: OnceLock<Regex> = OnceLock::new();
        let re = TOKEN_RE.get_or_init(|| Regex::new(r"[A-Za-z_.$][A-Za-z0-9_./:-]{2,}").unwrap());
        let mut anchors = Vec::new();
        let mut seen = HashSet::new();
        for kw in keywords {
            let t = kw.trim();
            if t.len() >= 2 && seen.insert(t.to_ascii_lowercase()) {
                anchors.push(t.to_string());
            }
        }
        for m in re.find_iter(task) {
            let raw = m
                .as_str()
                .trim_matches(|c: char| matches!(c, '.' | ':' | '/'));
            if raw.len() < 3 || raw.len() > 80 {
                continue;
            }
            if (7..=40).contains(&raw.len())
                && raw.bytes().all(|b| b.is_ascii_hexdigit())
                && raw.bytes().any(|b| b.is_ascii_digit())
            {
                continue;
            }
            if CODE_FILE_EXTENSIONS.contains(&raw.to_ascii_lowercase().as_str()) {
                continue;
            }
            let has_inner_upper = raw.chars().skip(1).any(|c| c.is_ascii_uppercase());
            let has_code_sep = raw.contains(['_', '-', '/', ':']) || m.as_str().starts_with('.');
            let needle = if m.as_str().starts_with('.') && !raw.contains('.') {
                format!(".{raw}")
            } else {
                raw.to_string()
            };
            if (has_inner_upper || has_code_sep)
                && !stop_word(&raw.to_ascii_lowercase())
                && seen.insert(needle.to_ascii_lowercase())
            {
                anchors.push(needle);
                if anchors.len() >= 8 {
                    break;
                }
            }
        }
        if anchors.is_empty() {
            return (String::new(), Vec::new());
        }
        let deadline = Instant::now() + Duration::from_millis(600);
        let queries: Vec<GrepQuery> = anchors
            .into_iter()
            .map(|q| GrepQuery {
                q,
                path: None,
                regex: false,
                case_sensitive: false,
            })
            .collect();
        let results = self.run_grep_batch(&queries, 10, deadline);
        let mut out = String::new();
        let mut sels = Vec::new();
        let want_tests = retrieval_plan(task).tests;
        for (idx, mut rows) in results.into_iter().enumerate() {
            rows.retain(|r| {
                want_tests || (!anchor_noise_path(&r.file) && !r.file.ends_with("_test.go"))
            });
            if rows.is_empty() {
                continue;
            }
            out.push_str(&format!("[{}]\n", queries[idx].q));
            for row in rows.iter().take(8) {
                out.push_str(&format!(
                    "  {}:{}: {}\n",
                    row.file,
                    row.ln,
                    clip_line(&row.text, 120)
                ));
                if sels.len() < 12 {
                    sels.push(Selection {
                        file: row.file.clone(),
                        start: Some(row.ln.saturating_sub(5).max(1)),
                        end: Some(row.ln + 10),
                        ..Default::default()
                    });
                }
            }
            if out.len() > 4_000 {
                break;
            }
        }
        (out, sels)
    }

    pub(crate) fn execute_batch(&self, req: &BatchRequest, want_tests: bool) -> String {
        let (list_out, grep_out, outline_out, read_out, git_out) = std::thread::scope(|s| {
            let h_list = s.spawn(|| self.exec_list(&req.list));
            let h_grep = s.spawn(|| self.exec_grep(&req.grep, want_tests));
            let h_outline = s.spawn(|| self.exec_outline(&req.outline, want_tests));
            let h_read = s.spawn(|| self.exec_read(&req.read));
            let h_git = s.spawn(|| self.exec_git(&req.git));
            (
                h_list.join().unwrap_or_default(),
                h_grep.join().unwrap_or_default(),
                h_outline.join().unwrap_or_default(),
                h_read.join().unwrap_or_default(),
                h_git.join().unwrap_or_default(),
            )
        });
        let mut sections = Vec::new();
        if !list_out.is_empty() {
            sections.push(format!("## LIST 结果\n{list_out}"));
        }
        if !grep_out.is_empty() {
            sections.push(format!("## GREP 结果\n{grep_out}"));
        }
        if !outline_out.is_empty() {
            sections.push(format!("## OUTLINE 结果\n{outline_out}"));
        }
        if !read_out.is_empty() {
            sections.push(format!("## READ 结果\n{read_out}"));
        }
        if !git_out.is_empty() {
            sections.push(format!("## GIT 结果\n{git_out}"));
        }
        if sections.is_empty() {
            "本批探索无命中结果。".into()
        } else {
            sections.join("\n\n")
        }
    }

    fn exec_list(&self, patterns: &[String]) -> String {
        if patterns.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        for pat in patterns.iter().take(MAX_LIST_PATTERNS) {
            let clean = normalize_rel(pat.trim()).trim_end_matches('/').to_string();
            if clean.is_empty() {
                continue;
            }
            let has_glob = clean.contains(['*', '?']);
            let mut matched: Vec<&String> = if has_glob {
                let with_sub = if clean.contains('/') {
                    clean.clone()
                } else {
                    format!("**/{clean}")
                };
                self.files
                    .iter()
                    .filter(|f| glob_match(&clean, f) || glob_match(&with_sub, f))
                    .collect()
            } else {
                let prefix = format!("{clean}/");
                let mut exact: Vec<&String> = self
                    .files
                    .iter()
                    .filter(|f| *f == &clean || f.starts_with(&prefix))
                    .collect();
                if exact.is_empty() {
                    let low = clean.to_ascii_lowercase();
                    exact = self
                        .files
                        .iter()
                        .filter(|f| f.to_ascii_lowercase().contains(&low))
                        .collect();
                }
                exact
            };
            out.push_str(&format!("[list: {pat}] ({} 项)\n", matched.len()));
            if matched.is_empty() {
                out.push_str("  (无匹配文件)\n");
                continue;
            }
            let total = matched.len();
            matched.truncate(MAX_LIST_RESULTS);
            for f in matched {
                let size_hint = fs::metadata(self.root.join(f))
                    .map(|m| {
                        let kb = m.len() / 1024;
                        if kb == 0 {
                            format!("{}B", m.len())
                        } else {
                            format!("{kb}KB")
                        }
                    })
                    .unwrap_or_default();
                out.push_str(&format!("  {f} ({size_hint})\n"));
            }
            if total > MAX_LIST_RESULTS {
                out.push_str(&format!(
                    "  ...(+{} 个文件，请缩小目录或用更精确 glob)\n",
                    total - MAX_LIST_RESULTS
                ));
            }
        }
        out
    }

    fn scoped_files(&self, scope: Option<&str>) -> Vec<&String> {
        match scope {
            None => self.files.iter().collect(),
            Some(p) if p.contains(['*', '?']) => {
                let with_sub = if p.contains('/') {
                    p.to_string()
                } else {
                    format!("**/{p}")
                };
                self.files
                    .iter()
                    .filter(|f| glob_match(p, f) || glob_match(&with_sub, f))
                    .collect()
            }
            Some(p) => {
                let prefix = format!("{p}/");
                self.files
                    .iter()
                    .filter(|f| f.as_str() == p || f.starts_with(&prefix))
                    .collect()
            }
        }
    }

    /// Execute a batch of grep queries fast:
    /// - Groups same-scope literal queries together (1 single in-process AhoCorasick scan or 1 single `rg` call!)
    /// - Avoids spawning 12 separate `rg.exe` processes on Windows.
    fn run_grep_batch(
        &self,
        queries: &[GrepQuery],
        max_hits: usize,
        deadline: Instant,
    ) -> Vec<Vec<SearchRow>> {
        let mut results: Vec<Vec<SearchRow>> = vec![Vec::new(); queries.len()];
        if queries.is_empty() || Instant::now() >= deadline {
            return results;
        }

        // Partition into (scope, case_sensitive) literal groups vs regex queries.
        let mut literal_groups: BTreeMap<(Option<String>, bool), Vec<usize>> = BTreeMap::new();
        let mut regex_indices: Vec<usize> = Vec::new();
        for (i, gq) in queries.iter().enumerate() {
            if gq.q.trim().is_empty() {
                continue;
            }
            if gq.regex {
                regex_indices.push(i);
            } else {
                let scope = gq
                    .path
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .and_then(|p| self.normalize_input_path(p).ok());
                literal_groups
                    .entry((scope, gq.case_sensitive))
                    .or_default()
                    .push(i);
            }
        }

        let group_list: Vec<((Option<String>, bool), Vec<usize>)> =
            literal_groups.into_iter().collect();

        let (group_outputs, regex_outputs): (
            Vec<Vec<(usize, Vec<SearchRow>)>>,
            Vec<(usize, Vec<SearchRow>)>,
        ) = std::thread::scope(|s| {
            let g_handles: Vec<_> = group_list
                .iter()
                .map(|((scope, case_sensitive), idxs)| {
                    s.spawn(|| {
                        self.run_literal_group(
                            scope.as_deref(),
                            *case_sensitive,
                            idxs,
                            queries,
                            max_hits,
                            deadline,
                        )
                    })
                })
                .collect();
            let r_handles: Vec<_> = regex_indices
                .iter()
                .map(|&idx| {
                    s.spawn(move || (idx, self.run_grep_one(&queries[idx], max_hits, deadline)))
                })
                .collect();
            (
                g_handles
                    .into_iter()
                    .filter_map(|h| h.join().ok())
                    .collect(),
                r_handles
                    .into_iter()
                    .filter_map(|h| h.join().ok())
                    .collect(),
            )
        });

        for batch in group_outputs {
            for (idx, rows) in batch {
                results[idx] = rows;
            }
        }
        for (idx, rows) in regex_outputs {
            results[idx] = rows;
        }
        results
    }

    fn run_literal_group(
        &self,
        scope: Option<&str>,
        case_sensitive: bool,
        idxs: &[usize],
        queries: &[GrepQuery],
        max_hits: usize,
        deadline: Instant,
    ) -> Vec<(usize, Vec<SearchRow>)> {
        let patterns: Vec<String> = idxs.iter().map(|&i| queries[i].q.trim().to_string()).collect();
        let lowered: Vec<String> = patterns.iter().map(|p| p.to_ascii_lowercase()).collect();
        let target_files = self.scoped_files(scope);

        // Fast path 1: In-process parallel AhoCorasick scan when file set is <= IN_PROCESS_GREP_FILE_LIMIT!
        // Avoids Windows process spawn + Defender overhead completely (~10-30ms for 12 queries!).
        if target_files.len() <= IN_PROCESS_GREP_FILE_LIMIT || !rg_available(&self.root) {
            let ac = aho_corasick::AhoCorasick::builder()
                .ascii_case_insensitive(!case_sensitive)
                .build(&patterns)
                .ok();
            let mut per_q: Vec<Vec<SearchRow>> = vec![Vec::new(); idxs.len()];
            let workers = std::thread::available_parallelism()
                .map(|n| n.get().clamp(2, 8))
                .unwrap_or(4);
            let chunk_size = (target_files.len() / workers).max(16);
            let partials: Vec<Vec<Vec<SearchRow>>> = std::thread::scope(|s| {
                let handles: Vec<_> = target_files
                    .chunks(chunk_size)
                    .map(|chunk| {
                        let ac = &ac;
                        let patterns = &patterns;
                        let lowered = &lowered;
                        s.spawn(move || {
                            let mut local: Vec<Vec<SearchRow>> = vec![Vec::new(); patterns.len()];
                            for file in chunk {
                                if Instant::now() >= deadline {
                                    break;
                                }
                                let Ok(lines) = self.cached_lines(file) else { continue };
                                let mut counts = vec![0usize; patterns.len()];
                                for (li, line) in lines.iter().enumerate() {
                                    let quick_hit = if let Some(m) = ac {
                                        if !case_sensitive && !line.is_ascii() {
                                            m.is_match(line.to_lowercase().as_bytes())
                                        } else {
                                            m.is_match(line.as_bytes())
                                        }
                                    } else {
                                        true
                                    };
                                    if !quick_hit {
                                        continue;
                                    }
                                    let line_low = (!case_sensitive).then(|| line.to_lowercase());
                                    for qi in 0..patterns.len() {
                                        if counts[qi] >= 12 || local[qi].len() >= max_hits * 3 {
                                            continue;
                                        }
                                        let matched = if case_sensitive {
                                            line.contains(&patterns[qi])
                                        } else {
                                            line_low
                                                .as_deref()
                                                .unwrap_or("")
                                                .contains(&lowered[qi])
                                        };
                                        if matched {
                                            counts[qi] += 1;
                                            local[qi].push(SearchRow {
                                                file: (*file).clone(),
                                                ln: li + 1,
                                                text: line.clone(),
                                            });
                                        }
                                    }
                                }
                            }
                            local
                        })
                    })
                    .collect();
                handles.into_iter().filter_map(|h| h.join().ok()).collect()
            });
            for part in partials {
                for (qi, rows) in part.into_iter().enumerate() {
                    per_q[qi].extend(rows);
                }
            }
            return idxs
                .iter()
                .copied()
                .zip(per_q.into_iter().map(|mut rows| {
                    rows.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.ln.cmp(&b.ln)));
                    rows.dedup_by(|a, b| a.file == b.file && a.ln == b.ln);
                    rows
                }))
                .collect();
        }

        // Fast path 2 (large repos > 650 files): ONE combined `rg` invocation for all queries in this group!
        let combined_rows = self.run_rg_multi(&patterns, scope, false, case_sensitive, deadline);
        let mut per_q: Vec<Vec<SearchRow>> = vec![Vec::new(); patterns.len()];
        for row in combined_rows {
            let line_low = (!case_sensitive).then(|| row.text.to_lowercase());
            for qi in 0..patterns.len() {
                if per_q[qi].len() >= max_hits * 4 {
                    continue;
                }
                let matched = if case_sensitive {
                    row.text.contains(&patterns[qi])
                } else {
                    line_low
                        .as_deref()
                        .unwrap_or("")
                        .contains(&lowered[qi])
                };
                if matched {
                    per_q[qi].push(row.clone());
                }
            }
        }
        idxs.iter()
            .copied()
            .zip(per_q.into_iter().map(|mut rows| {
                rows.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.ln.cmp(&b.ln)));
                rows.dedup_by(|a, b| a.file == b.file && a.ln == b.ln);
                rows
            }))
            .collect()
    }

    fn run_rg_multi(
        &self,
        patterns: &[String],
        scope: Option<&str>,
        regex: bool,
        case_sensitive: bool,
        deadline: Instant,
    ) -> Vec<SearchRow> {
        let run_in = |repo: &Path, path_arg: Option<&str>, prefix: &str| -> Vec<SearchRow> {
            let mut args = vec![
                "-n".into(),
                "--with-filename".into(),
                "--no-heading".into(),
                "--color".into(),
                "never".into(),
                "--max-count".into(),
                "25".into(),
                "--max-filesize".into(),
                "2M".into(),
                "--max-columns".into(),
                "240".into(),
            ];
            if !regex {
                args.push("-F".into());
            }
            if !case_sensitive {
                args.push("-i".into());
            }
            for glob in [
                "!**/node_modules/**",
                "!**/dist/**",
                "!**/target/**",
                "!**/coverage/**",
                "!**/package-lock.json",
                "!**/Cargo.lock",
                "!**/pnpm-lock.yaml",
                "!**/yarn.lock",
                "!**/go.sum",
                "!*.min.js",
                "!*.min.css",
                "!*.map",
                "!*.png",
                "!*.jpg",
                "!*.jpeg",
                "!*.gif",
                "!*.webp",
                "!*.ico",
                "!*.woff",
                "!*.woff2",
                "!*.ttf",
                "!*.bin",
                "!*.syso",
            ] {
                args.push("--glob".into());
                args.push(glob.into());
            }
            let mut target_path = ".".to_string();
            if let Some(p) = path_arg {
                if p.contains(['*', '?']) {
                    args.push("--glob".into());
                    args.push(p.to_string());
                } else if repo.join(p).exists() {
                    target_path = p.to_string();
                } else {
                    return Vec::new();
                }
            }
            for pat in patterns {
                args.push("-e".into());
                args.push(pat.clone());
            }
            args.push(target_path);
            let Some((stdout, _)) =
                run_command_partial(repo, "rg", &args, Some(deadline), 1024 * 1024)
            else {
                return Vec::new();
            };
            let mut parsed = parse_search_rows(stdout);
            if !prefix.is_empty() {
                for row in &mut parsed {
                    row.file.insert_str(0, prefix);
                }
            }
            parsed
        };

        let mut rows = Vec::new();
        if let Some((inner_repo, prefix)) = &self.nested {
            let inner_path = match scope {
                None => Some(None),
                Some(p) if p.starts_with(prefix) => {
                    Some(Some(p.strip_prefix(prefix).unwrap_or("")))
                }
                Some(p) if prefix.starts_with(&format!("{p}/")) => Some(None),
                Some(p) if p.contains(['*', '?']) => Some(Some(p)),
                Some(_) => None,
            };
            if let Some(sub) = inner_path {
                rows.extend(run_in(inner_repo, sub.filter(|s| !s.is_empty()), prefix));
            }
        }
        rows.extend(run_in(&self.root, scope, ""));
        rows.retain(|r| self.allowed.contains(&r.file) || is_project_text_file(&r.file));
        rows
    }

    fn run_grep_one(&self, q: &GrepQuery, max_hits: usize, deadline: Instant) -> Vec<SearchRow> {
        let pattern = q.q.trim();
        if pattern.is_empty() || Instant::now() >= deadline {
            return Vec::new();
        }
        let scope_rel = q
            .path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|p| self.normalize_input_path(p).ok());
        let target_files = self.scoped_files(scope_rel.as_deref());
        let mut rows = if target_files.len() <= IN_PROCESS_GREP_FILE_LIMIT || !rg_available(&self.root) {
            let flag = if q.case_sensitive { "" } else { "(?i)" };
            let Ok(re) = Regex::new(&format!("{flag}{pattern}")) else {
                return Vec::new();
            };
            let mut found = Vec::new();
            for file in target_files {
                if Instant::now() >= deadline || found.len() >= max_hits * 4 {
                    break;
                }
                let Ok(lines) = self.cached_lines(file) else { continue };
                let mut per_file = 0usize;
                for (i, line) in lines.iter().enumerate() {
                    if re.is_match(line) {
                        found.push(SearchRow {
                            file: (*file).clone(),
                            ln: i + 1,
                            text: line.clone(),
                        });
                        per_file += 1;
                        if per_file >= 12 {
                            break;
                        }
                    }
                }
            }
            found
        } else {
            self.run_rg_multi(
                &[pattern.to_string()],
                scope_rel.as_deref(),
                true,
                q.case_sensitive,
                deadline,
            )
        };
        rows.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.ln.cmp(&b.ln)));
        rows.dedup_by(|a, b| a.file == b.file && a.ln == b.ln);
        rows
    }

    fn exec_grep(&self, queries: &[GrepQuery], want_tests: bool) -> String {
        if queries.is_empty() {
            return String::new();
        }
        let deadline = Instant::now() + Duration::from_millis(2_500);
        let limited: Vec<GrepQuery> = queries.iter().take(MAX_GREP_QUERIES).cloned().collect();
        let results = self.run_grep_batch(&limited, MAX_GREP_HITS_PER_QUERY, deadline);

        let mut out = String::new();
        for (gq, mut rows) in limited.iter().zip(results.into_iter()) {
            if !want_tests {
                rows.sort_by(|a, b| {
                    let na = anchor_noise_path(&a.file) || a.file.ends_with("_test.go");
                    let nb = anchor_noise_path(&b.file) || b.file.ends_with("_test.go");
                    na.cmp(&nb)
                        .then_with(|| a.file.cmp(&b.file))
                        .then_with(|| a.ln.cmp(&b.ln))
                });
            }
            let scope_label = gq
                .path
                .as_deref()
                .map(|p| format!(" in {p}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "[grep: {:?}{scope_label}] ({} 命中)\n",
                gq.q,
                rows.len()
            ));
            if rows.is_empty() {
                out.push_str("  (无命中)\n");
                continue;
            }
            let shown = rows.len().min(MAX_GREP_HITS_PER_QUERY);
            for row in &rows[..shown] {
                let line = format!(
                    "  {}:{}: {}\n",
                    row.file,
                    row.ln,
                    clip_line(&row.text, 130)
                );
                if out.len() + line.len() > MAX_GREP_TOTAL_CHARS {
                    out.push_str("  ...（本批 grep 达到展示上限）\n");
                    return out;
                }
                out.push_str(&line);
            }
            if rows.len() > shown {
                out.push_str(&format!("  ...(+{} 条命中)\n", rows.len() - shown));
            }
        }
        out
    }

    fn exec_outline(&self, files: &[String], want_tests: bool) -> String {
        if files.is_empty() {
            return String::new();
        }
        let mut unique = Vec::new();
        for f in files {
            if let Ok(rel) = self.normalize_input_path(f) {
                if !unique.contains(&rel) {
                    unique.push(rel);
                    if unique.len() >= MAX_OUTLINE_FILES {
                        break;
                    }
                }
            }
        }
        self.outline_files(&unique, MAX_OUTLINE_TOTAL_CHARS, want_tests)
    }

    fn outline_files(&self, files: &[String], budget: usize, want_tests: bool) -> String {
        if files.is_empty() {
            return String::new();
        }
        let rendered: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = files
                .iter()
                .map(|f| s.spawn(|| self.outline_one(f, want_tests)))
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        let mut out = String::new();
        for chunk in rendered {
            if out.len() + chunk.len() > budget && !out.is_empty() {
                out.push_str("\n...（本批 outline 达到展示上限）\n");
                break;
            }
            out.push_str(&chunk);
            out.push('\n');
        }
        out
    }

    fn outline_one(&self, rel: &str, want_tests: bool) -> String {
        let lines = match self.cached_lines(rel) {
            Ok(lines) => lines,
            Err(e) => return format!("OUTLINE {rel}: 错误（{e}）\n"),
        };
        let total = lines.len();
        let mut out = format!("OUTLINE {rel} ({total}L):\n");
        if total == 0 {
            out.push_str("  (空文件)\n");
            return out;
        }
        if total <= 45 {
            for (i, line) in lines.iter().enumerate() {
                out.push_str(&format!("  {}| {}\n", i + 1, clip_line(line, 140)));
            }
            return out;
        }

        let lower = rel.to_ascii_lowercase();
        let ext = Path::new(&lower)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        if matches!(ext, "json" | "jsonc") {
            return self.outline_json(rel, &lines);
        }
        if ext == "md" {
            return self.outline_markdown(rel, &lines);
        }
        if matches!(ext, "css" | "scss" | "sass" | "less") {
            return self.outline_css(rel, &lines);
        }
        if matches!(ext, "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf") {
            return self.outline_config(rel, &lines);
        }

        let joined = lines.join("\n");
        let entry = scan_source(&joined, rel);

        let local_imports: Vec<String> = entry
            .imports
            .iter()
            .filter(|im| {
                im.from.starts_with('.')
                    || im.from.starts_with("crate::")
                    || im.from.starts_with("super::")
                    || im.from.contains('/')
            })
            .take(8)
            .map(|im| format!("{}<-{}", im.name, im.from))
            .collect();
        if !local_imports.is_empty() {
            out.push_str(&format!("  imports: {}\n", local_imports.join(", ")));
        }

        if entry.syms.is_empty() {
            for (i, line) in lines.iter().take(25).enumerate() {
                out.push_str(&format!("  {}| {}\n", i + 1, clip_line(line, 120)));
            }
            if total > 25 {
                out.push_str(&format!("  ...（共 {total} 行，可用 read 按行段查看）\n"));
            }
            return out;
        }

        // Detect Rust `#[cfg(test)] mod tests` ranges to avoid drowning production symbols in unit tests.
        let test_ranges: Vec<(usize, usize)> = if !want_tests && rel.ends_with(".rs") {
            entry
                .syms
                .iter()
                .filter(|s| {
                    s.kind == "mod"
                        && (s.name.contains("test")
                            || lines[s.ln.saturating_sub(4)..s.ln.min(total)]
                                .iter()
                                .any(|l| l.contains("#[cfg(test)]")))
                })
                .map(|s| (s.ln, s.end))
                .collect()
        } else {
            Vec::new()
        };

        let mut shown = 0usize;
        for sym in &entry.syms {
            if sym.ln == 0 || sym.end < sym.ln || sym.end > total {
                continue;
            }
            if test_ranges
                .iter()
                .any(|&(a, b)| sym.ln >= a && sym.end <= b)
            {
                continue;
            }
            if !want_tests
                && rel.ends_with(".rs")
                && lines[sym.ln.saturating_sub(4)..sym.ln.min(total)]
                    .iter()
                    .any(|l| l.contains("#[test]") || l.contains("::test]"))
            {
                continue;
            }
            let parents: Vec<&Symbol> = entry
                .syms
                .iter()
                .filter(|p| p.ln < sym.ln && p.end >= sym.end)
                .collect();
            if sym.kind == "prop"
                || parents
                    .iter()
                    .any(|p| matches!(p.kind.as_str(), "fn" | "method"))
            {
                continue;
            }
            let has_children = entry.syms.iter().any(|c| {
                c.ln > sym.ln
                    && c.end <= sym.end
                    && matches!(c.kind.as_str(), "fn" | "method")
            });
            let span = sym.end - sym.ln + 1;
            let is_container = matches!(sym.kind.as_str(), "class" | "impl" | "mod" | "trait")
                && (has_children || span > 60);
            let owner = parents
                .iter()
                .rev()
                .find(|p| matches!(p.kind.as_str(), "class" | "impl" | "trait" | "mod"))
                .map(|p| format!("{}::", p.name))
                .or_else(|| {
                    rel.ends_with(".go")
                        .then(|| go::receiver(&sym.sig).map(|(_, ty)| format!("{ty}::")))
                        .flatten()
                })
                .unwrap_or_default();
            let indent = if owner.is_empty() { "  " } else { "    " };
            let sig = clip_line(&sym.sig, 88);
            if is_container {
                out.push_str(&format!(
                    "{indent}L{}-{} {} {owner}{} ({span}L, 容器) | {sig}\n",
                    sym.ln, sym.end, sym.kind, sym.name
                ));
            } else {
                out.push_str(&format!(
                    "{indent}L{}-{} {} {owner}{} ({span}L) | {sig}\n",
                    sym.ln, sym.end, sym.kind, sym.name
                ));
            }
            shown += 1;
            if shown >= 90 {
                out.push_str(&format!(
                    "  ...（文件含更多符号；可用 grep/read 精确定位）\n"
                ));
                break;
            }
        }
        out
    }

    fn outline_json(&self, rel: &str, lines: &[String]) -> String {
        static KEY_RE: OnceLock<Regex> = OnceLock::new();
        let re =
            KEY_RE.get_or_init(|| Regex::new(r#"^(\s{0,4})"([^"]{1,60})"\s*:\s*(.*)$"#).unwrap());
        let mut out = format!("OUTLINE {rel} ({}L, JSON):\n", lines.len());
        let mut hits = 0usize;
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = re.captures(line) {
                let indent = cap[1].len();
                let key = &cap[2];
                let val = clip_line(&cap[3], 60);
                if indent <= 2 || matches!(key, "name" | "id" | "title" | "command" | "operation") {
                    out.push_str(&format!("  L{} {}: {val}\n", i + 1, key));
                    hits += 1;
                    if hits >= 60 {
                        out.push_str("  ...（其余项可用 read/grep 查看）\n");
                        break;
                    }
                }
            }
        }
        if hits == 0 {
            for (i, line) in lines.iter().take(25).enumerate() {
                out.push_str(&format!("  {}| {}\n", i + 1, clip_line(line, 120)));
            }
        }
        out
    }

    fn outline_markdown(&self, rel: &str, lines: &[String]) -> String {
        let mut out = format!("OUTLINE {rel} ({}L, Markdown):\n", lines.len());
        let mut hits = 0usize;
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') {
                out.push_str(&format!("  L{} {}\n", i + 1, clip_line(trimmed, 100)));
                hits += 1;
                if hits >= 50 {
                    break;
                }
            }
        }
        if hits == 0 {
            for (i, line) in lines.iter().take(20).enumerate() {
                out.push_str(&format!("  {}| {}\n", i + 1, clip_line(line, 120)));
            }
        }
        out
    }

    fn outline_css(&self, rel: &str, lines: &[String]) -> String {
        let mut out = format!("OUTLINE {rel} ({}L, Stylesheet):\n", lines.len());
        let mut hits = 0usize;
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            if t.starts_with('@') || (t.ends_with('{') && !t.starts_with("/*")) {
                out.push_str(&format!("  L{} {}\n", i + 1, clip_line(t, 100)));
                hits += 1;
                if hits >= 60 {
                    out.push_str("  ...（可用 grep 查具体选择器）\n");
                    break;
                }
            }
        }
        out
    }

    fn outline_config(&self, rel: &str, lines: &[String]) -> String {
        let mut out = format!("OUTLINE {rel} ({}L, Config):\n", lines.len());
        let mut hits = 0usize;
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            if !line.starts_with([' ', '\t']) || t.starts_with('[') {
                out.push_str(&format!("  L{} {}\n", i + 1, clip_line(t, 100)));
                hits += 1;
                if hits >= 60 {
                    break;
                }
            }
        }
        out
    }

    fn exec_read(&self, slices: &[ReadSlice]) -> String {
        if slices.is_empty() {
            return String::new();
        }
        let limited: Vec<&ReadSlice> = slices.iter().take(MAX_READ_SLICES).collect();
        let blocks: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = limited
                .iter()
                .map(|sl| s.spawn(|| self.read_one(sl)))
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        let mut out = String::new();
        for b in blocks {
            if out.len() + b.len() > MAX_READ_TOTAL_CHARS && !out.is_empty() {
                out.push_str("\n...（本批 read 达到展示上限）\n");
                break;
            }
            out.push_str(&b);
            out.push('\n');
        }
        out
    }

    fn read_one(&self, slice: &ReadSlice) -> String {
        let rel = match self.normalize_input_path(&slice.file) {
            Ok(r) => r,
            Err(e) => return format!("READ {}: 错误（{e}）\n", slice.file),
        };
        let lines = match self.cached_lines(&rel) {
            Ok(l) => l,
            Err(e) => return format!("READ {rel}: 错误（{e}）\n"),
        };
        if lines.is_empty() {
            return format!("READ {rel}: (空文件)\n");
        }
        let start = slice.start.unwrap_or(1).max(1).min(lines.len());
        let default_end = (start + 99).min(lines.len());
        let end = slice
            .end
            .unwrap_or(default_end)
            .max(start)
            .min((start + MAX_READ_LINES - 1).min(lines.len()));
        let mut out = format!("READ {rel}:{start}-{end} (共 {}L):\n", lines.len());
        for (idx, line) in lines[start - 1..end].iter().enumerate() {
            out.push_str(&format!("{}| {}\n", start + idx, clip_line(line, 240)));
        }
        out
    }

    fn exec_git(&self, queries: &[GitQuery]) -> String {
        if queries.is_empty() {
            return String::new();
        }
        let deadline = Instant::now() + Duration::from_millis(2_000);
        let limited: Vec<&GitQuery> = queries.iter().take(MAX_GIT_QUERIES).collect();
        let blocks: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = limited
                .iter()
                .map(|gq| s.spawn(|| self.git_one(gq, deadline)))
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        blocks.join("\n")
    }

    fn git_one(&self, gq: &GitQuery, deadline: Instant) -> String {
        let rev = gq.rev.trim();
        if rev.is_empty()
            || rev.starts_with('-')
            || rev.contains(char::is_whitespace)
            || rev.len() > 128
        {
            return format!("GIT {:?}: 无效引用\n", gq.rev);
        }
        let mut args = vec!["show".into(), "--no-color".into()];
        if gq.diff || gq.path.is_some() {
            args.push("-p".into());
            args.push("--unified=2".into());
            args.push("--stat=80".into());
        } else {
            args.push("--stat=100".into());
            args.push("--oneline".into());
        }
        args.push(rev.to_string());
        if let Some(path) = gq
            .path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if let Ok(rel) = self.normalize_input_path(path) {
                args.push("--".into());
                args.push(rel);
            }
        }
        let Some((bytes, _)) =
            run_command_partial(&self.root, "git", &args, Some(deadline), 128 * 1024)
        else {
            return format!("GIT {rev}: 查询失败或超时\n");
        };
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        if lines.is_empty() {
            return format!("GIT {rev}: 无输出\n");
        }
        let mut out = format!("GIT {rev}:\n");
        for line in lines.iter().take(MAX_GIT_LINES) {
            out.push_str(&format!("  {}\n", clip_line(line, 160)));
        }
        if lines.len() > MAX_GIT_LINES {
            out.push_str(&format!("  ...(+{} 行已省略)\n", lines.len() - MAX_GIT_LINES));
        }
        out
    }

    fn find_go_module_for(&self, rel: &str) -> (PathBuf, String) {
        let mut dir = Path::new(rel).parent();
        while let Some(d) = dir {
            let candidate = if d.as_os_str().is_empty() {
                self.root.join("go.mod")
            } else {
                self.root.join(d).join("go.mod")
            };
            if let Ok(canon) = dunce::canonicalize(&candidate) {
                if canon.starts_with(&self.root) {
                    if let Ok(text) = fs::read_to_string(&canon) {
                        if let Some(m) = text.lines().find_map(|l| {
                            l.trim()
                                .strip_prefix("module ")
                                .map(|m| m.trim().trim_matches('"').to_string())
                        }) {
                            return (d.to_path_buf(), m);
                        }
                    }
                }
            }
            if d.as_os_str().is_empty() {
                break;
            }
            dir = d.parent();
        }
        (PathBuf::new(), String::new())
    }

    /// Assemble verified verbatim source from disk with range merging, compact 1-hop deps, and budget safety.
    pub(crate) fn assemble(
        &self,
        selections: &[Selection],
        extra_notes: &[String],
        auto_deps: bool,
    ) -> Result<Assembled, String> {
        if selections.is_empty() {
            return Err("Polaris 未确定目标源码范围".into());
        }

        let mut file_order: Vec<String> = Vec::new();
        let mut by_file: HashMap<String, Vec<&Selection>> = HashMap::new();
        let mut notes: Vec<String> = self.notes.clone();
        notes.extend(extra_notes.iter().cloned());

        for sel in selections {
            match self.normalize_input_path(&sel.file) {
                Ok(rel) => {
                    if !by_file.contains_key(&rel) {
                        file_order.push(rel.clone());
                    }
                    by_file.entry(rel).or_default().push(sel);
                }
                Err(e) => {
                    notes.push(format!("已忽略无效路径选择（{e}）。"));
                }
            }
        }
        if file_order.is_empty() {
            return Err("Polaris 所有候选路径均无效或越出检索目录".into());
        }

        let loaded_vec: Vec<(String, Result<Arc<Vec<String>>, String>)> = std::thread::scope(|s| {
            let handles: Vec<_> = file_order
                .iter()
                .map(|f| s.spawn(|| (f.clone(), self.cached_lines(f))))
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        let mut file_lines: HashMap<String, Arc<Vec<String>>> = HashMap::new();
        let mut file_entries: HashMap<String, FileEntry> = HashMap::new();
        for (rel, res) in loaded_vec {
            match res {
                Ok(lines) => {
                    if !lines.is_empty() {
                        file_lines.insert(rel, lines);
                    }
                }
                Err(e) => notes.push(format!("跳过不可读文件 {rel}：{e}")),
            }
        }
        if file_lines.is_empty() {
            return Err(format!(
                "Polaris 未能读取任何有效源码文件。{}",
                notes.join(" ")
            ));
        }

        let mut primary_ranges: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        let mut symbol_seeds: Vec<(String, Symbol)> = Vec::new();

        for rel in &file_order {
            let Some(lines) = file_lines.get(rel) else { continue };
            let total = lines.len();
            let Some(sels) = by_file.get(rel) else { continue };
            let needs_scan = sels.iter().any(|s| !s.symbols.is_empty());
            if needs_scan && !file_entries.contains_key(rel) {
                file_entries.insert(rel.clone(), scan_source(&lines.join("\n"), rel));
            }
            for sel in sels {
                let mut added_any = false;
                if let Some(start) = sel.start {
                    let s = start.max(1).min(total);
                    let e = sel.end.unwrap_or(s).max(s).min(total);
                    primary_ranges.entry(rel.clone()).or_default().push((s, e));
                    added_any = true;
                }
                if let Some(spec) = &sel.lines {
                    for (s, e) in parse_line_ranges(spec) {
                        let s = s.max(1).min(total);
                        let e = e.max(s).min(total);
                        primary_ranges.entry(rel.clone()).or_default().push((s, e));
                        added_any = true;
                    }
                }
                if !sel.symbols.is_empty() {
                    let entry = &file_entries[rel];
                    for sym_query in &sel.symbols {
                        let q = sym_query.trim();
                        if q.is_empty() {
                            continue;
                        }
                        let matched_syms = find_matching_symbols(entry, q);
                        if !matched_syms.is_empty() {
                            for sym in matched_syms {
                                let has_children = entry.syms.iter().any(|c| {
                                    c.ln > sym.ln
                                        && c.end <= sym.end
                                        && matches!(c.kind.as_str(), "fn" | "method")
                                });
                                let span = sym.end.saturating_sub(sym.ln) + 1;
                                let start = expand_leading_comments(lines, sym.ln);
                                if matches!(sym.kind.as_str(), "class" | "impl" | "mod")
                                    && has_children
                                    && span > 200
                                {
                                    let first_method = entry
                                        .syms
                                        .iter()
                                        .filter(|c| {
                                            c.ln > sym.ln
                                                && c.end <= sym.end
                                                && matches!(c.kind.as_str(), "fn" | "method")
                                        })
                                        .map(|c| c.ln)
                                        .min()
                                        .unwrap_or(sym.ln + 40);
                                    let header_end = first_method
                                        .saturating_sub(1)
                                        .max(sym.ln)
                                        .min(start + 59)
                                        .min(total);
                                    primary_ranges
                                        .entry(rel.clone())
                                        .or_default()
                                        .push((start, header_end));
                                    notes.push(format!(
                                        "{rel}:{} ({span}L 容器) 仅展开头部与字段定义，避免整类膨胀。",
                                        sym.name
                                    ));
                                } else {
                                    primary_ranges
                                        .entry(rel.clone())
                                        .or_default()
                                        .push((start, sym.end.min(total)));
                                    symbol_seeds.push((rel.clone(), sym.clone()));
                                }
                                added_any = true;
                            }
                        } else {
                            let mut windows = 0usize;
                            for (i, line) in lines.iter().enumerate() {
                                if line.contains(q) {
                                    let ln = i + 1;
                                    primary_ranges.entry(rel.clone()).or_default().push((
                                        ln.saturating_sub(4).max(1),
                                        (ln + 8).min(total),
                                    ));
                                    added_any = true;
                                    windows += 1;
                                    if windows >= 3 {
                                        break;
                                    }
                                }
                            }
                            if windows == 0 {
                                notes.push(format!("未在 {rel} 中找到符号 `{q}`。"));
                            }
                        }
                    }
                }
                if sel.full || !added_any {
                    if total <= MAX_FULL_SELECTION_LINES {
                        primary_ranges
                            .entry(rel.clone())
                            .or_default()
                            .push((1, total));
                    } else {
                        primary_ranges
                            .entry(rel.clone())
                            .or_default()
                            .push((1, OVERSIZED_FULL_PREVIEW_LINES));
                        notes.push(format!(
                            "{rel} 共 {total} 行，整文件选择已截取前 {OVERSIZED_FULL_PREVIEW_LINES} 行；如需后续行段请按范围 read。"
                        ));
                    }
                }
            }
        }

        let mut dep_ranges: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        if auto_deps && !symbol_seeds.is_empty() {
            self.collect_compact_deps(
                &symbol_seeds,
                &mut file_lines,
                &mut file_entries,
                &mut dep_ranges,
            );
        }

        let mut dep_files: Vec<String> = dep_ranges
            .keys()
            .filter(|f| !file_order.contains(f))
            .cloned()
            .collect();
        dep_files.sort();

        let mut included_by_file: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
        let mut omitted: Vec<(String, usize, usize)> = Vec::new();
        let mut used_bytes = 0usize;

        for rel in &file_order {
            let Some(lines) = file_lines.get(rel) else { continue };
            let Some(ranges) = primary_ranges.get(rel) else { continue };
            for (s, e) in merge_ranges(ranges.clone()) {
                let block_bytes: usize =
                    lines[s - 1..e].iter().map(|l| l.len() + 1).sum::<usize>() + rel.len() + 32;
                if used_bytes + block_bytes > MAX_ASSEMBLED_BYTES && used_bytes > 0 {
                    omitted.push((rel.clone(), s, e));
                } else {
                    used_bytes += block_bytes;
                    included_by_file.entry(rel.clone()).or_default().push((s, e));
                }
            }
        }

        let mut dep_bytes_used = 0usize;
        for rel in file_order.iter().chain(dep_files.iter()) {
            let Some(lines) = file_lines.get(rel) else { continue };
            let Some(ranges) = dep_ranges.get(rel) else { continue };
            for (s, e) in merge_ranges(ranges.clone()) {
                if included_by_file
                    .get(rel)
                    .is_some_and(|inc| inc.iter().any(|&(a, b)| s >= a && e <= b))
                {
                    continue;
                }
                let block_bytes: usize =
                    lines[s - 1..e].iter().map(|l| l.len() + 1).sum::<usize>() + rel.len() + 32;
                if dep_bytes_used + block_bytes > MAX_AUTO_DEPS_BYTES
                    || used_bytes + block_bytes > MAX_ASSEMBLED_BYTES
                {
                    continue;
                }
                dep_bytes_used += block_bytes;
                used_bytes += block_bytes;
                included_by_file.entry(rel.clone()).or_default().push((s, e));
            }
        }

        let mut final_files: Vec<(String, Vec<(usize, usize)>)> = Vec::new();
        for rel in file_order.iter().chain(dep_files.iter()) {
            if let Some(ranges) = included_by_file.remove(rel) {
                let merged = if let Some(lines) = file_lines.get(rel) {
                    merge_ranges_bridging_blanks(ranges, lines)
                } else {
                    merge_ranges(ranges)
                };
                final_files.push((rel.clone(), merged));
            }
        }

        if final_files.is_empty() {
            return Err("Polaris 选定范围为空".into());
        }

        let mut text = format!(
            "检索根目录：{}\n## 相关源码（原文核验，覆盖未验证）\n返回已定位的文件原文、符号定义及紧凑一跳依赖；重叠行段已合并。仅保证所列行段原文，不保证全仓无遗漏，未运行测试。\n",
            self.root.display()
        );
        text.push_str(&format!("\n文件清单（{} 个文件）：\n", final_files.len()));
        for (rel, ranges) in &final_files {
            let spec = ranges
                .iter()
                .map(|(s, e)| format!("{s}-{e}"))
                .collect::<Vec<_>>()
                .join(", ");
            text.push_str(&format!("- {rel}: {spec}\n"));
        }
        if !omitted.is_empty() {
            text.push_str("\n超出输出预算未展开（可直接按行段 read）：\n");
            for (rel, s, e) in &omitted {
                text.push_str(&format!("- {rel}:{s}-{e} ({}行)\n", e - s + 1));
            }
        }
        if !notes.is_empty() {
            notes.dedup();
            text.push('\n');
            for n in &notes {
                text.push_str(n);
                text.push('\n');
            }
        }

        let mut block_count = 0usize;
        for (rel, ranges) in &final_files {
            let Some(lines) = file_lines.get(rel) else { continue };
            let abs = self.root.join(rel);
            for &(s, e) in ranges {
                let body = lines[s - 1..e].join("\n");
                text.push_str(&format!("\n## {}:{s}-{e}\n{body}\n", abs.display()));
                block_count += 1;
            }
        }

        Ok(Assembled {
            text,
            file_count: final_files.len(),
            block_count,
        })
    }

    fn collect_compact_deps(
        &self,
        seeds: &[(String, Symbol)],
        file_lines: &mut HashMap<String, Arc<Vec<String>>>,
        file_entries: &mut HashMap<String, FileEntry>,
        dep_ranges: &mut HashMap<String, Vec<(usize, usize)>>,
    ) {
        static IDENT_RE: OnceLock<Regex> = OnceLock::new();
        let ident_re = IDENT_RE.get_or_init(|| Regex::new(r"\b[A-Za-z_][A-Za-z0-9_]*\b").unwrap());

        // Pre-extract referenced identifiers from seed bodies so we only scan extra files that matter.
        let mut seed_codes: Vec<(String, Symbol, String)> = Vec::with_capacity(seeds.len());
        let mut wanted_idents: HashSet<String> = HashSet::new();
        for (rel, sym) in seeds {
            let Some(lines) = file_lines.get(rel) else { continue };
            if sym.ln == 0 || sym.end > lines.len() || sym.end < sym.ln {
                continue;
            }
            let slice = &lines[sym.ln - 1..sym.end];
            let code = if rel.ends_with(".go") {
                go::code(&slice.join("\n")).join("\n")
            } else {
                stripped_depth(slice, rel.ends_with(".rs")).0.join("\n")
            };
            for m in ident_re.find_iter(&code) {
                let name = m.as_str();
                if name != sym.name && name.len() >= 3 && !stop_word(&name.to_ascii_lowercase()) {
                    wanted_idents.insert(name.to_string());
                }
            }
            seed_codes.push((rel.clone(), sym.clone(), code));
        }
        if wanted_idents.is_empty() {
            return;
        }

        // Only scan directly imported files (cap 12) + same-package Go files sharing prefix (cap 12).
        let mut dep_candidate_files: Vec<String> = Vec::new();
        let mut seen_cand: HashSet<String> = file_entries.keys().cloned().collect();
        for (rel, _, _) in &seed_codes {
            if let Some(entry) = file_entries.get(rel) {
                for imp in &entry.imports {
                    if let Some(target) = resolve_specifier(&imp.from, rel, &self.allowed) {
                        if seen_cand.insert(target.clone()) {
                            dep_candidate_files.push(target);
                        }
                    }
                }
            }
            if rel.ends_with(".go") {
                let stem_prefix = Path::new(rel)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.split('_').next())
                    .unwrap_or("");
                if let Some(parent) = Path::new(rel).parent() {
                    let prefix = if parent.as_os_str().is_empty() {
                        String::new()
                    } else {
                        format!("{}/", parent.to_string_lossy().replace('\\', "/"))
                    };
                    for f in self.files.iter() {
                        if f.ends_with(".go")
                            && !f.ends_with("_test.go")
                            && f.starts_with(&prefix)
                            && !f[prefix.len()..].contains('/')
                            && (stem_prefix.is_empty() || f[prefix.len()..].starts_with(stem_prefix))
                            && seen_cand.insert(f.clone())
                        {
                            dep_candidate_files.push(f.clone());
                        }
                    }
                }
            }
        }
        dep_candidate_files.truncate(16);

        if !dep_candidate_files.is_empty() {
            let scanned: Vec<(String, Arc<Vec<String>>, FileEntry)> = std::thread::scope(|s| {
                let handles: Vec<_> = dep_candidate_files
                    .iter()
                    .map(|f| {
                        s.spawn(|| {
                            let lines = self.cached_lines(f).ok()?;
                            let joined = lines.join("\n");
                            let entry = scan_source(&joined, f);
                            Some((f.clone(), lines, entry))
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .filter_map(|h| h.join().ok().flatten())
                    .collect()
            });
            for (f, lines, entry) in scanned {
                file_lines.entry(f.clone()).or_insert(lines);
                file_entries.insert(f, entry);
            }
        }

        let mut index = IndexView::default();
        for (f, entry) in file_entries.iter() {
            let arc = Arc::new(entry.clone());
            index.files.insert(f.clone(), arc);
            for sym in &entry.syms {
                if sym.kind != "prop" {
                    index.defs.entry(sym.name.clone()).or_default().push(Definition {
                        file: f.clone(),
                        symbol: sym.clone(),
                    });
                }
            }
            let mut imp_map = HashMap::new();
            for imp in &entry.imports {
                if let Some(target) = resolve_specifier(&imp.from, f, &self.allowed) {
                    imp_map.insert(imp.name.clone(), target);
                }
            }
            if !imp_map.is_empty() {
                index.imports.insert(f.clone(), imp_map);
            }
        }

        let mut go_mod_cache: HashMap<String, String> = HashMap::new();
        for (rel, sym, code) in &seed_codes {
            let go_module = if rel.ends_with(".go") {
                let dir = Path::new(rel)
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                go_mod_cache
                    .entry(dir)
                    .or_insert_with(|| self.find_go_module_for(rel).1)
                    .clone()
            } else {
                String::new()
            };

            let mut added_for_seed = 0usize;
            let mut seen_tokens = HashSet::new();
            for m in ident_re.find_iter(code) {
                let name = m.as_str();
                if name == sym.name
                    || name.len() < 3
                    || stop_word(&name.to_ascii_lowercase())
                    || !index.defs.contains_key(name)
                    || !seen_tokens.insert(name)
                {
                    continue;
                }
                let before = &code[..m.start()];
                let resolved = if rel.ends_with(".go") {
                    let trimmed = before.trim_end();
                    let qualifier = trimmed.strip_suffix('.').and_then(|s| {
                        s.trim_end()
                            .rsplit(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                            .next()
                    });
                    if trimmed.ends_with('.') && qualifier.is_none() {
                        None
                    } else {
                        go::resolve(&index, name, rel, qualifier, &go_module, sym)
                    }
                } else if before.ends_with('.') {
                    None
                } else {
                    resolve_ref(&index, name, rel)
                };

                let Some(def) = resolved else { continue };
                if def.file == *rel && def.symbol.ln == sym.ln {
                    continue;
                }
                let span = def.symbol.end.saturating_sub(def.symbol.ln) + 1;
                if span > MAX_AUTO_DEP_LINES
                    || matches!(def.symbol.kind.as_str(), "class" | "impl" | "mod")
                {
                    continue;
                }
                let Some(target_lines) = file_lines.get(&def.file) else { continue };
                if def.symbol.ln == 0 || def.symbol.end > target_lines.len() {
                    continue;
                }
                let start = expand_leading_comments(target_lines, def.symbol.ln);
                dep_ranges
                    .entry(def.file.clone())
                    .or_default()
                    .push((start, def.symbol.end));
                added_for_seed += 1;
                if added_for_seed >= MAX_AUTO_DEPS_PER_FILE {
                    break;
                }
            }
        }
    }
}

fn expand_leading_comments(lines: &[String], ln: usize) -> usize {
    let mut start = ln;
    let mut in_block_comment = false;
    while start > 1 && ln - start < 24 {
        let prev = lines[start - 2].trim();
        if in_block_comment {
            start -= 1;
            if prev.starts_with("/*") {
                in_block_comment = false;
            }
            continue;
        }
        if prev.ends_with("*/") {
            start -= 1;
            if !prev.starts_with("/*") {
                in_block_comment = true;
            }
            continue;
        }
        if prev.starts_with('#') || prev.starts_with("//") || prev.starts_with('@') {
            start -= 1;
        } else {
            break;
        }
    }
    start
}

/// Merge overlapping, adjacent, or blank-line-only-separated ranges (gap <= 2 blank lines)
/// to avoid emitting dozens of tiny fragmented headers for consecutive declarations.
fn merge_ranges_bridging_blanks(ranges: Vec<(usize, usize)>, lines: &[String]) -> Vec<(usize, usize)> {
    let base = merge_ranges(ranges);
    if base.len() <= 1 {
        return base;
    }
    let mut out = Vec::with_capacity(base.len());
    let (mut cur_s, mut cur_e) = base[0];
    for (ns, ne) in base.into_iter().skip(1) {
        let gap = ns.saturating_sub(cur_e + 1);
        let only_blanks = gap <= 2
            && (cur_e + 1..ns).all(|ln| {
                lines
                    .get(ln - 1)
                    .is_some_and(|l| l.trim().is_empty())
            });
        if only_blanks {
            cur_e = cur_e.max(ne);
        } else {
            out.push((cur_s, cur_e));
            cur_s = ns;
            cur_e = ne;
        }
    }
    out.push((cur_s, cur_e));
    out
}

fn find_matching_symbols<'a>(entry: &'a FileEntry, query: &str) -> Vec<&'a Symbol> {
    let (wanted_owner, wanted_name) = match query.rsplit_once("::") {
        Some((o, n)) => (Some(o.trim()), n.trim()),
        None => match query.rsplit_once('.') {
            Some((o, n)) => (Some(o.trim()), n.trim()),
            None => (None, query.trim()),
        },
    };
    let owner_of = |sym: &Symbol| -> Option<String> {
        entry
            .syms
            .iter()
            .filter(|p| {
                p.ln < sym.ln
                    && p.end >= sym.end
                    && matches!(p.kind.as_str(), "class" | "impl" | "trait" | "mod")
            })
            .max_by_key(|p| p.ln)
            .map(|p| p.name.clone())
            .or_else(|| go::receiver(&sym.sig).map(|(_, ty)| ty))
    };
    let exact: Vec<&Symbol> = entry
        .syms
        .iter()
        .filter(|s| {
            s.name == wanted_name
                && match wanted_owner {
                    Some(o) => owner_of(s).as_deref() == Some(o),
                    None => true,
                }
        })
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    let ci: Vec<&Symbol> = entry
        .syms
        .iter()
        .filter(|s| {
            s.name.eq_ignore_ascii_case(wanted_name)
                && match wanted_owner {
                    Some(o) => owner_of(s).is_some_and(|ow| ow.eq_ignore_ascii_case(o)),
                    None => true,
                }
        })
        .collect();
    if !ci.is_empty() {
        return ci;
    }
    // If owner qualification didn't match (e.g. guessed class/struct wrapper), allow a unique unqualified symbol match.
    if wanted_owner.is_some() {
        let unqualified: Vec<&Symbol> = entry
            .syms
            .iter()
            .filter(|s| s.name.eq_ignore_ascii_case(wanted_name))
            .collect();
        if unqualified.len() == 1 {
            return unqualified;
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_and_word_splitting_cover_underscore_kebab_and_camel() {
        assert!(glob_match("local/appuse*.go", "local/appuse_elements.go"));
        assert!(glob_match("**/worker.js", "local/extension/vega-chrome/worker.js"));
        assert!(!glob_match("local/*.go", "local/sub/a.go"));
        assert!(glob_match("local/**/*.go", "local/sub/a.go"));
        let words = split_code_words("local/appuse_elements_windows.go");
        assert!(words.contains(&"appuse".into()) && words.contains(&"elements".into()));
        let camel = split_code_words("GetMetricTableSql");
        assert_eq!(camel, vec!["get", "metric", "table", "sql"]);
    }

    #[test]
    fn json_and_config_files_are_included_while_lockfiles_excluded() {
        assert!(is_project_text_file("local/extension/vega-chrome/manifest.json"));
        assert!(is_project_text_file("shared/chrome-tools.json"));
        assert!(is_project_text_file("local/extension/vega-chrome/popup.html"));
        assert!(is_project_text_file("local/go.mod"));
        assert!(is_project_text_file("Cargo.toml"));
        assert!(!is_project_text_file("package-lock.json"));
        assert!(!is_project_text_file("Cargo.lock"));
        assert!(!is_project_text_file("node_modules/pkg/index.js"));
        assert!(!is_project_text_file("web/dist/app.min.js"));
    }

    #[test]
    fn workspace_enforces_root_confinement_and_nested_git_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path();
        let child = outer.join("src/pages/intelligence");
        let page = child.join("pages/SourcesMetrics/Metrics");
        fs::create_dir_all(page.join("component/IntelligenceMetrics")).unwrap();
        for repo in [outer, child.as_path()] {
            assert!(Command::new("git")
                .args(["init", "-q"])
                .current_dir(repo)
                .status()
                .unwrap()
                .success());
        }
        fs::write(outer.join(".gitignore"), "src/pages/intelligence/\n").unwrap();
        fs::write(outer.join("layout.ts"), "export const locale = 'en';\n").unwrap();
        fs::write(child.join(".gitignore"), "private.ts\n").unwrap();
        fs::write(page.join("private.ts"), "export const leak = 'locale';\n").unwrap();
        fs::write(
            page.join("index.tsx"),
            "import IntelligenceMetrics from './component/IntelligenceMetrics';\nexport default () => <IntelligenceMetrics/>;\n",
        )
        .unwrap();
        fs::write(
            page.join("component/IntelligenceMetrics/index.tsx"),
            "export function IntelligenceMetrics() { getMetrics(locale); }\n",
        )
        .unwrap();

        let ws = Workspace::open(&page, Some(&outer.to_string_lossy()), &[]).unwrap();
        assert!(ws.files.iter().any(|f| f == "layout.ts"));
        assert!(ws.files.iter().any(|f| f.ends_with("/Metrics/index.tsx")));
        assert!(ws
            .files
            .iter()
            .any(|f| f.ends_with("/IntelligenceMetrics/index.tsx")));
        assert!(!ws.files.iter().any(|f| f.ends_with("private.ts")));

        let briefing = ws.briefing(
            "修复 /v2/intelligence/sourcesMetrics/metrics 切换语言重复请求",
            &[],
        );
        assert!(briefing.text.contains("Metrics/index.tsx"));
        assert!(!briefing.text.contains("private.ts"));

        let batch = ws.execute_batch(
            &BatchRequest {
                grep: vec![GrepQuery {
                    q: "locale".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            false,
        );
        assert!(batch.contains("layout.ts") && batch.contains("IntelligenceMetrics/index.tsx"));
        assert!(!batch.contains("private.ts"));

        let outside = tempfile::NamedTempFile::new().unwrap();
        assert!(Workspace::open(
            outer,
            None,
            &[outside.path().to_string_lossy().into_owned()]
        )
        .is_err());
    }

    #[test]
    fn outline_expands_class_methods_and_assemble_avoids_monolithic_class_dump() {
        let dir = tempfile::tempdir().unwrap();
        let mut big_class = String::from("export class Engine {\n  state = 1;\n");
        big_class.push_str("  alpha() {\n    return helper();\n  }\n");
        for i in 0..220 {
            big_class.push_str(&format!("  filler_{i}() {{ return {i}; }}\n"));
        }
        big_class.push_str("}\nfunction helper() { return 42; }\n");
        fs::write(dir.path().join("engine.ts"), &big_class).unwrap();

        let ws = Workspace::open(dir.path(), None, &[]).unwrap();
        let out = ws.exec_outline(&["engine.ts".into()], false);
        assert!(out.contains("class Engine") && out.contains("容器"), "{out}");
        assert!(out.contains("Engine::alpha"), "{out}");

        let assembled = ws
            .assemble(
                &[Selection {
                    file: "engine.ts".into(),
                    symbols: vec!["Engine::alpha".into()],
                    ..Default::default()
                }],
                &[],
                true,
            )
            .unwrap();
        assert!(assembled.text.contains("alpha()"));
        assert!(assembled.text.contains("function helper()"));
        assert!(!assembled.text.contains("filler_100"));

        let cls = ws
            .assemble(
                &[Selection {
                    file: "engine.ts".into(),
                    symbols: vec!["Engine".into()],
                    ..Default::default()
                }],
                &[],
                false,
            )
            .unwrap();
        assert!(cls.text.contains("export class Engine"));
        assert!(!cls.text.contains("filler_100"));
    }

    #[test]
    fn go_receiver_methods_resolve_by_owner_and_blank_gaps_merge() {
        let dir = tempfile::tempdir().unwrap();
        let src = "\
package main

type browserBridge struct {
\tport int
}

func (b *browserBridge) execute(cmd string) error {
\treturn nil
}

func (b *browserBridge) status() string {
\treturn \"ok\"
}
";
        fs::write(dir.path().join("browser.go"), src).unwrap();
        let ws = Workspace::open(dir.path(), None, &[]).unwrap();
        let assembled = ws
            .assemble(
                &[Selection {
                    file: "browser.go".into(),
                    symbols: vec!["browserBridge::execute".into(), "browserBridge::status".into()],
                    ..Default::default()
                }],
                &[],
                false,
            )
            .unwrap();
        assert!(assembled.text.contains("func (b *browserBridge) execute"));
        assert!(assembled.text.contains("func (b *browserBridge) status"));
        assert_eq!(assembled.block_count, 1, "single blank line gap should fuse into 1 block");
    }

    #[test]
    fn git_commit_inspection_surfaces_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success());
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        fs::write(dir.path().join("alpha.ts"), "export const a = 1;\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "feat: add alpha"]);
        let sha = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "--short=7", "HEAD"])
                .current_dir(dir.path())
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let sha = sha.trim();

        let ws = Workspace::open(dir.path(), None, &[]).unwrap();
        let briefing = ws.briefing(&format!("请参考提交 {sha} 的改动"), &[]);
        if sha.bytes().any(|b| b.is_ascii_digit())
            && sha.bytes().any(|b| matches!(b, b'a'..=b'f'))
        {
            assert!(briefing.text.contains("alpha.ts"), "{}", briefing.text);
        }
        let git_out = ws.exec_git(&[GitQuery {
            rev: sha.to_string(),
            path: None,
            diff: false,
        }]);
        assert!(
            git_out.contains("alpha.ts") && git_out.contains("feat: add alpha"),
            "{git_out}"
        );
    }
}
