//! 自更新：从 GitHub Releases 检查最新版本。
//!
//! 流程：「静默下载 + 角标提示」→ 用户确认后 `apply_staged`。
//!
//! Release 资产约定：
//! - Windows：`nova-{version}.zip`（内含 `Nova.exe`）
//! - macOS：`nova-macos-{aarch64|x86_64}-{version}.zip`（内含 `Nova`）
//! - Linux：`nova-linux-{aarch64|x86_64}-{version}.zip`（内含 `Nova`）
//!
//! 仓库：`option_env!("NOVA_GH_REPO")` 或下方默认 `llyb120/nova-client`。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Position, Size};
use tokio::time::Duration;

const APPLY_UPDATE_ARG: &str = "--nova-apply-update";
#[cfg(windows)]
const RESTART_PARENT_PID: &str = "NOVA_RESTART_PARENT_PID";
/// 后台定时下载与手动检查共用同一套临时文件，必须串行，避免进度交叉和暂存包互相覆盖。
static UPDATE_OPERATION_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// GitHub 仓库 owner/repo。编译时可设 NOVA_GH_REPO 覆盖。
fn github_repo() -> &'static str {
    option_env!("NOVA_GH_REPO").unwrap_or("llyb120/nova-client")
}

fn github_api_latest() -> String {
    format!(
        "https://api.github.com/repos/{}/releases/latest",
        github_repo()
    )
}

fn asset_name_for(version: &str) -> String {
    format!("{}-{version}.zip", update_channel())
}

/// 自更新主通道标识。
/// - Windows 继续用 `nova`（兼容存量客户端）
/// - macOS/Linux 使用带平台与架构的通道，避免跨平台误下更新包
fn update_channel() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "nova-macos-aarch64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "nova-macos-x86_64"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "nova-linux-aarch64"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "nova-linux-x86_64"
    }
    #[cfg(all(
        target_os = "linux",
        not(any(target_arch = "aarch64", target_arch = "x86_64"))
    ))]
    {
        "nova-linux-unsupported"
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        "nova"
    }
}

