//! agy CLI 地区限制补丁（副本方式，不改写原始二进制）。
//!
//! 特征码移植自 dalroot/antigravity-cleaner（Apache-2.0）的 `pkg/patcher`：agy 二进制
//! （Go 编译）内置「资格检查（eligibility）」与 `hasValidAuth` 门，命中后即使账号有效也会
//! 拒绝受限地区用户进入。这里按特征码定位这些门，把判断字节改写为恒真。
//!
//! 与原地补丁不同，本模块只产出补丁副本：`<cache_root>/agy-<len>-<mtime>/agy.exe`。
//! - 原 agy.exe 永远不被修改；关闭开关即回到官方原版，无需回滚。
//! - 仅绕客户端本地 ToS/eligibility 拦截；服务端按 IP 的地区拦截（HTTP 400）不在此列。
//! - 以源文件 (len, mtime) 指纹定位副本：存在即复用（跨重启零扫描）；agy 更新后指纹
//!   变化自动重新生成副本，旧指纹目录顺手清理。

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 带通配符的字节特征码：`None` 位置匹配任意字节。
struct Pattern {
    bytes: Vec<Option<u8>>,
}

impl Pattern {
    fn parse(sig: &str) -> Pattern {
        let bytes = sig
            .split_whitespace()
            .map(|token| {
                if token == "?" || token == "??" {
                    None
                } else {
                    Some(u8::from_str_radix(token, 16).expect("invalid hex in pattern"))
                }
            })
            .collect();
        Pattern { bytes }
    }

    /// 返回全部命中位置（互不重叠）。
    fn find_all(&self, data: &[u8]) -> Vec<usize> {
        let n = self.bytes.len();
        if n == 0 || data.len() < n {
            return Vec::new();
        }
        // 用模式里第一个确定字节做 memchr 锚点（SIMD 加速），避免逐字节朴素扫描。
        let Some(anchor) = self.bytes.iter().position(|b| b.is_some()) else {
            return Vec::new();
        };
        let anchor_byte = self.bytes[anchor].unwrap();
        let mut hits = Vec::new();
        let limit = data.len() - n;
        let mut scan_from = anchor;
        while scan_from <= limit + anchor && scan_from < data.len() {
            let Some(rel) = memchr::memchr(anchor_byte, &data[scan_from..]) else {
                break;
            };
            let anchor_pos = scan_from + rel;
            let start = anchor_pos.saturating_sub(anchor);
            if start > limit {
                break;
            }
            let matched = self
                .bytes
                .iter()
                .enumerate()
                .all(|(i, b)| b.map_or(true, |fixed| data[start + i] == fixed));
            if matched {
                hits.push(start);
                scan_from = start + n;
            } else {
                scan_from = anchor_pos + 1;
            }
        }
        hits
    }

    fn find(&self, data: &[u8]) -> Option<usize> {
        self.find_all(data).into_iter().next()
    }
}

/// 一个补丁门：未补丁特征、已补丁特征、替换字节及其在命中点起的偏移。
struct BinaryGate {
    name: &'static str,
    unpatched: Pattern,
    patched: Pattern,
    replacement: &'static [u8],
    offset: usize,
}

fn gate(
    name: &'static str,
    unpatched: &str,
    patched: &str,
    replacement: &'static [u8],
    offset: usize,
) -> BinaryGate {
    BinaryGate {
        name,
        unpatched: Pattern::parse(unpatched),
        patched: Pattern::parse(patched),
        replacement,
        offset,
    }
}

/// x64 资格检查门：`test rax,rax; jz …; cmp byte [rax+8],0; jnz …`
/// 改写 cmp 为 `test rax,rax; nop` —— rax 非空时 ZF=0，`jnz` 恒跳，等效资格恒真。
const X64_ELIGIBILITY_REPL: &[u8] = &[0x48, 0x85, 0xc0, 0x90];
/// x64 hasValidAuth 门：`cmp byte [rax+8],0; jz …` 改写为 `mov byte [rax+8],1; nop; nop`。
const X64_AUTH_REPL: &[u8] = &[0xc6, 0x40, 0x08, 0x01, 0x90, 0x90];
/// arm64 资格检查门：`ldrb w1,[x0,#8]` 改写为 `mov w1,#1`。
const ARM64_ELIGIBILITY_REPL: &[u8] = &[0x21, 0x00, 0x80, 0x52];
const ARM64_ELIGIBILITY_REPL_W4: &[u8] = &[0x24, 0x00, 0x80, 0x52];
const ARM64_ELIGIBILITY_REPL_W2: &[u8] = &[0x22, 0x00, 0x80, 0x52];
/// arm64 hasValidAuth 门：`ldrb w3,[x0,#8]` 改写为 `mov w3,#1; strb w3,[x0,#8]`。
const ARM64_AUTH_REPL: &[u8] = &[0x23, 0x00, 0x80, 0x52, 0x03, 0x20, 0x00, 0x39];

