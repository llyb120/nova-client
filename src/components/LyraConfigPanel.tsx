import { confirm } from "@tauri-apps/plugin-dialog";
import { createMemo, createSignal, flush, For, onCleanup, onSettled, Show } from "solid-js";
import { createStore, reconcile, snapshot } from "solid-js";
import { api } from "../ipc";
import {
  APIS,
  configFromDraft,
  draftFromConfig,
  hasVariants,
  isLocalPreset,
  modelDraft,
  parseMcpServers,
  providerDraft,
  serializeProvider,
  validate,
  type Kv,
  type LyraDraft,
  type ModelDraft,
  type Preset,
  type ProviderDraft,
} from "../lyraConfig";
import { effortRank } from "../modelEfforts";

const LOGIN_PRESETS = new Set(["devin", "codebuddy", "workbuddy"]);
/** 可在 Nova 里网页登录的预设。 */
const WEB_LOGIN = LOGIN_PRESETS;

/** 本机有可用 CLI 登录就直接用；没有时用浏览器登录。新添加且未登录时自动发起。 */
function LocalLogin(props: { preset: string; autoStart: boolean; onLoggedIn: () => void }) {
  const [status, setStatus] = createSignal<{ ok: boolean; error?: string } | null>(null);
  const [site, setSite] = createSignal("cn");
  const [waiting, setWaiting] = createSignal(false);
  const [error, setError] = createSignal("");
  // Devin 登录后跳回本机；浏览器在另一台机器时由用户粘贴跳转地址。
  const [pasted, setPasted] = createSignal("");
  let cancelled = false;
  onCleanup(() => (cancelled = true));
  const check = async () => {
    const next = await api.lyraLoginStatus(props.preset).catch((e) => ({ ok: false, error: String(e) }));
    setStatus(next);
    return next.ok;
  };
  const login = async () => {
    setError("");
    setWaiting(true);
    setPasted("");
    cancelled = false;
    try {
      const { state, url } = await api.lyraLoginStart(props.preset, site());
      await api.openUrl(url);
      // 与 CLI 一致：最多等 5 分钟。
      for (const end = Date.now() + 5 * 60_000; Date.now() < end && !cancelled; ) {
        await new Promise((r) => setTimeout(r, 1500));
        if (!cancelled && (await api.lyraLoginPoll(props.preset, site(), state, /[?&]code=[^&]+.*state=|state=.*[?&]code=/.test(pasted()) ? pasted() : ""))) {
          if (await check()) props.onLoggedIn();
          return;
        }
      }
      if (!cancelled) setError("登录超时，请重试");
    } catch (e) {
      setError(String(e));
    } finally {
      setWaiting(false);
    }
  };
  onSettled(() => {
    void check().then(ok => { if (!ok && props.autoStart && WEB_LOGIN.has(props.preset)) void login(); });
  });
  return (
    <div class="lyra-fetch">
      <span class="field-hint" role="status">
        {waiting()
          ? "已在浏览器打开登录页，完成登录后自动继续…"
          : status() === null ? "检查登录状态…"
          : status()!.ok ? "已登录（优先使用本机 CLI 登录）。"
          : WEB_LOGIN.has(props.preset) ? "未登录：本机没有可用的 CLI 登录，可直接用浏览器登录。"
          : `未登录：${status()!.error ?? ""}`}
        <Show when={error()}><span class="lyra-error"> {error()}</span></Show>
      </span>
      <Show when={WEB_LOGIN.has(props.preset) && !status()?.ok}>
        <div style={{ display: "flex", gap: "8px" }}>
          <Show when={props.preset === "codebuddy"}>
            <select class="field-input" aria-label="登录站点" value={site()} disabled={waiting()} onChange={(e) => setSite(e.currentTarget.value)}>
              <option value="cn">国内站</option>
              <option value="intl">国际站</option>
            </select>
          </Show>
          <Show when={waiting()} fallback={<button type="button" class="btn secondary" onClick={() => void login()}>登录</button>}>
            <button type="button" class="btn secondary" onClick={() => (cancelled = true)}>取消</button>
          </Show>
        </div>
        <Show when={waiting() && props.preset === "devin"}>
          <input
            class="field-input"
            placeholder="登录后页面打不开？把地址栏里 http://127.0.0.1…/callback?code=… 粘贴到这里"
            aria-label="Devin 登录跳转地址"
            value={pasted()}
            onInput={(e) => setPasted(e.currentTarget.value)}
          />
        </Show>
      </Show>
    </div>
  );
}