fn compiled_app_version() -> String {
    serde_json::from_str::<Value>(include_str!("../tauri.conf.json"))
        .ok()
        .and_then(|value| value["version"].as_str().map(str::to_string))
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

/// Pure CLI updater used by `Nova server update`; it never initializes Tauri, GTK, or Xvfb.
pub fn run_server_update(check_only: bool) -> Result<(), String> {
    let runtime = tokio::runtime::Runtime::new().map_err(|error| error.to_string())?;
    runtime.block_on(run_server_update_async(check_only))
}

async fn run_server_update_async(check_only: bool) -> Result<(), String> {
    let current = compiled_app_version();
    let client = update_http_client(
        Some(&format!("Nova/{current}")),
        Some(Duration::from_secs(30)),
    )?;
    let response = client
        .get(github_api_latest())
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| format!("检查更新失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!("检查更新失败：HTTP {}", response.status()));
    }
    let release: Value = response
        .json()
        .await
        .map_err(|error| format!("更新信息解析失败：{error}"))?;
    let latest = release["tag_name"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .trim_start_matches('v')
        .to_string();
    if latest.is_empty() {
        return Err("最新 Release 没有有效版本号".into());
    }
    let has_update = matches!(
        (parse_ver(&latest), parse_ver(&current)),
        (Some(latest), Some(current)) if latest > current
    );
    println!("当前版本：{current}");
    println!("最新版本：{latest}");
    if !has_update {
        println!("已是最新版本。");
        return Ok(());
    }
    if check_only {
        println!("发现可用更新。运行 `Nova server update` 安装。");
        return Ok(());
    }

    let asset_name = asset_name_for(&latest);
    let download_url = release["assets"]
        .as_array()
        .and_then(|assets| {
            assets.iter().find_map(|asset| {
                (asset["name"].as_str() == Some(asset_name.as_str()))
                    .then(|| asset["browser_download_url"].as_str())
                    .flatten()
            })
        })
        .ok_or_else(|| format!("Release 中没有当前平台更新包：{asset_name}"))?;
    println!("正在下载 {asset_name} ...");
    let bytes = client
        .get(download_url)
        .send()
        .await
        .map_err(|error| format!("下载更新失败：{error}"))?
        .error_for_status()
        .map_err(|error| format!("下载更新失败：{error}"))?
        .bytes()
        .await
        .map_err(|error| format!("读取更新包失败：{error}"))?;

    let stage_dir = std::env::temp_dir().join(format!(
        "nova-server-update-{}-{}",
        latest,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&stage_dir);
    std::fs::create_dir_all(&stage_dir).map_err(|error| error.to_string())?;
    let extraction = (|| -> Result<PathBuf, String> {
        let cursor = std::io::Cursor::new(bytes);
        let mut archive =
            zip::ZipArchive::new(cursor).map_err(|error| format!("更新包损坏：{error}"))?;
        archive
            .extract(&stage_dir)
            .map_err(|error| format!("解压更新包失败：{error}"))?;
        let staged = find_exe(&stage_dir, "Nova").ok_or("更新包中没有 Nova 可执行文件")?;
        validate_staged_exe(&staged)?;
        Ok(staged)
    })();
    let staged = match extraction {
        Ok(path) => path,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&stage_dir);
            return Err(error);
        }
    };

    let target = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .ok_or("无法确定当前 Nova 可执行文件")?;
    ensure_install_dir_writable(&target)?;
    let backup = target.with_file_name(format!(
        "{}.old",
        target
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default()
    ));
    remove_file_if_exists(&backup);
    std::fs::rename(&target, &backup).map_err(|error| format!("备份当前版本失败：{error}"))?;
    if let Err(error) = std::fs::copy(&staged, &target) {
        let _ = std::fs::rename(&backup, &target);
        let _ = std::fs::remove_dir_all(&stage_dir);
        return Err(format!("安装新版本失败：{error}"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&target)
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(permissions.mode() | 0o755);
        std::fs::set_permissions(&target, permissions).map_err(|error| error.to_string())?;
    }
    let _ = std::fs::remove_dir_all(&stage_dir);
    println!("已更新至 {latest}：{}", target.display());
    println!("旧版本备份：{}", backup.display());
    println!("请重新启动 Nova Server。");
    Ok(())
}

pub const EV_PROGRESS: &str = "update:progress";
/// 新版本已暂存就绪、可重启更新。后端定时器检测到并下载暂存后发出，前端据此显示左上角角标。
/// 载荷与 `check` 返回一致（current/latest/size/staged 等）。
pub const EV_AVAILABLE: &str = "update:available";
/// 新版本已下载好，且当前空闲（没有任务在跑）→ 主动弹窗让用户选择是否现在更新。
/// 与 EV_AVAILABLE 载荷一致；前端据此弹出更新对话框（每个版本每次运行只弹一次）。
pub const EV_PROMPT: &str = "update:prompt";

/// 暂存的更新信息（写到 app_data_dir/update-staged.json）
#[derive(Serialize, Deserialize, Clone)]
struct StagedMarker {
    version: String,
    dir: String,
}

/// 升级重启前窗口的完整状态：位置（含多屏的全局坐标）、大小、最大化/最小化、
/// 可见性与是否前台。重启后据此把窗口还原成「更新前的样子」，做到无感升级。
#[derive(Serialize, Deserialize, Clone)]
struct WindowState {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    #[serde(default)]
    maximized: bool,
    #[serde(default)]
    minimized: bool,
    #[serde(default = "default_true")]
    visible: bool,
    #[serde(default = "default_true")]
    focused: bool,
}

fn default_true() -> bool {
    true
}

impl WindowState {
    /// 最小化的窗口在 Windows 上位置会是 -32000 之类的哨兵值，此时坐标不可信，
    /// 只还原最小化状态、不套用坐标（避免把窗口丢到屏幕外）。
    fn geometry_valid(&self) -> bool {
        self.width > 0 && self.height > 0 && self.x > -30000 && self.y > -30000
    }
}

/// 升级重启后用来恢复「重启前状态」的标记（app_data_dir/update-restore.json）。
/// 替换 exe 前写入：前端先消费 `thread_id` 并恢复会话，页面稳定后后端再消费 `window` 显示窗口。
#[derive(Serialize, Deserialize, Clone, Default)]
struct RestoreMarker {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window: Option<WindowState>,
}

fn parse_ver(s: &str) -> Option<(u64, u64, u64)> {
    let mut it = s.trim().trim_start_matches('v').split('.');
    let a = it.next()?.parse().ok()?;
    let b = it.next().unwrap_or("0").parse().ok()?;
    let c = it.next().unwrap_or("0").parse().ok()?;
    Some((a, b, c))
}

fn marker_path(app: &AppHandle) -> Option<PathBuf> {
    Some(crate::nova_data_dir(app).join("update-staged.json"))
}

fn read_marker(app: &AppHandle) -> Option<StagedMarker> {
    let path = marker_path(app)?;
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_marker(app: &AppHandle, marker: &StagedMarker) {
    if let Some(path) = marker_path(app) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(marker) {
            let _ = std::fs::write(path, json);
        }
    }
}

fn remove_file_if_exists(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(windows)]
fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    let slice = bytes.get(offset..end)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

#[cfg(windows)]
fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let slice = bytes.get(offset..end)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

#[cfg(windows)]
fn expected_pe_machine() -> Option<u16> {
    #[cfg(target_arch = "x86_64")]
    {
        Some(0x8664)
    }
    #[cfg(target_arch = "x86")]
    {
        Some(0x014c)
    }
    #[cfg(target_arch = "aarch64")]
    {
        Some(0xaa64)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
    {
        None
    }
}

#[cfg(windows)]
fn pe_machine_name(machine: u16) -> &'static str {
    match machine {
        0x014c => "x86",
        0x8664 => "x64",
        0xaa64 => "arm64",
        _ => "unknown",
    }
}

#[cfg(windows)]
fn validate_pe_image(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 0x40 {
        return Err("文件太小".into());
    }
    if bytes.get(0..2) != Some(b"MZ") {
        return Err("缺少 MZ 文件头".into());
    }
    let pe_offset = read_u32(bytes, 0x3c).ok_or("缺少 PE 偏移")? as usize;
    let coff_end = pe_offset.checked_add(24).ok_or("PE 头偏移溢出")?;
    if coff_end > bytes.len() {
        return Err("PE 头不完整".into());
    }
    if bytes.get(pe_offset..pe_offset + 4) != Some(b"PE\0\0") {
        return Err("缺少 PE 文件头".into());
    }

    let machine = read_u16(bytes, pe_offset + 4).ok_or("缺少架构字段")?;
    if let Some(expected) = expected_pe_machine() {
        if machine != expected {
            return Err(format!(
                "架构不匹配，包内是 {}，当前需要 {}",
                pe_machine_name(machine),
                pe_machine_name(expected)
            ));
        }
    }

    let section_count = read_u16(bytes, pe_offset + 6).ok_or("缺少节数量")? as usize;
    if section_count == 0 {
        return Err("没有 PE 节".into());
    }
    let optional_header_size = read_u16(bytes, pe_offset + 20).ok_or("缺少可选头长度")? as usize;
    let optional_header_start = pe_offset + 24;
    let optional_header_end = optional_header_start
        .checked_add(optional_header_size)
        .ok_or("可选头长度溢出")?;
    if optional_header_end > bytes.len() {
        return Err("可选头不完整".into());
    }
    let magic = read_u16(bytes, optional_header_start).ok_or("缺少可选头标记")?;
    if magic != 0x010b && magic != 0x020b {
        return Err("可选头标记无效".into());
    }

    let section_table_end = optional_header_end
        .checked_add(section_count.checked_mul(40).ok_or("节表长度溢出")?)
        .ok_or("节表长度溢出")?;
    if section_table_end > bytes.len() {
        return Err("节表不完整".into());
    }

    for i in 0..section_count {
        let offset = optional_header_end + i * 40;
        let raw_size = read_u32(bytes, offset + 16).ok_or("缺少节大小")? as usize;
        let raw_ptr = read_u32(bytes, offset + 20).ok_or("缺少节偏移")? as usize;
        if raw_size == 0 {
            continue;
        }
        let raw_end = raw_ptr.checked_add(raw_size).ok_or("节范围溢出")?;
        if raw_ptr == 0 || raw_end > bytes.len() {
            return Err(format!(
                "文件被截断，需要至少 {} 字节，实际 {} 字节",
                raw_end,
                bytes.len()
            ));
        }
    }

    Ok(())
}

/// Mach-O CPU types we accept for the current build.
#[cfg(target_os = "macos")]
fn expected_mach_o_cputype() -> Option<u32> {
    #[cfg(target_arch = "aarch64")]
    {
        Some(0x0100_000c) // CPU_TYPE_ARM64
    }
    #[cfg(target_arch = "x86_64")]
    {
        Some(0x0100_0007) // CPU_TYPE_X86_64
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn validate_mach_o_image(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 8 {
        return Err("文件太小".into());
    }
    let magic = u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    // MH_MAGIC_64 / MH_CIGAM_64 / FAT_MAGIC / FAT_CIGAM
    const MH_MAGIC_64: u32 = 0xfeed_facf;
    const MH_CIGAM_64: u32 = 0xcffa_edfe;
    const FAT_MAGIC: u32 = 0xcafe_babe;
    const FAT_CIGAM: u32 = 0xbeba_feca;
    const MH_MAGIC: u32 = 0xfeed_face;
    const MH_CIGAM: u32 = 0xcefa_edfe;

    let is_fat = magic == FAT_MAGIC || magic == FAT_CIGAM;
    let is_thin = matches!(magic, MH_MAGIC_64 | MH_CIGAM_64 | MH_MAGIC | MH_CIGAM);
    if !is_fat && !is_thin {
        return Err("缺少 Mach-O 文件头".into());
    }

    if is_thin {
        if bytes.len() < 12 {
            return Err("Mach-O 头不完整".into());
        }
        let cputype = if magic == MH_CIGAM_64 || magic == MH_CIGAM {
            u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]])
        } else {
            u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]])
        };
        if let Some(expected) = expected_mach_o_cputype() {
            if cputype != expected {
                return Err(format!(
                    "架构不匹配，包内 cputype=0x{cputype:x}，当前需要 0x{expected:x}"
                ));
            }
        }
    }
    // Universal binary：至少确认是合法 fat header，具体切片由内核按本机 arch 选择
    Ok(())
}

