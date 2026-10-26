import { For, Index, Match, Show, Switch, createEffect, createMemo, createSignal, on, onCleanup } from "solid-js";
import { fileUriPath } from "../ipc";
import { createHtmlPromptImage } from "../createHtmlPromptImage";
import { processLiveLines, processSegments, processSummary, splitTurnBody } from "../processDisplay";
import type { ProcessSegment } from "../processDisplay";
import { promptDisplayOf } from "../promptDisplay";
import { editUserMessage, isExpanded, state, toggleExpanded } from "../store";
import type { Item, ToolContent, ToolItem, UserItem } from "../types";
import { displayToolTitle, stripAnsi, toolDisplayText } from "../utils";
import { openWorkspaceFile } from "../workspaceLinks";
import { createFileContextMenu } from "./FileContextMenu";
import { createImageAttachments, ImageAttachmentStrip } from "./ImageAttachmentStrip";
import { Markdown } from "./Markdown";
import { HtmlPromptText } from "./HtmlPromptText";
import type { Group } from "./TurnGroup";
import { fmtDuration, fmtTokens, turnAvgTokensPerSec, turnTokenTitle } from "./TurnGroup";
import { IconCheck, IconChevron, IconCopy, IconFile, IconPaperclip, IconPencil, IconRefresh, toolIcon } from "./icons";
import "./HtmlTurn.css";

type HtmlTurnProps = {
  group: Group;
  running: boolean;
  readOnly?: boolean;
  preview: boolean;
  threadId: string | null;
  onBrowseDetail?: () => void;
  onReturnToCurrent: () => void;
  loadItems?: (ids: number[]) => Promise<void>;
};
type DetailProps = {
  onBrowseDetail?: () => void;
  onFileMenu: (event: MouseEvent, path: string) => void;
};

/** One measured group; its parent owns width, outer scrolling and windowing. */
export function HtmlTurn(props: HtmlTurnProps) {
  const fileMenu = createFileContextMenu();
  const body = createMemo(() => splitTurnBody(props.group.body, !!props.group.turn));
  const active = () => props.running && !props.group.turn;
  const turnKey = () => `turn-${props.group.turn?.id ?? props.group.user?.id ?? body().process[0]?.id ?? 0}`;
  const turnOpen = () => isExpanded(turnKey(), body().process.some(item =>
    isExpanded(item.type === "tool" ? `tool-${item.id}` : item.type === "thought" ? `thought-${item.id}` : String(item.id))));
  const foldedTurn = () => !!props.group.turn && body().process.length > 0;
  const deferredIds = createMemo(() => props.group.body.filter(item => item.deferred).map(item => item.id));
  const [loadError, setLoadError] = createSignal("");
  const [loading, setLoading] = createSignal(false);
  let loadGeneration = 0;
  onCleanup(() => { loadGeneration++; });
  const load = async () => {
    if (!props.loadItems || !deferredIds().length) return;
    const generation = ++loadGeneration;
    setLoadError("");
    setLoading(true);
    try { await props.loadItems(deferredIds()); }
    catch (error) { if (generation === loadGeneration) setLoadError(String(error)); }
    finally { if (generation === loadGeneration) setLoading(false); }
  };
  createEffect(on(() => `${props.threadId}:${deferredIds().join(",")}`, () => { void load(); }));
  const turnLabel = () => {
    const turn = props.group.turn;
    if (!turn) return "";
    const rate = turnAvgTokensPerSec(turn);
    return ["已处理", fmtDuration(turn.durationMs), turn.totalTokens ? `· ${fmtTokens(turn.totalTokens)} tokens` : "",
      rate != null ? `· 平均输出 ${fmtTokens(rate)} tok/s` : ""].filter(Boolean).join(" ");
  };

  return <section class="html-turn" aria-label="对话轮次">
    <Show when={!deferredIds().length} fallback={
      <div class="ht-history" data-history-loading aria-busy={loading()}>
        <For each={deferredIds()}>{id => <span data-item-id={id} />}</For>
        <button type="button" disabled={loading() || !props.loadItems} onClick={() => void load()}>
          {loadError() ? "历史内容加载失败，点击重试" : "正在加载历史内容…（点击重试）"}
        </button>
        <Show when={loadError()}><span role="alert">{loadError()}</span></Show>
      </div>
    }>
      {/* Boolean Show, not keyed by group/user objects: streaming replacements must not destroy an editor. */}
      <Show when={!!props.group.user}>
        <HtmlUser item={props.group.user!} running={props.running} readOnly={props.readOnly}
          threadId={props.threadId} onBrowseDetail={props.onBrowseDetail} onFileMenu={fileMenu.open} />
      </Show>
      <Show when={!!props.group.turn}>
        <span class="ht-turn-marker" data-item-id={props.group.turn!.id} />
      </Show>
      <Show when={!!props.group.turn?.actualModel}>
        <div class="ht-model">实际模型：{props.group.turn!.actualModel}</div>
      </Show>
      <Show when={foldedTurn()}>
        <button type="button" class="ht-fold ht-turn-fold" title={turnTokenTitle(props.group.turn)} aria-expanded={turnOpen()}
          onClick={() => { props.onBrowseDetail?.(); toggleExpanded(turnKey(), !turnOpen()); }}>
          <span>{turnLabel()}</span><IconChevron size={12} open={turnOpen()} />
        </button>
      </Show>
      {/* A closed finished process mounts no item/Markdown/tool bodies. */}
      <Show when={!foldedTurn() || turnOpen()}>
        <div classList={{ "ht-turn-process": foldedTurn() }}>
          <HtmlProcess items={body().process} live={active()} onBrowseDetail={props.onBrowseDetail} onFileMenu={fileMenu.open} />
        </div>
      </Show>
      <HtmlItems items={body().conclusion} onBrowseDetail={props.onBrowseDetail} onFileMenu={fileMenu.open} />
    </Show>
    <fileMenu.Menu />
  </section>;
}