function KvEditor(props: {
  title: string;
  hint: string;
  rows: Kv[];
  keyPlaceholder: string;
  valuePlaceholder: string;
  onChange: (fn: (rows: Kv[]) => void) => void;
}) {
  return (
    <div class="field">
      <span class="field-label">{props.title}</span>
      <span class="field-hint">{props.hint}</span>
      <div class="session-shortcut-list">
        <For keyed={false} each={props.rows}>
          {(row, i) => (
            <div class="session-shortcut-row env-var-row">
              <input
                class="field-input"
                placeholder={props.keyPlaceholder}
                aria-label={`${props.title}名称`}
                value={row().key}
                onInput={(e) => props.onChange((rows) => (rows[i].key = e.currentTarget.value))}
              />
              <input
                class="field-input"
                placeholder={props.valuePlaceholder}
                aria-label={`${props.title}值`}
                value={row().value}
                onInput={(e) => props.onChange((rows) => (rows[i].value = e.currentTarget.value))}
              />
              <button type="button" class="btn secondary" onClick={() => props.onChange((rows) => rows.splice(i, 1))}>
                删除
              </button>
            </div>
          )}
        </For>
      </div>
      <button
        type="button"
        class="btn secondary"
        style={{ "align-self": "flex-start" }}
        onClick={() => props.onChange((rows) => rows.push({ key: "", value: "" }))}
      >
        添加
      </button>
    </div>
  );
}

const HEADER_HINT = "支持 {env:NAME} 占位符；模型级同名头覆盖 Provider 级，值填 null 可删除继承的头。";
const FIELD_HINT =
  "写入 options，未知键原样透传到请求体顶层（如 tool_stream、thinking_budget），内置键如 temperature、topP、clearThinking 由 Lyra 消费。值按 JSON 解析（0.7、true、{...}），失败按字符串；要发送字符串 \"123\" 请带引号。";
const ACCOUNTING_HINT =
  '上下文用量以服务商返回的 usage 为准，只对之后新增的内容做本地估算。responsesContinuation 默认关闭；仅支持 previous_response_id 的 OpenAI Responses 服务可设置为 "auto"，同时需要 store=true。store=true 会让服务商保存响应，请先确认隐私策略；本地 Codex 订阅不适用。';