fn validate_staged_exe(path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读取新版文件失败:{e}"))?;
    #[cfg(windows)]
    {
        validate_pe_image(&bytes).map_err(|e| format!("更新包里的可执行文件无效:{e}"))
    }
    #[cfg(target_os = "macos")]
    {
        validate_mach_o_image(&bytes).map_err(|e| format!("更新包里的可执行文件无效:{e}"))
    }
    #[cfg(target_os = "linux")]
    {
        #[cfg(target_arch = "x86_64")]
        let expected_machine = 62;
        #[cfg(target_arch = "aarch64")]
        let expected_machine = 183;
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        return Err("当前 Linux 架构暂不支持自动更新".into());
        validate_elf_image(&bytes, expected_machine)
            .map_err(|e| format!("更新包里的可执行文件无效:{e}"))
    }
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
    {
        if bytes.is_empty() {
            return Err("更新包里的可执行文件无效:文件为空".into());
        }
        Ok(())
    }
}

#[cfg(any(target_os = "linux", test))]
fn validate_elf_image(bytes: &[u8], expected_machine: u16) -> Result<(), String> {
    if bytes.len() < 20 || bytes.get(..4) != Some(b"\x7fELF") {
        return Err("缺少 ELF 文件头".into());
    }
    if !matches!(bytes[4], 1 | 2) {
        return Err("ELF 位数标记无效".into());
    }
    let machine = match bytes[5] {
        1 => u16::from_le_bytes([bytes[18], bytes[19]]),
        2 => u16::from_be_bytes([bytes[18], bytes[19]]),
        _ => return Err("ELF 字节序标记无效".into()),
    };
    if machine != expected_machine {
        return Err(format!(
            "ELF 架构不匹配:machine={machine}, expected={expected_machine}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod cli_update_tests {
    use super::*;

    #[test]
    fn validates_elf_header_and_architecture() {
        let mut bytes = vec![0u8; 64];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
        assert!(validate_elf_image(&bytes, 62).is_ok());
        assert!(validate_elf_image(&bytes, 183).is_err());
        bytes[0] = 0;
        assert!(validate_elf_image(&bytes, 62).is_err());
    }
}

fn restore_path(app: &AppHandle) -> Option<PathBuf> {
    Some(crate::nova_data_dir(app).join("update-restore.json"))
}

fn write_restore(app: &AppHandle, marker: &RestoreMarker) {
    let Some(path) = restore_path(app) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(marker) {
        let _ = std::fs::write(path, json);
    }
}

/// 抓取当前主窗口状态（替换 exe 前调用），用于重启后原样还原。
fn capture_window_state(app: &AppHandle) -> Option<WindowState> {
    let win = main_window(app)?;
    let minimized = win.is_minimized().unwrap_or(false);
    let maximized = win.is_maximized().unwrap_or(false);
    let visible = win.is_visible().unwrap_or(true);
    let focused = win.is_focused().unwrap_or(false);
    let (x, y) = win.outer_position().map(|p| (p.x, p.y)).unwrap_or((0, 0));
    let (width, height) = win
        .inner_size()
        .map(|s| (s.width, s.height))
        .unwrap_or((0, 0));
    Some(WindowState {
        x,
        y,
        width,
        height,
        maximized,
        minimized,
        visible,
        focused,
    })
}

/// 记录升级重启前的状态（当前会话 + 窗口）。thread_id 为 None 表示当时停在主页。
pub fn write_restore_state(app: &AppHandle, thread_id: Option<&str>) {
    write_restore(
        app,
        &RestoreMarker {
            thread_id: thread_id.map(|s| s.to_string()),
            window: capture_window_state(app),
        },
    );
}

/// 读取并摘除「待恢复会话」字段，保留窗口状态供页面稳定后再恢复。
/// 仅升级重启会写入该标记，普通启动读到 None。
pub fn take_restore_thread(app: &AppHandle) -> Option<String> {
    let path = restore_path(app)?;
    let text = std::fs::read_to_string(&path).ok()?;
    let mut marker: RestoreMarker = serde_json::from_str(&text).ok()?;
    let thread_id = marker.thread_id.take();
    if marker.window.is_some() {
        if let Ok(json) = serde_json::to_string_pretty(&marker) {
            let _ = std::fs::write(&path, json);
        }
    } else {
        let _ = std::fs::remove_file(&path);
    }
    thread_id
}

/// 取主窗口（label 固定为 "main"；兜底取任意一个窗口，避免 label 变化导致取不到）。
fn main_window(app: &AppHandle) -> Option<tauri::WebviewWindow> {
    app.get_webview_window("main")
        .or_else(|| app.webview_windows().into_values().next())
}

/// 读取窗口恢复状态并从标记里摘除（保留 thread_id 供前端后续恢复会话）。
fn take_restore_window(app: &AppHandle) -> Option<WindowState> {
    let path = restore_path(app)?;
    let text = std::fs::read_to_string(&path).ok()?;
    let mut marker: RestoreMarker = serde_json::from_str(&text).ok()?;
    let ws = marker.window.take();
    if marker.thread_id.is_some() {
        // 还留着待恢复会话：改写文件、只去掉 window 字段
        if let Ok(json) = serde_json::to_string_pretty(&marker) {
            let _ = std::fs::write(&path, json);
        }
    } else {
        let _ = std::fs::remove_file(&path);
    }
    ws
}

fn apply_window_state(win: &tauri::WebviewWindow, ws: &WindowState) {
    // 先还原几何：最大化优先，否则按保存的全局坐标+大小（全局坐标天然支持多屏）
    if ws.maximized {
        let _ = win.maximize();
    } else if ws.geometry_valid() {
        let _ = win.set_position(Position::Physical(PhysicalPosition { x: ws.x, y: ws.y }));
        let _ = win.set_size(Size::Physical(PhysicalSize {
            width: ws.width,
            height: ws.height,
        }));
    }
    // 再还原前后台。升级后如果保持完全隐藏，用户看到的就是“程序没重启”；
    // 所以不可见状态改为后台显示，最小化仍按最小化还原。
    if !ws.visible {
        let _ = win.show();
        return;
    }
    if ws.minimized {
        let _ = win.show();
        let _ = win.minimize();
    } else if ws.focused {
        let _ = win.show();
        let _ = win.set_focus();
    } else {
        // 升级前在后台（可见但非前台）：显示但不抢占焦点，避免打断用户当前操作
        let _ = win.show();
    }
}

/// 启动早期调用：升级重启则把窗口还原成更新前的样子；普通启动则正常显示并聚焦。
/// 窗口在配置里以 visible:false 创建，统一由这里决定如何呈现，避免升级重启时的闪烁/抢焦点。
pub fn restore_window_on_launch(app: &AppHandle) {
    let Some(win) = main_window(app) else {
        return;
    };
    match take_restore_window(app) {
        Some(ws) => apply_window_state(&win, &ws),
        None => {
            let _ = win.show();
            let _ = win.set_focus();
        }
    }
}

/// 已暂存且仍然有效（目录里有可执行文件）的更新版本
fn valid_staged(app: &AppHandle) -> Option<StagedMarker> {
    let marker = read_marker(app)?;
    let exe_name = current_exe_name();
    let dir = Path::new(&marker.dir);
    let exe = find_exe(dir, &exe_name)?;
    match validate_staged_exe(&exe) {
        Ok(()) => Some(marker),
        Err(_) => {
            if let Some(path) = marker_path(app) {
                remove_file_if_exists(&path);
            }
            let _ = std::fs::remove_dir_all(dir);
            None
        }
    }
}

/// 本地判断：是否已下载好「比当前版本更新」的暂存版本（不联网，供静默升级判定用）。
/// 返回该版本号；无暂存或暂存不比当前新时返回 None。
pub fn staged_upgrade_version(app: &AppHandle) -> Option<String> {
    let marker = valid_staged(app)?;
    let current = app.package_info().version.to_string();
    match (parse_ver(&marker.version), parse_ver(&current)) {
        (Some(staged), Some(cur)) if staged > cur => Some(marker.version),
        _ => None,
    }
}

fn current_exe_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_else(|| {
            #[cfg(windows)]
            {
                "Nova.exe".into()
            }
            #[cfg(not(windows))]
            {
                "Nova".into()
            }
        })
}

/// 更新检查/下载专用 HTTP 客户端：走系统代理（Windows 注册表 / macOS 网络偏好 /
/// 环境变量 HTTP(S)_PROXY，与浏览器「使用系统代理」一致）。
fn update_http_client(
    user_agent: Option<&str>,
    request_timeout: Option<Duration>,
) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder().connect_timeout(Duration::from_secs(15));
    if let Some(t) = request_timeout {
        builder = builder.timeout(t);
    }
    if let Some(ua) = user_agent {
        builder = builder.user_agent(ua);
    }
    builder.build().map_err(|e| e.to_string())
}

/// 查询最新版本并与当前版本比较（GitHub Releases）
pub async fn check(app: &AppHandle) -> Result<Value, String> {
    let current = app.package_info().version.to_string();
    let client = update_http_client(
        Some(&format!("Nova/{current}")),
        Some(Duration::from_secs(15)),
    )?;
    let resp = client
        .get(github_api_latest())
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("检查更新失败:{e}"))?;
    if resp.status().as_u16() == 404 {
        return Ok(json!({ "current": current, "hasUpdate": false }));
    }
    if !resp.status().is_success() {
        return Err(format!("检查更新失败:HTTP {}", resp.status()));
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("更新信息解析失败:{e}"))?;

    let tag = v["tag_name"].as_str().unwrap_or_default();
    let latest = tag.trim().trim_start_matches('v').to_string();
    if latest.is_empty() {
        return Ok(json!({ "current": current, "hasUpdate": false }));
    }

    let want = asset_name_for(&latest);
    let mut download_url = String::new();
    let mut size = json!(null);
    if let Some(assets) = v["assets"].as_array() {
        for a in assets {
            let name = a["name"].as_str().unwrap_or_default();
            if name.eq_ignore_ascii_case(&want) {
                download_url = a["browser_download_url"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                size = a["size"].clone();
                break;
            }
        }
    }
    if download_url.is_empty() {
        return Ok(json!({ "current": current, "latest": latest, "hasUpdate": false }));
    }

    let has_update = matches!(
        (parse_ver(&latest), parse_ver(&current)),
        (Some(l), Some(c)) if l > c
    );
    let staged = valid_staged(app)
        .map(|m| m.version == latest)
        .unwrap_or(false);
    Ok(json!({
        "current": current,
        "latest": latest,
        "hasUpdate": has_update,
        "staged": has_update && staged,
        "size": size,
        "downloadUrl": download_url,
    }))
}

/// 在解压目录中找新版可执行文件（优先同名；Windows 回退任意 .exe，Unix 回退名为 Nova 的文件）
fn find_exe(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut fallback = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let fname = p.file_name().map(|f| f.to_string_lossy().to_string());
            let Some(fname) = fname else { continue };
            if fname.eq_ignore_ascii_case(name) {
                return Some(p);
            }
            #[cfg(windows)]
            if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
                fallback.get_or_insert(p);
            }
            #[cfg(not(windows))]
            if fname.eq_ignore_ascii_case("Nova") || fname.eq_ignore_ascii_case("nova") {
                fallback.get_or_insert(p);
            }
        }
    }
    fallback
}