fn cli_gates() -> Vec<BinaryGate> {
    vec![
        gate(
            "eligibility check bypass (x64)",
            "48 85 c0 0f 84 ?? ?? ?? ?? 80 78 08 00 0f 85 ?? ?? ?? ??",
            "48 85 c0 0f 84 ?? ?? ?? ?? 48 85 c0 90 0f 85 ?? ?? ?? ??",
            X64_ELIGIBILITY_REPL,
            9,
        ),
        gate(
            "internal hasValidAuth (x64)",
            "80 78 08 00 74 ?? 48 8b ?? 24 ?? 48 89 ?? 60",
            "c6 40 08 01 90 90 48 8b ?? 24 ?? 48 89 ?? 60",
            X64_AUTH_REPL,
            0,
        ),
        gate(
            "eligibility check bypass (arm64)",
            "?? ?? ?? b5 ?? ?? ?? b4 01 20 40 39 ?? ?? ?? 37",
            "?? ?? ?? b5 ?? ?? ?? b4 21 00 80 52 ?? ?? ?? 37",
            ARM64_ELIGIBILITY_REPL,
            8,
        ),
        gate(
            "eligibility check bypass (arm64, w4)",
            "?? ?? ?? b4 04 20 40 39 ?? ?? ?? 37 04 94 44 a9",
            "?? ?? ?? b4 24 00 80 52 ?? ?? ?? 37 04 94 44 a9",
            ARM64_ELIGIBILITY_REPL_W4,
            4,
        ),
        gate(
            "eligibility check bypass (arm64, w2)",
            "?? ?? ?? b5 ?? ?? ?? b4 02 20 40 39 ?? ?? ?? 37",
            "?? ?? ?? b5 ?? ?? ?? b4 22 00 80 52 ?? ?? ?? 37",
            ARM64_ELIGIBILITY_REPL_W2,
            8,
        ),
        gate(
            "internal hasValidAuth (arm64)",
            "03 20 40 39 ?? ?? ?? 36 ?? ?? ?? ?? 03 10 06 a9",
            "23 00 80 52 03 20 00 39 ?? ?? ?? ?? 03 10 06 a9",
            ARM64_AUTH_REPL,
            0,
        ),
    ]
}

/// 对内存中的二进制映像套用全部补丁门，返回改写结果；已补丁/无特征时不做任何修改。
fn apply_gates(data: &mut [u8]) -> GateOutcome {
    let gates = cli_gates();
    let mut applied: Vec<String> = Vec::new();
    for g in &gates {
        let locs = g.unpatched.find_all(data);
        if locs.is_empty() {
            continue;
        }
        for idx in &locs {
            let target = idx + g.offset;
            data[target..target + g.replacement.len()].copy_from_slice(g.replacement);
        }
        applied.push(format!("{} ×{}", g.name, locs.len()));
    }
    if !applied.is_empty() {
        return GateOutcome::Patched(applied);
    }
    for g in &gates {
        if g.patched.find(data).is_some() {
            return GateOutcome::AlreadyPatched;
        }
    }
    GateOutcome::SignatureNotFound
}

enum GateOutcome {
    Patched(Vec<String>),
    AlreadyPatched,
    SignatureNotFound,
}

/// `ensure_patched_copy` 的结果分类（供日志）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchState {
    /// 补丁副本已存在（或源二进制本身已被外部工具补丁过），本次未改动。
    AlreadyPatched,
    /// 本次生成了新的补丁副本。
    PatchedNow,
}

pub struct PreparedBinary {
    /// 用于启动的可执行文件路径：通常是补丁副本；
    /// 源二进制本身已是补丁态时直接返回源路径（无需再复制）。
    pub path: PathBuf,
    pub state: PatchState,
    /// 本次实际改写的门描述（仅 PatchedNow 时非空）。
    pub details: Vec<String>,
}

struct CacheEntry {
    len: u64,
    modified_secs: u64,
    launch: PathBuf,
}

/// 同一进程内的结果缓存：命中后每次启动只剩两次 stat。
static PREPARE_CACHE: Mutex<Option<HashMap<PathBuf, CacheEntry>>> = Mutex::new(None);
/// 串行化副本生成：避免后台预热与首启会话并发重复复制/扫描。
static PREPARE_LOCK: Mutex<()> = Mutex::new(());

fn fingerprint(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(path).ok()?;
    let modified_secs = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((meta.len(), modified_secs))
}

fn cache_lookup(source: &Path, len: u64, modified_secs: u64) -> Option<PathBuf> {
    let guard = PREPARE_CACHE.lock().unwrap();
    let entry = guard.as_ref()?.get(source)?;
    if entry.len == len && entry.modified_secs == modified_secs && entry.launch.exists() {
        Some(entry.launch.clone())
    } else {
        None
    }
}

