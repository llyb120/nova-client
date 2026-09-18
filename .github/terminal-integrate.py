from pathlib import Path
import json

def change(path,before,after):
    p=Path(path); s=p.read_text(encoding='utf-8'); assert s.count(before)==1,(path,before[:60],s.count(before)); p.write_text(s.replace(before,after),encoding='utf-8')

change('src/store.ts','  if (unreadRoots.length === 0) return;','''  if (unreadRoots.length === 0) {
    const target = nextRunningThread(
      state.threads.filter(thread => !isPendingThreadId(thread.id)), state.currentId, state.running,
    );
    if (target) { setView("home"); await openThread(target.id); }
    return;
  }''')
p=Path('src/store.ts'); p.write_text('import { nextRunningThread } from "./nextRunningThread";\n'+p.read_text())
change('src/store.ts',' * 口径与侧栏普通模式列表一致：排除训练会话；减少焦虑模式下排除室女座运行链。',' * 未读沿用侧栏普通列表口径；无未读时循环进行中的普通会话（含室女座）。')
change('src/types.ts','  | "openUnread"\n','  | "openUnread"\n  /** 打开 / 收起右侧终端，仅应用内生效。 */\n  | "toggleTerminal"\n')
change('src/types.ts','  editor: string;\n','  editor: string;\n  /** 内嵌终端程序，空为系统默认 shell。 */\n  terminalShell: string;\n  /** 每项是一个独立的启动参数。 */\n  terminalArgs: string[];\n')
change('src/types.ts','循环打开普通模式下有未读轮次的会话；无 target。','优先未读；无未读时循环打开进行中的会话；无 target。')
change('src/sessionShortcuts.ts','/** 运行时合并内置 Esc 终止；剥离历史配置里的 stopSession。 */','''export const DEFAULT_TERMINAL_SHORTCUT: SessionShortcut = {
  id: "default-toggle-terminal", keys: "Ctrl+`", action: "toggleTerminal", target: "",
};
/** 默认终端按键和 Esc；已配置同动作或占用同按键时尊重用户设置。 */''')
change('src/sessionShortcuts.ts','    ...shortcuts.filter((item) => item.action !== "stopSession"),','''    ...shortcuts.filter((item) => item.action !== "stopSession"),
    ...(!shortcuts.some(item => item.action === "toggleTerminal" ||
      ["ctrl+`", "ctrl+~", "ctrl+shift+~", "ctrl+shift+`"].includes(item.keys.trim().toLowerCase()))
      ? [{ ...DEFAULT_TERMINAL_SHORTCUT }] : []),''')
change('src/sessionShortcuts.ts','  return shortcuts.find((item) => normalizeShortcutKeys(item.keys) === needle) ?? null;','''  const exact = shortcuts.find((item) => normalizeShortcutKeys(item.keys) === needle);
  if (exact) return exact;
  // The ~ keycap is commonly used to describe Ctrl+Backquote, with or without Shift.
  if (event.ctrlKey && !event.altKey && !event.metaKey &&
      (event.code === "Backquote" || event.key === "`" || event.key === "~")) {
    return shortcuts.find(item => item.id === DEFAULT_TERMINAL_SHORTCUT.id &&
      item.action === "toggleTerminal" && item.keys === DEFAULT_TERMINAL_SHORTCUT.keys) ?? null;
  }
  return null;''')
change('src/sessionShortcuts.ts','  onOpenUnread?: () => void;','  onOpenUnread?: () => void;\n  onToggleTerminal?: () => void;')
change('src/sessionShortcuts.ts','    if (!allowed.has(hit.action)) return;','''    if (!allowed.has(hit.action)) return;
    if (event.target instanceof HTMLElement && event.target.closest(".workspace-terminal") &&
        !["toggleTerminal", "openUnread", "newSession"].includes(hit.action)) return;''')
change('src/sessionShortcuts.ts','      hit.action !== "openUnread" &&','      hit.action !== "openUnread" &&\n      hit.action !== "toggleTerminal" &&')
change('src/sessionShortcuts.ts','    if (hit.action === "openUnread") {','''    if (hit.action === "toggleTerminal") {
      event.preventDefault(); event.stopPropagation(); options.onToggleTerminal?.(); return;
    }
    if (hit.action === "openUnread") {''')
