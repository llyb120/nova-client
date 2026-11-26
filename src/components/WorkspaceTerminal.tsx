import { createEffect, createSignal, onSettled, Show, untrack } from "solid-js";
import { attachTerminal, closeTerminalTab, createTerminalTab, getTerminalGroup, setTerminalGroupFontSize, type TerminalTab } from "../terminalSessions";
import "@xterm/xterm/css/xterm.css";
import "./WorkspaceTerminal.css";

function TerminalSurface(props: { tab: TerminalTab }) {
  let host!: HTMLDivElement;
  onSettled(() => attachTerminal(props.tab, host));
  return (
    <div
      ref={host}
      class="workspace-terminal-surface"
      role="tabpanel"
      aria-label={props.tab.title()}
      onMouseDown={(event) => {
        const target = event.target as HTMLElement | null;
        if (target === host) {
          event.preventDefault();
          props.tab.terminal.focus();
        }
      }}
      onContextMenu={(event) => {
        event.preventDefault();
        if (props.tab.terminal.hasSelection() && navigator.clipboard) {
          void navigator.clipboard
            .writeText(props.tab.terminal.getSelection())
            .then(() => props.tab.terminal.clearSelection())
            .catch((error) => props.tab.setError(String(error)));
        }
      }}
    />
  );
}
export type TerminalAgentTone = "idle" | "running" | "waiting" | "done" | "error";

export default function WorkspaceTerminal(props: {
  threadId?: string;
  cwd: string;
  initCommand?: string;
  statusSummary?: string;
  agentStateLabel?: string;
  agentStateTone?: TerminalAgentTone;
  fontSize?: number;
}) {
  const group = untrack(() => getTerminalGroup(props.threadId ? `thread:${props.threadId}` : "home"));
  const [error, setError] = createSignal("");
  const active = () => group.tabs()[0];
  const showingAgentState = () => {
    const tab = active();
    return !!tab && tab.status() === "running" && !tab.error() && !!props.agentStateLabel;
  };
  const statusLabel = () => {
    const tab = active();
    if (!tab) return "无终端";
    const status = tab.status();
    if (status === "error") return "启动失败";
    if (tab.error()) return "错误";
    if (status === "running" && props.agentStateLabel) return props.agentStateLabel;
    return { starting: "启动中", running: "运行中", exited: "已退出" }[status];
  };
  const tabOptions = () => ({
    initCommand: props.initCommand,
    fontSize: props.fontSize,
  });
  const add = (allowResume = false) => {
    if (group.tabs().length > 0) return;
    try { setError(""); createTerminalTab(group, props.threadId ?? null, props.cwd, { ...tabOptions(), allowResume }); }
    catch (error) { setError(String(error)); }
  };
  const restart = () => {
    const current = active();
    if (current) {
      void closeTerminalTab(group, current)
        .then(() => {
          if (!group.tabs().length) add(true);
        })
        .catch(error => setError(String(error)));
    } else {
      add(true);
    }
  };
  createEffect(() => props.fontSize, size => {
    if (typeof size === "number" && Number.isFinite(size)) untrack(() => setTerminalGroupFontSize(group, size));
  });
  const initSummary = () => {
    if (props.statusSummary) return props.statusSummary;
    const firstLine = (props.initCommand ?? "").split(/\r?\n/).map(line => line.trim()).find(Boolean) ?? "";
    if (!firstLine) return "终端配置可在设置中修改";
    return `默认命令：${firstLine.length > 28 ? `${firstLine.slice(0, 28)}…` : firstLine}`;
  };
  onSettled(() => { if (!group.tabs().length) add(true); });
  return <section class="workspace-terminal" aria-label="交互式终端">
    <Show when={error() || active()?.error()}><p class="workspace-error" role="alert">{error() || active()?.error()}</p></Show>
    <Show keyed when={active()} fallback={<div class="workspace-empty"><button onClick={() => add(true)}>启动终端</button></div>}>
      {tab => <TerminalSurface tab={tab} />}
    </Show>
    <div class="workspace-terminal-status">
      <span title={active()?.cwd || props.cwd}>{active()?.cwd || props.cwd || "默认目录"}</span>
      <span
        class={[
          "workspace-terminal-state",
          showingAgentState() && props.agentStateTone ? `tone-${props.agentStateTone}` : "",
        ]}
        role="status"
      >
        {statusLabel()}
      </span>
      <span title={props.statusSummary || props.initCommand?.trim() || "终端配置可在设置中修改"}>{initSummary()}</span>
      <span class="workspace-terminal-status-actions">
        <Show when={active()?.status() === "exited" || active()?.status() === "error"}>
          <button type="button" class="workspace-status-btn" aria-label="重启终端" title="重新启动当前终端" onClick={restart}>重启</button>
        </Show>
        <button type="button" class="workspace-status-btn" aria-label="清空终端显示" disabled={!active()} onClick={() => { active()?.terminal.clear(); active()?.terminal.focus(); }}>清空</button>
      </span>
    </div>
  </section>;
}
