import type { AgentKind } from "./types";

/** agent 展示名；未迁移/未知标识原样显示，不能伪装成 Lyra。 */
export function agentLabel(kind: AgentKind): string {
  return kind === "lyra" ? "Lyra" : `不支持的后端（${kind || "未指定"}）`;
}

/** agent 单字徽标（侧边栏紧凑展示）。 */
export function agentShort(kind: AgentKind): string {
  return kind === "lyra" ? "L" : "?";
}

// 剥离 ANSI 转义序列（颜色/光标控制等），终端输出在 GUI 里直接显示会变乱码
// eslint-disable-next-line no-control-regex
const ANSI_RE = /[\u001b\u009b][[\]()#;?]*(?:[0-9]{1,4}(?:;[0-9]{0,4})*)?[0-9A-ORZcf-nqry=><]/g;
// ESC 字符在传输中丢失时残留的裸 SGR 序列，如 "[31;1m"
const BARE_SGR_RE = /\[[0-9;]{1,16}m/g;

export function stripAnsi(text: string): string {
  return text.replace(ANSI_RE, "").replace(BARE_SGR_RE, "");
}

/** 旧 ACP 记录可能只剩截断的图片编码；仅隐藏显示副本，不改会话原文。
 * ponytail: 旧记录已丢类型，按截断标记和长编码串识别；新记录由后端按图片类型清理。 */
export function toolDisplayText(text: string): string {
  return stripAnsi(text)
    .replace(/data:image\/[\w.+-]+;base64,[A-Za-z0-9+/=_-]+/g, "[图片编码已隐藏]")
    .replace(/^(\[输出过长[^\n]*\]\n)[A-Za-z0-9+/]{4096,}={0,2}/, "$1[已截断的图片编码已隐藏]")
    .trim();
}

/** 历史工具记录可能保留旧后端名；统一为当前品牌展示。 */
export function displayToolTitle(title: string): string {
  return title.replace(/^(?:Alkaid|Vega)(\s*\/)/, "Lyra$1");
}

// ─── 工具调用摘要展示（会话记录工具行使用） ───────────────────────────────────

/** 输入值紧凑展示：剥 ANSI、对象 JSON 化、压空白、限长 */
export function compactToolValue(value: unknown): string {
  if (value == null) return "";
  if (typeof value === "string") return stripAnsi(value).trim();
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  let text: string;
  try {
    text = JSON.stringify(value);
  } catch {
    text = String(value);
  }
  text = stripAnsi(text).replace(/\s+/g, " ").trim();
  return text.length > 180 ? `${text.slice(0, 180)}...` : text;
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

/** Tauri 拖放坐标为物理像素；按最上层命中元素分流到文件面板，避免聊天附件重复接收。 */
export function workspaceFileDropTarget(position: { x: number; y: number }): Element | null {
  return document.elementFromPoint(position.x / window.devicePixelRatio, position.y / window.devicePixelRatio)?.closest('.workspace-panel') ?? null;
}
