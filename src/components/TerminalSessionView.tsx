import { message } from "@tauri-apps/plugin-dialog";
import { createSignal, Show } from "solid-js";
import { api } from "../ipc";
import { refreshThreads, setState, state } from "../store";
import { TitleBarPortal } from "./TitleBar";
import { TypewriterText } from "./TypewriterText";
import WorkspaceTerminal from "./WorkspaceTerminal";
import { IconFolder, IconStar, IconTerminal } from "./icons";

export default function TerminalSessionView(props: { threadId: string }) {
  const currentMeta = () => state.threads.find((thread) => thread.id === props.threadId);
  const cwd = () => state.cwd || currentMeta()?.cwd || "";
  const title = () => state.title || currentMeta()?.title || "终端";
  const initCommand = () => {
    const cfg = currentMeta()?.terminalConfig;
    return cfg != null ? (cfg.initCommand ?? "") : (state.settings?.terminalInitCommand ?? "");
  };
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [starUpdating, setStarUpdating] = createSignal(false);

  const startRename = () => {
    setDraft(title());
    setEditing(true);
  };

  const commitRename = async () => {
    setEditing(false);
    const next = draft().trim();
    if (!next || next === title()) return;
    try {
      await api.renameThread(props.threadId, next);
      setState(s => {
        if (s.currentId === props.threadId) s.title = next;
        const thread = s.threads.find((item) => item.id === props.threadId);
        if (thread) thread.title = next;
      });
    } catch (error) {
      void message(String(error), { kind: "error" });
    }
  };

  const toggleStar = async () => {
    const meta = currentMeta();
    if (!meta || starUpdating()) return;
    const starred = !meta.starred;
    setStarUpdating(true);
    setState(s => {
      const thread = s.threads.find((item) => item.id === meta.id);
      if (thread) thread.starred = starred;
    });
    try {
      await api.setThreadStarred(meta.id, starred);
      void refreshThreads();
    } catch (error) {
      setState(s => {
        const thread = s.threads.find((item) => item.id === meta.id);
        if (thread) thread.starred = !starred;
      });
      void message(String(error), { kind: "error" });
    } finally {
      setStarUpdating(false);
    }
  };

  const openExternalTerminal = () => {
    const dir = cwd();
    if (!dir) return;
    void api.openInTerminal(dir).catch((error) => {
      void message(String(error), { kind: "error" });
    });
  };

  const openExplorer = () => {
    const dir = cwd();
    if (!dir) return;
    void api.openInExplorer(dir).catch((error) => {
      void message(String(error), { kind: "error" });
    });
  };

  return (
    <main class="chat terminal-session-view">
      <TitleBarPortal>
        <header class="chat-head">
          <div class="chat-context">
            <Show
              when={editing()}
              fallback={
                <div class="chat-title" onDblClick={startRename} title="双击重命名">
                  <TypewriterText
                    text={title()}
                    title={title()}
                    animate={!!state.titleTyping[props.threadId]}
                  />
                </div>
              }
            >
              <input
                class="chat-title-input"
                value={draft()}
                onInput={(e) => setDraft(e.currentTarget.value)}
                onBlur={() => void commitRename()}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void commitRename();
                  if (e.key === "Escape") setEditing(false);
                }}
                ref={(el) => queueMicrotask(() => el.focus())}
              />
            </Show>
            <Show when={currentMeta() && !currentMeta()?.roamingRole}>
              <button
                type="button"
                class={["chat-star", { starred: !!currentMeta()?.starred }]}
                title={currentMeta()?.starred ? "取消星标" : "加星标并在项目内置顶"}
                aria-pressed={!!currentMeta()?.starred ? "true" : "false"}
                onClick={() => void toggleStar()}
              >
                <IconStar size={15} filled={!!currentMeta()?.starred} />
              </button>
            </Show>
            <span class="agent-badge terminal" title="终端会话">
              终端
            </span>
            <div
              class="chat-cwd"
              title={
                currentMeta()?.worktree
                  ? `源仓库：${currentMeta()!.worktree!.repo}\n分支：${currentMeta()!.worktree!.branch}${
                      cwd() && cwd() !== currentMeta()!.worktree!.repo
                        ? `\n工作目录：${cwd()}`
                        : ""
                    }`
                  : cwd()
              }
            >
              <Show when={currentMeta()?.worktree} fallback={cwd()}>
                <span class="chat-cwd-repo">{currentMeta()!.worktree!.repo}</span>
                <span class="chat-cwd-wt">⎇ {currentMeta()!.worktree!.branch}</span>
              </Show>
            </div>
          </div>
          <div class="chat-actions">
            <button
              type="button"
              class="chat-action-btn"
              title="在外部终端窗口中打开当前目录"
              disabled={!cwd()}
              onClick={openExternalTerminal}
            >
              <IconTerminal size={14} />
              <span>外部终端</span>
            </button>
            <button
              type="button"
              class="chat-action-btn"
              title="在资源管理器中打开当前目录"
              disabled={!cwd()}
              onClick={openExplorer}
            >
              <IconFolder size={14} />
              <span>资源管理器</span>
            </button>
          </div>
        </header>
      </TitleBarPortal>
      <div class="terminal-session-body">
        <WorkspaceTerminal
          threadId={props.threadId}
          cwd={cwd()}
          initCommand={initCommand()}
          fontSize={state.settings?.terminalFontSize ?? 13}
        />
      </div>
    </main>
  );
}
