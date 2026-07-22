import type { AgentKind } from "./types";

/** agent 展示名（徽标 / 标题 / 提示文案统一用） */
export function agentLabel(kind: AgentKind): string {
  switch (kind) {
    case "alkaid":
      return "Vega";
    case "codex":
      return "Codex";
    case "codebuddy":
      return "CodeBuddy";
    case "claudecode":
      return "Claude Code";
    case "cursor":
      return "Cursor";
    case "opencode":
      return "OpenCode";
    default:
      return "Devin";
  }
}

/** agent 单字徽标（侧边栏紧凑展示）：Vega=V / Devin=D / Codex=C / CodeBuddy=B /
 *  Claude Code=CC / Cursor=CS / OpenCode=OC */
export function agentShort(kind: AgentKind): string {
  switch (kind) {
    case "alkaid":
      return "V";
    case "codex":
      return "C";
    case "codebuddy":
      return "B";
    case "claudecode":
      return "CC";
    case "cursor":
      return "CS";
    case "opencode":
      return "OC";
    default:
      return "D";
  }
}

// 剥离 ANSI 转义序列（颜色/光标控制等），终端输出在 GUI 里直接显示会变乱码
// eslint-disable-next-line no-control-regex
const ANSI_RE = /[\u001b\u009b][[\]()#;?]*(?:[0-9]{1,4}(?:;[0-9]{0,4})*)?[0-9A-ORZcf-nqry=><]/g;
// ESC 字符在传输中丢失时残留的裸 SGR 序列，如 "[31;1m"
const BARE_SGR_RE = /\[[0-9;]{1,16}m/g;

export function stripAnsi(text: string): string {
  return text.replace(ANSI_RE, "").replace(BARE_SGR_RE, "");
}

/** 历史工具记录仍保留内部后端名；只在界面层替换品牌展示。 */
export function displayToolTitle(title: string): string {
  return title.replace(/^Alkaid(\s*\/)/, "Vega$1");
}

const SCRATCH_MARK = "Nova-scratch";

/** 是否为「不使用项目」的临时会话目录 */
export function isScratch(cwd: string): boolean {
  return cwd.includes(SCRATCH_MARK);
}

/** 临时会话的统一父目录（每个会话有独立子目录，侧边栏按父目录归为一组） */
export function scratchParent(cwd: string): string {
  const i = cwd.indexOf(SCRATCH_MARK);
  return i >= 0 ? cwd.slice(0, i + SCRATCH_MARK.length) : cwd;
}

/** 设置弹层打开时屏蔽聊天区的全局文件拖放，避免与 Skills 拖入冲突 */
let fileDropBlocked = false;
export function setFileDropBlocked(blocked: boolean) {
  fileDropBlocked = blocked;
}
export function isFileDropBlocked() {
  return fileDropBlocked;
}