/// 把解压目录里 exe 之外的文件也拷到安装目录(尽力而为,失败忽略)
fn copy_extras(from: &Path, to: &Path, skip: &Path) {
    let mut stack = vec![from.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p == skip {
                continue;
            }
            let Ok(rel) = p.strip_prefix(from) else {
                continue;
            };
            let dest = to.join(rel);
            if p.is_dir() {
                let _ = std::fs::create_dir_all(&dest);
                stack.push(p);
            } else {
                let _ = std::fs::copy(&p, &dest);
            }
        }
    }
}

fn ensure_install_dir_writable(target: &Path) -> Result<(), String> {
    let dir = target.parent().ok_or("安装目录不可用")?;
    let probe = dir.join(format!(
        ".nova-update-write-test-{}.tmp",
        std::process::id()
    ));
    std::fs::write(&probe, b"ok")
        .map_err(|e| format!("安装目录无写入权限，请用管理员权限运行后再更新:{e}"))?;
    let _ = std::fs::remove_file(probe);
    Ok(())
}

fn arg_value(args: &[String], key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    let mut i = 0;
    while i < args.len() {
        if args[i] == key {
            return args.get(i + 1).cloned();
        }
        if let Some(value) = args[i].strip_prefix(&prefix) {
            return Some(value.to_string());
        }
        i += 1;
    }
    None
}

