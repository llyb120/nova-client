import DOMPurify from "dompurify";
import { marked } from "marked";
import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createSignal, onCleanup, untrack } from "solid-js";
import { api } from "../ipc";
import { state } from "../store";

marked.setOptions({ gfm: true, breaks: true });

// 流式输出时 props.text 持续增长，若每个 delta 都重新 parse 整段 Markdown 并替换
// 整棵 innerHTML，长消息会明显卡顿（开销随长度增长）。三层优化：
// 1. 匀速出字：delta 是一坨一坨到达的（网络/后端合批），直接显示会「一顿一顿蹦字」。
//    这里维护一个逐步逼近目标文本的 shown 指针，每 TICK_MS 放出一小步（步长随积压自适应），
//    把突发的整块文本摊成连续的打字机效果，即使 delta 间有间隔也在持续出字。
// 2. 增量渲染：把「离流式尾部足够远、结构上安全」的前缀固化成稳定 DOM（不再重新 parse/重建），
//    每次只重渲染活跃尾部——长消息的单次渲染成本从 O(全文) 降为 O(尾部)。
// 3. 渲染频率天然受 TICK_MS 限制（出字节拍即渲染节拍），无需额外节流。

/** 出字节拍：约 30fps，与后端 delta 合批窗口（33ms）对齐，观感连续且开销可控 */
const TICK_MS = 33;
/** 每拍至少放出的字符数（积压很小时的底速，避免尾巴拖太久） */
const MIN_STEP = 2;
/** 追赶系数：每拍放出 backlog/CATCH_UP 个字符，约 8 拍（~260ms）追平当前积压 */
const CATCH_UP = 8;
/** 积压超过该值直接跳到最新：会话回放/重同步等场景不做动画 */
const JUMP_AT = 3000;

/** 尾部至少积累这么多字符才尝试固化一段前缀（小消息不走增量路径） */
const STABLE_MIN_CHUNK = 1200;
/** 永远保留在活跃尾部的字符数：正在生成的结构（表格/列表/代码块）随时会变，不能固化 */
const TAIL_KEEP = 600;

/** 行首是「延续性结构」（列表/引用/表格/缩进代码）——不能在其前后切分，
 *  否则会打断跨段结构（如 loose list 的连续性、有序列表编号、多行引用） */
const CONTINUATION_LINE = /^(\s{4,}|\s{0,3}([-*+]\s|\d{1,9}[.)]\s|>|\|))/;