function HtmlUser(props: DetailProps & { item: UserItem; running: boolean; readOnly?: boolean; threadId: string | null }) {
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [sending, setSending] = createSignal(false);
  const [error, setError] = createSignal("");
  const [copied, setCopied] = createSignal(false);
  const attachments = createImageAttachments();
  let copyTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(copyTimer));
  createEffect(on(() => `${props.threadId}:${props.item.id}`, () => {
    setEditing(false); setDraft(""); setError(""); setCopied(false); clearTimeout(copyTimer); attachments.clear();
  }, { defer: true }));
  const sysKey = () => `user-sys-${props.item.id}`;
  const textKey = () => `user-text-${props.item.id}`;
  const typed = createMemo(() => promptDisplayOf(props.item.text));
  const source = () => typed() !== undefined && !isExpanded(sysKey()) ? typed()! : props.item.text;
  const long = () => source().length > 4000;
  const text = () => long() && !isExpanded(textKey()) ? source().slice(0, 1000).replace(/[\uD800-\uDBFF]$/, "") + "…" : source();
  const canEdit = () => !props.running && !props.readOnly && !!props.threadId;
  const beginEdit = () => {
    props.onBrowseDetail?.();
    setDraft(props.item.text); attachments.set(props.item.images ?? []); setError(""); setEditing(true);
  };
  const send = async (original = false) => {
    if (!canEdit() || sending() || props.threadId !== state.currentId) return;
    const text = original ? props.item.text : draft().trim();
    const images = original ? props.item.images ?? [] : attachments.images();
    if (!text.trim() && !images.length) return;
    setSending(true); setError("");
    try { await editUserMessage(props.item.id, text, images); setEditing(false); }
    catch (error) { setError(String(error)); }
    finally { setSending(false); }
  };
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(props.item.text);
      setCopied(true); clearTimeout(copyTimer); copyTimer = setTimeout(() => setCopied(false), 1200);
    } catch (error) { setError(String(error)); }
  };
  return <div class="ht-user" data-item-id={props.item.id}>
    <Show when={editing()} fallback={
      <div class="ht-user-display">
        <div class="ht-user-column">
          <div class="ht-bubble-wrap">
            <div class="ht-user-actions">
              <button type="button" title="复制完整提示词" aria-label="复制完整提示词" classList={{ copied: copied() }} onClick={() => void copy()}>
                <Show when={copied()} fallback={<IconCopy size={13} />}><IconCheck size={13} /></Show>
              </button>
              <Show when={canEdit()}>
                <button type="button" title="原样重发此消息" aria-label="原样重发此消息" disabled={sending()} onClick={() => void send(true)}><IconRefresh size={13} /></button>
                <button type="button" title="编辑消息" aria-label="编辑消息" disabled={sending()} onClick={beginEdit}><IconPencil size={13} /></button>
              </Show>
            </div>
            <div class="ht-user-bubble">
              <Show when={!!props.item.images?.length}>
                <For each={[props.item.id]}>{() => <div class="ht-bubble-images"><Index each={props.item.images}>{image => {
                  const path = () => image().uri ? fileUriPath(image().uri!) : "";
                  const name = () => image().name || "图片附件";
                  return <Show when={image().mimeType.startsWith("image/")} fallback={
                    <button type="button" class="ht-attachment" disabled={!path()} onClick={() => openWorkspaceFile(path())}
                      onContextMenu={e => { if (path()) props.onFileMenu(e, path()); }}><IconFile size={14} />{image().name}</button>
                  }>{isImage => {
                    // Stable positional img: persisted URI commits only after decode, with its natural size.
                    const shown = createHtmlPromptImage(image);
                    const [failed, setFailed] = createSignal<ReturnType<typeof shown>>();
                    const broken = () => failed() === shown();
                    const label = () => broken() ? `${name()}（图片无法加载）` : name();
                    return <img src={shown().src} width={shown().width} height={shown().height} alt={label()}
                      title={broken() ? `${label()}${path() ? "，点击打开原图" : ""}` : path() ? "点击打开原图" : name()}
                      loading="lazy" draggable={false} onLoad={e => { if (e.currentTarget.naturalWidth > 0) setFailed(undefined); }}
                      onError={e => {
                        const img = e.currentTarget;
                        if (img.currentSrc === shown().src && img.complete && !img.naturalWidth) setFailed(shown());
                      }}
                      classList={{ "ht-clickable": !!path(), "ht-image-broken": broken() }} role={path() ? "button" : undefined} tabIndex={path() ? 0 : undefined}
                      onClick={() => { if (path()) openWorkspaceFile(path()); }}
                      onKeyDown={e => { if (path() && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); openWorkspaceFile(path()); } }}
                      onContextMenu={e => { if (path()) props.onFileMenu(e, path()); }} />;
                  }}</Show>;
                }}</Index></div>}</For>
              </Show>
              <HtmlPromptText text={text()} />
            </div>
          </div>
          <Show when={typed() !== undefined}>
            <button type="button" class="ht-fold ht-user-fold" aria-expanded={isExpanded(sysKey())}
              onClick={() => { props.onBrowseDetail?.(); toggleExpanded(sysKey()); }}>
              <span>{isExpanded(sysKey()) ? "收起系统修改内容" : "该提示词经过系统修改，展开查看完整内容"}</span><IconChevron size={12} open={isExpanded(sysKey())} />
            </button>
          </Show>
          <Show when={long()}>
            <div class="ht-long-prompt">
              <button type="button" class="ht-fold ht-user-fold" aria-expanded={isExpanded(textKey())}
                onClick={() => { props.onBrowseDetail?.(); toggleExpanded(textKey()); }}>
                <span>{isExpanded(textKey()) ? "收起长提示词" : `展开完整提示词（${fmtTokens(source().length)} 字符）`}</span><IconChevron size={12} open={isExpanded(textKey())} />
              </button>
              <button type="button" class="ht-copy-prompt" title="复制完整提示词" aria-label="复制完整提示词" onClick={() => void copy()}>
                <Show when={copied()} fallback={<IconCopy size={13} />}><IconCheck size={13} /></Show>
              </button>
            </div>
          </Show>
        </div>
      </div>
    }>
      <div class="ht-editor" data-editing>
        <ImageAttachmentStrip images={attachments.images()} onRemove={attachments.remove} />
        <textarea aria-label="编辑消息" value={draft()} rows={Math.min(9, Math.max(2, draft().split("\n").length))}
          disabled={sending()} onPaste={attachments.onPaste} onInput={e => setDraft(e.currentTarget.value)}
          onKeyDown={e => {
            if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); void send(); }
            if (e.key === "Escape" && !sending()) setEditing(false);
          }}
          ref={el => queueMicrotask(() => { if (el.isConnected) { el.focus(); el.setSelectionRange(el.value.length, el.value.length); } })} />
        <div class="ht-editor-actions">
          <button type="button" title="添加附件" aria-label="添加附件" disabled={sending()}
            onClick={() => void attachments.pickFiles().catch(error => setError(String(error)))}><IconPaperclip size={16} /></button>
          <span>发送后将从此处重新开始会话</span>
          <button type="button" class="btn secondary small" disabled={sending()} onClick={() => setEditing(false)}>取消</button>
          <button type="button" class="btn primary small" disabled={sending() || !canEdit() || (!draft().trim() && !attachments.images().length)} onClick={() => void send()}>发送</button>
        </div>
      </div>
    </Show>
    <Show when={error()}><div class="ht-error" role="alert">{error()}</div></Show>
  </div>;
}

