import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { attachTerminal, closeTerminalTab, createTerminalTab, getTerminalGroup, type TerminalTab } from "../terminalSessions";
import { IconX } from "./icons";
import "@xterm/xterm/css/xterm.css";
import "./WorkspaceTerminal.css";

function TerminalSurface(props: { tab: TerminalTab }) {
  let host!: HTMLDivElement;
  onMount(() => onCleanup(attachTerminal(props.tab, host)));
  return <div ref={host} class="workspace-terminal-surface" role="tabpanel" aria-label={props.tab.title()} />;
}
export default function WorkspaceTerminal(props: { threadId?: string; cwd: string }) {
  const group = getTerminalGroup(props.threadId ? `thread:${props.threadId}` : "home");
  const [error, setError] = createSignal("");
  const active = () => group.tabs().find(tab => tab.id === group.activeId());
  const statusLabel = () => {
    const tab = active();
    if (!tab) return "无终端";
    const status = tab.status();
    if (status === "error") return "启动失败";
    if (tab.error()) return "错误";
    return { starting: "启动中", running: "运行中", exited: "已退出" }[status];
  };
  const add = () => {
    try { setError(""); createTerminalTab(group, props.threadId ?? null, props.cwd); }
    catch (error) { setError(String(error)); }
  };
  const close = (tab: TerminalTab) => { void closeTerminalTab(group, tab).catch(error => setError(String(error))); };
  onMount(() => { if (!group.tabs().length) add(); });
  return <section class="workspace-terminal" aria-label="交互式终端">
    <div class="workspace-toolbar">
      <div class="workspace-tabs" role="tablist" aria-label="终端标签页" onKeyDown={event => {
        if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
        const tabs = group.tabs(); if (!tabs.length) return;
        event.preventDefault();
        const index = tabs.findIndex(tab => tab.id === group.activeId());
        const next = event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1
          : (index + (event.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
        group.setActiveId(tabs[next].id);
      }}>
        <For each={group.tabs()}>{tab => <div class="workspace-tab" classList={{ active: tab.id === group.activeId() }}>
          <button role="tab" aria-selected={tab.id === group.activeId()} tabindex={tab.id === group.activeId() ? 0 : -1}
            title={`${tab.title()} · ${tab.cwd}`} onClick={() => group.setActiveId(tab.id)}>
            <span>{tab.title()}</span>
          </button>
          <button class="workspace-tab-close" aria-label={`关闭终端 ${tab.title()}`} title="关闭标签并结束此终端进程" onClick={() => close(tab)}><IconX size={12} /></button>
        </div>}</For>
      </div>
      <button aria-label="新建终端" title="按默认终端配置新建标签" onClick={add}>＋</button>
      <button aria-label="清空终端显示" disabled={!active()} onClick={() => active()?.terminal.clear()}>清空</button>
    </div>
    <Show when={error() || active()?.error()}><p class="workspace-error" role="alert">{error() || active()?.error()}</p></Show>
    <Show keyed when={active()} fallback={<div class="workspace-empty"><button onClick={add}>新建终端</button></div>}>
      {tab => <TerminalSurface tab={tab} />}
    </Show>
    <div class="workspace-terminal-status">
      <span title={active()?.cwd || props.cwd}>{active()?.cwd || props.cwd || "默认目录"}</span>
      <span class="workspace-terminal-state" role="status">{statusLabel()}</span>
      <span>默认终端可在设置中修改</span>
    </div>
  </section>;
}