fn cache_store(source: &Path, len: u64, modified_secs: u64, launch: PathBuf) {
    let mut guard = PREPARE_CACHE.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() > 8 {
        map.clear();
    }
    map.insert(
        source.to_path_buf(),
        CacheEntry {
            len,
            modified_secs,
            launch,
        },
    );
}

/// 原子写入补丁副本：先写同目录临时文件再 rename，避免崩溃留下半个可执行文件。
fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} 没有父目录", path.display()))?;
    let tmp = dir.join(format!(
        ".partial-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let write_result = (|| -> Result<(), String> {
        let mut file = fs::File::create(&tmp).map_err(|e| format!("创建临时文件失败：{e}"))?;
        file.write_all(data)
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("写入临时文件失败：{e}"))?;
        drop(file);
        fs::rename(&tmp, path).map_err(|e| format!("生成补丁副本失败：{e}"))
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write_result
}

/// macOS 上改写 Mach-O 后需要 ad-hoc 重签，否则 AMFI 拒绝启动。尽力而为，失败忽略。
#[cfg(target_os = "macos")]
fn resign_after_patch(path: &Path) {
    use std::process::Command;
    let _ = Command::new("xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(path)
        .output();
    let _ = Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(path)
        .output();
}

#[cfg(not(target_os = "macos"))]
fn resign_after_patch(_path: &Path) {}

/// 清理 cache_root 下除 current 以外的旧指纹副本目录（agy 每次更新都会产生新版本）。
fn cleanup_stale_copies(cache_root: &Path, current: &Path) {
    let Ok(entries) = fs::read_dir(cache_root) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p != current
            && p.is_dir()
            && entry
                .file_name()
                .to_str()
                .map_or(false, |n| n.starts_with("agy-"))
        {
            // 旧副本可能被仍在运行的会话占用：删除失败就留着，下次再清。
            let _ = fs::remove_dir_all(&p);
        }
    }
}

/// 确保 source 指向的 agy 二进制存在对应的补丁副本，返回可启动路径。
///
/// 成本：副本已就绪时仅 stat（跨重启也一样）；指纹变化才重新读入扫描（一次性）。
/// 失败场景：源文件不可读、特征码未命中（版本不适配）、写盘失败。
pub fn ensure_patched_copy(source: &Path, cache_root: &Path) -> Result<PreparedBinary, String> {
    let (len, modified_secs) = fingerprint(source)
        .ok_or_else(|| format!("无法读取文件信息：{}", source.display()))?;

    let _serialize = PREPARE_LOCK.lock().unwrap();

    if let Some(launch) = cache_lookup(source, len, modified_secs) {
        return Ok(PreparedBinary {
            path: launch,
            state: PatchState::AlreadyPatched,
            details: Vec::new(),
        });
    }

    let file_name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "agy".into());
    let copy_dir = cache_root.join(format!("agy-{len}-{modified_secs}"));
    let copy_path = copy_dir.join(&file_name);

    // 跨重启复用：指纹目录与副本都在，直接用它，连源文件都不必读。
    if copy_path.is_file() {
        cache_store(source, len, modified_secs, copy_path.clone());
        return Ok(PreparedBinary {
            path: copy_path,
            state: PatchState::AlreadyPatched,
            details: Vec::new(),
        });
    }

    let mut data =
        fs::read(source).map_err(|e| format!("读取 {} 失败：{e}", source.display()))?;
    match apply_gates(&mut data) {
        GateOutcome::SignatureNotFound => Err(format!(
            "{} 中未找到资格检查特征码（agy 版本过新/过旧或已被其他工具修改）",
            source.display()
        )),
        // 源二进制本身就是补丁态（例如已被外部工具原地补丁）：直接用源路径，省掉副本。
        GateOutcome::AlreadyPatched => {
            cache_store(source, len, modified_secs, source.to_path_buf());
            Ok(PreparedBinary {
                path: source.to_path_buf(),
                state: PatchState::AlreadyPatched,
                details: Vec::new(),
            })
        }
        GateOutcome::Patched(details) => {
            fs::create_dir_all(&copy_dir)
                .map_err(|e| format!("创建副本目录失败：{e}"))?;
            atomic_write(&copy_path, &data)?;
            resign_after_patch(&copy_path);
            cleanup_stale_copies(cache_root, &copy_dir);
            cache_store(source, len, modified_secs, copy_path.clone());
            Ok(PreparedBinary {
                path: copy_path,
                state: PatchState::PatchedNow,
                details,
            })
        }
    }
}