function HtmlProcess(props: DetailProps & { items: Item[]; live: boolean }) {
  // Key by primitive IDs, not processSegments' freshly allocated objects. New deltas retain DOM/focus/scroll.
  const segments = createMemo(() => new Map(processSegments(props.items).map(segment => [segment.id, segment])));
  const ids = createMemo(() => [...segments().keys()]);
  return <For each={ids()}>{id => {
    const segment = () => segments().get(id)!;
    const items = () => (segment() as Extract<ProcessSegment, { type: "process" }>).items;
    const live = () => props.live && id === ids().at(-1);
    const key = () => `process-${id}-${live() ? "live" : "done"}`;
    const lines = () => live() ? processLiveLines(items()) : [processSummary(items())];
    return <Show when={segment().type === "process"} fallback={
      <HtmlItem item={(segment() as Extract<ProcessSegment, { type: "item" }>).item} active={props.live && id === props.items.at(-1)?.id}
        onBrowseDetail={props.onBrowseDetail} onFileMenu={props.onFileMenu} />
    }>
      <div class="ht-process-segment">
        <button type="button" class="ht-fold ht-process-fold" aria-expanded={isExpanded(key())}
          onClick={() => { props.onBrowseDetail?.(); toggleExpanded(key()); }}>
          <Show when={live()} fallback={
            <span class="ht-process-line" title={lines()[0]}><span>{lines()[0]}</span><IconChevron size={12} open={isExpanded(key())} /></span>
          }>
            {/* At most two source lines suffice: wrap natively, bottom-align and clip to the last two visual lines.
                Keep the chevron out of line measurement, like Canvas's width-40 text + trailing icon. */}
            <span class="ht-live-preview" title={lines().join("\n")}>
              <span class="ht-live-lines"><For each={lines()}>{(line, index) => <span class="ht-live-line">{line}
                <Show when={index() === lines().length - 1}><span class="ht-live-chevron"><IconChevron size={12} open={isExpanded(key())} /></span></Show>
              </span>}</For></span>
            </span>
          </Show>
        </button>
        <Show when={isExpanded(key())}>
          <div class="ht-process-items"><HtmlItems items={items()} onBrowseDetail={props.onBrowseDetail} onFileMenu={props.onFileMenu} /></div>
        </Show>
      </div>
    </Show>;
  }}</For>;
}

