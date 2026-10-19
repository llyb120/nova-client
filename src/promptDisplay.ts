/**
 * 系统改写提示词（/easy、/plan、/fire 等）的显示别名：会话中默认只显示用户
 * 录入的原文，可展开查看实际发送的完整提示词。仅影响页面显示，发送内容不变。
 */
const STORAGE_KEY = "nova.promptDisplay.v1";
// ponytail: 只在本机 localStorage 保留最近 300 条；换机、清缓存或更早的消息回落为直接显示完整提示词。
const MAX_ENTRIES = 300;

let entries: Map<string, string> | null = null;

/** 按实际发送文本取短键（长度 + FNV-1a），避免把整段长提示词存进 localStorage。 */
function keyOf(text: string): string {
  const s = text.trim();
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 0x01000193);
  return `${s.length}:${(h >>> 0).toString(36)}`;
}

function load(): Map<string, string> {
  if (entries) return entries;
  entries = new Map();
  try {
    const raw: unknown = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "[]");
    if (Array.isArray(raw)) {
      for (const e of raw) if (Array.isArray(e) && typeof e[0] === "string" && typeof e[1] === "string") entries.set(e[0], e[1]);
    }
  } catch {
    // 损坏的缓存只影响折叠显示，直接丢弃。
  }
  return entries;
}

/** 记录实际发送的提示词对应的用户原始输入；两者相同或原始输入为空时不记录。 */
export function rememberPromptDisplay(sent: string, typed: string): void {
  const display = typed.trim();
  if (!display || sent.trim() === display) return;
  const map = load();
  const key = keyOf(sent);
  map.delete(key);
  map.set(key, display);
  while (map.size > MAX_ENTRIES) map.delete(map.keys().next().value!);
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify([...map]));
  } catch {
    // 存储失败仅影响刷新后的折叠显示，不影响发送。
  }
}

/** 蝴蝶助理在后端拼装的投递（taskbar_status/assistant.rs scheduled），不经前端发送，按结构识别。 */
const ASSISTANT_JOB = /^\[蝴蝶助理投递 [^\]\n]+\]\n用户原文：\n([\s\S]*?)\n\n补充上下文（不得扩大用户授权）：\n/;

/** 实际发送内容经过系统改写时，返回用户原始输入；否则 undefined。 */
export function promptDisplayOf(sent: string): string | undefined {
  const job = ASSISTANT_JOB.exec(sent.trimStart())?.[1]?.trim();
  if (job) return job;
  const map = load();
  return map.size ? map.get(keyOf(sent)) : undefined;
}