change('src/workspaceLayout.ts',"'files' | 'artifacts' | 'git' | 'browser'","'files' | 'artifacts' | 'git' | 'browser' | 'terminal'")
change('src/workspaceLayout.ts',"value === 'browser';","value === 'browser' || value === 'terminal';")
p=Path('src/workspaceLayout.ts');p.write_text('import { createSignal } from "solid-js";\n'+p.read_text()+'\nexport const [homeTerminalCwd, setHomeTerminalCwd] = createSignal("");\n')
change('src/components/HomeView.tsx','import { mountSessionShortcuts } from "../sessionShortcuts";','import { mountSessionShortcuts } from "../sessionShortcuts";\nimport { setHomeTerminalCwd } from "../workspaceLayout";')
change('src/components/HomeView.tsx','  const [agentKind, setAgentKind] = createSignal<AgentKind>(','  createEffect(() => setHomeTerminalCwd(cwd()));\n  const [agentKind, setAgentKind] = createSignal<AgentKind>(')
change('src/App.tsx','import { createEffect, createSignal, onCleanup, onMount, Show }','import { createEffect, createSignal, lazy, onCleanup, onMount, Show, Suspense }')
change('src/App.tsx','import { setWorkspaceLayout } from "./workspaceLayout";','import { setWorkspaceLayout, workspaceLayout } from "./workspaceLayout";\nconst HomeTerminalPanel = lazy(() => import("./components/HomeTerminalPanel"));')
change('src/App.tsx','    allowedActions: ["openUnread", "hideToVirgo"],','    allowedActions: ["openUnread", "hideToVirgo", "toggleTerminal"],')
change('src/App.tsx','    onHideToVirgo: hideCurrentThreadToVirgo,','''    onHideToVirgo: hideCurrentThreadToVirgo,
    onToggleTerminal: () => {
      if (state.threads.find(thread => thread.id === state.currentId)?.roamingRole === "guest") return;
      setWorkspaceLayout({ open: !(workspaceLayout.open && workspaceLayout.mode === "terminal"), mode: "terminal" });
    },''')
change('src/App.tsx','      <Show when={showSettings()}>','''      <Show when={!state.currentId && workspaceLayout.open && workspaceLayout.mode === "terminal"}>
        <Suspense><HomeTerminalPanel /></Suspense>
      </Show>
      <Show when={showSettings()}>''')
change('src/components/WorkspacePanel.tsx','const WorkspaceSheet = lazy(','const WorkspaceTerminal = lazy(() => import("./WorkspaceTerminal"));\nconst WorkspaceSheet = lazy(')
change('src/components/WorkspacePanel.tsx','mode() !== "browser" && (e.ctrlKey','mode() !== "browser" && mode() !== "terminal" && (e.ctrlKey')
change('src/components/WorkspacePanel.tsx','aria-label="产物与项目文件"','aria-label="产物、项目文件与终端"')
change('src/components/WorkspacePanel.tsx','      <button class="workspace-panel-close"','      <button aria-pressed={mode() === "terminal"} title="终端 (Ctrl+`)" onClick={() => { setMode("terminal"); setBrowse(false); }}>终端</button>\n      <button class="workspace-panel-close"')
change('src/components/WorkspacePanel.tsx','    <Show when={mode() === "browser"}><WorkspaceBrowser threadId={threadId} /></Show>','''    <Show when={mode() === "browser"}><WorkspaceBrowser threadId={threadId} /></Show>
    <Show when={mode() === "terminal"}><Suspense fallback={<p role="status">正在加载终端…</p>}><WorkspaceTerminal threadId={threadId} cwd={state.cwd} /></Suspense></Show>''')
change('src/components/WorkspacePanel.tsx','display: mode() === "browser" ? "none"','display: mode() === "browser" || mode() === "terminal" ? "none"')
change('src/components/WorkspacePanel.tsx','display: mode() === "git" || mode() === "browser" ? "none"','display: mode() === "git" || mode() === "browser" || mode() === "terminal" ? "none"')
change('src/components/ChatView.tsx','    setWorkspaceOpen(true);','    setWorkspaceLayout({ open: true, mode: "files" });')
change('src/components/ChatView.tsx','request={workspaceRequest()}','request={workspaceLayout.mode === "terminal" ? null : workspaceRequest()}')
change('src-tauri/src/settings.rs','    pub editor: String,','    pub editor: String,\n    pub terminal_shell: String,\n    pub terminal_args: Vec<String>,')
change('src-tauri/src/settings.rs','            editor: "code".into(),','            editor: "code".into(),\n            terminal_shell: String::new(),\n            terminal_args: Vec::new(),')
change('src/components/SettingsModal.tsx','  const [tab, setTab] = createSignal<SettingsTab>("general");','''  const [tab, setTab] = createSignal<SettingsTab>("general");
  const [terminalShell, setTerminalShell] = createSignal(s?.terminalShell ?? "");
  const [terminalArgs, setTerminalArgs] = createSignal((s?.terminalArgs ?? []).join("\\n"));''')