function HtmlItems(props: DetailProps & { items: Item[] }) {
  const items = createMemo(() => new Map(props.items.map(item => [item.id, item])));
  return <For each={[...items().keys()]}>{id => <HtmlItem item={items().get(id)!} active={false}
    onBrowseDetail={props.onBrowseDetail} onFileMenu={props.onFileMenu} />}</For>;
}

function HtmlItem(props: DetailProps & { item: Item; active: boolean }) {
  const text = () => "text" in props.item ? props.item.text : "";
  const thoughtKey = () => `thought-${props.item.id}`;
  const thoughtOpen = () => isExpanded(thoughtKey(), props.active);
  return <Switch>
    <Match when={props.item.type === "assistant" && !!text().trim() && text().trim() !== "None"}>
      <div class="ht-assistant" classList={{ "ht-large-markdown": text().length > 32_768 }} data-item-id={props.item.id}><Markdown text={text()} markFiles live={props.active} virtual /></div>
    </Match>
    <Match when={props.item.type === "thought" && !!text().trim() && text().trim() !== "None"}>
      <div class="ht-thought" data-item-id={props.item.id}>
        <Show when={text() !== "思考中…"} fallback={<div class="ht-thinking" role="status">● 思考中…</div>}>
          <button type="button" class="ht-fold ht-thought-fold" aria-expanded={thoughtOpen()}
            onClick={() => { props.onBrowseDetail?.(); toggleExpanded(thoughtKey(), !thoughtOpen()); }}>
            <IconChevron size={12} open={thoughtOpen()} /><span>思考过程</span>
          </button>
          <Show when={thoughtOpen()}><div class="ht-thought-body" classList={{ "ht-large-markdown": text().length > 32_768 }}><Markdown text={text()} markFiles live={props.active} virtual /></div></Show>
        </Show>
      </div>
    </Match>
    <Match when={props.item.type === "tool"}>
      <HtmlTool item={props.item as ToolItem} active={props.active} onBrowseDetail={props.onBrowseDetail} onFileMenu={props.onFileMenu} />
    </Match>
    <Match when={props.item.type === "system"}>
      <div class="ht-system-space" data-item-id={props.item.id}>
        <div class="ht-system" data-level={props.item.type === "system" ? props.item.level : ""}>{text()}</div>
      </div>
    </Match>
  </Switch>;
}