/// 按设置里的配置（裸命令名或绝对路径）解析 agy 并准备补丁副本。
pub fn prepare_configured_copy(
    configured: &str,
    cache_root: &Path,
) -> Result<PreparedBinary, String> {
    let trimmed = configured.trim();
    let name = if trimmed.is_empty() { "agy" } else { trimmed };
    let resolved = crate::acp::resolve_program_on_path(name)
        .ok_or_else(|| format!("未找到 Antigravity CLI（{name}）"))?;
    ensure_patched_copy(&resolved, cache_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_sample_x64() -> Vec<u8> {
        // 构造一个嵌入两处 x64 特征的假二进制。
        let mut data = vec![0u8; 4096];
        let eligibility: [u8; 20] = [
            0x48, 0x85, 0xc0, 0x0f, 0x84, 0x11, 0x22, 0x33, 0x44, 0x80, 0x78, 0x08, 0x00, 0x0f,
            0x85, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        ];
        // auth 特征码 15 字节：80 78 08 00 74 ?? 48 8b ?? 24 ?? 48 89 ?? 60
        let auth: [u8; 15] = [
            0x80, 0x78, 0x08, 0x00, 0x74, 0x05, 0x48, 0x8b, 0x44, 0x24, 0x08, 0x48, 0x89, 0x24,
            0x60,
        ];
        data[100..120].copy_from_slice(&eligibility);
        data[1000..1015].copy_from_slice(&auth);
        data
    }

    #[test]
    fn patches_x64_gates_and_is_idempotent() {
        let mut data = build_sample_x64();
        match apply_gates(&mut data) {
            GateOutcome::Patched(details) => {
                assert_eq!(details.len(), 2, "应命中两个 x64 门：{details:?}");
            }
            _ => panic!("首次扫描应命中未补丁特征"),
        }
        // eligibility: 偏移 9 起写入 48 85 c0 90
        assert_eq!(&data[109..113], &[0x48, 0x85, 0xc0, 0x90]);
        // 改写前的 cmp byte [rax+8],0 应已消失
        assert_ne!(&data[109..112], &[0x80, 0x78, 0x08]);
        // hasValidAuth: 偏移 0 起写入 c6 40 08 01 90 90
        assert_eq!(&data[1000..1006], &[0xc6, 0x40, 0x08, 0x01, 0x90, 0x90]);
        // 再次扫描应识别为已补丁
        assert!(matches!(apply_gates(&mut data), GateOutcome::AlreadyPatched));
    }

    #[test]
    fn unknown_binary_reports_no_signature() {
        let mut data = vec![0x55u8; 8192];
        assert!(matches!(
            apply_gates(&mut data),
            GateOutcome::SignatureNotFound
        ));
    }

    #[test]
    fn copy_is_created_without_touching_source() {
        let base = std::env::temp_dir().join(format!("nova_unlock_test_{}", std::process::id()));
        let src_dir = base.join("src");
        let cache_root = base.join("cache");
        fs::create_dir_all(&src_dir).unwrap();
        let source = src_dir.join("agy.exe");
        let original = build_sample_x64();
        fs::write(&source, &original).unwrap();

        // 第一次：生成补丁副本，源文件不动。
        let report = ensure_patched_copy(&source, &cache_root).unwrap();
        assert_eq!(report.state, PatchState::PatchedNow);
        assert_ne!(report.path, source);
        assert!(report.path.is_file());
        assert_eq!(fs::read(&source).unwrap(), original, "源文件必须保持不变");

        // 副本内容与打补丁后的映像一致。
        let mut expected = original.clone();
        assert!(matches!(apply_gates(&mut expected), GateOutcome::Patched(_)));
        assert_eq!(fs::read(&report.path).unwrap(), expected);

        // 第二次：复用副本，不再改写。
        let report2 = ensure_patched_copy(&source, &cache_root).unwrap();
        assert_eq!(report2.state, PatchState::AlreadyPatched);
        assert_eq!(report2.path, report.path);

        // 模拟 agy 更新：源文件内容变化（长度变）→ 生成新副本并清理旧目录。
        let mut updated = build_sample_x64();
        updated.extend_from_slice(&[0u8; 128]);
        fs::write(&source, &updated).unwrap();
        let report3 = ensure_patched_copy(&source, &cache_root).unwrap();
        assert_eq!(report3.state, PatchState::PatchedNow);
        assert_ne!(report3.path, report.path);
        assert!(!report.path.exists(), "旧副本目录应被清理");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn unpatcheable_source_fails() {
        let base = std::env::temp_dir().join(format!("nova_unlock_fail_{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        let source = base.join("agy.exe");
        fs::write(&source, vec![0x55u8; 2048]).unwrap();
        assert!(ensure_patched_copy(&source, &base.join("cache")).is_err());
        let _ = fs::remove_dir_all(&base);
    }
}