fn restore_old_exe(target: &Path, old: &Path) {
    remove_file_if_exists(target);
    let _ = std::fs::rename(old, target);
}

fn spawn_installed_app(target: &Path) -> Result<(), String> {
    let install_dir = target.parent().ok_or("target has no parent")?;
    let mut cmd = Command::new(target);
    cmd.current_dir(install_dir);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
        // 新实例先等 helper 退出，再清理 .old 并进入正常启动流程。
        cmd.env(RESTART_PARENT_PID, std::process::id().to_string());
    }
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("启动版本失败:{e}"))
}

fn restart_after_failed_update(args: &[String]) -> Result<(), String> {
    let target = PathBuf::from(arg_value(args, "--target").ok_or("missing --target")?);
    if !target.exists() {
        let old = PathBuf::from(arg_value(args, "--old").ok_or("missing --old")?);
        if old.exists() {
            std::fs::rename(&old, &target).map_err(|e| format!("恢复旧版本失败:{e}"))?;
        }
    }
    spawn_installed_app(&target)
}

fn apply_update_from_helper_args(args: &[String]) -> Result<(), String> {
    let target = PathBuf::from(arg_value(args, "--target").ok_or("missing --target")?);
    let source = PathBuf::from(arg_value(args, "--source").ok_or("missing --source")?);
    let stage_dir = PathBuf::from(arg_value(args, "--stage-dir").ok_or("missing --stage-dir")?);
    let marker = PathBuf::from(arg_value(args, "--marker").ok_or("missing --marker")?);
    let old = PathBuf::from(arg_value(args, "--old").ok_or("missing --old")?);
    let error_log = PathBuf::from(arg_value(args, "--error-log").ok_or("missing --error-log")?);
    let install_dir = target.parent().ok_or("target has no parent")?.to_path_buf();
    let source_len = std::fs::metadata(&source)
        .map_err(|e| format!("读取新版文件失败:{e}"))?
        .len();

    if !target.exists() && old.exists() {
        std::fs::rename(&old, &target).map_err(|e| format!("恢复旧版本失败:{e}"))?;
    }
    remove_file_if_exists(&old);

    let started = std::time::Instant::now();
    loop {
        match std::fs::rename(&target, &old) {
            Ok(()) => break,
            Err(e) => {
                if started.elapsed() >= Duration::from_secs(120) {
                    return Err(format!("等待旧版本退出失败:{e}"));
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }

    match std::fs::copy(&source, &target) {
        Ok(written) if written == source_len => {}
        Ok(written) => {
            restore_old_exe(&target, &old);
            return Err(format!("新版文件写入不完整:{written}/{source_len}"));
        }
        Err(e) => {
            restore_old_exe(&target, &old);
            return Err(format!("写入新版本失败:{e}"));
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&target) {
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o755);
            let _ = std::fs::set_permissions(&target, perms);
        }
    }

    let target_len = match std::fs::metadata(&target) {
        Ok(meta) => meta.len(),
        Err(e) => {
            restore_old_exe(&target, &old);
            return Err(format!("校验新版本失败:{e}"));
        }
    };
    if target_len != source_len {
        restore_old_exe(&target, &old);
        return Err(format!("新版文件校验失败:{target_len}/{source_len}"));
    }

    copy_extras(&stage_dir, &install_dir, &source);

    if let Err(e) = spawn_installed_app(&target) {
        restore_old_exe(&target, &old);
        return Err(e);
    }

    remove_file_if_exists(&marker);
    remove_file_if_exists(&error_log);
    let _ = std::fs::remove_dir_all(&stage_dir);
    Ok(())
}

/// 新版 exe 被旧版当作更新助手启动时，执行替换并退出，不进入 GUI。
pub fn maybe_run_apply_helper() -> bool {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some(APPLY_UPDATE_ARG) {
        return false;
    }
    if let Err(e) = apply_update_from_helper_args(&args[2..]) {
        let mut detail = e;
        if let Err(restart_error) = restart_after_failed_update(&args[2..]) {
            detail.push_str(&format!("\n恢复启动旧版本也失败:{restart_error}"));
        }
        if let Some(path) = arg_value(&args[2..], "--error-log") {
            let _ = std::fs::write(path, detail);
        }
        std::process::exit(1);
    }
    true
}

fn spawn_update_helper(
    helper_exe: &Path,
    current_exe: &Path,
    extract_dir: &Path,
    marker_path: &Path,
    old_path: &Path,
    error_log: &Path,
) -> Result<(), String> {
    remove_file_if_exists(error_log);
    let mut cmd = Command::new(helper_exe);
    cmd.arg(APPLY_UPDATE_ARG)
        .arg("--target")
        .arg(current_exe)
        .arg("--source")
        .arg(helper_exe)
        .arg("--stage-dir")
        .arg(extract_dir)
        .arg("--marker")
        .arg(marker_path)
        .arg("--old")
        .arg(old_path)
        .arg("--error-log")
        .arg(error_log);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("启动更新助手失败:{e}"))
}

/// 静默下载并解压最新版本到暂存目录（不替换、不重启）。
/// 已暂存过同版本则直接返回，避免重复下载。
pub async fn download_and_stage(app: AppHandle) -> Result<Value, String> {
    let _operation = UPDATE_OPERATION_LOCK.lock().await;
    let info = check(&app).await?;
    if !info["hasUpdate"].as_bool().unwrap_or(false) {
        return Ok(json!({ "ready": false, "hasUpdate": false }));
    }
    let latest = info["latest"].as_str().unwrap_or_default().to_string();

    // 已暂存好同版本：跳过下载，直接就绪。
    if let Some(marker) = valid_staged(&app) {
        if marker.version == latest {
            return Ok(json!({ "ready": true, "version": latest }));
        }
    }

    let url = info["downloadUrl"]
        .as_str()
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "缺少下载地址".to_string())?;

    let emit = |phase: &str, downloaded: u64, total: u64| {
        let _ = app.emit(
            EV_PROGRESS,
            json!({ "phase": phase, "downloaded": downloaded, "total": total, "version": latest }),
        );
    };

    // 1. 下载（流式，带进度；走系统代理；不设整请求超时，大包由分块进度推进）
    let client = update_http_client(None, None)?;
    let mut resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("下载失败:{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载失败:HTTP {}", resp.status()));
    }
    let total = resp
        .content_length()
        .or_else(|| info["size"].as_u64())
        .unwrap_or(0);
    let channel = update_channel();
    let tmp_zip = std::env::temp_dir().join(format!("{channel}-update-{latest}.zip"));
    let mut file = std::fs::File::create(&tmp_zip).map_err(|e| format!("创建临时文件失败:{e}"))?;
    let mut downloaded = 0u64;
    let mut last = std::time::Instant::now();
    emit("downloading", 0, total);
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("下载中断:{e}"))? {
        file.write_all(&chunk)
            .map_err(|e| format!("写入失败:{e}"))?;
        downloaded += chunk.len() as u64;
        if last.elapsed().as_millis() >= 80 {
            emit("downloading", downloaded, total);
            last = std::time::Instant::now();
        }
    }
    drop(file);
    emit("downloading", downloaded, total.max(downloaded));

    // 2. 解压到暂存目录（按版本号隔离）
    emit("extracting", downloaded, total.max(downloaded));
    let extract_dir = std::env::temp_dir()
        .join(format!("{channel}-update-staged"))
        .join(&latest);
    let _ = std::fs::remove_dir_all(&extract_dir);
    std::fs::create_dir_all(&extract_dir).map_err(|e| e.to_string())?;
    {
        let zip_path = tmp_zip.clone();
        let dir = extract_dir.clone();
        tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
            let f = std::fs::File::open(&zip_path).map_err(|e| e.to_string())?;
            let mut archive = zip::ZipArchive::new(f).map_err(|e| format!("zip 损坏:{e}"))?;
            archive.extract(&dir).map_err(|e| format!("解压失败:{e}"))
        })
        .await
        .map_err(|e| e.to_string())??;
    }
    let _ = std::fs::remove_file(&tmp_zip);

    // 校验暂存目录里确有可执行文件，然后写 marker。
    let exe_name = current_exe_name();
    let Some(staged_exe) = find_exe(&extract_dir, &exe_name) else {
        let _ = std::fs::remove_dir_all(&extract_dir);
        return Err("安装包里没有可执行文件，更新中止".into());
    };
    if let Err(e) = validate_staged_exe(&staged_exe) {
        let _ = std::fs::remove_dir_all(&extract_dir);
        return Err(format!("{e}，更新中止"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&staged_exe) {
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o755);
            let _ = std::fs::set_permissions(&staged_exe, perms);
        }
    }
    write_marker(
        &app,
        &StagedMarker {
            version: latest.clone(),
            dir: extract_dir.to_string_lossy().to_string(),
        },
    );
    emit("staged", downloaded, total.max(downloaded));
    Ok(json!({ "ready": true, "version": latest }))
}