function fenceCount(s: string): number {
  const m = s.match(/^\s{0,3}(```|~~~)/gm);
  return m ? m.length : 0;
}

function lastNonEmptyLine(s: string): string {
  let end = s.length;
  while (end > 0) {
    const start = s.lastIndexOf("\n", end - 1) + 1;
    const line = s.slice(start, end);
    if (line.trim() !== "") return line;
    end = start - 1;
  }
  return "";
}

function firstNonEmptyLine(s: string): string {
  let start = 0;
  while (start < s.length) {
    let end = s.indexOf("\n", start);
    if (end === -1) end = s.length;
    const line = s.slice(start, end);
    if (line.trim() !== "") return line;
    start = end + 1;
  }
  return "";
}

/**
 * 在 tail 中找一个安全的固化切分点，返回可固化的前缀长度（0 = 本次不固化）。
 * 切分点选在段落边界（\n\n）上，且保证拆开渲染与整体渲染视觉等价：
 * - 代码围栏在前缀内成对闭合（不把代码块拦腰切断）
 * - 边界两侧都不是列表/引用/表格/缩进代码行
 * - 末尾 TAIL_KEEP 字符不固化（流式生成中的部分随时会变）
 */
function findStableCut(tail: string): number {
  if (tail.length < STABLE_MIN_CHUNK + TAIL_KEEP) return 0;
  let idx = tail.lastIndexOf("\n\n", tail.length - TAIL_KEEP);
  // 只考察离尾部最近的几个边界：都不安全（如身处超长代码块中）就等下轮再试，
  // 避免每次渲染都把整段 tail 的边界扫一遍（退化为 O(n²)）
  for (let attempts = 0; idx > 0 && attempts < 8; attempts++) {
    const cut = idx + 2;
    const prefix = tail.slice(0, cut);
    if (
      fenceCount(prefix) % 2 === 0 &&
      !CONTINUATION_LINE.test(lastNonEmptyLine(prefix)) &&
      !CONTINUATION_LINE.test(firstNonEmptyLine(tail.slice(cut)))
    ) {
      return cut;
    }
    idx = tail.lastIndexOf("\n\n", idx - 1);
  }
  return 0;
}

const COPY_SVG =
  '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>';
const CHECK_SVG =
  '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5"/></svg>';
const FILE_SVG =
  '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"/><path d="M14 2v4a2 2 0 0 0 2 2h4"/></svg>';
const FILE_EXTENSIONS =
  "7z|avif|avi|bmp|c|cc|cfg|conf|cpp|cs|css|csv|docx?|env|fig|gif|go|gz|h|hpp|html?|ico|ini|java|jpe?g|js|json|jsx|lock|log|md|mjs|mov|mp3|mp4|pdf|php|png|pptx?|ps1|psd|py|rar|rb|rs|scss|sh|sql|svg|tar|toml|ts|tsx|txt|vue|wav|webm|webp|xlsx?|xml|ya?ml|zip";
const FILE_REFERENCE_RE = new RegExp(
  String.raw`(?:[A-Za-z]:[\\/]|(?:\.{1,2})?[\\/])?[^\s<>"'\x60()[\]{}，。；：！？、]+(?:[\\/][^\s<>"'\x60()[\]{}，。；：！？、]+)*\.(?:${FILE_EXTENSIONS})(?::\d+)?(?![\w./\\:-])`,
  "gi",
);
const WHOLE_FILE_REFERENCE_RE = new RegExp(String.raw`\.(?:${FILE_EXTENSIONS})(?::\d+)?$`, "i");
const FILE_REFERENCE_CANDIDATE_RE = new RegExp(String.raw`\.(?:${FILE_EXTENSIONS})(?::\d+)?`, "i");
const IMAGE_FILE_RE = /\.(?:avif|bmp|gif|ico|jpe?g|png|svg|webp)$/i;

/** 给所有代码块包一层容器并附上复制按钮（点击事件走容器委托） */
function withCopyButtons(html: string): string {
  return html
    .replace(/<pre>/g, `<div class="codeblock"><button class="code-copy" title="复制">${COPY_SVG}</button><pre>`)
    .replace(/<\/pre>/g, "</pre></div>");
}

function fileReference(pathWithLine: string): HTMLButtonElement {
  const lineMatch = pathWithLine.match(/:(\d+)$/);
  const path = lineMatch ? pathWithLine.slice(0, -lineMatch[0].length) : pathWithLine;
  const button = document.createElement("button");
  button.type = "button";
  button.className = "md-file-ref";
  button.dataset.path = path;
  if (lineMatch) button.dataset.line = lineMatch[1];
  button.title = IMAGE_FILE_RE.test(path) ? `打开图片 ${path}` : `打开文件 ${pathWithLine}`;
  button.innerHTML = `${FILE_SVG}<span></span>`;
  button.querySelector("span")!.textContent = pathWithLine;
  return button;
}

function withFileReferences(html: string): string {
  const template = document.createElement("template");
  template.innerHTML = html;

  for (const code of template.content.querySelectorAll("code:not(pre code)")) {
    const path = code.textContent?.trim();
    if (path && WHOLE_FILE_REFERENCE_RE.test(path)) code.replaceWith(fileReference(path));
  }
  for (const link of template.content.querySelectorAll<HTMLAnchorElement>("a[href]")) {
    const href = link.getAttribute("href") ?? "";
    if (/^(?:https?:|mailto:|#)/i.test(href)) continue;
    const path = decodeURIComponent(href.replace(/^file:\/+/i, ""));
    if (WHOLE_FILE_REFERENCE_RE.test(path)) link.replaceWith(fileReference(path));
  }

  const walker = document.createTreeWalker(template.content, NodeFilter.SHOW_TEXT);
  const nodes: Text[] = [];
  while (walker.nextNode()) {
    const node = walker.currentNode as Text;
    if (!node.parentElement?.closest("pre, code, a, button, svg")) nodes.push(node);
  }
  for (const node of nodes) {
    const text = node.data;
    FILE_REFERENCE_RE.lastIndex = 0;
    if (!FILE_REFERENCE_RE.test(text)) continue;
    FILE_REFERENCE_RE.lastIndex = 0;
    const fragment = document.createDocumentFragment();
    let end = 0;
    for (const match of text.matchAll(FILE_REFERENCE_RE)) {
      fragment.append(text.slice(end, match.index), fileReference(match[0]));
      end = match.index + match[0].length;
    }
    fragment.append(text.slice(end));
    node.replaceWith(fragment);
  }
  return template.innerHTML;
}

function renderMarkdown(src: string, markFiles: boolean): string {
  if (!src) return "";
  const html = DOMPurify.sanitize(withCopyButtons(marked.parse(src, { async: false }) as string));
  // 大多数回答没有文件路径，先用源文本做廉价预检，避免无意义地构建 template 并遍历整棵 DOM。
  return markFiles && FILE_REFERENCE_CANDIDATE_RE.test(src) ? withFileReferences(html) : html;
}

export function Markdown(props: { text: string; markFiles?: boolean; live?: boolean }) {
  // 平滑出字层：shown 是 props.text 的一个前缀，按节拍逐步追上目标。
  // 初始即为完整文本——历史消息、非流式内容立即全量显示，不做动画。
  const [shown, setShown] = createSignal(props.text);
  let timer: number | undefined;

  const stopTick = () => {
    if (timer !== undefined) {
      window.clearTimeout(timer);
      timer = undefined;
    }
  };

  const tick = () => {
    timer = undefined;
    const target = props.text;
    const cur = untrack(shown);
    if (!target.startsWith(cur)) {
      setShown(target);
      return;
    }
    const backlog = target.length - cur.length;
    if (backlog <= 0) return;
    if (backlog > JUMP_AT) {
      setShown(target);
      return;
    }
    const step = Math.max(MIN_STEP, Math.ceil(backlog / CATCH_UP));
    let end = cur.length + step;
    // 不在代理对（emoji 等）中间断开，避免瞬时渲染出乱码
    const c = target.charCodeAt(end - 1);
    if (c >= 0xd800 && c <= 0xdbff && end < target.length) end += 1;
    setShown(target.slice(0, end));
    if (end < target.length) timer = window.setTimeout(tick, TICK_MS);
  };

  createEffect(() => {
    const target = props.text;
    const cur = untrack(shown);
    if (!target.startsWith(cur)) {
      // 非纯追加（编辑/重同步/切换内容）：立即同步，不做动画
      stopTick();
      setShown(target);
      return;
    }
    // 有新增且当前没有动画在跑：立即放出第一步（首字不等节拍），随后按节拍续
    if (target.length > cur.length && timer === undefined) tick();
  });

  onCleanup(stopTick);

  // 增量渲染状态：stableSrc 为已固化的源文本前缀，其 DOM 只追加、从不重建；尾部每次重渲染
  let stableSrc = "";
  let stableRef: HTMLDivElement | undefined;
  const [tailHtml, setTailHtml] = createSignal("");

  createEffect(() => {
    const full = shown();
    if (!full.startsWith(stableSrc)) {
      // 非纯追加更新（编辑消息 / 漫游重同步 / 组件被复用切换内容）：重置增量状态全量重渲
      stableSrc = "";
      if (stableRef) stableRef.innerHTML = "";
    }
    let tail = full.slice(stableSrc.length);
    const cut = findStableCut(tail);
    if (cut > 0 && stableRef) {
      const chunk = tail.slice(0, cut);
      stableSrc += chunk;
      // 稳定前缀只处理一次，可以在固化时完成较重的文件引用标记。
      stableRef.insertAdjacentHTML("beforeend", renderMarkdown(chunk, !!props.markFiles));
      tail = tail.slice(cut);
    }
    // 流式尾部每 33ms 更新：此时跳过 template/TreeWalker 扫描，结束后 effect 会自动补齐。
    setTailHtml(renderMarkdown(tail, !!props.markFiles && !props.live));
  });

  const onClick = (e: MouseEvent) => {
    const target = e.target as HTMLElement;
    const btn = target.closest<HTMLButtonElement>(".code-copy");
    if (btn) {
      const pre = btn.parentElement?.querySelector("pre");
      if (!pre) return;
      void navigator.clipboard.writeText(pre.innerText);
      btn.innerHTML = CHECK_SVG;
      btn.classList.add("copied");
      setTimeout(() => {
        btn.innerHTML = COPY_SVG;
        btn.classList.remove("copied");
      }, 1200);
      return;
    }

    const link = target.closest<HTMLAnchorElement>("a[href]");
    const href = link?.href;
    if (href && /^https?:\/\//i.test(href)) {
      e.preventDefault();
      void api.openUrl(href).catch((err) => console.error("open url failed", err));
      return;
    }

    const file = target.closest<HTMLButtonElement>(".md-file-ref");
    const path = file?.dataset.path;
    const id = state.currentId;
    if (!path || !id) return;
    const action = IMAGE_FILE_RE.test(path)
      ? api.openFileDefault(id, path)
      : api.openInEditor(id, path, file.dataset.line ? Number(file.dataset.line) : undefined);
    void action.catch((err) => void message(String(err), { kind: "error" }));
  };

  // md-seg 为 display:contents（不产生盒子），布局与单容器完全一致。
  // 稳定段直接经 insertAdjacentHTML 追加 DOM（已渲染的节点从不重建），尾部走响应式重渲。
  return (
    <div class="markdown" onClick={onClick}>
      <div class="md-seg" ref={stableRef} />
      <div class="md-seg" innerHTML={tailHtml()} />
    </div>
  );
}