change('src/components/SettingsModal.tsx','    editor: editor().trim() || "code",','''    editor: editor().trim() || "code",
    terminalShell: terminalShell().trim(),
    terminalArgs: terminalArgs().split(/\\r?\\n/).filter(arg => arg.length > 0),''')
change('src/components/SettingsModal.tsx','                ? "openUnread"\n                : item.action === "insertText"','                ? "openUnread"\n                : item.action === "toggleTerminal" ? "toggleTerminal"\n                : item.action === "insertText"')
change('src/components/SettingsModal.tsx','item.action === "newSession" || item.action === "openUnread" || item.action === "hideToVirgo"','item.action === "newSession" || item.action === "openUnread" || item.action === "toggleTerminal" || item.action === "hideToVirgo"')
change('src/components/SettingsModal.tsx','                                        ? "openUnread"\n                                        : action === "insertText"','                                        ? "openUnread"\n                                        : action === "toggleTerminal" ? "toggleTerminal"\n                                        : action === "insertText"')
change('src/components/SettingsModal.tsx','                              <option value="openUnread">打开未读消息</option>','                              <option value="openUnread">未读 / 进行中会话</option>\n                              <option value="toggleTerminal">打开 / 收起终端</option>')
change('src/components/SettingsModal.tsx','                                  item().action === "openUnread" ||','                                  item().action === "openUnread" ||\n                                  item().action === "toggleTerminal" ||')
change('src/components/SettingsModal.tsx','? "循环打开有未读轮次的普通会话"','? "优先未读；无未读时循环切换进行中的会话"\n                                    : item().action === "toggleTerminal" ? "任意本地页 · 打开 / 收起右侧终端"')
change('src/components/SettingsModal.tsx','默认 Esc 终止当前回合。','默认 Esc 终止当前回合（终端获得焦点时交给终端）；默认 Ctrl+`（~ 键）打开或收起终端，可添加「打开 / 收起终端」动作修改按键。')
change('src/components/SettingsModal.tsx','''              <div class="field">
                <span class="field-label">自动清理过期会话</span>''','''              <label class="field">
                <span class="field-label">默认终端</span>
                <input class="field-input" value={terminalShell()} onInput={e => setTerminalShell(e.currentTarget.value)} placeholder="系统默认（pwsh.exe / cmd.exe / /bin/bash 等）" />
                <span class="field-hint">填写 shell 程序或绝对路径，不含参数或外层引号；留空使用系统默认。新标签在当前会话或首页已选项目目录启动，修改配置不影响已运行的标签。</span>
              </label>
              <label class="field">
                <span class="field-label">终端启动参数（每行一个）</span>
                <textarea class="field-input" rows={3} value={terminalArgs()} onInput={e => setTerminalArgs(e.currentTarget.value)} placeholder="例如 -NoLogo 或 -l" />
                <span class="field-hint">每行原样作为一个参数，带空格的路径无需引号。配置的是 PowerShell、CMD、Bash、Zsh 等 shell，不是外部终端窗口程序。</span>
              </label>
              <div class="field">
                <span class="field-label">自动清理过期会话</span>''')
change('src-tauri/Cargo.toml','wait-timeout = "0.2"','wait-timeout = "0.2"\nportable-pty = "0.9"')
change('src-tauri/src/lib.rs','mod workspace_files;','mod workspace_files;\nmod workspace_terminal;')
change('src-tauri/src/lib.rs','    let builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init());','    let builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init())\n        .manage(workspace_terminal::TerminalManager::default());')
change('src-tauri/src/lib.rs','        .invoke_handler(tauri::generate_handler![','''        .invoke_handler(tauri::generate_handler![
            workspace_terminal::terminal_create,
            workspace_terminal::terminal_write,
            workspace_terminal::terminal_resize,
            workspace_terminal::terminal_ack,
            workspace_terminal::terminal_close,''')
change('src-tauri/src/lib.rs','            if let tauri::RunEvent::Exit = event {','''            if let tauri::RunEvent::WindowEvent { label, event: tauri::WindowEvent::Destroyed, .. } = &event {
                if label == "main" { app.state::<workspace_terminal::TerminalManager>().close_all(); }
            }
            if let tauri::RunEvent::Exit = event {
                app.state::<workspace_terminal::TerminalManager>().close_all();''')
p=Path('package.json');j=json.loads(p.read_text());j['dependencies']['@xterm/xterm']='^5.5.0';j['dependencies']['@xterm/addon-fit']='^0.10.0';j['scripts']['test:terminal']='node scripts/terminal-shortcuts.test.mjs && node scripts/workspace-terminal.test.mjs';p.write_text(json.dumps(j,ensure_ascii=False,indent=2)+'\n')
print('Integrated terminal and running-session fallback into pre-release sources')