/// 应用已暂存的更新：替换 exe 并重启。
pub async fn apply_staged(app: AppHandle) -> Result<(), String> {
    let _operation = UPDATE_OPERATION_LOCK.lock().await;
    let marker = valid_staged(&app).ok_or("没有已下载好的更新")?;
    let extract_dir = PathBuf::from(&marker.dir);
    let marker_file = marker_path(&app).ok_or("更新标记路径不可用")?;
    let error_log = crate::nova_data_dir(&app).join("update-error.log");

    let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe_name = current_exe
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(current_exe_name);
    let new_exe = find_exe(&extract_dir, &exe_name).ok_or("暂存目录里没有可执行文件")?;
    validate_staged_exe(&new_exe)?;
    ensure_install_dir_writable(&current_exe)?;

    let _ = app.emit(
        EV_PROGRESS,
        json!({ "phase": "applying", "downloaded": 1, "total": 1, "version": marker.version }),
    );

    // 起新实例前，先杀掉本进程拉起的所有后端进程树（ACP agents/Codex 及其 node/shell 后代）。
    // 否则静默/手动升级重启后，旧后端及其 spawn 的 shell 会变孤儿残留——多次自动升级会越堆越多，
    // 塞满 shell 通道。虽然随后的 app.exit(0) 也会触发 Exit 清理，但那时新实例已启动、时序靠后，
    // 这里提前同步清干净更稳妥。
    if let Some(state) = app.try_state::<crate::AppState>() {
        crate::shutdown_agent_processes(&state).await;
        // 会话持久化已改为后台节流落盘：新实例马上要读 threads.json，
        // 这里先把可能还没 flush 的脏数据同步写盘，避免升级丢最近几百毫秒的记录
        state.store.lock().unwrap().save_now();
    }

    // 让暂存目录里的新版先进入 helper 模式。旧进程退出并释放文件锁后，
    // helper 再完成替换、校验和正式重启。
    let old = current_exe.with_file_name(format!("{exe_name}.old"));
    spawn_update_helper(
        &new_exe,
        &current_exe,
        &extract_dir,
        &marker_file,
        &old,
        &error_log,
    )?;

    // helper 已成功启动，再记录会话和窗口状态。前置校验失败时不留下假的升级恢复标记。
    let active = app
        .try_state::<crate::AppState>()
        .map(|state| state.active_thread.lock().unwrap().clone())
        .unwrap_or(None);
    write_restore_state(&app, active.as_deref());

    let _ = app.emit(
        EV_PROGRESS,
        json!({ "phase": "restarting", "downloaded": 1, "total": 1, "version": marker.version }),
    );
    app.exit(0);
    std::process::exit(0);
}

/// 启动时清理上次更新留下的旧版本文件
pub fn cleanup_old() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else { return };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".old") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