/** onSaver 注册给设置弹窗的统一「保存」：未加载或未修改时为空操作。 */
export function LyraConfigPanel(props: { section?: "providers" | "mcp"; onSaver: (save: () => Promise<void>) => void }) {
  const [draft, setDraft] = createStore<LyraDraft>({
    model: "",
    providers: [],
    mcpServers: "{}",
  });
  const mcpError = createMemo(() => {
    try {
      parseMcpServers(draft.mcpServers);
      return "";
    } catch (e) {
      return e instanceof Error ? e.message : String(e);
    }
  });
  const [error, setError] = createSignal("");
  const [loaded, setLoaded] = createSignal(false);
  const [dirty, setDirty] = createSignal(false);
  const [presets, setPresets] = createSignal<Preset[]>([]);
  let raw: Record<string, any> = {};
  let root: HTMLElement | undefined;

  const load = async () => {
    setError("");
    try {
      raw = await api.getLyraConfig();
      const next = draftFromConfig(raw);
      flush(() => setDraft(reconcile(next)));
      setMcpEditing(false);
      setMcpJsonMode(!!mcpError());
      setLoaded(true);
      setDirty(next.providers.some((p) => p.preset !== (raw.provider?.[p.id]?.preset ?? "")));
    } catch (e) {
      // 解析失败时不允许保存，避免用空配置覆盖手写文件。
      setLoaded(false);
      setError(`${String(e)}（请修正 config.jsonc 后重新加载）`);
    }
  };
  onSettled(() => {
    void load();
    api.getLyraPresets().then(setPresets, () => {});
  });

  const [reloadMsg, setReloadMsg] = createSignal("");
  // 手动改过 config.jsonc 后使用：重读文件（丢弃未保存的图形修改）并让后端重载。
  const reload = async () => {
    if ((dirty() || mcpEditing()) && !(await confirm("放弃未保存的修改并从文件重新加载？", { kind: "warning" }))) return;
    await load();
    try {
      await api.refreshLyraConfig();
      setReloadMsg("已重载配置，模型列表刷新中…");
      setTimeout(() => setReloadMsg(""), 4000);
    } catch (e) {
      setReloadMsg(`刷新失败：${String(e)}`);
    }
  };

  const edit = (fn: (d: LyraDraft) => void) => {
    setDraft(fn);
    setDirty(true);
  };

  props.onSaver(async () => {
    if (!loaded()) return;
    // 底部统一保存也提交正在编辑的单项，不能静默丢掉未点「完成编辑」的输入。
    if (mcpEditing()) flush(commitMcpEditor);
    if (!dirty()) return;
    const invalid = validate(draft.providers, presets());
    if (invalid) throw new Error(`Lyra 配置：${invalid}`);
    const config = configFromDraft(raw, snapshot(draft));
    await api.saveLyraConfig(config);
    raw = config;
    setDirty(false);
  });

  // 「获取模型」结果，按 provider ID 记录；只用于展示，模型本身由后端缓存合并。
  const [fetched, setFetched] = createStore<Record<string, { busy?: boolean; error?: string; models?: Record<string, any> }>>({});
  const fetchModels = async (p: ProviderDraft) => {
    const id = p.id.trim();
    if (!id) return setFetched(s => { s[""] = { error: "请先填写 Provider ID" }; });
    setFetched(s => { Object.assign(s[id] ??= {}, { busy: true, error: undefined }); });
    try {
      const models = await api.fetchLyraModels(id, serializeProvider(snapshot(p)));
      setFetched(s => { (s[id] ??= {}).models = models; });
    } catch (e) {
      setFetched(s => { (s[id] ??= {}).error = String(e); });
    } finally {
      setFetched(s => { (s[id] ??= {}).busy = false; });
    }
  };

  const addProvider = (presetId: string) => {
    let id = presetId === "openai-compatible" ? "" : presetId;
    for (let n = 2; id && draft.providers.some((p) => p.id === id); n++) id = `${presetId}-${n}`;
    // 新项插到最前，紧跟「添加」按钮，服务商再多也不用翻到底。
    edit((d) => void d.providers.unshift(presetId === "manual" ? providerDraft("", { api: APIS[0] }) : providerDraft(id, { preset: presetId })));
    requestAnimationFrame(() => {
      const el = root?.querySelector<HTMLElement>(".lyra-provider");
      el?.scrollIntoView({ block: "nearest", behavior: "smooth" });
      el?.querySelector<HTMLElement>("input:not([type=checkbox])")?.focus({ preventScroll: true });
    });
  };

  const removeProvider = async (index: number) => {
    const p = draft.providers[index];
    if (await confirm(`删除 Provider「${p.name || p.id || "未命名"}」及其 ${p.models.length} 个模型？`, { kind: "warning" })) {
      edit((d) => void d.providers.splice(index, 1));
    }
  };

  const mcpServers = createMemo(() => {
    try { return parseMcpServers(draft.mcpServers); } catch { return null; }
  });
  const [mcpJsonMode, setMcpJsonMode] = createSignal(false);
  const [mcpEditing, setMcpEditing] = createSignal(false);
  const [mcpDraft, setMcpDraft] = createStore<{
    original: string | null; name: string; transport: "stdio" | "http";
    server: Record<string, any>; env: Kv[]; headers: Kv[];
  }>({ original: null, name: "", transport: "stdio", server: {}, env: [], headers: [] });
  const beginMcpEditor = (name: string | null = null) => {
    const servers = mcpServers();
    if (!servers) return;
    const server = name === null ? { enabled: false, command: "" } : servers[name];
    setMcpDraft(reconcile({
      original: name, name: name ?? "", transport: Object.hasOwn(server, "url") ? "http" : "stdio",
      server: structuredClone(server),
      env: Object.entries(server.env ?? {}).map(([key, value]) => ({ key, value: String(value) })),
      headers: Object.entries(server.headers ?? {}).map(([key, value]) => ({ key, value: String(value) })),
    }));
    setMcpEditing(true);
    queueMicrotask(() => root?.querySelector<HTMLInputElement>(".lyra-mcp-editor input")?.focus());
  };
  const mcpTextFromEditor = () => {
    const servers = parseMcpServers(draft.mcpServers);
    const { original, name, transport } = mcpDraft;
    if (name !== original && Object.hasOwn(servers, name)) throw new Error(`MCP：服务器名称「${name}」已存在。`);
    const server = { ...mcpDraft.server };
    for (const key of transport === "stdio" ? ["url", "headers"] : ["command", "args", "env", "cwd"]) delete server[key];
    const endpoint = transport === "stdio" ? "command" : "url";
    server[endpoint] ??= "";
    const key = transport === "stdio" ? "env" : "headers";
    const rows = mcpDraft[key];
    if (new Set(rows.map((row) => row.key)).size !== rows.length) throw new Error(`MCP：${key} 名称不能重复。`);
    if (rows.length || Object.hasOwn(server, key)) server[key] = Object.fromEntries(rows.map((row) => [row.key, row.value]));
    if (original !== null && name !== original) delete servers[original];
    const text = JSON.stringify({ ...servers, [name]: server }, null, 2);
    parseMcpServers(text);
    return text;
  };
  const mcpEditorError = createMemo(() => {
    if (!mcpEditing()) return "";
    try { mcpTextFromEditor(); return ""; } catch (e) { return e instanceof Error ? e.message : String(e); }
  });
  const commitMcpEditor = () => {
    const text = mcpTextFromEditor();
    edit((d) => { d.mcpServers = text; });
    setMcpEditing(false);
  };
  const removeMcpServer = async (name: string) => {
    if (!(await confirm(`删除 MCP 服务器「${name}」？保存后生效。`, { kind: "warning" }))) return;
    const servers = parseMcpServers(draft.mcpServers);
    delete servers[name];
    edit((d) => { d.mcpServers = JSON.stringify(servers, null, 2); });
  };
  const setMcpOptional = (key: string, value: string, numeric = false) => setMcpDraft(s => {
    if (value === "") delete s.server[key];
    else s.server[key] = numeric ? Number(value) : value;
  });

  return (
    <section class="lyra-config" ref={root}>
      <Show when={error()}>
        <span class="field-hint lyra-error" role="alert">{error()}</span>
      </Show>
      <Show when={loaded()}>
        <div class="lyra-pane" hidden={props.section === "mcp"}>
        <div class="lyra-section-head lyra-providers-head">
          <div class="lyra-heading">
            <h3>模型服务商 <span class="lyra-count">{draft.providers.length}</span></h3>
            <p class="field-hint">添加服务商并获取可用模型。</p>
          </div>
          <select
            class="field-input lyra-add-provider"
            aria-label="添加 Provider"
            onChange={(e) => {
              addProvider(e.currentTarget.value);
              e.currentTarget.value = "";
            }}
          >
            <option value="" selected>＋ 添加服务商</option>
            <For each={presets()}>{(x) => <option value={x.id}>{x.name}（{x.local ? "无需填 Key" : "填 Key 一键获取模型"}）</option>}</For>
            <option value="manual">手动配置</option>
          </select>
        </div>
        <Show when={!draft.providers.length}>
          <div class="lyra-empty">
            <span class="field-label">添加第一个模型服务商</span>
            <span class="field-hint">server support 使用团队中转和 token；本地 Codex / Claude Code / Devin / CodeBuddy / WorkBuddy 自动读取本机认证，其他预设填写 API Key。</span>
          </div>
        </Show>
        {/* For 按对象跟踪：头部插入时各项的展开状态不会错位。 */}
        <For each={draft.providers}>
          {(item, index) => {
            const p = () => item;
            const pi = index();
            const setP = (fn: (p: ProviderDraft) => void) => edit((d) => fn(d.providers[index()]));
            const preset = () => presets().find((x) => x.id === p().preset);
            const local = () => isLocalPreset(p().preset);
            const advancedInitiallyOpen = !p().id;
            const initiallyOpen = pi === 0 || !Object.hasOwn(raw.provider ?? {}, p().id);
            const setPreset = (id: string) =>
              setP((x) => {
                x.preset = id;
                // 预设按模型自动识别协议；ID 未填时用预设名，方便一步配好。
                if (id) x.api = "";
                if (id && !x.id.trim() && id !== "openai-compatible") x.id = id;
              });
            return (
              <details class="lyra-provider" open={initiallyOpen}>
                <summary>
                  <span class="lyra-provider-title">{p().name || p().id || preset()?.name || "未命名服务商"}</span>
                  <span class="lyra-provider-meta">{preset()?.name || "手动配置"}</span>
                  <span class="lyra-count">{fetched[p().id.trim()]?.models || p().models.length
                    ? `${new Set([...Object.keys(fetched[p().id.trim()]?.models ?? {}), ...p().models.map((m) => m.id)]).size} 个模型`
                    : p().preset ? "自动获取模型" : "待添加模型"}</span>
                  <button
                    type="button"
                    class="link-btn lyra-del"
                    onClick={(e) => {
                      e.preventDefault();
                      void removeProvider(index());
                    }}
                  >
                    删除
                  </button>
                </summary>
                <div class="lyra-provider-body">
                  <h4>连接配置</h4>
                  <div class="backend-fields">
                    <label class="backend-field">
                      <span class="field-label" title="选择 Provider 自动获取模型；server support 使用团队 token，本地预设沿用本机认证">类型</span>
                      <select class="field-input" onChange={(e) => setPreset(e.currentTarget.value)}>
                        <option value="" selected={!p().preset}>手动配置</option>
                        <Show when={p().preset && presets().length && !preset()}>
                          <option value={p().preset} selected>{p().preset}（不支持）</option>
                        </Show>
                        <For each={presets()}>{(x) => <option value={x.id} selected={x.id === p().preset}>{x.name}</option>}</For>
                      </select>
                    </label>
                    <label class="backend-field">
                      <span class="field-label">显示名称</span>
                      <input class="field-input" placeholder={preset()?.name} value={p().name} onInput={(e) => setP((x) => (x.name = e.currentTarget.value))} />
                    </label>
                  </div>
                  <div class="backend-fields">
                    <Show when={p().preset !== "server-support"}><label class="backend-field">
                      <span class="field-label">Base URL</span>
                      <input class="field-input" type="url" placeholder={local() ? "留空跟随本地配置" : preset()?.baseURL ? `留空使用 ${preset()!.baseURL}` : "https://api.openai.com/v1"} value={p().baseURL} onInput={(e) => setP((x) => (x.baseURL = e.currentTarget.value))} />
                    </label></Show>
                    <Show when={!local()}>
                      <label class="backend-field">
                        <span class="field-label">API Key</span>
                        <input class="field-input" type="text" autocomplete="off" placeholder="sk-… 或 {env:NAME}" value={p().apiKey} onInput={(e) => setP((x) => (x.apiKey = e.currentTarget.value))} />
                      </label>
                    </Show>
                  </div>

                  <Show when={LOGIN_PRESETS.has(p().preset)}>
                    <LocalLogin
                      preset={p().preset}
                      autoStart={!Object.hasOwn(raw.provider ?? {}, p().id)}
                      onLoggedIn={() => void fetchModels(p())}
                    />
                  </Show>
                  <Show when={p().preset === "local-codex" || p().preset === "local-claude-code"}>
                    <p class="field-hint">优先使用 CLI 自定义 API，否则使用内置订阅反代；无需 Node 或独立 Agent。订阅连接使用 Lyra 代理。凭据过期时请到对应 CLI 刷新登录；不会静默将订阅凭据发送给第三方。</p>
                  </Show>

                  <section class="lyra-models">
                    <div class="lyra-section-head">
                      <h4>模型</h4>
                      <button type="button" class="btn secondary" onClick={() => setP((x) => void x.models.push(modelDraft("", {})))}>
                        添加模型
                      </button>
                    </div>
                    <Show when={preset()}>
                      {(() => {
                        const result = () => fetched[p().id.trim()];
                        const names = () => Object.entries(result()?.models ?? {}).map(([mid, m]) => m?.name || mid);
                        return (
                          <div class="lyra-fetch">
                            <span class="field-hint">{p().preset === "server-support" ? "使用已保存的团队中转地址和 token，无需填写 Key。" : local() ? "沿用本机认证，无需填写 Key。" : "填写 API Key 后获取可用模型。"}模型每 6 小时自动更新，同 ID 的手动配置优先。</span>
                            <button type="button" class="btn secondary" disabled={result()?.busy} onClick={() => void fetchModels(p())}>
                              {result()?.busy ? "获取中…" : "获取模型"}
                            </button>
                            <span class="field-hint" role="status" aria-live="polite">
                              {result()?.error
                                ? `获取失败：${result()!.error}`
                                : result()?.models
                                  ? `已获取 ${names().length} 个模型`
                                  : ""}
                            </span>
                            <Show when={names().length}>
                              <details class="lyra-model-catalog">
                                <summary>查看模型列表</summary>
                                <div class="lyra-model-names"><For each={names()}>{(name) => <span>{name}</span>}</For></div>
                              </details>
                            </Show>
                          </div>
                        );
                      })()}
                    </Show>
                    <For keyed={false} each={p().models}>
                      {(m, mi) => {
                        const setM = (fn: (m: ModelDraft) => void) => setP((x) => fn(x.models[mi]));
                        const initiallyOpen = !m().id;
                        return (
                          <details class="lyra-model" open={initiallyOpen}>
                            <summary>
                              <span class="lyra-provider-title">{m().name || m().id || "未命名模型"}</span>
                              <span class="field-hint">手动配置</span>
                              <button
                                type="button"
                                class="link-btn lyra-del"
                                onClick={(e) => {
                                  e.preventDefault();
                                  setP((x) => void x.models.splice(mi, 1));
                                }}
                              >
                                删除
                              </button>
                            </summary>
                            <div class="lyra-detail-body">
                              <div class="backend-fields">
                                <label class="backend-field">
                                  <span class="field-label">模型 ID</span>
                                  <input class="field-input" placeholder="如 gpt-5" value={m().id} onInput={(e) => setM((x) => (x.id = e.currentTarget.value))} />
                                </label>
                                <label class="backend-field">
                                  <span class="field-label">显示名称</span>
                                  <input class="field-input" value={m().name} onInput={(e) => setM((x) => (x.name = e.currentTarget.value))} />
                                </label>
                                <label class="backend-field">
                                  <span class="field-label">上下文窗口</span>
                                  <input class="field-input" inputmode="numeric" placeholder="128000" value={m().context} onInput={(e) => setM((x) => (x.context = e.currentTarget.value))} />
                                </label>
                                <label class="backend-field">
                                  <span class="field-label">最大输出</span>
                                  <input class="field-input" inputmode="numeric" placeholder="32000" value={m().output} onInput={(e) => setM((x) => (x.output = e.currentTarget.value))} />
                                </label>
                              </div>
                              <div class="backend-card-head">
                                <label>
                                  <input type="checkbox" checked={m().reasoning} onChange={(e) => setM((x) => (x.reasoning = e.currentTarget.checked))} /> 支持思考
                                </label>
                                <label>
                                  <input type="checkbox" checked={m().image} onChange={(e) => setM((x) => (x.image = e.currentTarget.checked))} /> 支持图片输入
                                </label>
                                <Show when={hasVariants(m().raw)}>
                                  <span class="field-hint">思考强度：{[...Object.keys(m().raw.variants)].sort((a, b) => effortRank(a) - effortRank(b)).join(" / ")}</span>
                                </Show>
                              </div>
                              <details class="lyra-advanced">
                                <summary>高级参数<span class="field-hint">{m().headers.length + m().fields.length} 项自定义配置</span></summary>
                                <div class="lyra-detail-body">
                                  <KvEditor title="自定义请求头" hint="覆盖 Provider 级同名头。" rows={m().headers} keyPlaceholder="Header 名" valuePlaceholder="值" onChange={(fn) => setM((x) => fn(x.headers))} />
                                  <KvEditor title="自定义字段" hint={`覆盖 Provider 级同名字段。${ACCOUNTING_HINT}`} rows={m().fields} keyPlaceholder="字段名" valuePlaceholder="值" onChange={(fn) => setM((x) => fn(x.fields))} />
                                </div>
                              </details>
                            </div>
                          </details>
                        );
                      }}
                    </For>
                  </section>
                  <details class="lyra-advanced" open={advancedInitiallyOpen || !p().preset}>
                    <summary>高级设置<span class="field-hint">ID、协议、代理 · {p().headers.length + p().fields.length} 项自定义配置</span></summary>
                    <div class="lyra-detail-body">
                      <div class="backend-fields">
                        <label class="backend-field">
                          <span class="field-label">ID</span>
                          <input class="field-input" placeholder="如 openai" value={p().id} onInput={(e) => setP((x) => (x.id = e.currentTarget.value))} />
                        </label>
                        <label class="backend-field">
                          <span class="field-label">协议</span>
                          <select class="field-input" onChange={(e) => setP((x) => (x.api = e.currentTarget.value))}>
                            <Show when={p().raw.npm}>
                              <option value="" selected={!p().api}>按 npm 推导（{p().raw.npm}）</option>
                            </Show>
                            <Show when={!p().raw.npm && preset()}>
                              <option value="" selected={!p().api}>{local() ? "跟随本地配置" : "按模型自动识别"}</option>
                            </Show>
                            <Show when={!p().raw.npm && !preset()}>
                              <option value="" disabled selected={!p().api}>请选择</option>
                            </Show>
                            <Show when={p().api && !APIS.includes(p().api)}>
                              <option value={p().api} selected>{p().api}（不支持）</option>
                            </Show>
                            <For each={APIS}>{(a) => <option value={a} selected={a === p().api}>{a}</option>}</For>
                          </select>
                        </label>
                        <label class="backend-field">
                          <span class="field-label" title="仅在模型选择器中保留 ID 或名称包含这些关键词的模型；多个用逗号分隔，不分大小写">模型关键词过滤</span>
                          <input class="field-input" placeholder="如 claude, gpt-5；留空不过滤" value={p().modelFilter} onInput={(e) => setP((x) => (x.modelFilter = e.currentTarget.value))} />
                        </label>
                      </div>
                      <KvEditor title="自定义请求头" hint={HEADER_HINT} rows={p().headers} keyPlaceholder="Header 名，如 X-Api-Version" valuePlaceholder="值" onChange={(fn) => setP((x) => fn(x.headers))} />
                      <KvEditor title="自定义字段" hint={`${FIELD_HINT} ${ACCOUNTING_HINT}`} rows={p().fields} keyPlaceholder="字段名，如 temperature" valuePlaceholder="值，如 0.7" onChange={(fn) => setP((x) => fn(x.fields))} />
                    </div>
                  </details>
                  <div class="lyra-provider-footer">
                    <span class="field-hint">修改后点击底部「保存」生效</span>
                  </div>
                </div>
              </details>
            );
          }}
        </For>
        </div>
        <div class="lyra-pane lyra-mcp" hidden={props.section !== "mcp"}>
          <div class="lyra-section-head">
            <div class="lyra-heading">
              <h3>MCP 服务器 <span class="lyra-count">{Object.keys(mcpServers() ?? {}).length}</span></h3>
              <p class="field-hint">逐个管理工具扩展，修改后点击底部「保存」生效。</p>
            </div>
            <button type="button" class="btn secondary" disabled={mcpEditing() || !mcpServers()} onClick={() => setMcpJsonMode(!mcpJsonMode())}>
              {mcpJsonMode() || !mcpServers() ? "返回服务器列表" : "高级：编辑 JSON"}
            </button>
          </div>
          <p class="field-hint">仅启用你信任的服务器：本地程序或远程服务可能读取文件、使用凭据、联网或修改数据。新增项默认禁用；配置以明文保存，敏感值请用 {"{env:NAME}"} 引用环境变量。</p>
          <Show when={!mcpJsonMode() && mcpServers()} fallback={
            <div class="lyra-detail-body">
              <p class="field-hint" id="lyra-mcp-hint">高级编辑：填写服务器名称到配置的 JSON 对象，不要再包一层 mcpServers；用 {"{}"} 清空。修正错误后可返回服务器列表。</p>
              <label class="field" for="lyra-mcp-json">
                <span class="field-label">mcpServers JSON</span>
                <textarea id="lyra-mcp-json" class="field-input" rows={14} spellcheck={false} autocomplete="off"
                  style={{ "font-family": "monospace", resize: "vertical" }}
                  aria-describedby="lyra-mcp-hint lyra-mcp-error" aria-invalid={mcpError() ? "true" : "false"}
                  value={draft.mcpServers} onInput={(e) => edit((d) => { d.mcpServers = e.currentTarget.value; })} />
              </label>
              <span id="lyra-mcp-error" class="field-hint lyra-error" role="status" aria-live="polite">{mcpError()}</span>
            </div>
          }>
            <button type="button" class="btn secondary" disabled={mcpEditing() || Object.keys(mcpServers()!).length >= 64} onClick={() => beginMcpEditor()}>添加 MCP 服务器</button>
            <Show when={!Object.keys(mcpServers()!).length && !mcpEditing()}>
              <div class="lyra-empty"><span class="field-hint">尚未添加 MCP 服务器。支持本地命令（stdio）和远程地址（Streamable HTTP）。</span></div>
            </Show>
            <Show when={mcpEditing()}>
              <section class="lyra-provider lyra-mcp-editor" aria-label="MCP 服务器编辑">
                <div class="lyra-provider-body">
                  <h4>{mcpDraft.original === null ? "添加 MCP 服务器" : "编辑 MCP 服务器"}</h4>
                  <div class="backend-fields">
                    <label class="backend-field">
                      <span class="field-label">服务器名称</span>
                      <input class="field-input" placeholder="如 filesystem（字母、数字、._-）" value={mcpDraft.name} onInput={(e) => setMcpDraft(s => { s.name = e.currentTarget.value; })} />
                    </label>
                    <label class="backend-field">
                      <span class="field-label">连接方式</span>
                      <select class="field-input" value={mcpDraft.transport} onChange={(e) => setMcpDraft(s => { s.transport = e.currentTarget.value as "stdio" | "http"; })}>
                        <option value="stdio">本地命令（stdio）</option><option value="http">远程地址（Streamable HTTP）</option>
                      </select>
                    </label>
                  </div>
                  <Show when={mcpDraft.transport === "stdio"} fallback={
                    <>
                      <label class="field"><span class="field-label">服务地址</span>
                        <input class="field-input" type="url" placeholder="https://example.com/mcp" value={mcpDraft.server.url ?? ""} onInput={(e) => setMcpDraft(s => { s.server.url = e.currentTarget.value; })} />
                        <span class="field-hint">非本机地址必须使用 HTTPS；不支持旧 SSE 入口或 OAuth。</span>
                      </label>
                      <KvEditor title="请求头" hint="支持 {env:NAME} 环境变量引用，例如 Authorization。" rows={mcpDraft.headers} keyPlaceholder="Header 名" valuePlaceholder="值或 {env:NAME}" onChange={(fn) => setMcpDraft(s => { fn(s.headers); })} />
                    </>
                  }>
                    <label class="field"><span class="field-label">启动命令</span>
                      <input class="field-input" placeholder="如 npx 或可执行文件的完整路径" value={mcpDraft.server.command ?? ""} onInput={(e) => setMcpDraft(s => { s.server.command = e.currentTarget.value; })} />
                      <span class="field-hint">只填程序名或路径，参数在下方逐个添加，无需给带空格的路径加引号。</span>
                    </label>
                    <div class="field">
                      <span class="field-label">启动参数</span>
                      <For keyed={false} each={mcpDraft.server.args ?? []}>{(arg, i) => (
                        <div class="session-shortcut-row">
                          <input class="field-input" aria-label={`参数 ${i + 1}`} value={arg()} onInput={(e) => setMcpDraft(s => { s.server.args[i] = e.currentTarget.value; })} />
                          <button type="button" class="btn secondary" aria-label={`删除参数 ${i + 1}`} onClick={() => setMcpDraft(s => { s.server.args.splice(i, 1); })}>删除</button>
                        </div>
                      )}</For>
                      <button type="button" class="btn secondary" onClick={() => setMcpDraft(s => { (s.server.args ??= []).push(""); })}>添加参数</button>
                    </div>
                    <label class="field"><span class="field-label">工作目录（可选）</span>
                      <input class="field-input" placeholder="绝对路径；留空使用会话初始目录" value={mcpDraft.server.cwd ?? ""} onInput={(e) => setMcpOptional("cwd", e.currentTarget.value)} />
                    </label>
                    <KvEditor title="环境变量" hint="值支持 {env:NAME} 引用；不会在保存时展开。" rows={mcpDraft.env} keyPlaceholder="变量名" valuePlaceholder="值或 {env:NAME}" onChange={(fn) => setMcpDraft(s => { fn(s.env); })} />
                  </Show>
                  <label class="backend-switch"><input type="checkbox" checked={mcpDraft.server.enabled} onChange={(e) => setMcpDraft(s => { s.server.enabled = e.currentTarget.checked; })} /><span>启用此服务器</span></label>
                  <details class="lyra-advanced">
                    <summary>高级选项：超时与只读工具授权</summary>
                    <div class="lyra-detail-body">
                      <div class="backend-fields">
                        <label class="backend-field"><span class="field-label">启动超时（毫秒）</span>
                          <input class="field-input" type="number" min="1" max="600000" step="1" placeholder="默认 10000" value={mcpDraft.server.startup_timeout_ms ?? ""} onInput={(e) => setMcpOptional("startup_timeout_ms", e.currentTarget.value, true)} />
                        </label>
                        <label class="backend-field"><span class="field-label">请求超时（毫秒）</span>
                          <input class="field-input" type="number" min="1" max="600000" step="1" placeholder="默认 30000" value={mcpDraft.server.request_timeout_ms ?? ""} onInput={(e) => setMcpOptional("request_timeout_ms", e.currentTarget.value, true)} />
                        </label>
                      </div>
                      <label class="field"><span class="field-label">允许只读会话调用的工具</span>
                        <textarea class="field-input" rows={3} placeholder="每行一个精确工具名；留空不授权" value={(mcpDraft.server.readOnlyTools ?? []).join("\n")} onInput={(e) => setMcpDraft(s => { s.server.readOnlyTools = e.currentTarget.value === "" ? [] : e.currentTarget.value.split("\n"); })} />
                        <span class="field-hint">最多 64 个工具。不信任服务器 annotations 自动授权；这不是沙箱，也不保证工具只读。</span>
                      </label>
                    </div>
                  </details>
                  <span class="field-hint lyra-error" role="status" aria-live="polite">{mcpEditorError()}</span>
                  <div class="lyra-mcp-actions">
                    <button type="button" class="btn primary" disabled={!!mcpEditorError()} onClick={commitMcpEditor}>完成编辑</button>
                    <button type="button" class="btn secondary" onClick={() => setMcpEditing(false)}>取消编辑</button>
                  </div>
                </div>
              </section>
            </Show>
            <For each={Object.entries(mcpServers()!)}>{([name, server]) => (
              <div class="lyra-mcp-server">
                <div class="lyra-heading"><strong>{name}</strong><span class="field-hint">{Object.hasOwn(server, "command") ? "本地命令（stdio）" : "Streamable HTTP"} · {server.enabled ? "已启用" : "已禁用"}</span></div>
                <div class="lyra-mcp-actions">
                  <button type="button" class="btn secondary" disabled={mcpEditing()} onClick={() => beginMcpEditor(name)}>编辑</button>
                  <button type="button" class="btn secondary" disabled={mcpEditing()} onClick={() => void removeMcpServer(name)}>删除</button>
                </div>
              </div>
            )}</For>
          </Show>
        </div>
      </Show>
      <details class="lyra-file-settings" open={!loaded()}>
        <summary>配置文件与重新加载</summary>
        <div class="lyra-detail-body">
          <span class="field-hint">配置文件：~/.nova/alkaid/config.jsonc。保存后立即重载；文件会转为标准 JSON，手写注释会丢失，首次覆盖前备份为 config.jsonc.bak。</span>
          <button type="button" class="btn secondary" onClick={() => void reload()}>从文件重新加载</button>
          <span class="field-hint" role="status" aria-live="polite">{reloadMsg()}</span>
        </div>
      </details>
    </section>
  );
}
