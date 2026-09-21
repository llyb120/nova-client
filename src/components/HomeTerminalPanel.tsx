import { homeTerminalCwd } from "../workspaceLayout";
import WorkspaceTerminal from "./WorkspaceTerminal";
import { IconX } from "./icons";
import "./WorkspacePanel.css";
export default function HomeTerminalPanel(props: { onClose: () => void }) {
  return <aside class="workspace-panel home-terminal-panel" aria-label="终端面板">
    <div class="workspace-toolbar"><span style="flex:1">终端</span>
      <button aria-label="收起终端面板" title="收起面板，终端继续运行" onClick={props.onClose}><IconX size={14}/></button>
    </div>
    <WorkspaceTerminal cwd={homeTerminalCwd()}/>
  </aside>;
}
