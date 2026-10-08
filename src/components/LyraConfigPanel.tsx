import { confirm } from "@tauri-apps/plugin-dialog";
import { createSignal, For, Index, onMount, Show } from "solid-js";
import { createStore, produce, unwrap } from "solid-js/store";
import { api } from "../ipc";
import {
  APIS,
  configFromDraft,
  draftFromConfig,
  hasVariants,
  isObject,
  isLocalPreset,
  modelDraft,
  providerDraft,
  serializeProvider,
  validate,
  type Kv,
  type LyraDraft,
  type ModelDraft,
  type Preset,
  type ProviderDraft,
} from "../lyraConfig";

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
        <Index each={props.rows}>
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
        </Index>
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

/** onSaver 注册给设置弹窗的统一「保存」：未加载或未修改时为空操作。 */
export function LyraConfigPanel(props: { onSaver: (save: () => Promise<void>) => void }) {
  const [draft, setDraft] = createStore<LyraDraft>({
    model: "",
    providers: [],
  });
  const [error, setError] = createSignal("");
  const [loaded, setLoaded] = createSignal(false);
  const [dirty, setDirty] = createSignal(false);
  const [presets, setPresets] = createSignal<Preset[]>([]);
  let raw: Record<string, any> = {};

  const load = async () => {
    setError("");
    try {
      raw = await api.getLyraConfig();
      const next = draftFromConfig(raw);
      setDraft(next);
      setLoaded(true);
      setDirty(next.providers.some((p) => p.preset !== (raw.provider?.[p.id]?.preset ?? "")));
    } catch (e) {
      // 解析失败时不允许保存，避免用空配置覆盖手写文件。
      setLoaded(false);
      setError(`${String(e)}（请修正 config.jsonc 后重新加载）`);
    }
  };
  onMount(() => {
    void load();
    api.getLyraPresets().then(setPresets, () => {});
  });

  const [reloadMsg, setReloadMsg] = createSignal("");
  // 手动改过 config.jsonc 后使用：重读文件（丢弃未保存的图形修改）并让后端重载。
  const reload = async () => {
    if (dirty() && !(await confirm("放弃未保存的修改并从文件重新加载？", { kind: "warning" }))) return;
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
    setDraft(produce(fn));
    setDirty(true);
  };

  props.onSaver(async () => {
    if (!loaded() || !dirty()) return;
    const invalid = validate(draft.providers, presets());
    if (invalid) throw new Error(`Lyra 配置：${invalid}`);
    const config = configFromDraft(raw, unwrap(draft));
    await api.saveLyraConfig(config);
    raw = config;
    setDirty(false);
  });

  // 「获取模型」结果，按 provider ID 记录；只用于展示和默认模型候选，模型本身由后端缓存合并。
  const [fetched, setFetched] = createStore<Record<string, { busy?: boolean; error?: string; models?: Record<string, any> }>>({});
  const fetchModels = async (p: ProviderDraft) => {
    const id = p.id.trim();
    if (!id) return setFetched("", { error: "请先填写 Provider ID" });
    setFetched(id, { busy: true, error: undefined });
    try {
      setFetched(id, { models: await api.fetchLyraModels(id, serializeProvider(unwrap(p))) });
    } catch (e) {
      setFetched(id, { error: String(e) });
    } finally {
      setFetched(id, "busy", false);
    }
  };

  const modelChoices = () =>
    draft.providers.flatMap((p) => {
      const models = [
        ...Object.entries(fetched[p.id]?.models ?? {}).filter(([mid]) => !p.models.some((m) => m.id === mid)),
        ...p.models.map((m) => [m.id, m.raw] as const),
      ];
      return models.flatMap(([mid, raw]) => {
        const base = `${p.id}/${mid}`;
        const variants = isObject(raw?.variants) ? Object.keys(raw.variants) : [];
        return [base, ...variants.map((v) => `${base}/variant/${v}`)];
      });
    });

  const addProvider = (presetId: string) => {
    let id = presetId === "openai-compatible" ? "" : presetId;
    for (let n = 2; id && draft.providers.some((p) => p.id === id); n++) id = `${presetId}-${n}`;
    edit((d) => void d.providers.push(presetId === "manual" ? providerDraft("", { api: APIS[0] }) : providerDraft(id, { preset: presetId })));
  };

  // 预设 provider 的模型自动拉取，界面里看不到，选中的默认模型不应标为不存在。
  const fetchedModel = (value: string) => draft.providers.some((p) => p.preset && value.startsWith(`${p.id}/`));

  const removeProvider = async (index: number) => {
    const p = draft.providers[index];
    if (await confirm(`删除 Provider「${p.name || p.id || "未命名"}」及其 ${p.models.length} 个模型？`, { kind: "warning" })) {
      edit((d) => void d.providers.splice(index, 1));
    }
  };

  return (
    <section class="settings-group">
      <h3 class="settings-group-title">模型配置</h3>
      <span class="field-hint">
        对应 ~/.nova/alkaid/config.jsonc，点击底部「保存」写回并立即重载。图形保存会改写为标准 JSON，手写注释会丢失，首次覆盖前备份为 config.jsonc.bak。
      </span>
      <div class="backend-card-head">
        <Show when={error()}>
          <span class="field-hint" role="alert">{error()}</span>
        </Show>
        <span class="field-hint" role="status" aria-live="polite">{reloadMsg()}</span>
        <button type="button" class="btn secondary" style={{ "margin-left": "auto" }} onClick={() => void reload()}>
          从文件重新加载
        </button>
      </div>
      <Show when={loaded()}>
        <label class="field">
          <span class="field-label">默认模型</span>
          <select class="field-input" onChange={(e) => edit((d) => void (d.model = e.currentTarget.value))}>
            <option value="" selected={!draft.model}>未设置（使用第一个模型）</option>
            <Show when={draft.model && !modelChoices().includes(draft.model)}>
              <option value={draft.model} selected>{draft.model}{fetchedModel(draft.model) ? "（自动拉取）" : "（不存在）"}</option>
            </Show>
            <For each={modelChoices()}>{(value) => <option value={value} selected={value === draft.model}>{value}</option>}</For>
          </select>
        </label>

        <Index each={draft.providers}>
          {(p, pi) => {
            const setP = (fn: (p: ProviderDraft) => void) => edit((d) => fn(d.providers[pi]));
            const preset = () => presets().find((x) => x.id === p().preset);
            const local = () => isLocalPreset(p().preset);
            const setPreset = (id: string) =>
              setP((x) => {
                x.preset = id;
                // 预设按模型自动识别协议；ID 未填时用预设名，方便一步配好。
                if (id) x.api = "";
                if (id && !x.id.trim() && id !== "openai-compatible") x.id = id;
              });
            return (
              <div class="backend-card">
                <div class="backend-card-head">
                  <span class="agent-badge lyra">{p().name || p().id || "未命名 Provider"}</span>
                  <span class="field-hint">{new Set([...Object.keys(fetched[p().id.trim()]?.models ?? {}), ...p().models.map((m) => m.id)]).size} 个模型</span>
                  <button type="button" class="link-btn" style={{ "margin-left": "auto" }} onClick={() => void removeProvider(pi)}>
                    删除 Provider
                  </button>
                </div>
                <div class="backend-fields">
                  <label class="backend-field">
                    <span class="field-label" title="选择 Provider 自动获取模型；本地 Codex / Claude Code 沿用本地认证，无需填写 Key">类型</span>
                    <select class="field-input" onChange={(e) => setPreset(e.currentTarget.value)}>
                      <option value="" selected={!p().preset}>手动配置</option>
                      <Show when={p().preset && presets().length && !preset()}>
                        <option value={p().preset} selected>{p().preset}（不支持）</option>
                      </Show>
                      <For each={presets()}>{(x) => <option value={x.id} selected={x.id === p().preset}>{x.name}</option>}</For>
                    </select>
                  </label>
                  <label class="backend-field">
                    <span class="field-label">ID</span>
                    <input class="field-input" placeholder="如 openai" value={p().id} onInput={(e) => setP((x) => (x.id = e.currentTarget.value))} />
                  </label>
                  <label class="backend-field">
                    <span class="field-label">显示名称</span>
                    <input class="field-input" placeholder={preset()?.name} value={p().name} onInput={(e) => setP((x) => (x.name = e.currentTarget.value))} />
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
                </div>
                <div class="backend-fields">
                  <label class="backend-field backend-field-wide">
                    <span class="field-label">Base URL</span>
                    <input class="field-input" type="url" placeholder={local() ? "留空跟随本地配置" : preset()?.baseURL ? `留空使用 ${preset()!.baseURL}` : "https://api.openai.com/v1"} value={p().baseURL} onInput={(e) => setP((x) => (x.baseURL = e.currentTarget.value))} />
                  </label>
                  <Show when={!local()}>
                    <label class="backend-field">
                      <span class="field-label">API Key</span>
                      <input class="field-input" type="text" autocomplete="off" placeholder="sk-… 或 {env:NAME}" value={p().apiKey} onInput={(e) => setP((x) => (x.apiKey = e.currentTarget.value))} />
                    </label>
                  </Show>
                  <label class="backend-field">
                    <span class="field-label">代理</span>
                    <input class="field-input" placeholder="留空跟随 Lyra 全局代理" value={p().proxy} onInput={(e) => setP((x) => (x.proxy = e.currentTarget.value))} />
                  </label>
                </div>
                <KvEditor title="自定义请求头" hint={HEADER_HINT} rows={p().headers} keyPlaceholder="Header 名，如 X-Api-Version" valuePlaceholder="值" onChange={(fn) => setP((x) => fn(x.headers))} />
                <KvEditor title="自定义字段" hint={FIELD_HINT} rows={p().fields} keyPlaceholder="字段名，如 temperature" valuePlaceholder="值，如 0.7" onChange={(fn) => setP((x) => fn(x.fields))} />

                <span class="field-label">模型</span>
                <Show when={preset()}>
                  {(() => {
                    const result = () => fetched[p().id.trim()];
                    const names = () => Object.entries(result()?.models ?? {}).map(([mid, m]) => m?.name || mid);
                    return (
                      <div class="backend-card-head">
                        <button type="button" class="btn secondary" disabled={result()?.busy} onClick={() => void fetchModels(p())}>
                          {result()?.busy ? "获取中…" : "获取模型"}
                        </button>
                        <span class="field-hint" role="status" aria-live="polite">
                          {result()?.error
                            ? `获取失败：${result()!.error}`
                            : result()?.models
                              ? `已获取 ${names().length} 个模型：${names().join("、")}`
                              : local()
                                ? "沿用本地 API 配置，无需填写 Key；点击获取全部模型，之后每 6 小时自动更新。下方同 ID 配置会覆盖获取结果。"
                                : "填好 API Key 后点击获取全部模型；之后每 6 小时自动更新。下方手写的同 ID 模型会覆盖获取结果。"}
                        </span>
                      </div>
                    );
                  })()}
                </Show>
                <Index each={p().models}>
                  {(m, mi) => {
                    const setM = (fn: (m: ModelDraft) => void) => setP((x) => fn(x.models[mi]));
                    return (
                      <div class="backend-card">
                        <div class="backend-card-head">
                          <span class="field-label">{m().name || m().id || "未命名模型"}</span>
                          <button type="button" class="link-btn" style={{ "margin-left": "auto" }} onClick={() => setP((x) => void x.models.splice(mi, 1))}>
                            删除模型
                          </button>
                        </div>
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
                            <span class="field-hint">思考强度：{Object.keys(m().raw.variants).join(" / ")}</span>
                          </Show>
                        </div>
                        <KvEditor title="自定义请求头" hint="覆盖 Provider 级同名头。" rows={m().headers} keyPlaceholder="Header 名" valuePlaceholder="值" onChange={(fn) => setM((x) => fn(x.headers))} />
                        <KvEditor title="自定义字段" hint="覆盖 Provider 级同名字段。" rows={m().fields} keyPlaceholder="字段名" valuePlaceholder="值" onChange={(fn) => setM((x) => fn(x.fields))} />
                      </div>
                    );
                  }}
                </Index>
                <button
                  type="button"
                  class="btn secondary"
                  style={{ "align-self": "flex-start" }}
                  onClick={() => setP((x) => void x.models.push(modelDraft("", {})))}
                >
                  添加模型
                </button>
              </div>
            );
          }}
        </Index>
        <select
          class="field-input"
          style={{ "align-self": "flex-start", width: "auto" }}
          aria-label="添加 Provider"
          onChange={(e) => {
            addProvider(e.currentTarget.value);
            e.currentTarget.value = "";
          }}
        >
          <option value="" selected>添加 Provider…</option>
          <For each={presets()}>{(x) => <option value={x.id}>{x.name}（{x.local ? "无需填 Key" : "填 Key 一键获取模型"}）</option>}</For>
          <option value="manual">手动配置</option>
        </select>
      </Show>
    </section>
  );
}