function rawPreview(value: unknown): string {
  if (value == null) return "";
  try { return stripAnsi(typeof value === "string" ? value : JSON.stringify(value, null, 2)).trim().slice(0, 300); }
  catch { return ""; }
}

function contentText(content: ToolContent): string {
  if (content.type !== "content") return "";
  const inner = content.content as { type?: string; text?: string } | undefined;
  return inner?.type === "text" && typeof inner.text === "string" ? toolDisplayText(inner.text) : "";
}

function HtmlTool(props: DetailProps & { item: ToolItem; active: boolean }) {
  const busy = () => props.item.status === "pending" || props.item.status === "in_progress";
  const key = () => `tool-${props.item.id}`;
  const open = () => isExpanded(key(), props.active || busy());
  const hasBody = () => !!(props.item.content.length || props.item.locations.length || props.item.rawInput !== undefined || props.item.rawOutput !== undefined);
  const title = () => displayToolTitle(stripAnsi(props.item.title || props.item.kind));
  const [now, setNow] = createSignal(Date.now());
  createEffect(() => {
    if (!busy()) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 100);
    onCleanup(() => clearInterval(timer));
  });
  const duration = () => {
    const output = props.item.rawOutput;
    const value = output && typeof output === "object" ? (output as Record<string, unknown>).durationMs ?? (output as Record<string, unknown>).duration_ms : undefined;
    const fixed = typeof value === "number" && Number.isFinite(value) && value >= 0;
    const ms = fixed ? value : busy() ? Math.max(0, now() - props.item.ts) : undefined;
    if (ms === undefined || (!fixed && ms < 1000)) return "";
    return ms < 1000 ? `${Math.round(ms)}ms` : ms < 60_000 ? `${(ms / 1000).toFixed(ms < 10_000 ? 2 : 1)}s` : fmtDuration(ms);
  };
  return <div class="ht-tool" data-item-id={props.item.id} data-status={props.item.status}>
    <button type="button" class="ht-tool-line" aria-expanded={hasBody() ? open() : undefined} aria-disabled={!hasBody()}
      title={title()} onClick={() => { if (hasBody()) { props.onBrowseDetail?.(); toggleExpanded(key(), !open()); } }}>
      <span class="ht-tool-icon">{toolIcon(props.item.kind)}</span><span class="ht-tool-title">{title()}</span>
      <Show when={busy() || duration()}><span class="ht-tool-duration" classList={{ busy: busy() }}>{duration()}</span></Show>
      <Show when={busy()}><span class="ht-spinner" role="status" aria-label="进行中" /></Show>
      <Show when={props.item.status === "failed"}><span class="ht-failed" role="img" aria-label="失败" /></Show>
      <Show when={hasBody()}><IconChevron size={12} open={open()} /></Show>
    </button>
    <Show when={hasBody() && open()}>
      <div class="ht-tool-body">
        <Show when={props.item.locations.some(loc => !!loc.path)}>
          <div class="ht-locations"><For each={props.item.locations}>{loc => <Show when={!!loc.path}>
            <button type="button" class="ht-location" title={`在侧栏中打开 ${loc.path}`}
              onClick={() => openWorkspaceFile(loc.path!, loc.line)} onContextMenu={e => props.onFileMenu(e, loc.path!)}>
              {loc.path!.split(/[\\/]/).pop()}{loc.line != null ? `:${loc.line}` : ""}
            </button>
          </Show>}</For></div>
        </Show>
        <Index each={props.item.content}>{content => {
          // Content has no IDs; keep its positional DOM/scroll when streaming replaces a content object.
          const diff = () => content() as { path: string; oldText?: string | null; newText: string };
          const text = createMemo(() => contentText(content()));
          const pathLabel = () => {
            const path = String(diff().path).replace(/\\/g, "/");
            const cwd = state.cwd?.replace(/\\/g, "/").replace(/\/$/, "");
            return cwd && path.toLowerCase().startsWith(cwd.toLowerCase() + "/") ? path.slice(cwd.length + 1) : String(diff().path);
          };
          return <Switch>
            <Match when={content().type === "diff"}>
              <div class="ht-diff">
                <button type="button" class="ht-diff-path" title={`在侧栏中打开 ${diff().path}`}
                  onClick={() => openWorkspaceFile(diff().path)} onContextMenu={e => props.onFileMenu(e, diff().path)}>{pathLabel()}</button>
                <pre class="ht-tool-output" tabIndex={0}>{(diff().oldText ?? "").slice(0, 200) + "\n→\n" + diff().newText.slice(0, 200)}</pre>
              </div>
            </Match>
            <Match when={!!text()}><pre class="ht-tool-output" tabIndex={0}>{text()}</pre></Match>
          </Switch>;
        }}</Index>
        <Show when={!props.item.locations.some(loc => !!loc.path) && !props.item.content.some(content => content.type === "diff" || !!contentText(content))}>
          <Show when={rawPreview(props.item.rawInput) || rawPreview(props.item.rawOutput)}>{preview =>
            <pre class="ht-tool-output ht-raw-output" tabIndex={0}>{preview()}</pre>
          }</Show>
        </Show>
      </div>
    </Show>
  </div>;
}
