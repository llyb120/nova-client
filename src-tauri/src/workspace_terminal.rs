//! Main-webview-only interactive PTYs, retained while the workspace is hidden.
use crate::AppState;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use std::{collections::HashMap, io::{Read, Write}, path::PathBuf, sync::{Arc, Condvar, Mutex}};
use tauri::{ipc::Channel, State, Webview};
const MAX_TERMINALS: usize = 32;
const OUTPUT_WINDOW: usize = 128 * 1024;
const INPUT_LIMIT: usize = 64 * 1024;
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TerminalEvent { Data { data: Vec<u8> }, Exit { code: Option<u32> }, Error { message: String } }
#[derive(Default)]
struct Flow { pending: usize, closed: bool }
struct Session {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    pid: Option<u32>, flow: Mutex<Flow>, ready: Condvar,
}
impl Session {
    fn stop(&self) {
        let mut flow = self.flow.lock().unwrap_or_else(|e| e.into_inner());
        if flow.closed { return; }
        flow.closed = true; self.ready.notify_all();
        if let Some(pid) = self.pid { crate::acp::kill_process_tree(pid); }
        let _ = self.killer.lock().unwrap_or_else(|e| e.into_inner()).kill();
    }
    fn finish(&self) {
        self.flow.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        self.ready.notify_all();
    }
    fn acknowledge(&self, bytes: usize) {
        let mut flow = self.flow.lock().unwrap_or_else(|e| e.into_inner());
        flow.pending = flow.pending.saturating_sub(bytes); self.ready.notify_all();
    }
}
#[derive(Clone, Default)]
pub struct TerminalManager { sessions: Arc<Mutex<HashMap<String, Arc<Session>>>> }
impl TerminalManager {
    fn get(&self, id: &str) -> Result<Arc<Session>, String> {
        self.sessions.lock().map_err(|e| e.to_string())?.get(id).cloned().ok_or_else(|| "终端已关闭".into())
    }
    pub fn close_all(&self) {
        let sessions: Vec<_> = self.sessions.lock().unwrap_or_else(|e| e.into_inner()).drain().map(|(_, s)| s).collect();
        for session in sessions { session.stop(); }
    }
    fn close(&self, id: &str) -> Result<(), String> {
        let session = self.sessions.lock().map_err(|e| e.to_string())?.remove(id);
        if let Some(session) = session { session.stop(); }
        Ok(())
    }
}
fn require_main(webview: &Webview) -> Result<(), String> {
    let url = webview.url().map_err(|e| e.to_string())?;
    let local = url.scheme() == "tauri" || (matches!(url.scheme(), "http" | "https") && matches!(url.host_str(), Some("tauri.localhost" | "localhost" | "127.0.0.1" | "[::1]")));
    if webview.label() != "main" || !local { return Err("终端仅允许本地主界面访问".into()); }
    Ok(())
}
fn size(cols: u16, rows: u16) -> Result<PtySize, String> {
    if !(1..=1000).contains(&cols) || !(1..=1000).contains(&rows) { return Err("终端尺寸必须为 1–1000 行/列".into()); }
    Ok(PtySize { cols, rows, pixel_width:0, pixel_height:0 })
}
fn shell_command(shell: &str, args: &[String], cwd: &std::path::Path) -> Result<CommandBuilder, String> {
    let shell = if shell.trim().is_empty() {
        if cfg!(windows) { std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into()) }
        else { std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into()) }
    } else { shell.trim().to_owned() };
    if shell.contains('\0') || args.iter().any(|arg| arg.contains('\0')) { return Err("终端程序或参数含有无效字符".into()); }
    if !cwd.is_dir() { return Err(format!("终端工作目录不存在：{}", cwd.display())); }
    let mut command = CommandBuilder::new(shell);
    command.args(args); command.cwd(cwd);
    command.env("TERM", "xterm-256color"); command.env("COLORTERM", "truecolor");
    Ok(command)
}
fn start(manager: TerminalManager, id: String, command: CommandBuilder, size: PtySize, events: Channel<TerminalEvent>) -> Result<(), String> {
    let mut sessions = manager.sessions.lock().map_err(|e| e.to_string())?;
    if sessions.contains_key(&id) { return Err("终端标识已存在".into()); }
    if sessions.len() >= MAX_TERMINALS { return Err("最多同时运行 32 个终端，请先关闭不用的标签".into()); }
    let pair = native_pty_system().openpty(size).map_err(|e| format!("创建终端失败：{e}"))?;
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let mut child = pair.slave.spawn_command(command).map_err(|e| format!("启动终端失败，请检查程序和参数：{e}"))?;
    drop(pair.slave);
    let session = Arc::new(Session { master:Mutex::new(pair.master), writer:Mutex::new(writer), killer:Mutex::new(child.clone_killer()), pid:child.process_id(), flow:Mutex::new(Flow::default()), ready:Condvar::new() });
    sessions.insert(id.clone(), session.clone()); drop(sessions);
    let live = session.clone(); let cleanup = manager.clone(); let thread_id = id.clone();
    let spawned = std::thread::Builder::new().name("nova-terminal-reader".into()).spawn(move || {
        let mut buffer = [0u8;8192];
        loop {
            let mut flow = live.flow.lock().unwrap_or_else(|e| e.into_inner());
            while !flow.closed && flow.pending >= OUTPUT_WINDOW { flow = live.ready.wait(flow).unwrap_or_else(|e| e.into_inner()); }
            if flow.closed { break; } drop(flow);
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    live.flow.lock().unwrap_or_else(|e| e.into_inner()).pending += count;
                    if events.send(TerminalEvent::Data { data:buffer[..count].to_vec() }).is_err() { live.stop(); break; }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if cfg!(unix) && error.raw_os_error() == Some(5) => break,
                Err(error) => { let _ = events.send(TerminalEvent::Error { message:error.to_string() }); live.stop(); break; }
            }
        }
        let code = child.wait().ok().map(|s| s.exit_code());
        live.finish();
        if let Ok(mut all) = cleanup.sessions.lock() { all.remove(&thread_id); }
        let _ = events.send(TerminalEvent::Exit { code });
    });
    if let Err(error) = spawned { session.stop(); manager.close(&id)?; return Err(format!("启动终端读取线程失败：{error}")); }
    Ok(())
}
#[tauri::command]
pub async fn terminal_create(webview: Webview, state: State<'_, AppState>, manager: State<'_, TerminalManager>, id: String, thread_id: Option<String>, cwd: Option<String>, cols: u16, rows: u16, on_event: Channel<TerminalEvent>) -> Result<(), String> {
    require_main(&webview)?; uuid::Uuid::parse_str(&id).map_err(|_| "无效的终端标识")?;
    let size = size(cols, rows)?;
    let cwd = match thread_id {
        Some(id) => crate::workspace_files::root(&state, &id)?,
        None => match cwd.filter(|p| !p.trim().is_empty()) { Some(path) => PathBuf::from(path), None => std::env::current_dir().map_err(|e| e.to_string())? },
    };
    let (shell, args) = { let settings = state.settings.lock().map_err(|e| e.to_string())?; (settings.terminal_shell.clone(), settings.terminal_args.clone()) };
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || start(manager, id, shell_command(&shell, &args, &cwd)?, size, on_event)).await.map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn terminal_write(webview: Webview, manager: State<'_, TerminalManager>, id: String, data: Vec<u8>) -> Result<(), String> {
    require_main(&webview)?; if data.len() > INPUT_LIMIT { return Err("单次终端输入不能超过 64 KiB".into()); }
    let session = manager.get(&id)?;
    tauri::async_runtime::spawn_blocking(move || { let mut writer = session.writer.lock().map_err(|e| e.to_string())?; writer.write_all(&data).and_then(|_| writer.flush()).map_err(|e| e.to_string()) }).await.map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn terminal_resize(webview: Webview, manager: State<'_, TerminalManager>, id: String, cols: u16, rows: u16) -> Result<(), String> {
    require_main(&webview)?; let size = size(cols, rows)?; let session = manager.get(&id)?;
    tauri::async_runtime::spawn_blocking(move || session.master.lock().map_err(|e| e.to_string())?.resize(size).map_err(|e| e.to_string())).await.map_err(|e| e.to_string())?
}
#[tauri::command]
pub fn terminal_ack(webview: Webview, manager: State<'_, TerminalManager>, id: String, bytes: usize) -> Result<(), String> {
    require_main(&webview)?;
    if let Ok(session) = manager.get(&id) { session.acknowledge(bytes.min(OUTPUT_WINDOW)); }
    Ok(())
}
#[tauri::command]
pub async fn terminal_close(webview: Webview, manager: State<'_, TerminalManager>, id: String) -> Result<(), String> {
    require_main(&webview)?; let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.close(&id)).await.map_err(|e| e.to_string())?
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dimensions_and_arguments() {
        assert!(size(80,24).is_ok()); assert!(size(0,24).is_err()); assert!(size(80,0).is_err()); assert!(size(1001,24).is_err());
        let dir = tempfile::tempdir().unwrap();
        let command = shell_command("custom shell", &["a b".into(), "x;echo unsafe".into()], dir.path()).unwrap();
        let args:Vec<_> = command.get_argv().iter().map(|s| s.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["custom shell", "a b", "x;echo unsafe"]);
        assert!(shell_command("bad\0shell", &[], dir.path()).is_err());
        assert!(shell_command("sh", &[], &dir.path().join("missing")).is_err());
    }
    #[test]
    fn real_pty_output_resize_and_exit() {
        let dir = tempfile::tempdir().unwrap();
        let (shell,args) = if cfg!(windows) { ("cmd.exe", vec!["/D".into(),"/C".into(),"echo nova-pty-probe".into()]) }
          else { ("/bin/sh", vec!["-c".into(),"printf 'nova-pty-probe\\n'; pwd".into()]) };
        let pair = native_pty_system().openpty(size(80,24).unwrap()).unwrap();
        pair.master.resize(size(100,30).unwrap()).unwrap(); assert_eq!(pair.master.get_size().unwrap().cols,100);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut child = pair.slave.spawn_command(shell_command(shell,&args,dir.path()).unwrap()).unwrap(); drop(pair.slave);
        let (tx,rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new(); let mut chunk = [0u8;1024];
            loop { match reader.read(&mut chunk) { Ok(0) => break, Ok(n) => bytes.extend_from_slice(&chunk[..n]), Err(e) if cfg!(unix) && e.raw_os_error() == Some(5) => break, Err(e) => panic!("PTY read failed: {e}") } }
            let _ = tx.send(bytes);
        });
        let output = rx.recv_timeout(std::time::Duration::from_secs(15)); if output.is_err() { let _ = child.kill(); }
        let exit = child.wait().unwrap(); let output = String::from_utf8_lossy(&output.expect("PTY did not reach EOF")).into_owned();
        assert!(output.contains("nova-pty-probe"),"{output:?}"); assert_eq!(exit.exit_code(),0);
        if cfg!(unix) { assert!(output.contains(dir.path().to_str().unwrap()),"{output:?}"); }
    }
}
