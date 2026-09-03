import { listen } from "@tauri-apps/api/event";
import { message } from "@tauri-apps/plugin-dialog";
import { batch, createSignal } from "solid-js";
import { createStore, produce, reconcile, unwrap } from "solid-js/store";
import { LruMap } from "./lruMap";
import { api } from "./ipc";
import type {
  Achievement,
  AgentKind,
  BranchList,
  CaptureClueResult,
  ClueAttachment,
  ClueCard,
  ClueNodeGroup,
  EffortChoice,
  IncomingRoamRequest,
  IncomingShare,
  IncomingWorkflowShare,
  Item,
  LiveUsage,
  ModelChoice,
  ModelCost,
  ModelOptions,
  ModeChoice,
  Peer,
  PeerModels,
  PendingNewSessionSeed,
  PermissionRequest,
  PlanEntry,
  ProjectEntry,
  PromptImage,
  Quota,
  QuotaRoamingProgress,
  RelayStatus,
  Settings,
  SlashCommand,
  Status,
  Thread,
  ThreadMeta,
  TurnEvent,
  UpdateInfo,
  UpdateOp,
  UpdateProgress,
} from "./types";
import { isScratch, scratchParent } from "./utils";
import {
  handleTurnEnd as handleWorkflowTurnEnd,
  handleTurnStart as handleWorkflowTurnStart,
  initWorkflowRuntime,
  preparePrompt as prepareWorkflowPrompt,
  startWorkflow,
  suspendActive as suspendWorkflowActive,
} from "./workflow/runtime";
import {
  findTriggeredWorkflow,
  findWorkflowByName,
  registerTransientWorkflow,
  unregisterTransientWorkflow,
} from "./workflow/storage";
import { normalizeGeneratedWorkflow } from "./workflow/types";
import { buildEasyPrompt, buildHardDesignPrompt, buildIntegrateModelPrompt, buildPlanPrompt } from "./builtinPrompts";

/** 界面皮肤：深色（默认）/ 浅色 */
export type ThemePref = "ink-dark" | "ink-light";

const THEME_KEY = "fd:theme";
const MODEL_FAVORITES_KEY = "fd:modelFavorites";

function readThemePref(): ThemePref {
  return localStorage.getItem(THEME_KEY) === "ink-light" ? "ink-light" : "ink-dark";
}

function applyThemeToDom(theme: ThemePref) {
  document.documentElement.dataset.theme = theme;
}

function readModelFavorites(): string[] {
  try {
    const value = JSON.parse(localStorage.getItem(MODEL_FAVORITES_KEY) ?? "[]");
    return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
  } catch {
    return [];
  }
}

/** 模型收藏以 settings.json 为可靠存储，localStorage 仅用于升级兼容和即时兜底。 */
export const [modelFavoriteIds, setModelFavoriteIds] = createSignal(readModelFavorites());

export function toggleModelFavorite(id: string) {
  const next = modelFavoriteIds().includes(id)
    ? modelFavoriteIds().filter((favorite) => favorite !== id)
    : [...modelFavoriteIds(), id];
  setModelFavoriteIds(next);
  localStorage.setItem(MODEL_FAVORITES_KEY, JSON.stringify(next));
  const settings = state.settings;
  if (!settings) return;
  const updated = { ...settings, modelFavorites: next };
  setState("settings", updated);
  void api.setSettings(updated).catch(() => {
    // settings.json 落盘失败时仍保留本次 localStorage 写入，不打断选择模型。
  });
}

/** 在首屏渲染前调用，按已保存偏好设置主题，避免明暗闪烁 */
export function initTheme() {
  applyThemeToDom(readThemePref());
}

interface AppStore {
  threads: ThreadMeta[];
  projects: ProjectEntry[];
  currentId: string | null;
  /** 当前打开线程的 transcript */
  items: Item[];
  plan: PlanEntry[] | null;
  /** Plan 模式产出的 proposed plan：非空时展示「实施此计划」选项 */
  proposedPlan: string | null;
  cwd: string;
  /** 大熊座当前项目；切换视图时保留，worktree 由后端归一到主仓库。 */
  trainingCwd: string;
  title: string;
  /** 当前线程的模型/模式（"" = 默认） */
  agentKind: AgentKind;
  model: string;
  mode: string;
  reasoningEffort: string;
  /** 当前打开线程若是漫游 guest，其对端（host）token；否则 null。用于取对端模型列表 */
  roamingPeer: string | null;
  /** 各会话未读新轮次结论数：每次轮次正常结束且该会话非当前打开则 +1，stage 链各自累计 */
  unreadTurns: Record<string, number>;
  running: Record<string, boolean>;
  permissions: PermissionRequest[];
  connected: boolean;
  agent: Status["agent"];
  settings: Settings | null;
  modelOptions: Record<AgentKind, ModelOptions | null>;
  logs: string[];
  loadingThread: boolean;
  liveUsage: LiveUsage | null;
  quota: Quota | null;
  modelCosts: Record<string, ModelCost> | null;
  update: UpdateInfo | null;
  updateStaging: boolean;
  updatePromptAt: number;
  slashCommands: Record<AgentKind, SlashCommand[]>;
  updateProgress: UpdateProgress | null;
  relay: RelayStatus;
  peers: Peer[];
  peerModels: Record<string, PeerModels>;
  peerBranches: Record<string, BranchList>;
  inbox: IncomingShare[];
  workflowInbox: IncomingWorkflowShare[];
  achievements: Achievement[];
  achievementsLoaded: boolean;
  achievementsError: string;
  unseenAchievementIds: string[];
  inboxPromptAt: number;
  incomingRoams: IncomingRoamRequest[];
  quotaRoamingProgress: QuotaRoamingProgress | null;
  roamingFolders: string[];
  expanded: Record<string, boolean>;
  titleTyping: Record<string, boolean>;
  /** 主区域视图（currentId 非空时优先显示会话，与本字段无关） */
  view: "home" | "clues" | "workflows" | "training" | "browser";
  /** 当前证据链空间。个人空间始终本地保存，团队空间通过中转站共享。 */
  clueSpace: "personal" | "team";
  /** 证据链的隐藏节点组；界面只渲染其中的 ClueCard。 */
  clueGroups: ClueNodeGroup[];
  pendingClueCard: { id: string; title: string } | null;
  pendingNewSessionSeed: PendingNewSessionSeed | null;
  homeComposerFocusAt: number;
  clueOpenRequest: string | null;
  unreadClueMentions: string[];
  theme: ThemePref;
  backendAvailability: Record<string, boolean>;
}

export const [state, setState] = createStore<AppStore>({
  threads: [],
  projects: [],
  currentId: null,
  items: [],
  plan: null,
  proposedPlan: null,
  cwd: "",
  trainingCwd: "",
  title: "",
  agentKind: "devin",
  model: "",
  mode: "",
  reasoningEffort: "",
  roamingPeer: null,
  running: {},
  permissions: [],
  connected: false,
  agent: null,
  settings: null,
  modelOptions: {
    lyra: null,
    devin: null,
    codex: null,
    codebuddy: null,
    claudecode: null,
    cursor: null,
    opencode: null,
  },
  logs: [],
  loadingThread: false,
  liveUsage: null,
  quota: null,
  modelCosts: null,
  update: null,
  updateStaging: false,
  updatePromptAt: 0,
  slashCommands: {
    lyra: [],
    devin: [],
    codex: [],
    codebuddy: [],
    claudecode: [],
    cursor: [],
    opencode: [],
  },
  updateProgress: null,
  relay: { enabled: false, connected: false },
  peers: [],
  peerModels: {},
  peerBranches: {},
  inbox: [],
  workflowInbox: [],
  achievements: [],
  achievementsLoaded: false,
  achievementsError: "",
  unseenAchievementIds: [],
  inboxPromptAt: 0,
  incomingRoams: [],
  quotaRoamingProgress: null,
  roamingFolders: [],
  expanded: {},
  titleTyping: {},
  unreadTurns: {},
  view: "home",
  clueSpace: "personal",
  clueGroups: [],
  pendingClueCard: null,
  pendingNewSessionSeed: null,
  homeComposerFocusAt: 0,
  clueOpenRequest: null,
  unreadClueMentions: [],
  theme: readThemePref(),
  backendAvailability: {},
});

function isThemePref(v: unknown): v is ThemePref {
  return v === "ink-dark" || v === "ink-light";
}

/**
 * 把主题持久化到后端 settings.json：localStorage 在 WebView2 里是惰性落盘，
 * 自更新重启/异常退出可能丢失最近一次写入（表现为「主题有时没保存」）。
 * 后端 settings.json 由 Rust 同步写盘，作为可靠的真相来源。
 */
function persistThemeToBackend(theme: ThemePref) {
  const s = state.settings;
  if (!s || s.theme === theme) return;
  const next = { ...s, theme };
  setState("settings", next);
  void api.setSettings(next).catch(() => {
    // 落盘失败仅丢失跨重启持久化，localStorage 兜底，不打断使用
  });
}

/** 切换并持久化界面皮肤，立即应用到 DOM */
export function setTheme(theme: ThemePref) {
  localStorage.setItem(THEME_KEY, theme);
  applyThemeToDom(theme);
  setState("theme", theme);
  persistThemeToBackend(theme);
}

/**
 * 详情展开状态放在 store 而非组件本地：
 * 流式更新会重建分组对象导致组件重挂载，本地 signal 会丢失（手动展开被收起）
 */
export function isExpanded(key: number | string, fallback = false): boolean {
  // 必须无条件读取 state.expanded[k] 来建立 Solid 响应式订阅：
  // hasOwnProperty 之类的存在性检查不会被 Solid 跟踪，首次赋值新 key 时组件不会重渲染，
  // 表现为「首次点击展不开 / 展开后收不回」。读取值后再判断 undefined 区分未设置与显式 false。
  const v = state.expanded[String(key)];
  return v === undefined ? fallback : !!v;
}

const [expandedRevisionValue, setExpandedRevision] = createSignal(0);
export const expandedRevision = expandedRevisionValue;

export function toggleExpanded(key: number | string, value?: boolean) {
  const k = String(key);
  setState("expanded", k, value ?? !state.expanded[k]);
  setExpandedRevision((revision) => revision + 1);
}

function resetExpanded(): void {
  setState("expanded", reconcile({}));
  setExpandedRevision((revision) => revision + 1);
}

/** 可选模型列表（来自 devin）。
 *  source 显式传入时用它（漫游用对端列表；传 null 表示对端该后端无列表 → 空）；
 *  不传则用本机全局 modelOptions。 */
export function modelChoices(
  agentKind: AgentKind = state.agentKind,
  source?: ModelOptions | null,
): ModelChoice[] {
  const opts = (source !== undefined ? source : state.modelOptions[agentKind])?.configOptions;
  if (!opts) return [];
  const model = opts.find((o) => o.id === "model");
  const choices = (model?.options as ModelChoice[]) ?? [];
  if (agentKind !== "codex" && agentKind !== "opencode") return choices;
  // OpenCode 的 Auto 只能路由到 GPT；未配置任何 GPT 时不展示，避免产生无效入口。
  if (
    agentKind === "opencode" &&
    !choices.some((choice) => choice.value.toLowerCase().includes("gpt"))
  ) {
    return choices;
  }
  const auto: ModelChoice[] = [
    {
      value: "__nova_auto_community__",
      name: "Auto（按社区评分）",
      description: "新会话首次发送前获取近 24 小时社区体感分第一名（排除 ultra），后续固定复用；数据来自 Codex 雷达 codexradar.com",
    },
  ];
  return [...auto, ...choices.filter((choice) => !choice.value.startsWith("__nova_auto_"))];
}

/** 在可选列表中解析应使用的模型。
 *  优先 preferred；明确模型即使暂不在列表也必须保留，避免异步加载时被覆盖；
 *  仅 preferred 为空时才回退上次模型 / 第一项（跳过 Cursor「Auto」这类 value="" 入口）。 */
export function resolveAvailableModel(
  agentKind: AgentKind,
  preferred: string,
  source?: ModelOptions | null,
): string {
  const choices = modelChoices(agentKind, source);
  if (choices.length === 0) return preferred;
  if (preferred && choices.some((c) => c.value === preferred)) return preferred;
  // 有明确选择但不在当前列表：保留，避免模型列表不全/中间态覆盖最近选择。
  if (preferred) return preferred;
  const previous = lastUsed.model(agentKind);
  if (previous && choices.some((c) => c.value === previous)) return previous;
  // 跳过 Cursor「Auto」哨兵入口（非空但不选具体模型），保持默认落到第一个具体模型；
  // 显式选中过 Auto 的走上面 previous 分支保留。
  return (
    choices.find((c) => c.value && c.value !== "__cursor_auto__")?.value ??
    choices[0]?.value ??
    ""
  );
}

/** 界面可选会话模式：仅 Build。Plan 不进选择器，只由 /plan 隐式启动；
 *  发送时由 Rust 侧把 build 翻译成各后端真实模式 id（bypass / bypassPermissions / agent …）。 */
export const UNIFIED_MODES: ModeChoice[] = [{ id: "build", name: "Build" }];

/** 可选会话模式列表（界面用）。参数保留以兼容既有调用点。 */
export function modeChoices(
  _agentKind: AgentKind = state.agentKind,
  _source?: ModelOptions | null,
): ModeChoice[] {
  return UNIFIED_MODES;
}

/** 旧模式值 → 统一模式 id。Plan 已废弃，一律归一为 Build。识别不了返回 undefined。 */
export function normalizeUnifiedMode(m?: string | null): "build" | undefined {
  if (!m) return undefined;
  if (
    ["build", "bypass", "bypassPermissions", "agent", "dontAsk", "fullAccess", "plan"].includes(
      m.toLowerCase(),
    )
  ) {
    return "build";
  }
  return undefined;
}

function selectedModelChoice(agentKind: AgentKind, model: string): ModelChoice | undefined {
  const choices = modelChoices(agentKind);
  return (
    choices.find((m) => m.value === model) ??
    choices.find((m) => m._meta?.["codex.ai/default"] === true) ??
    choices[0]
  );
}

export function reasoningEffortChoices(
  agentKind: AgentKind = state.agentKind,
  model: string = state.model,
): EffortChoice[] {
  if (agentKind !== "codex") return [];
  const selected = selectedModelChoice(agentKind, model);
  const raw = selected?._meta?.["codex.ai/supportedReasoningEfforts"];
  if (Array.isArray(raw)) {
    return raw
      .map((e) => {
        if (typeof e === "string") return { value: e, name: e } satisfies EffortChoice;
        if (e && typeof e === "object") {
          const obj = e as Record<string, unknown>;
          const value = obj.value ?? obj.reasoningEffort;
          const name = obj.name ?? value;
          if (typeof value === "string" && typeof name === "string") {
            return {
              value,
              name,
              description:
                typeof obj.description === "string" ? obj.description : undefined,
            } satisfies EffortChoice;
          }
        }
        return null;
      })
      .filter((e): e is EffortChoice => !!e);
  }
  const opts = state.modelOptions[agentKind]?.configOptions?.find((o) => o.id === "effort");
  return ((opts?.options as EffortChoice[] | undefined) ?? []).filter((e) => !!e.value);
}

const modelOptionsLoading = new Set<AgentKind>();
/** Lyra 配置文件当前声明的默认模型；用于配置热更新时同步新会话页。 */
export const [lyraConfigDefaultModel, setLyraConfigDefaultModel] = createSignal("");

/** store 的对象赋值是浅合并，占位里的 pending 不会被后来的真实列表冲掉，必须显式复位。 */
function setModelOptions(agentKind: AgentKind, opts: ModelOptions | null) {
  setState("modelOptions", agentKind, opts && { pending: false, ...opts });
}

export async function ensureModelOptions(agentKind: AgentKind) {
  // pending 是后端「还没拉到」的占位：缓存它会让填完 API Key 后永远停在空列表，
  // 所以只有拿到真实列表才算加载完成。
  const cached = state.modelOptions[agentKind];
  if ((cached && !cached.pending) || modelOptionsLoading.has(agentKind)) return;
  modelOptionsLoading.add(agentKind);
  try {
    const opts = await api.getModelOptions(agentKind);
    setModelOptions(agentKind, opts);
    syncLyraConfigDefaultModel(agentKind, opts);
    // 选项就绪后回填友好名，供下次冷启动触发器使用
    const model = lastUsed.model(agentKind);
    const name = modelChoices(agentKind).find((c) => c.value === model)?.name;
    if (model && name) lastUsed.setModelName(agentKind, name);
  } catch {
    // 模型列表拉取失败不影响会话发送，后端仍可使用 agent 默认模型
  } finally {
    modelOptionsLoading.delete(agentKind);
  }
}

/** 模型后端固定展示顺序 */
export const ALL_AGENT_KINDS: AgentKind[] = [
  "lyra",
  "devin",
  "codex",
  "codebuddy",
  "claudecode",
  "cursor",
  "opencode",
];

/** 某后端在设置里是否启用。缺字段（老版本 settings）按启用处理（!== false）。 */
function agentEnabled(s: Settings, k: AgentKind): boolean {
  switch (k) {
    case "lyra":
      return s.lyraEnabled !== false;
    case "devin":
      return s.devinEnabled !== false;
    case "codex":
      return s.codexEnabled !== false;
    case "codebuddy":
      return s.codebuddyEnabled !== false;
    case "claudecode":
      return s.claudecodeEnabled !== false;
    case "cursor":
      return s.cursorEnabled !== false;
    case "opencode":
      return s.opencodeEnabled !== false;
  }
}

/** 设置中已启用的模型后端（按固定顺序）。
 *  启用状态是选择器是否展示后端的唯一条件；CLI 探测结果只用于设置页提示，
 *  不再隐藏后端，避免环境探测失败让用户连配置或尝试启动的入口都看不到。
 *  settings 未加载前返回空，避免组件抢先探测全部后端。 */
export function enabledAgentKinds(): AgentKind[] {
  const s = state.settings;
  if (!s) return [];
  return ALL_AGENT_KINDS.filter((k) => agentEnabled(s, k));
}

/** 把 agentKind 收敛到「已启用」集合：已启用则原样返回，否则回退到第一个启用项 */
export function resolveEnabledAgentKind(kind: AgentKind): AgentKind {
  const list = enabledAgentKinds();
  return list.includes(kind) ? kind : (list[0] ?? kind);
}

let refreshThreadsRequest = 0;
// 事件版本比单纯比较 running 值可靠：同一状态的重复事件也不能被旧快照覆盖。
const runningEventVersions = new Map<string, number>();
// send_prompt 先乐观置忙，后端随后才会登记 manager.running；在这段窗口内刷新
// list_threads 只能看到 false，额度租借创建线程时尤其容易撞上这个竞态。
const optimisticRunningThreads = new Set<string>();

export async function refreshThreads() {
  const request = ++refreshThreadsRequest;
  // list_threads 可能与 send_prompt 并发：额度会话创建后会立即触发一次刷新，
  // 但后端此时还没来得及把异步 agent 任务登记为 running。记录请求开始时的
  // 事件版本，若期间已经收到 acp:turn，就不能再用这份旧快照覆盖事件结果。
  const runningVersionsBeforeRequest = new Map(runningEventVersions);
  const threads = await api.listThreads();
  // 多次刷新并发时，较早的响应不能覆盖较新的响应。
  if (request !== refreshThreadsRequest) return;
  // 按 id reconcile 而非整体替换：保留未变线程的对象身份，
  // 避免 <For> 重建整个列表 DOM 导致侧边栏滚动位置被重置
  setState("threads", reconcile(threads, { key: "id" }));
  const running: Record<string, boolean> = {};
  for (const t of threads) {
    // 运行事件在本次请求期间到达时，以事件为准；否则使用后端快照。
    // 这样不会因额度租借的「创建线程刷新」竞态把实际运行态冲回 false。
    const before = runningVersionsBeforeRequest.get(t.id) ?? 0;
    const current = runningEventVersions.get(t.id) ?? 0;
    running[t.id] = optimisticRunningThreads.has(t.id)
      ? true
      : current !== before
        ? !!state.running[t.id]
        : t.running;
  }
  setState("running", running);
  // 完整 transcript 只在 WebView 中保留少量 LRU 快照。旧实现会在启动和每次刷新时
  // 把所有会话补齐进 JS，运行过的长会话越多，内存与 GC 停顿越明显，最终连点击和发送都会卡。
  const ids = new Set(threads.map((thread) => thread.id));
  for (const id of threadSnapshots.keys()) if (!ids.has(id)) threadSnapshots.delete(id);
  for (const thread of threads) {
    const snapshot = threadSnapshots.peek(thread.id);
    if (snapshot && thread.updatedAt > snapshot.updatedAt) staleThreadSnapshots.add(thread.id);
  }
}

export async function refreshProjects() {
  setState("projects", await api.listProjects());
}

export async function refreshQuota() {
  try {
    setState("quota", await api.getQuota());
  } catch {
    // 网络不可用等场景静默失败，保留上次数据
  }
}

export async function refreshModelCosts() {
  try {
    setState("modelCosts", await api.getModelCosts());
  } catch {
    // 拉取失败仅丢失费用展示，不影响选模型
  }
}

function normalizePeers(raw: { peers: Peer[] } | Peer[]): Peer[] {
  const arr = Array.isArray(raw) ? raw : raw?.peers;
  return Array.isArray(arr)
    ? arr.map((peer) => ({
        ...peer,
        folders: Array.isArray(peer.folders)
          ? peer.folders.filter((folder) => !isScratch(folder.path))
          : [],
      }))
    : [];
}

export async function refreshRelayStatus() {
  try {
    setState("relay", await api.getRelayStatus());
  } catch {
    // 中转站不可用时静默
  }
}

export function clueMentionPeers(): Peer[] {
  const ownToken = state.settings?.relayToken ?? "";
  const firstGroup =
    (state.settings?.relayGroups ?? "")
      .split(/[,;\s]+/)
      .map((group) => group.trim())
      .find(Boolean) ?? "";
  return state.peers.filter((peer) => {
    if (peer.token === ownToken) return false;
    if (!Array.isArray(peer.groups)) return true;
    const groups = peer.groups.length > 0 ? peer.groups : [""];
    return groups.includes(firstGroup);
  });
}

export async function refreshInbox() {
  try {
    setState("inbox", await api.getRelayInbox());
  } catch {
    // 忽略
  }
}

export async function refreshWorkflowInbox() {
  try {
    setState("workflowInbox", await api.getRelayWorkflowInbox());
  } catch {
    // 忽略
  }
}

// ===== 成就 =====

const ACHIEVEMENT_IMAGE_CACHE_KEY = "nova.achievementImageRefresh";
const SEEN_ACHIEVEMENTS_KEY = "nova.seenAchievements";

function createAchievementImageCacheKey(): string {
  return `${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

function loadAchievementImageCacheKey(): string {
  const saved = localStorage.getItem(ACHIEVEMENT_IMAGE_CACHE_KEY);
  if (saved) return saved;

  const created = createAchievementImageCacheKey();
  localStorage.setItem(ACHIEVEMENT_IMAGE_CACHE_KEY, created);
  return created;
}

/** 给徽章图追加/替换刷新参数，强制重新拉取（服务器可能按身份换图） */
function reloadAchievementImage(url: string, cacheKey: string): string {
  const hashIndex = url.indexOf("#");
  const resource = hashIndex >= 0 ? url.slice(0, hashIndex) : url;
  const hash = hashIndex >= 0 ? url.slice(hashIndex) : "";
  const encodedCacheKey = encodeURIComponent(cacheKey);
  const existingKey = /([?&])_nova_refresh=[^&#]*/;
  if (existingKey.test(resource)) {
    return `${resource.replace(existingKey, `$1_nova_refresh=${encodedCacheKey}`)}${hash}`;
  }
  const separator = resource.includes("?") ? "&" : "?";
  return `${resource}${separator}_nova_refresh=${encodedCacheKey}${hash}`;
}

let achievementImageCacheKey = loadAchievementImageCacheKey();

function loadSeenAchievementIds(): string[] {
  try {
    const raw = localStorage.getItem(SEEN_ACHIEVEMENTS_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    return Array.isArray(parsed)
      ? parsed.filter((v): v is string => typeof v === "string")
      : [];
  } catch {
    return [];
  }
}

export async function refreshAchievements(reloadImages = false) {
  if (!state.settings?.relayToken?.trim()) {
    setState({
      achievements: [],
      achievementsLoaded: true,
      achievementsError: "",
      unseenAchievementIds: [],
    });
    return;
  }
  if (reloadImages) {
    achievementImageCacheKey = createAchievementImageCacheKey();
    localStorage.setItem(ACHIEVEMENT_IMAGE_CACHE_KEY, achievementImageCacheKey);
  }
  try {
    const list = await api.listAchievements();
    const seen = new Set(loadSeenAchievementIds());
    setState({
      achievements: list.map((achievement) => ({
        ...achievement,
        imageUrl: achievement.imageUrl
          ? reloadAchievementImage(achievement.imageUrl, achievementImageCacheKey)
          : achievement.imageUrl,
      })),
      achievementsLoaded: true,
      achievementsError: "",
      unseenAchievementIds: list.filter((a) => !seen.has(a.id)).map((a) => a.id),
    });
  } catch (error) {
    setState({
      achievements: [],
      achievementsLoaded: true,
      achievementsError: String(error),
      unseenAchievementIds: [],
    });
  }
}

/** 打开成就页后调用：当前成就全部标记为已看，清掉侧栏角标 */
export function markAchievementsSeen() {
  if (state.unseenAchievementIds.length === 0) return;
  const seen = new Set(loadSeenAchievementIds());
  for (const achievement of state.achievements) seen.add(achievement.id);
  localStorage.setItem(SEEN_ACHIEVEMENTS_KEY, JSON.stringify([...seen]));
  setState("unseenAchievementIds", []);
}

export async function refreshRoamingFolders() {
  try {
    setState("roamingFolders", await api.listRoamingFolders());
  } catch {
    // 忽略
  }
}

export function setView(view: "home" | "clues" | "workflows" | "training" | "browser") {
  setState("view", view);
}

export function setTrainingProject(cwd: string) {
  setState("trainingCwd", cwd);
}

export function clueCurrentVersion(card: ClueCard) {
  return card.versions.find((version) => version.id === card.currentVersionId) ?? card.versions.at(-1);
}

export function clueCardById(cardId: string | null | undefined): ClueCard | undefined {
  if (!cardId) return undefined;
  for (const group of state.clueGroups) {
    const card = group.cards.find((item) => item.id === cardId);
    if (card) return card;
  }
  return undefined;
}

export async function refreshClueGroups() {
  const space = state.clueSpace;
  const groups = await api.listClueGroups(space);
  if (state.clueSpace === space) setState("clueGroups", reconcile(groups));
}

export async function setClueSpace(space: "personal" | "team") {
  if (space === state.clueSpace) return;
  setState({ clueSpace: space, clueGroups: [] });
  await refreshClueGroups();
}

export async function captureClue(
  threadId: string | null,
  title: string,
  content: string,
  placement: "update" | "parallel" | "new",
  targetCardId: string | null,
  mentionTokens: string[] = [],
  attachments: ClueAttachment[] = [],
  spaceOverride?: "personal" | "team",
): Promise<CaptureClueResult> {
  const space = spaceOverride ?? state.clueSpace;
  const result = await api.captureClue(
    threadId, title, content, placement, targetCardId, mentionTokens, attachments, space,
  );
  // 只把结果合并进当前正在查看的空间；Flow 弹窗可保存到另一个空间。
  if (state.clueSpace === space) {
    const index = state.clueGroups.findIndex((group) => group.id === result.group.id);
    if (index >= 0) setState("clueGroups", index, reconcile(result.group));
    else setState("clueGroups", (groups) => [result.group, ...groups]);
  }
  return result;
}

export async function addClueComment(
  cardId: string,
  content: string,
  parentCommentId: string | null,
  mentionTokens: string[] = [],
) {
  await api.addClueComment(cardId, content, parentCommentId, mentionTokens, state.clueSpace);
  await refreshClueGroups();
}

/** 用轻量级模型总结会话，失败时回退原模型，供线索表单填入 */
export async function summarizeClue(threadId: string) {
  return api.summarizeClue(threadId);
}

export async function associateClues(beforeCardId: string, afterCardId: string) {
  await api.associateClues(beforeCardId, afterCardId, state.clueSpace);
  await refreshClueGroups();
}

export async function disassociateClues(beforeCardId: string, afterCardId: string) {
  await api.disassociateClues(beforeCardId, afterCardId, state.clueSpace);
  await refreshClueGroups();
}

export async function splitClue(cardId: string) {
  await api.splitClue(cardId, state.clueSpace);
  await refreshClueGroups();
}

export async function stackClues(cardIds: string[]) {
  await api.stackClues(cardIds, state.clueSpace);
  await refreshClueGroups();
}

export async function deleteClue(cardId: string) {
  await api.deleteClue(cardId, state.clueSpace);
  await Promise.all([refreshClueGroups(), refreshThreads()]);
}

export function startSessionFromClue(card: ClueCard) {
  const version = clueCurrentVersion(card);
  setState("pendingClueCard", { id: card.id, title: version?.title || "未命名线索" });
  closeThread();
  setView("home");
}

export function clearPendingClueCard() {
  setState("pendingClueCard", null);
}

/** 打开新会话页；若当前在会话中，把工作目录与模型带给 HomeView。 */
export function openNewSession(quote = "") {
  const id = state.currentId;
  if (id) {
    const meta = state.threads.find((thread) => thread.id === id);
    // worktree 会话的 state.cwd 已指向 worktree 工作目录，新会话应留在同一 worktree；
    // 只有 cwd 缺失时才回退到源仓库（后端也会按已知 worktree 目录补齐标注）。
    const cwd = state.cwd || meta?.worktree?.repo || "";
    const seed: PendingNewSessionSeed = {
      cwd,
      agentKind: state.agentKind,
      model: state.model,
      mode: state.mode,
      reasoningEffort: state.reasoningEffort,
      roam: null,
      quotaPeerToken: null,
      quote: quote.trim(),
    };
    if (meta?.roamingRole === "guest" && state.roamingPeer) {
      seed.roam = { peerToken: state.roamingPeer, folder: state.cwd };
      seed.cwd = state.cwd;
    } else if (meta?.quotaPeerName && state.roamingPeer) {
      seed.quotaPeerToken = state.roamingPeer;
    }
    setState("pendingNewSessionSeed", seed);
  } else {
    setState("pendingNewSessionSeed", null);
  }
  closeThread();
  setView("home");
  setState("homeComposerFocusAt", Date.now());
}

/** HomeView 挂载时取走一次继承种子。 */
export function takePendingNewSessionSeed(): PendingNewSessionSeed | null {
  const seed = state.pendingNewSessionSeed;
  if (seed) setState("pendingNewSessionSeed", null);
  return seed;
}

export function openClueCard(cardId: string) {
  if (!cardId) return;
  setState("unreadClueMentions", (ids) => ids.filter((id) => id !== cardId));
  setView("clues");
  closeThread();
  setState("clueOpenRequest", cardId);
  void refreshClueGroups();
}

export function clearClueOpenRequest(cardId: string) {
  if (state.clueOpenRequest === cardId) setState("clueOpenRequest", null);
}

export function markClueMentionRead(cardId: string) {
  setState("unreadClueMentions", (ids) => ids.filter((id) => id !== cardId));
}

/** 在线的其他人（排除自己）。漫游只能选择对方已共享（上报）的目录，不再支持手输路径；
 *  没有共享目录的队友会在下拉里提示「对方暂未共享可漫游的项目」。 */
export function roamingPeers(): Peer[] {
  const me = state.settings?.relayToken ?? "";
  return state.peers.filter((p) => p.online && p.token !== me);
}

/** 额度共享可选的队友（排除自己，离线也保留）：只要对方未撤销共享，
 *  借用方应可持续使用已获取的凭证/模型列表，不应因对方关机/离线而消失。 */
export function quotaPeers(): Peer[] {
  const me = state.settings?.relayToken ?? "";
  return state.peers.filter((p) => p.token !== me && !!p.token);
}

/** 漫游：确保已拉取某对端（host）的模型/模式列表。已缓存则跳过，force 时强制刷新。
 *  对端异步回传，经 relay:peer-models 事件写入 state.peerModels[token]。 */
export function ensurePeerModels(token: string, force = false) {
  if (!token) return;
  if (!force && state.peerModels[token]) return;
  void api.requestPeerModels(token).catch(() => {
    // 对端离线/未连接时静默失败，选择器回退为空，用户可稍后重试
  });
}

/** worktree「基于分支」缓存 key：对端 token + 目录 */
export function peerBranchKey(token: string, folder: string): string {
  return `${token}:${folder}`;
}

/** 漫游：请求对端某目录的本地分支列表（worktree「基于分支」下拉）。
 *  结果经 relay:peer-branches 事件写入 state.peerBranches。总是重新请求以拿到最新分支。 */
export function ensurePeerBranches(token: string, folder: string) {
  if (!token || !folder) return;
  void api.requestPeerBranches(token, folder).catch(() => {
    // 对端离线/未连接时静默失败，下拉回退为空，用户可手填 base
  });
}

/**
 * 后台静默预加载「本群组在线队友」的漫游模型列表：presence 变化 / 重连时调用。
 * 目的是等用户真正发起漫游时，对端模型列表大概率已就绪，不必「用到再加载」而干等。
 * force=true 会强制刷新已缓存的列表（用于定期更新，保持与对端后端配置同步）。
 */
export function preloadPeerModels(force = false) {
  if (!state.relay.connected) return;
  const me = state.settings?.relayToken ?? "";
  for (const p of state.peers) {
    if (p.online && p.token && p.token !== me) ensurePeerModels(p.token, force);
  }
}

/** 检查更新并静默下载暂存；返回给用户看的提示文案 */
export async function checkAndStageUpdate(): Promise<string> {
  const info = await api.checkUpdate();
  if (!info.hasUpdate) return "已是最新版本";
  if (info.staged) {
    setState("update", info);
    return "新版本已就绪，可重启更新";
  }
  setState("updateProgress", null);
  setState("updateStaging", true);
  try {
    const res = await api.downloadStagedUpdate();
    if (res.ready) {
      setState("update", { ...info, staged: true });
      return "新版本已下载好，可重启更新";
    }
    return "已是最新版本";
  } finally {
    setState("updateStaging", false);
  }
}

/** 应用已下载好的更新（替换并重启） */
export async function applyStagedUpdate() {
  await api.applyStagedUpdate();
}

let lastActivityReport = 0;
/** 活动上报节流窗口：鼠标移动等高频事件最多每 5 秒上报一次，足够后端判断空闲 */
const ACTIVITY_REPORT_MS = 5000;

/**
 * 上报一次用户活动（最近操作时间 + 当前打开的会话），供后端静默升级判断空闲与恢复会话。
 * force=true 用于会话切换等关键时刻立即上报，确保后端记录的「当前会话」始终最新。
 */
export function reportActivity(force = false) {
  const now = Date.now();
  if (!force && now - lastActivityReport < ACTIVITY_REPORT_MS) return;
  lastActivityReport = now;
  void api.reportActivity(state.currentId).catch(() => {
    // 上报失败仅影响静默升级判定，不打断使用
  });
}

let activityTrackingStarted = false;
/** 监听全局鼠标/键盘/滚动/聚焦等交互，节流上报活动；用户长时间无操作时后端据此判定空闲 */
function initActivityTracking() {
  if (activityTrackingStarted) return;
  activityTrackingStarted = true;
  const onActivity = () => reportActivity();
  for (const ev of ["pointerdown", "pointermove", "keydown", "wheel", "focus"]) {
    window.addEventListener(ev, onActivity, { passive: true });
  }
  reportActivity(true);
}

/** 漫游：在某个在线用户的目录上新建会话并打开 */
export async function createRoamingThread(
  peer: Peer,
  folder: string,
  agentKind: AgentKind,
  model: string,
  mode: string,
  firstPrompt = "",
  clueCardId = "",
  worktree = false,
  worktreeBranch = "",
  worktreeBase = "",
): Promise<string> {
  const t = await api.createRoamingThread(
    peer.token,
    peer.name,
    folder,
    agentKind,
    model || null,
    mode || null,
    firstPrompt.trim() || null,
    clueCardId || null,
    worktree,
    worktreeBranch.trim() || null,
    worktreeBase.trim() || null,
  );
  rememberThreadSnapshot(t);
  setState("expanded", reconcile({}));
  setState({
    currentId: t.id,
    items: t.items,
    plan: (t.plan as PlanEntry[] | null) ?? null,
    proposedPlan: null,
    cwd: t.cwd,
    title: t.title,
    agentKind: t.agentKind ?? agentKind,
    model: t.model ?? "",
    mode: t.mode ?? "",
    reasoningEffort: "",
    roamingPeer: peer.token,
    loadingThread: false,
  });
  setState("running", t.id, false);
  reportActivity(true);
  void refreshThreads();
  void ensureModelOptions(t.agentKind ?? agentKind);
  ensurePeerModels(peer.token);
  return t.id;
}

/** host：应答一条漫游请求（接受/拒绝），无论成败都从队列移除 */
export async function respondRoamRequest(
  reqId: string,
  accept: boolean,
  changes: Parameters<typeof api.respondRoamRequest>[2],
) {
  try {
    await api.respondRoamRequest(reqId, accept, changes);
  } finally {
    setState("incomingRoams", (prev) => prev.filter((r) => r.reqId !== reqId));
  }
}

/** 本机目录执行，使用已同步的队友共享后端额度。 */
export async function createQuotaThread(
  peer: Peer,
  cwd: string,
  agentKind: AgentKind,
  model: string,
  mode: string,
  clueCardId = "",
): Promise<string> {
  const operationId = crypto.randomUUID();
  const t = await api.createQuotaThread(
    peer.token,
    peer.name,
    cwd,
    agentKind,
    model || null,
    mode || null,
    clueCardId || null,
    operationId,
  );
  rememberThreadSnapshot(t);
  setState("expanded", reconcile({}));
  setState({
    currentId: t.id,
    items: t.items,
    plan: (t.plan as PlanEntry[] | null) ?? null,
    proposedPlan: null,
    cwd: t.cwd,
    title: t.title,
    agentKind: t.agentKind ?? agentKind,
    model: t.model ?? "",
    mode: t.mode ?? "",
    reasoningEffort: "",
    roamingPeer: peer.token,
    loadingThread: false,
  });
  // 新线程通常紧接着就会由首页 sendPrompt 投递；不要在这里把后端尚未登记
  // running 的瞬间覆盖掉事件/发送方的乐观忙碌态。
  if (!optimisticRunningThreads.has(t.id)) setState("running", t.id, false);
  reportActivity(true);
  void refreshThreads();
  ensurePeerModels(peer.token);
  return t.id;
}

export function clearQuotaRoamingProgress() {
  setState("quotaRoamingProgress", null);
}

// 覆盖典型 /stage 链（源会话 + 多个 stage 节点）来回切换，避免快照互相挤出后
// 每次切换都退化成 getThread 全量拉取整条 transcript。items 是 unwrap 浅引用，
// 单条快照内存开销可控，上限取 8 而非无上限。
const THREAD_SNAPSHOT_LIMIT = 8;
const threadSnapshots = new LruMap<string, Thread>(THREAD_SNAPSHOT_LIMIT);
const staleThreadSnapshots = new Set<string>();
/** 运行中各会话最近一次实时用量；切换会话时恢复，避免等待下一次 usage 事件。 */
const liveUsageByThread = new Map<string, LiveUsage>();

function rememberThreadSnapshot(thread: Thread) {
  const evicted = threadSnapshots.set(thread.id, thread, state.currentId ?? undefined);
  staleThreadSnapshots.delete(thread.id);
  for (const id of evicted) staleThreadSnapshots.delete(id);
}

function getThreadSnapshot(id: string): Thread | undefined {
  return threadSnapshots.get(id);
}

function rememberCurrentThreadSnapshot() {
  const id = state.currentId;
  if (!id) return;
  const thread = threadSnapshots.peek(id);
  if (!thread) return;
  rememberThreadSnapshot({
    ...thread,
    title: state.title,
    cwd: state.cwd,
    agentKind: state.agentKind,
    model: state.model || null,
    mode: state.mode || null,
    reasoningEffort: state.reasoningEffort || null,
    // Solid store 的 raw 数组本就驻留内存；保留引用即可，切换时不深拷贝整段 transcript。
    items: unwrap(state.items),
    plan: state.plan ? unwrap(state.plan) : null,
  });
}

function showThreadSnapshot(thread: Thread, loadingThread: boolean, reconcileItems = false) {
  const agentKind = thread.agentKind ?? "devin";
  if (reconcileItems) {
    setState("items", reconcile(thread.items, { key: "id" }));
  }
  setState({
    currentId: thread.id,
    ...(!reconcileItems ? { items: thread.items } : {}),
    plan: (thread.plan as PlanEntry[] | null) ?? null,
    proposedPlan: recoverProposedPlan(thread),
    cwd: thread.cwd,
    title: thread.title,
    agentKind,
    model: thread.model ?? "",
    mode: thread.mode ?? "",
    reasoningEffort: thread.reasoningEffort ?? "",
    roamingPeer:
      thread.roamingRole === "guest"
        ? thread.roamingPeer ?? null
        : thread.quotaPeer ?? null,
    loadingThread,
    // 运行中切回会话时恢复此前收到的 usage；结束会话仍以 Turn 项的最终值为准。
    liveUsage: state.running[thread.id] ? (liveUsageByThread.get(thread.id) ?? null) : null,
  });
}

/** Plan 模式且已结束的会话：从最后一轮助手正文恢复「实施此计划」按钮 */
function recoverProposedPlan(_thread: Thread): string | null {
  // Plan 模式已废弃；proposed plan 仅在当轮流式 proposed_plan 事件中展示。
  return null;
}

let openThreadRequest = 0;

/** 切换会话耗时自测：仅在总耗时超阈值时写一行 agent 日志，release 包也能定位卡点。 */
let switchTraceStart = 0;
/** 用户在会话行上按下指针的瞬间（早于 click/openThread），用于暴露"点击→开始切换"盲区。 */
let switchPointerDownAt = 0;
export function markThreadSwitchPointerDown() {
  switchPointerDownAt = performance.now();
}
export function markThreadSwitchStart() {
  switchTraceStart = performance.now();
  // 点击/按下 到 openThread 真正开始执行之间的等待（主线程被占时点击看似没反应）。
  if (switchPointerDownAt) {
    const wait = Math.round(switchTraceStart - switchPointerDownAt);
    switchPointerDownAt = 0;
    if (wait >= 150) {
      setState("logs", (logs) => [...logs, `[切换卡顿] 点击→开始切换 等待 ${wait}ms（主线程被占）`]);
    }
  }
}
function traceThreadSwitch(id: string, phase: string) {
  if (!switchTraceStart) return;
  const ms = Math.round(performance.now() - switchTraceStart);
  if (ms < 150) return; // 150ms 内不算卡，不刷日志
  setState("logs", (logs) => [...logs, `[切换卡顿] ${phase} +${ms}ms (会话 ${id.slice(0, 8)})`]);
}
/** 布局落地后收尾：写一行总耗时并清零，避免后续无关操作重复打点。 */
export function traceThreadSwitchLayoutDone(threadId: string | null, groupCount: number) {
  if (!switchTraceStart || !threadId) return;
  const ms = Math.round(performance.now() - switchTraceStart);
  switchTraceStart = 0;
  if (ms < 150) return;
  setState("logs", (logs) => [
    ...logs,
    `[切换卡顿] 布局+绘制落地 总${ms}ms (${groupCount} 组, 会话 ${threadId.slice(0, 8)})`,
  ]);
}

export async function openThread(id: string) {
  if (state.unreadTurns[id]) setState("unreadTurns", id, 0);
  const switching = state.currentId !== id;
  const request = switching ? ++openThreadRequest : openThreadRequest;
  const previousId = state.currentId;
  if (switching && !switchTraceStart) markThreadSwitchStart();
  flushPendingStreamUpdates();
  if (switching) {
    rememberCurrentThreadSnapshot();
    if (previousId && state.running[previousId] && threadSnapshots.has(previousId)) {
      staleThreadSnapshots.add(previousId);
    }
  }

  const cached = getThreadSnapshot(id);
  const commitSnapshot = (thread: Thread, loadingThread: boolean, reconcileItems = false) => {
    discardPendingStreamUpdates();
    resetExpanded();
    showThreadSnapshot(thread, loadingThread, reconcileItems);
  };

  // 缓存未命中也要在本次同步调用内切走旧 transcript；否则 await IPC 期间
  // currentId/items 仍指向旧会话，Canvas/DOM 会再绘制一帧旧内容后才换新。
  if (switching && !cached) {
    const meta = state.threads.find((thread) => thread.id === id);
    discardPendingStreamUpdates();
    resetExpanded();
    setState({
      currentId: id,
      items: [],
      plan: null,
      proposedPlan: null,
      cwd: meta?.cwd ?? "",
      title: meta?.title ?? "",
      agentKind: meta?.agentKind ?? state.agentKind,
      model: meta?.model ?? "",
      mode: "",
      reasoningEffort: "",
      roamingPeer: null,
      loadingThread: true,
      liveUsage: state.running[id] ? (liveUsageByThread.get(id) ?? null) : null,
    });
  }

  // 内存命中时立即完成切换；后面的后端内存快照只做静默校准，不进入加载态。
  if (cached) {
    commitSnapshot(cached, false);
    if (request !== openThreadRequest || state.currentId !== id) return;
  }

  try {
    // 先切换后端 active_thread，再静默校准快照，补齐后台会话运行期间未广播的高频增量。
    await api.reportActivity(id);
    traceThreadSwitch(id, "reportActivity(IPC) 返回");
    lastActivityReport = Date.now();
    if (request !== openThreadRequest) return;
    if (cached && switching && !staleThreadSnapshots.has(id)) {
      const agentKind = cached.agentKind ?? "devin";
      const roamingPeer =
        cached.roamingRole === "guest" ? cached.roamingPeer ?? null : cached.quotaPeer ?? null;
      if (roamingPeer) ensurePeerModels(roamingPeer);
      else void ensureModelOptions(agentKind);
      return;
    }
    const t = await api.getThread(id);
    traceThreadSwitch(id, "getThread(IPC) 返回");
    if (request !== openThreadRequest) return;
    rememberThreadSnapshot(t);
    const agentKind = t.agentKind ?? "devin";
    if (cached) {
      if (state.currentId !== id) return;
      showThreadSnapshot(t, false, true);
    } else {
      commitSnapshot(t, false);
      if (request !== openThreadRequest || state.currentId !== id) return;
    }
    const roamingPeer =
      t.roamingRole === "guest" ? t.roamingPeer ?? null : t.quotaPeer ?? null;
    if (roamingPeer) ensurePeerModels(roamingPeer);
    else void ensureModelOptions(agentKind);
  } catch {
    if (state.currentId === id) setState({ loadingThread: false });
  }
}

export function closeThread() {
  flushPendingStreamUpdates();
  rememberCurrentThreadSnapshot();
  discardPendingStreamUpdates();
  resetExpanded();
  const agentKind = lastUsed.agentKind();
  setState({
    currentId: null,
    items: [],
    plan: null,
    proposedPlan: null,
    cwd: "",
    title: "",
    agentKind,
    model: lastUsed.model(agentKind),
    mode: lastUsed.mode(agentKind),
    reasoningEffort: lastUsed.reasoningEffort(agentKind),
    roamingPeer: null,
    liveUsage: null,
  });
  reportActivity(true);
}

export async function createThread(
  cwd: string,
  agentKind: AgentKind,
  model: string,
  mode: string,
  reasoningEffort: string,
  ephemeral = false,
  worktree = false,
  worktreeBranch = "",
  worktreeBase = "",
  clueCardId = "",
): Promise<string> {
  const t = await api.createThread(
    cwd,
    agentKind,
    model || null,
    mode || null,
    agentKind === "codex" ? reasoningEffort || null : null,
    ephemeral,
    worktree,
    worktreeBranch.trim() || null,
    worktreeBase.trim() || null,
    clueCardId || null,
  );
  rememberThreadSnapshot(t);
  const storedAgentKind = t.agentKind ?? agentKind;
  lastUsed.setMode(storedAgentKind, t.mode ?? "");
  if (storedAgentKind === "codex") {
    lastUsed.setReasoningEffort(storedAgentKind, t.reasoningEffort ?? "");
  }
  setState("expanded", reconcile({}));
  setState({
    currentId: t.id,
    items: t.items,
    plan: (t.plan as PlanEntry[] | null) ?? null,
    proposedPlan: null,
    cwd: t.cwd,
    title: t.title,
    agentKind: storedAgentKind,
    model: t.model ?? "",
    mode: t.mode ?? "",
    reasoningEffort: t.reasoningEffort ?? "",
    loadingThread: false,
  });
  setState("running", t.id, false);
  reportActivity(true);
  void refreshThreads();
  void refreshProjects();
  void ensureModelOptions(storedAgentKind);
  return t.id;
}

/** 待创建会话的占位 id 前缀：永不落库，仅用于乐观进入聊天页。 */
export const PENDING_THREAD_PREFIX = "__pending__";

export function isPendingThreadId(id: string | null | undefined): boolean {
  return !!id && id.startsWith(PENDING_THREAD_PREFIX);
}

/**
 * 乐观创建本地会话：先切进聊天页并上屏用户消息，后台 create_thread 完成后
 * 把占位替换成真会话并补发首条提示词。失败时回退到首页并保留输入。
 * 与 worktree 的「会话先落库返回」同一体验目标：消除提交后到进入界面的卡顿。
 */
export function createThreadOptimistic(
  cwd: string,
  agentKind: AgentKind,
  model: string,
  mode: string,
  reasoningEffort: string,
  text: string,
  images: PromptImage[],
  ephemeral: boolean,
  clueCardId: string,
): void {
  const pendingId = PENDING_THREAD_PREFIX + crypto.randomUUID();
  setState("expanded", reconcile({}));
  setState({
    currentId: pendingId,
    items: [],
    plan: null,
    proposedPlan: null,
    cwd,
    title: "",
    agentKind,
    model,
    mode,
    reasoningEffort,
    loadingThread: false,
  });
  // 用户消息立即上屏，与 deliverPrompt 的乐观项同一约定（负 id 临时项）。
  setState("items", 0, {
    type: "user",
    id: -Date.now(),
    text,
    images,
    ts: Date.now(),
  } as Item);
  bumpChatScrollToBottom();
  void (async () => {
    try {
      const t = await api.createThread(
        cwd,
        agentKind,
        model || null,
        mode || null,
        agentKind === "codex" ? reasoningEffort || null : null,
        ephemeral,
        false,
        null,
        null,
        clueCardId || null,
        null,
      );
      rememberThreadSnapshot(t);
      const storedAgentKind = t.agentKind ?? agentKind;
      lastUsed.setMode(storedAgentKind, t.mode ?? "");
      if (storedAgentKind === "codex") {
        lastUsed.setReasoningEffort(storedAgentKind, t.reasoningEffort ?? "");
      }
      // 用户可能已切走：仅在仍停留在该占位会话时才接管界面。
      if (state.currentId === pendingId) {
        setState({
          currentId: t.id,
          items: t.items,
          plan: (t.plan as PlanEntry[] | null) ?? null,
          proposedPlan: null,
          cwd: t.cwd,
          title: t.title,
          agentKind: storedAgentKind,
          model: t.model ?? "",
          mode: t.mode ?? "",
          reasoningEffort: t.reasoningEffort ?? "",
          loadingThread: false,
        });
        setState("running", t.id, true);
        reportActivity(true);
        await sendPromptTo(t.id, text, images);
      } else {
        // 已切走：后台建好后直接发，保持 optimisticRunningThreads 状态一致。
        optimisticRunningThreads.add(t.id);
        setState("running", t.id, true);
        try {
          await api.sendPrompt(t.id, text, images);
        } catch (error) {
          optimisticRunningThreads.delete(t.id);
          setState("running", t.id, false);
          throw error;
        }
      }
      void refreshThreads();
      void refreshProjects();
      void ensureModelOptions(storedAgentKind);
    } catch (error) {
      if (state.currentId === pendingId) {
        setState("items", (items) => items.filter((item) => item.id >= 0));
        setState("currentId", null);
        setView("home");
      }
      console.error("optimistic create_thread failed", error);
    }
  })();
}

/** 记住最近一次选择的模型/模式，作为新会话默认值 */
export const lastUsed = {
  agentKind: (): AgentKind => {
    const raw = localStorage.getItem("fd:lastAgentKind") as AgentKind | null;
    // Vega 已移除：旧的 alkaid 选择指向 Lyra。
    return !raw || raw === ("alkaid" as AgentKind) ? "lyra" : raw;
  },
  setAgentKind: (v: AgentKind) => localStorage.setItem("fd:lastAgentKind", v),
  model: (agentKind: AgentKind = "devin") => {
    if (agentKind !== lastUsed.agentKind()) return "";
    return (
      localStorage.getItem("fd:lastUsedModel") ??
      localStorage.getItem(`fd:${agentKind}:lastModel`) ??
      (agentKind === "devin" ? localStorage.getItem("fd:lastModel") ?? "" : "")
    );
  },
  /** 与 lastModel 成对保存的友好名；选项未到时触发器先显示它，避免闪裸 id */
  modelName: (agentKind: AgentKind = "devin") => {
    if (agentKind !== lastUsed.agentKind()) return "";
    return (
      localStorage.getItem("fd:lastUsedModelName") ??
      localStorage.getItem(`fd:${agentKind}:lastModelName`) ??
      (agentKind === "devin" ? localStorage.getItem("fd:lastModelName") ?? "" : "")
    );
  },
  mode: (agentKind: AgentKind = "devin") =>
    localStorage.getItem(`fd:${agentKind}:lastMode`) ??
    (agentKind === "devin" ? localStorage.getItem("fd:lastMode") ?? "" : ""),
  reasoningEffort: (agentKind: AgentKind = "codex") =>
    localStorage.getItem(`fd:${agentKind}:lastReasoningEffort`) ?? "",
  setModel: (agentKind: AgentKind, v: string, name?: string | null) => {
    const prevModel = lastUsed.model(agentKind);
    const prevName = lastUsed.modelName(agentKind);
    lastUsed.setAgentKind(agentKind);
    localStorage.setItem("fd:lastUsedModel", v);
    const resolved =
      name?.trim() ||
      modelChoices(agentKind).find((c) => c.value === v)?.name ||
      (v && v === prevModel ? prevName : "");
    if (resolved) {
      localStorage.setItem("fd:lastUsedModelName", resolved);
    }
  },
  setModelName: (agentKind: AgentKind, name: string) => {
    const resolved = name.trim();
    if (!resolved) return;
    if (agentKind === lastUsed.agentKind()) localStorage.setItem("fd:lastUsedModelName", resolved);
  },
  setMode: (agentKind: AgentKind, v: string) => {
    localStorage.setItem(`fd:${agentKind}:lastMode`, v);
  },
  setReasoningEffort: (agentKind: AgentKind, v: string) =>
    localStorage.setItem(`fd:${agentKind}:lastReasoningEffort`, v),
};

function syncLyraConfigDefaultModel(agentKind: AgentKind, options: ModelOptions | null) {
  if (agentKind !== "lyra" || !options || options.pending) return;
  const configured = options.configOptions
    ?.find((option) => option.id === "model")
    ?.currentValue?.trim();
  if (!configured) return;

  const previous = lyraConfigDefaultModel();
  if (previous && previous !== configured && lastUsed.agentKind() === "lyra") {
    const remembered = lastUsed.model("lyra");
    if (!remembered || remembered === previous) {
      const choice = modelChoices("lyra").find((item) => item.value === configured);
      lastUsed.setModel("lyra", configured, choice?.name);
    }
  }
  setLyraConfigDefaultModel(configured);
}

export async function setThreadModel(model: string) {
  const id = state.currentId;
  if (!id) return;
  setState("model", model);
  try {
    await api.setThreadModel(id, model || null);
  } catch (error) {
    // 额度会话只能切到出借方已共享且本机已持有租约的模型：被拒时以后端为准回滚选择
    await openThread(id).catch(() => {});
    void message(String(error), { kind: "error" });
  }
}

/** 进行中的会话切换模型：同 agent 仅换模型；跨 agent（Devin⇄Codex）连同 agent 一起切，
 *  旧 remote 会话作废、上下文不互通，由后端补一条系统提示。 */
export async function pickThreadModel(agentKind: AgentKind, model: string) {
  const id = state.currentId;
  if (!id) return;
  if (agentKind === state.agentKind) {
    await setThreadModel(model);
    return;
  }
  const mode = lastUsed.mode(agentKind);
  const reasoningEffort = agentKind === "codex" ? lastUsed.reasoningEffort(agentKind) : "";
  setState({ agentKind, model, mode, reasoningEffort });
  void ensureModelOptions(agentKind);
  void refreshSlashCommands(agentKind);
  try {
    await api.setThreadAgent(id, agentKind, model || null, mode || null, reasoningEffort || null);
  } catch {
    // 切换失败（如运行中）：以后端状态为准，重新加载会话恢复一致
    await openThread(id);
  }
}

export async function setThreadMode(mode: string) {
  const id = state.currentId;
  if (!id) return;
  setState("mode", mode);
  lastUsed.setMode(state.agentKind, mode);
  await api.setThreadMode(id, mode || null);
}

/** Plan 模式收尾：切换到 Build 并提交实施指令 */
export async function implementProposedPlan() {
  const plan = state.proposedPlan;
  if (!plan || !state.currentId) return;
  setState("proposedPlan", null);
  await setThreadMode("build");
  await sendPrompt(`请按以下计划开始实施：\n\n${plan}`);
}

export function dismissProposedPlan() {
  setState("proposedPlan", null);
}

export async function setThreadReasoningEffort(reasoningEffort: string) {
  const id = state.currentId;
  if (!id) return;
  setState("reasoningEffort", reasoningEffort);
  lastUsed.setReasoningEffort(state.agentKind, reasoningEffort);
  await api.setThreadReasoningEffort(id, reasoningEffort || null);
}

export async function deleteThread(id: string) {
  await api.deleteThread(id);
  threadSnapshots.delete(id);
  if (state.currentId === id) closeThread();
  await refreshThreads();
}

/** 批量删除会话（运行中的由后端跳过），返回实际删除数量 */
export async function deleteThreads(ids: string[]): Promise<number> {
  const deleted = await api.deleteThreads(ids);
  for (const id of ids) threadSnapshots.delete(id);
  if (state.currentId && ids.includes(state.currentId)) closeThread();
  await refreshThreads();
  return deleted;
}

/** 项目侧栏一键清理：后端会保留星标会话及其所在会话树。 */
export async function deleteProjectThreads(ids: string[]): Promise<number> {
  const activeId = state.currentId;
  const deleted = await api.deleteProjectThreads(ids);
  for (const id of ids) threadSnapshots.delete(id);
  await refreshThreads();
  if (activeId && !state.threads.some((thread) => thread.id === activeId)) closeThread();
  return deleted;
}

export async function sendPrompt(
  text: string,
  images: PromptImage[] = [],
  workflowId?: string | null,
  /** 新会话启动工作流时用户原始选择的后端/模型（createThread 可能已被首节点覆盖），
   *  作为「跟随会话」节点的跟随锚点。 */
  workflowFollowFrom?: { agentKind: AgentKind; model: string | null },
) {
  let id = state.currentId;
  if (!id || (!text.trim() && images.length === 0)) return;
  // /train 是 Nova 内置命令：不写入当前会话，也不发送给模型。
  if (text.trim() === "/train" && images.length === 0) {
    const cwd = state.threads.find((thread) => thread.id === id)?.worktree?.repo || state.cwd;
    if (!cwd) throw new Error("请先选择一个项目再训练");
    setTrainingProject(cwd);
    await api.trainExperience(cwd);
    return;
  }
  // 新会话页选择了工作流：会话输入就是首节点输入，文本作为 goal、图片作为首节点附件。
  if (workflowId) {
    await startWorkflow(workflowId, { goal: text.trim() }, id, images, workflowFollowFrom);
    return;
  }
  // 内置命令优先于工作流触发器，避免 /fire、/hard 等被当成普通内容。
  if (await tryBuiltinPrompt(id, text, images)) return;
  // 在历史分支预览中追加提示词时才发生时间跳跃：先恢复该分支，再把新提示词
  // 发送到恢复出的会话。仅浏览或点击当前时间线不会恢复会话。
  if (timeMachineEditTarget?.threadId === id) {
    const target = timeMachineEditTarget;
    timeMachineEditTarget = null;
    const restored = await api.restoreTimeMachineCheckpoint(id, target.checkpointId);
    await refreshThreads();
    await openThread(restored.threadId);
    id = restored.threadId;
  }
  await deliverPrompt(id, text, images);
}

type StageInput = { currentPrompt: string; stagePrompt: string; stageIndex: number };

function parseStageInput(input: string): StageInput | null {
  const match = /(^|\s)\/stage(\d*)(?:[ \t]+|(?=\r?\n)|$)/i.exec(input);
  if (!match) return null;
  const commandStart = match.index + match[1].length;
  const stageNumber = match[2] ? Number(match[2]) : 1;
  if (!Number.isSafeInteger(stageNumber) || stageNumber < 1) {
    throw new Error("Stage 编号必须从 1 开始，例如 /stage2 复核当前方案");
  }
  const stagePrompt = input.slice(commandStart + match[0].length - match[1].length).trim();
  if (!stagePrompt) throw new Error("请在 /stage 后输入新会话提示词，例如 /stage 复核当前方案");
  return { currentPrompt: input.slice(0, commandStart).trim(), stagePrompt, stageIndex: stageNumber - 1 };
}

async function startStageThread(
  sourceThreadId: string,
  prompt: string,
  stageIndex: number,
): Promise<void> {
  const thread = await api.createStageThread(sourceThreadId, stageIndex);
  const ownTitle = prompt.split(/\r?\n/, 1)[0].trim().slice(0, 40) || "Stage";
  await api.renameThread(thread.id, ownTitle);
  thread.title = ownTitle;
  rememberThreadSnapshot(thread);
  await refreshThreads();
  await openThread(thread.id);
  setState("running", thread.id, true);
  // Stage 使用自己的任务生成标题，不沿用来源会话名；失败时保留上面的提示词兜底标题。
  void api.generateThreadTitle(thread.id, prompt.slice(0, 1200)).catch(() => {});
  try {
    await api.sendPrompt(thread.id, prompt);
  } catch (error) {
    setState("running", thread.id, false);
    throw error;
  }
}

// ---- /hard：从第一个 Stage 设计并展示工作流，再直接执行工作流入口 ----

interface PendingHardDesign {
  goal: string;
}

/** 设计轮次 id → 待执行信息。设计正常结束后由 turn 事件驱动解析并启动工作流。 */
const pendingHardDesign = new Map<string, PendingHardDesign>();

function extractWorkflowJson(raw: string): string {
  const start = raw.indexOf("{");
  const end = raw.lastIndexOf("}");
  if (start < 0 || end < start) throw new Error("Agent 未返回 JSON 工作流");
  return raw.slice(start, end + 1);
}

function lastAssistantText(thread: Thread): string {
  for (let i = thread.items.length - 1; i >= 0; i--) {
    const item = thread.items[i];
    if (item.type === "assistant" && item.text.trim()) return item.text.trim();
  }
  return "";
}

/** 第一个阶段：当前会话本身立即成为工作流设计 Stage，不再派生一个空父/设计会话。 */
async function startHardWorkflow(threadId: string, goal: string): Promise<void> {
  const designTitle = "[Hard] 工作流设计";
  await api.renameThread(threadId, designTitle);
  setState("title", designTitle);
  setState("threads", (thread) => thread.id === threadId, "title", designTitle);
  pendingHardDesign.set(threadId, { goal });
  try {
    await deliverPrompt(threadId, buildHardDesignPrompt(goal), []);
  } catch (error) {
    pendingHardDesign.delete(threadId);
    throw error;
  }
}

/** 设计 Stage 正常结束后：在结尾展示工作流图；入口节点从下一个 Stage 会话开始执行。 */
async function finalizeHardDesign(threadId: string): Promise<void> {
  const pending = pendingHardDesign.get(threadId);
  if (!pending) return;
  pendingHardDesign.delete(threadId);
  let transientWorkflowId: string | null = null;

  try {
    const thread = await api.getThread(threadId);
    const raw = lastAssistantText(thread);
    if (!raw) throw new Error("Agent 没有返回工作流设计");
    const workflow = normalizeGeneratedWorkflow(JSON.parse(extractWorkflowJson(raw)));
    registerTransientWorkflow(workflow);
    transientWorkflowId = workflow.id;

    // 单独插入一个只读流程图 Stage：设计会话保留设计原文，下一 Stage 复用
    // 工作流配置画布展示结构，再由后续 Stage 开始执行入口节点。
    const workflowJson = JSON.stringify(workflow);
    const previewThread = await api.createStageThread(threadId, 1, true);
    const previewTitle = "[Hard] 工作流图";
    await api.renameThread(previewThread.id, previewTitle);
    previewThread.title = previewTitle;
    rememberThreadSnapshot(previewThread);
    await api.pushSystemItem(previewThread.id, workflowJson, "workflow");
    await refreshThreads();
    await openThread(previewThread.id);

    const entry = workflow.stages.find((stage) => stage.id === workflow.entry)!;
    const execThread = await api.createStageThread(previewThread.id, 2, true);
    const execTitle = `[WF] ${entry.name}`;
    await api.renameThread(execThread.id, execTitle);
    execThread.title = execTitle;
    rememberThreadSnapshot(execThread);
    await refreshThreads();
    // 执行节点在后台立即开始；界面停留在流程图 Stage，用户可先查看完整画布，
    // 再从 Stage 导航进入已经启动的入口节点。
    await startWorkflow(workflow.id, { goal: pending.goal }, execThread.id, []);
  } catch (error) {
    if (transientWorkflowId) unregisterTransientWorkflow(transientWorkflowId);
    const message = error instanceof Error ? error.message : String(error);
    await api.pushSystemItem(threadId, `Hard 工作流设计失败：${message}`, "error");
    throw error;
  }
}

/** 处理 /stage、/stage2、/fire、/plan 等内置命令。返回 true 表示已消费。 */
async function tryBuiltinPrompt(
  threadId: string,
  text: string,
  images: PromptImage[],
): Promise<boolean> {
  const builtInInput = text.trim();
  const stage = parseStageInput(builtInInput);
  if (stage) {
    if (images.length > 0) throw new Error("/stage 暂不支持附件");
    // 命令前有实质内容时，先作为普通提示追加到当前会话；Stage 随后独立启动。
    if (stage.currentPrompt) await deliverPrompt(threadId, stage.currentPrompt, []);
    await startStageThread(threadId, stage.stagePrompt, stage.stageIndex);
    return true;
  }
  if (/^\/plan(?:\s|$)/i.test(builtInInput)) {
    const goal = builtInInput.replace(/^\/plan(?:[ \t]+|(?=\r?\n)|$)/i, "").trim();
    if (!goal) throw new Error("请在 /plan 后输入规划目标，例如 /plan 设计登录流程");
    // 不切入 Plan 模式：仅静默展开为「先规划、少追问」提示词，仍走 Build。
    await deliverPrompt(threadId, buildPlanPrompt(goal), images);
    return true;
  }
  if (/^\/easy(?:\s|$)/i.test(builtInInput)) {
    const goal = builtInInput.replace(/^\/easy(?:[ \t]+|(?=\r?\n)|$)/i, "").trim();
    if (!goal) throw new Error("请在 /easy 后输入明确的小修改目标，例如 /easy 修复这个类型错误");
    await deliverPrompt(threadId, buildEasyPrompt(goal), images);
    return true;
  }
  if (/^\/hard(?:\s|$)/i.test(builtInInput)) {
    if (images.length > 0) throw new Error("/hard 暂不支持附件");
    const goal = builtInInput.replace(/^\/hard(?:[ \t]+|(?=\r?\n)|$)/i, "").trim();
    if (!goal) throw new Error("请在 /hard 后输入目标，例如 /hard 修复登录问题并完成测试");
    await startHardWorkflow(threadId, goal);
    return true;
  }
  if (/^\/fire(?:\s|$)/i.test(builtInInput)) {
    if (images.length > 0) throw new Error("/fire 暂不支持附件");
    const parsed = parseFireInput(builtInInput);
    await startFireRelay(parsed.goal, parsed.acceptanceCriteria, threadId);
    return true;
  }
  if (/^\/run(?:\s|$)/i.test(builtInInput)) {
    const parsed = parseRunInput(builtInInput);
    await startWorkflow(parsed.workflowId, parsed.vars, threadId, images);
    return true;
  }
  if (/^\/setup(?:\s|$)/i.test(builtInInput)) {
    const goal = builtInInput.replace(/^\/setup(?:[ \t]+|(?=\r?\n)|$)/i, "").trim();
    if (!goal) throw new Error("请在 /setup 后输入要接入的模型，例如 /setup qwen3.8");
    // /setup 由 agent 修改本地 config.jsonc；本轮结束后复用设置页的刷新机制，
    // 让 Lyra 立刻重读配置并刷新模型列表，而不要求用户手动点「刷新配置」。
    pendingSetupConfigRefresh.add(threadId);
    try {
      await deliverPrompt(threadId, buildIntegrateModelPrompt(goal), images);
    } catch (error) {
      pendingSetupConfigRefresh.delete(threadId);
      throw error;
    }
    return true;
  }
  // 触发条件：提示词命中某工作流的 slash/contains/regex 触发器时自动启动。
  const triggered = findTriggeredWorkflow(builtInInput);
  if (triggered) {
    await startWorkflow(triggered.id, { goal: triggered.goal }, threadId, images);
    return true;
  }
  if (/^\/target(?:\s|$)/i.test(builtInInput)) {
    throw new Error("/target 只能与 /fire 一起发送");
  }
  return false;
}

/**
 * 解析 /run <工作流名> <目标> [-- key=value ...]。
 * 工作流名取第一个空白分隔的词；「--」之前（工作流名之后）为目标 goal；
 * 「--」之后为 key=value 形式的流程变量（值可含空格，最后一个 key 吞掉剩余文本），
 * 可在节点提示词模板里用 {{key}} 引用，例如 -- criteria=不能有类型错误。
 */
function parseRunInput(input: string): { workflowId: string; vars: Record<string, string> } {
  const body = input.replace(/^\/run(?:[ \t]+|(?=\r?\n)|$)/i, "").trim();
  if (!body) throw new Error("请在 /run 后输入工作流名称和目标");
  const [nameToken, ...restTokens] = body.split(/\s+/);
  const workflow = findWorkflowByName(nameToken);
  if (!workflow) throw new Error(`找不到名为「${nameToken}」的工作流，可在设置·工作流中查看`);
  const rest = restTokens.join(" ");
  const sepMatch = /(?:^|\s)--(?:\s|$)/.exec(rest);
  const goal = (sepMatch ? rest.slice(0, sepMatch.index) : rest).trim();
  if (!goal) throw new Error("请在 /run <工作流名> 后输入目标");
  const vars: Record<string, string> = { goal };
  const varsText = sepMatch ? rest.slice(sepMatch.index + sepMatch[0].length).trim() : "";
  if (varsText) {
    // 按「空白后跟 key=」切分，允许值里包含空格。
    const parts = varsText.split(/\s+(?=[\w一-龥-]+=)/);
    for (const part of parts) {
      const eq = part.indexOf("=");
      if (eq <= 0) continue;
      const key = part.slice(0, eq).trim();
      const value = part.slice(eq + 1).trim();
      if (key && value) vars[key] = value;
    }
  }
  return { workflowId: workflow.id, vars };
}

/** 创建会话 / 暂存前提前校验内置命令，避免 worktree 建完才发现 /fire 非法。 */
export function assertBuiltinPrompt(text: string, images: PromptImage[] = []) {
  const builtInInput = text.trim();
  const stage = parseStageInput(builtInInput);
  if (stage) {
    if (images.length > 0) throw new Error("/stage 暂不支持附件");
    return;
  }
  if (/^\/plan(?:\s|$)/i.test(builtInInput)) {
    const goal = builtInInput.replace(/^\/plan(?:[ \t]+|(?=\r?\n)|$)/i, "").trim();
    if (!goal) throw new Error("请在 /plan 后输入规划目标，例如 /plan 设计登录流程");
    return;
  }
  if (/^\/easy(?:\s|$)/i.test(builtInInput)) {
    const goal = builtInInput.replace(/^\/easy(?:[ \t]+|(?=\r?\n)|$)/i, "").trim();
    if (!goal) throw new Error("请在 /easy 后输入明确的小修改目标，例如 /easy 修复这个类型错误");
    return;
  }
  if (/^\/hard(?:\s|$)/i.test(builtInInput)) {
    if (images.length > 0) throw new Error("/hard 暂不支持附件");
    const goal = builtInInput.replace(/^\/hard(?:[ \t]+|(?=\r?\n)|$)/i, "").trim();
    if (!goal) throw new Error("请在 /hard 后输入目标，例如 /hard 修复登录问题并完成测试");
    return;
  }
  if (/^\/fire(?:\s|$)/i.test(builtInInput)) {
    if (images.length > 0) throw new Error("/fire 暂不支持附件");
    parseFireInput(builtInInput);
    return;
  }
  if (/^\/browser(?:\s|$)/i.test(builtInInput)) {
    const goal = builtInInput.replace(/^\/browser(?:[ \t]+|(?=\r?\n)|$)/i, "").trim();
    if (!goal) throw new Error("请在 /browser 后输入网址和调试目标，例如 /browser localhost:5173 检查登录页");
    return;
  }
  if (/^\/browser-exit(?:\s|$)/i.test(builtInInput)) {
    if (!/^\/browser-exit\s*$/i.test(builtInInput)) throw new Error("/browser-exit 后不需要附加内容");
    return;
  }
  if (/^\/setup(?:\s|$)/i.test(builtInInput)) {
    const goal = builtInInput.replace(/^\/setup(?:[ \t]+|(?=\r?\n)|$)/i, "").trim();
    if (!goal) throw new Error("请在 /setup 后输入要接入的模型，例如 /setup qwen3.8");
    return;
  }
  if (/^\/target(?:\s|$)/i.test(builtInInput)) {
    throw new Error("/target 只能与 /fire 一起发送");
  }
}

/** 向指定会话投递普通提示词（含 Fire 阶段续跑）。不处理内置命令。一律 Build。 */
async function deliverPrompt(threadId: string, text: string, images: PromptImage[]) {
  // 后端 user 事件到达前先把用户刚发送的内容上屏，避免首轮仍显示“请在下方输入”。
  // applyUpsert 收到真实 user item 后会移除这个负 id 临时项。
  const optimisticId = state.currentId === threadId ? -Date.now() : null;
  if (optimisticId !== null) {
    setState("items", state.items.length, {
      type: "user",
      id: optimisticId,
      text,
      images,
      ts: Date.now(),
    } as Item);
    bumpChatScrollToBottom();
  }

  // Fire 阶段在暂停后，或已经产出过判断后，仍允许用户从该会话补充提示继续流程。
  // 本轮正常结束时会重新进入自动验收，而不是退化成不受跟踪的普通会话。
  const resumedFireStep = resumeFireRelay(threadId);
  // 非 Fire 会话再尝试挂回通用工作流；只读阶段会改写为续跑提示。
  const workflowOutbound = resumedFireStep ? null : prepareWorkflowPrompt(threadId, text);
  const currentMode = state.currentId === threadId ? state.mode : "";
  setState("proposedPlan", null);
  optimisticRunningThreads.add(threadId);
  setState("running", threadId, true);
  try {
    // ThreadMeta 不持久化 mode：非当前会话按未知处理，无条件在后端置为 build。
    // 一律 Build：含历史 Plan 会话、以及后端原生 bypass/agent 等。
    if ((currentMode || "").toLowerCase() !== "build") {
      await api.setThreadMode(threadId, "build");
      if (state.currentId === threadId) {
        setState("mode", "build");
        lastUsed.setMode(state.agentKind, "build");
      }
    }
    // 判断阶段被重新唤起时仍然只是验收者：补充内容要并入本轮核验，实现工作交给
    // 下一个执行阶段，否则判断会话会自己动手改项目。
    const outbound = resumedFireStep?.role === "judge"
      ? fireJudgeResumePrompt(text)
      : workflowOutbound ?? text;
    await api.sendPrompt(threadId, outbound, images);
  } catch (e) {
    optimisticRunningThreads.delete(threadId);
    if (optimisticId !== null && state.currentId === threadId) {
      setState("items", (items) => items.filter((item) => item.id !== optimisticId));
    }
    if (resumedFireStep && fireRelaySteps.get(threadId) === resumedFireStep) {
      fireRelaySteps.delete(threadId);
      suspendedFireRelaySteps.set(threadId, resumedFireStep);
      persistFireRelayState();
    } else if (workflowOutbound !== null) {
      suspendWorkflowActive(threadId);
    }
    setState("running", threadId, false);
    throw e;
  }
}

type FireRelayStep = {
  rootId: string;
  goal: string;
  acceptanceCriteria: string | null;
  role: "work" | "judge";
  attempt: number;
};

const fireRelaySteps = new Map<string, FireRelayStep>();
// 已发送 /setup、等待该轮结束后重载 Lyra 本地配置的会话。
const pendingSetupConfigRefresh = new Set<string>();
// 中断不丢弃 Fire 上下文。用户在阶段再次发言时可重新挂回自动验收流程，
// 同时避免网络错误把一次尚未完成的响应误当成最终结论送去判断。
const suspendedFireRelaySteps = new Map<string, FireRelayStep>();
// 正常结束的阶段也保留流程身份。否则它一旦创建判断会话便会从 fireRelaySteps
// 删除，用户回到该阶段补充内容时只能进行普通对话，后续结果不会再触发验收。
const fireRelayStepHistory = new Map<string, FireRelayStep>();
// 每条 Fire 链只允许最后一个会话继续推进。显式记录最后会话，避免旧阶段补充消息
// 分叉流程，也避免中断事件遗留的 active 记录挡住最后阶段恢复。
const latestFireThreadByRoot = new Map<string, string>();
const completedFireRoots = new Set<string>();
const FIRE_RELAY_STATE_KEY = "fd:fireRelayState:v1";
const FIRE_MAX_ATTEMPTS = 20;

type PersistedFireRelayState = {
  steps: [string, FireRelayStep][];
  latest: [string, string][];
  completed: string[];
};

function persistFireRelayState() {
  const steps = new Map(fireRelayStepHistory);
  for (const [threadId, step] of fireRelaySteps) steps.set(threadId, step);
  for (const [threadId, step] of suspendedFireRelaySteps) steps.set(threadId, step);
  const snapshot: PersistedFireRelayState = {
    steps: [...steps],
    latest: [...latestFireThreadByRoot],
    completed: [...completedFireRoots],
  };
  try {
    localStorage.setItem(FIRE_RELAY_STATE_KEY, JSON.stringify(snapshot));
  } catch {
    // 持久化失败不能打断当前 Fire 流程；本次窗口内仍由内存状态继续跟踪。
  }
}

function restoreFireRelayState() {
  try {
    const snapshot = JSON.parse(
      localStorage.getItem(FIRE_RELAY_STATE_KEY) ?? "null",
    ) as PersistedFireRelayState | null;
    if (!snapshot || !Array.isArray(snapshot.steps) || !Array.isArray(snapshot.latest)) return;
    for (const [threadId, step] of snapshot.steps) {
      if (
        !threadId || !step?.rootId || !step.goal ||
        (step.role !== "work" && step.role !== "judge") ||
        !Number.isInteger(step.attempt)
      ) continue;
      // 重启时不能假定上次正在执行的 turn 已正常结束，统一按暂停恢复，避免把
      // 半截回复直接送去验收；用户继续该会话后仍会回到完整 Fire 流程。
      suspendedFireRelaySteps.set(threadId, step);
      fireRelayStepHistory.set(threadId, step);
    }
    for (const [rootId, threadId] of snapshot.latest) {
      if (rootId && threadId) latestFireThreadByRoot.set(rootId, threadId);
    }
    if (Array.isArray(snapshot.completed)) {
      for (const rootId of snapshot.completed) if (rootId) completedFireRoots.add(rootId);
    }
  } catch {
    localStorage.removeItem(FIRE_RELAY_STATE_KEY);
  }
}

restoreFireRelayState();

// 通用工作流运行时（/run）：复用会话接力模式，与 /fire 专用路径并存。
initWorkflowRuntime({
  currentId: () => state.currentId,
  isRunning: (id) => !!state.running[id],
  setRunning: (id, v) => setState("running", id, v),
  refreshThreads: () => refreshThreads(),
  openThread: (id) => openThread(id),
  bumpScrollToBottom: () => bumpChatScrollToBottom(),
  clearProposedPlan: () => setState("proposedPlan", null),
});

type ParsedFireInput = {
  goal: string;
  acceptanceCriteria: string | null;
};

function parseFireInput(input: string): ParsedFireInput {
  const body = input.replace(/^\/fire(?:[ \t]+|(?=\r?\n)|$)/i, "");
  const targetMatches = [...body.matchAll(/^\/target(?:[ \t]+(.*)|[ \t]*)$/gim)];
  if (targetMatches.length > 1) throw new Error("每个 /fire 只能指定一次 /target");

  const target = targetMatches[0];
  if (!target || target.index === undefined) {
    return { goal: body.trim(), acceptanceCriteria: null };
  }

  const goal = body.slice(0, target.index).trim();
  const firstRule = target[1]?.trim() ?? "";
  const remainingRules = body.slice(target.index + target[0].length).trim();
  const acceptanceCriteria = [firstRule, remainingRules].filter(Boolean).join("\n");
  if (!acceptanceCriteria) throw new Error("请在 /target 后输入验收规则");
  return { goal, acceptanceCriteria };
}

function fireConclusion(thread: Thread): string {
  return [...thread.items]
    .reverse()
    .find((item): item is Extract<Item, { type: "assistant" }> =>
      item.type === "assistant" && !!item.text.trim())?.text.trim() ?? "（会话没有给出结论）";
}

function fireWorkPrompt(goal: string, previousVerdict?: string): string {
  const resultNote = `最终回复保持简短、只写可核实的信息：完成结果、实际改动的文件或产物、验证结果；只有确实存在时才写未完成项或阻塞。不要复述任务、输出泛泛总结或编造后续建议。`;

  if (!previousVerdict) {
    return `直接在当前项目中完成下面的目标，不要只给建议或方案。开始前先检查项目现状、已有实现和未提交改动，在已有基础上推进并避免覆盖现有成果。\n\n目标：\n${goal}\n\n${resultNote}`;
  }

  return `这是一个独立的后续执行阶段，你看不到之前阶段的完整对话。先检查当前项目状态、版本控制差异和相关文件，确认已有成果；不要从零重做，只处理上次验收明确指出的未满足项。验收内容是线索而不是项目事实，修改前请自行核实。\n\n目标：\n${goal}\n\n上次验收结果：\n${previousVerdict}\n\n${resultNote}`;
}

function fireJudgePrompt(step: FireRelayStep, conclusion: string): string {
  const criteria = step.acceptanceCriteria
    ? `以下验收规则具有最高优先级且每一条都必须满足：\n${step.acceptanceCriteria}\n\n逐条核验这些规则；任何一条不满足或无法从项目状态中确认，都必须判定为不符合。目标描述和执行阶段说明不能替代、弱化或改写验收规则。`
    : `根据目标逐项核验实际完成情况；任何关键要求不满足或无法确认，都必须判定为不符合。`;
  return `你是独立验收者，不要继续实现或修改任务。\n\n目标：\n${step.goal}\n\n${criteria}\n\n执行阶段的最终说明（仅作为定位线索，不是完成证据）：\n${conclusion}\n\n请检查当前项目的实际状态、版本控制差异、相关文件和可用的验证结果，不要依据执行阶段回复的篇幅、格式或自述做判断。必须主动选择成本最低且足以覆盖改动的验证方式，并检查目标产物在实际使用场景中的错误信号，例如编译或类型错误、测试失败、启动或运行异常、控制台报错、无效 API 调用以及关键交互不可用。只要存在与本次目标相关的未解释错误、验证失败，或因错误导致关键行为无法验证，就必须判定为不符合；不得因为部分功能存在、界面看似完成或执行阶段声称完成而放宽。若受环境限制无法执行必要验证，也应判定为无法确认而不符合，并说明限制。\n\n回复应简洁：先给出逐条验收结果；若不符合，只列出未满足项、依据和下一阶段需要采取的具体动作；若符合，只列出实际执行过的验证及关键证据。不要复述无关的阶段总结。最后必须单独输出一行 FIRE_ACCEPTED 或 FIRE_REJECTED，且该标记必须是回复的最后一行。`;
}

function fireJudgeResumePrompt(text: string): string {
  return `当前会话仍处于 Fire 自动验收流程的判断阶段：你只做核验，不要实现功能、修改文件或执行任何写操作，需要改动的部分会由下一个执行阶段完成。\n\n用户补充：\n${text}\n\n请结合该补充重新核验当前项目的实际状态；补充中若包含新的要求，同样纳入本次核验，未满足即判定为不符合，并写清未满足项和下一阶段需要采取的具体动作。最后必须单独输出一行 FIRE_ACCEPTED 或 FIRE_REJECTED，且该标记必须是回复的最后一行。`;
}

function fireStepTitle(step: FireRelayStep, status = ""): string {
  let base: string;
  if (step.role === "judge") {
    base = `[Fire] 判断 ${step.attempt}`;
  } else if (step.attempt === 1) {
    base = `[Fire] 目标 · ${step.goal.slice(0, 28)}`;
  } else {
    base = `[Fire] 阶段 ${step.attempt}`;
  }
  return status ? `${base} · ${status}` : base;
}

function resumeFireRelay(threadId: string): FireRelayStep | null {
  const step = fireRelaySteps.get(threadId)
    ?? suspendedFireRelaySteps.get(threadId)
    ?? fireRelayStepHistory.get(threadId);
  if (!step || latestFireThreadByRoot.get(step.rootId) !== threadId) return null;

  // 用户在最后会话继续对话时原样执行其提示，只撤销该链上一次的暂停或最终结果，
  // 让这次回复结束后重新按当前阶段角色推进；旧阶段仍不能分叉流程。
  const wasCompleted = completedFireRoots.delete(step.rootId);
  for (const [activeId, activeStep] of fireRelaySteps) {
    if (activeId !== threadId && activeStep.rootId === step.rootId) {
      fireRelaySteps.delete(activeId);
      suspendedFireRelaySteps.set(activeId, activeStep);
    }
  }
  const wasSuspended = suspendedFireRelaySteps.delete(threadId);
  fireRelaySteps.set(threadId, step);
  if (wasSuspended || wasCompleted) {
    // 标题恢复不应阻塞用户刚提交的提示。
    void api.renameThread(threadId, fireStepTitle(step)).then(refreshThreads).catch(() => {});
  }
  persistFireRelayState();
  return step;
}

async function createFireThread(
  step: FireRelayStep,
  title: string,
  prompt: string,
): Promise<string> {
  const root = await api.getThread(step.rootId);
  const thread = await api.createThread(
    root.cwd,
    root.agentKind,
    root.model ?? null,
    "build",
    null,
    false,
    false,
    null,
    null,
    null,
    step.rootId,
  );
  await api.renameThread(thread.id, title);
  fireRelaySteps.set(thread.id, step);
  fireRelayStepHistory.set(thread.id, step);
  latestFireThreadByRoot.set(step.rootId, thread.id);
  persistFireRelayState();
  await refreshThreads();
  // 用户正在看这条 Fire 链时才切到新阶段；看别的会话时后台继续跑。
  if (isViewingFireChain(step.rootId)) await openThread(thread.id);
  setState("running", thread.id, true);
  await api.sendPrompt(thread.id, prompt);
  return thread.id;
}

function isViewingFireChain(rootId: string): boolean {
  const currentId = state.currentId;
  if (!currentId) return false;
  if (currentId === rootId) return true;
  const step = fireRelayStepHistory.get(currentId)
    ?? fireRelaySteps.get(currentId)
    ?? suspendedFireRelaySteps.get(currentId);
  return step?.rootId === rootId;
}

async function advanceFireRelay(threadId: string) {
  const step = fireRelaySteps.get(threadId);
  if (!step) return;
  fireRelaySteps.delete(threadId);
  persistFireRelayState();
  const thread = await api.getThread(threadId);
  if (step.role === "work") {
    const conclusion = fireConclusion(thread);
    await createFireThread(
      { ...step, role: "judge" },
      `[Fire] 判断 ${step.attempt}`,
      fireJudgePrompt(step, conclusion),
    );
    return;
  }

  const verdict = fireConclusion(thread);
  if (/FIRE_ACCEPTED\s*$/i.test(verdict)) {
    completedFireRoots.add(step.rootId);
    persistFireRelayState();
    await api.renameThread(thread.id, `[Fire] 判断 ${step.attempt} · 符合`);
    await refreshThreads();
    await api.notifyFireDone(thread.id, true);
    return;
  }
  if (step.attempt >= FIRE_MAX_ATTEMPTS) {
    completedFireRoots.add(step.rootId);
    persistFireRelayState();
    await api.renameThread(thread.id, `[Fire] 判断 ${step.attempt} · 已停止`);
    await refreshThreads();
    await api.notifyFireDone(thread.id, false);
    return;
  }
  await createFireThread(
    {
      rootId: step.rootId,
      goal: step.goal,
      acceptanceCriteria: step.acceptanceCriteria,
      role: "work",
      attempt: step.attempt + 1,
    },
    `[Fire] 阶段 ${step.attempt + 1}`,
    fireWorkPrompt(step.goal, verdict),
  );
}

async function suspendFireRelay(threadId: string, manual: boolean) {
  const step = fireRelaySteps.get(threadId);
  if (!step) return;
  fireRelaySteps.delete(threadId);
  suspendedFireRelaySteps.set(threadId, step);
  persistFireRelayState();
  await api.renameThread(threadId, fireStepTitle(step, manual ? "已暂停" : "异常暂停"));
  await refreshThreads();
  // 手动停止和异常都只暂停当前阶段，不结束 Fire。用户继续发送后会恢复自动验收。
}

export async function startFireRelay(
  goal: string,
  acceptanceCriteria: string | null = null,
  threadId?: string | null,
) {
  const rootId = threadId ?? state.currentId;
  const trimmed = goal.trim();
  if (!rootId || !trimmed) throw new Error("请在 /fire 后输入目标");
  // 首页刚创建会话后会立即发送首条提示，此时异步 refreshThreads 可能尚未完成；
  // 直接读取后端快照，不能依赖列表中已经出现该会话。
  const root = await api.getThread(rootId);
  if (root.roamingRole || root.quotaPeerName) {
    throw new Error("/fire 仅支持本地普通会话");
  }
  if (state.running[rootId]) throw new Error("请等待当前会话结束后再启动 /fire");
  // Fire 始终执行而不是规划；根会话和后续所有阶段都强制使用 Build。
  await api.setThreadMode(rootId, "build");
  if (state.currentId === rootId) setState("mode", "build");
  await api.renameThread(rootId, `[Fire] 目标 · ${trimmed.slice(0, 28)}`);
  suspendedFireRelaySteps.delete(rootId);
  completedFireRoots.delete(rootId);
  const rootStep: FireRelayStep = {
    rootId,
    goal: trimmed,
    acceptanceCriteria,
    role: "work",
    attempt: 1,
  };
  fireRelaySteps.set(rootId, rootStep);
  fireRelayStepHistory.set(rootId, rootStep);
  latestFireThreadByRoot.set(rootId, rootId);
  persistFireRelayState();
  await refreshThreads();
  // 远程或其它非 Composer 入口启动时，用户可能正在看别的会话；必须发到 rootId。
  if (state.currentId === rootId) bumpChatScrollToBottom();
  setState("proposedPlan", null);
  setState("running", rootId, true);
  try {
    await api.sendPrompt(rootId, fireWorkPrompt(trimmed), []);
  } catch (e) {
    if (fireRelaySteps.get(rootId) === rootStep) {
      fireRelaySteps.delete(rootId);
      suspendedFireRelaySteps.set(rootId, rootStep);
      persistFireRelayState();
    }
    setState("running", rootId, false);
    throw e;
  }
}

async function handleFireStart(threadId: string, text: string) {
  const parsed = parseFireInput(text.trim());
  await startFireRelay(parsed.goal, parsed.acceptanceCriteria, threadId);
}

/** ChatView 订阅：发送新提示词时强制滚到底 */
const [chatScrollToBottomTick, setChatScrollToBottomTick] = createSignal(0);
const [timeMachineChangedTick, setTimeMachineChangedTick] = createSignal(0);
let timeMachineEditTarget: { threadId: string; checkpointId: string } | null = null;
export function setTimeMachineEditTarget(
  target: { threadId: string; checkpointId: string } | null,
) {
  timeMachineEditTarget = target;
}
export function chatScrollToBottomSignal() {
  return chatScrollToBottomTick();
}
export function timeMachineChangedSignal() {
  return timeMachineChangedTick();
}
function bumpChatScrollToBottom() {
  setChatScrollToBottomTick((n) => n + 1);
}

/** 本地 worktree 会话：worktree 在后台创建，暂存首条提示词，就绪后（acp:worktree-ready）再自动发送 */
const pendingWorktreePrompts = new Map<
  string,
  {
    text: string;
    images: PromptImage[];
    workflowId?: string | null;
    followFrom?: { agentKind: AgentKind; model: string | null };
  }
>();

function flushWorktreePrompt(threadId: string) {
  const prompt = pendingWorktreePrompts.get(threadId);
  if (!prompt) return;
  pendingWorktreePrompts.delete(threadId);
  // 新会话页选了工作流：就绪后直接启动工作流（goal 为暂存提示词），否则按普通提示词发送。
  const action = prompt.workflowId
    ? startWorkflow(prompt.workflowId, { goal: prompt.text.trim() }, threadId, prompt.images, prompt.followFrom)
    : sendPromptTo(threadId, prompt.text, prompt.images);
  void action.catch((error) => {
    console.error("worktree prompt flush failed", error);
  });
}

export function stashWorktreePrompt(
  threadId: string,
  text: string,
  images: PromptImage[],
  workflowId?: string | null,
  followFrom?: { agentKind: AgentKind; model: string | null },
) {
  if (!text.trim() && images.length === 0) return;
  pendingWorktreePrompts.set(threadId, { text, images, workflowId: workflowId ?? null, followFrom });
  // create_thread 可能直接复用已就绪的 worktree，或后台 ready 事件可能先于 invoke 返回。
  // 主动核对持久化状态，避免首条提示词永远留在暂存 Map。
  void api
    .getThread(threadId)
    .then((thread) => {
      if (thread.worktree && thread.cwd === thread.worktree.path) flushWorktreePrompt(threadId);
    })
    .catch((error) => console.error("getThread after worktree creation failed", error));
}

/**
 * 向指定会话发送（不依赖 currentId）。
 * 所有「已知 threadId 的提示词投递」应走这里，以便统一拦截 /fire 等内置命令
 * （worktree 就绪补发、以及未来其它非 Composer 入口）。
 */
export async function sendPromptTo(threadId: string, text: string, images: PromptImage[]) {
  if (!text.trim() && images.length === 0) return;
  if (await tryBuiltinPrompt(threadId, text, images)) return;
  await deliverPrompt(threadId, text, images);
}

/** 编辑历史用户消息并从该处重新开始：界面立即更新，SDK restore/fork 在后端排队完成后再发送。 */
export async function editUserMessage(itemId: number, text: string, images: PromptImage[] = []) {
  let id = state.currentId;
  if (!id || (!text.trim() && images.length === 0)) return;
  // 历史分支预览中发生编辑时，先恢复对应快照；随后的 truncate_thread 会自动
  // 从被编辑提示词处截断并创建新分支，无需用户先手动执行时间跳跃。
  if (timeMachineEditTarget?.threadId === id) {
    const target = timeMachineEditTarget;
    timeMachineEditTarget = null;
    const restored = await api.restoreTimeMachineCheckpoint(id, target.checkpointId);
    await refreshThreads();
    await openThread(restored.threadId);
    id = restored.threadId;
  }
  const targetIndex = state.items.findIndex((item) => item.id === itemId);
  const retained = targetIndex < 0 ? state.items : state.items.slice(0, targetIndex);
  // 临时 id 只存在于前端；后端 restore 完成、发出真实 user item 后由快照/事件替换。
  const optimisticId = -Date.now();
  setState({
    items: [
      ...retained,
      { type: "user", id: optimisticId, text, images, ts: Date.now() } as Item,
    ],
    plan: null,
    proposedPlan: null,
  });
  setState("expanded", reconcile({}));
  setState("running", id, true);
  // 先置 running 再解挂：停止留下的 hold 否则会挡住本轮结束后的队列自动投递。
  // 必须在 running=true 之后释放，避免解挂瞬间把仍停留在队列里的条目立刻发出。
  // 动态导入避免 store ↔ promptQueue 循环依赖。
  const { releasePromptQueue } = await import("./promptQueue");
  releasePromptQueue(id);
  bumpChatScrollToBottom();
  // 「停止 → 立刻编辑重发」的竞态：后端 cancel 可能尚未完成，truncate 会被
  // 「会话正在运行」校验拒绝，直接抛错会让这次编辑静默丢失（表现为第一次发送失败）。
  // 短暂重试等 cancel 落地，仍失败才抛出。
  for (let attempt = 0; ; attempt++) {
    try {
      await api.truncateThread(id, itemId, text, images);
      setTimeMachineChangedTick((n) => n + 1);
      break;
    } catch (e) {
      if (attempt >= 10) {
        setState("running", id, false);
        if (state.currentId === id) await openThread(id);
        throw e;
      }
      await new Promise((r) => setTimeout(r, 300));
      if (state.currentId !== id) return;
    }
  }
}

export async function cancelTurn(stopReason?: string, deleteWork = false) {
  const id = state.currentId;
  if (!id) return;
  optimisticRunningThreads.delete(id);
  await api.cancelTurn(id, stopReason, deleteWork);
  // 部分后端的取消调用会先返回，结束事件稍后才到；主动释放前端忙碌态，
  // 避免停止成功后历史消息仍被 running 门控，必须切换会话才能编辑。
  setState("running", id, false);
}

/** 手动压缩当前会话上下文（仅 Codex）：把长历史浓缩为摘要，加快后续响应。
 *  忙碌态由后端 acp:turn 事件驱动，这里乐观置位以即时反馈。 */
export async function compactThread() {
  const id = state.currentId;
  if (!id) return;
  setState("running", id, true);
  try {
    await api.compactThread(id);
  } catch (e) {
    setState("running", id, false);
    throw e;
  }
}

export async function respondPermission(requestKey: string, optionId: string) {
  try {
    await api.respondPermission(requestKey, optionId);
  } finally {
    setState(
      "permissions",
      state.permissions.filter((p) => p.requestKey !== requestKey),
    );
  }
}

const pendingDeltas = new Map<number, string>();
let deltaFlushTimer: number | undefined;
let lastDeltaFlush = 0;
/** delta 合并窗口：足够小保证流式顺滑，配合 leading-edge 让首字几乎即时 */
const DELTA_FLUSH_MS = 33;
const pendingToolItems = new Map<number, Extract<Item, { type: "tool" }>>();
let toolUpdateFlushTimer: number | undefined;
let lastToolUpdateFlush = 0;
/** 工具更新是整条快照；比文本增量稍长的窗口可避免详情高频整块重绘。 */
const TOOL_UPDATE_FLUSH_MS = 50;

function discardPendingDeltas() {
  if (deltaFlushTimer !== undefined) {
    window.clearTimeout(deltaFlushTimer);
    deltaFlushTimer = undefined;
  }
  pendingDeltas.clear();
}

function discardPendingToolUpserts() {
  if (toolUpdateFlushTimer !== undefined) {
    window.clearTimeout(toolUpdateFlushTimer);
    toolUpdateFlushTimer = undefined;
  }
  pendingToolItems.clear();
}

function discardPendingStreamUpdates() {
  discardPendingDeltas();
  discardPendingToolUpserts();
}

function flushPendingDeltas() {
  if (deltaFlushTimer !== undefined) {
    window.clearTimeout(deltaFlushTimer);
    deltaFlushTimer = undefined;
  }
  if (pendingDeltas.size === 0) return;
  lastDeltaFlush = performance.now();
  const deltas = Array.from(pendingDeltas.entries());
  pendingDeltas.clear();
  setState(
    "items",
    produce((items) => {
      for (const [itemId, text] of deltas) {
        for (let i = items.length - 1; i >= 0; i--) {
          const it = items[i];
          if (it.id === itemId && "text" in it) {
            (it as { text: string }).text += text;
            break;
          }
        }
      }
    }),
  );
}

function queueDelta(op: Extract<UpdateOp, { t: "delta" }>) {
  pendingDeltas.set(op.itemId, (pendingDeltas.get(op.itemId) ?? "") + op.text);
  if (deltaFlushTimer !== undefined) return;
  // leading-edge：距上次刷新越久（一轮刚开始）等待越短，首字几乎立即出现；
  // 高频流式时按 DELTA_FLUSH_MS 窗口合并，避免过度重渲染
  const wait = Math.max(0, DELTA_FLUSH_MS - (performance.now() - lastDeltaFlush));
  deltaFlushTimer = window.setTimeout(flushPendingDeltas, wait);
}

function applyUpsert(item: Item) {
  // Turn 落库意味着本轮用量已有终值，清掉进行中的实时值，防止短暂双计。
  if (item.type === "turn") {
    if (state.currentId) liveUsageByThread.delete(state.currentId);
    if (state.liveUsage) setState("liveUsage", null);
  }
  if (item.type === "user") {
    const optimistic = state.items.findIndex(
      (current) => current.type === "user" && current.id < 0,
    );
    if (optimistic >= 0) setState("items", (items) => items.filter((_, i) => i !== optimistic));
  }
  const idx = state.items.findIndex((current) => current.id === item.id);
  if (idx >= 0) setState("items", idx, reconcile(item));
  else setState("items", state.items.length, item);
}

function flushPendingToolUpserts() {
  if (toolUpdateFlushTimer !== undefined) {
    window.clearTimeout(toolUpdateFlushTimer);
    toolUpdateFlushTimer = undefined;
  }
  if (pendingToolItems.size === 0) return;
  lastToolUpdateFlush = performance.now();
  const items = Array.from(pendingToolItems.values());
  pendingToolItems.clear();
  batch(() => {
    for (const item of items) applyUpsert(item);
  });
}

function queueToolItem(item: Extract<Item, { type: "tool" }>) {
  pendingToolItems.set(item.id, item);
  if (toolUpdateFlushTimer !== undefined) return;
  const wait = Math.max(0, TOOL_UPDATE_FLUSH_MS - (performance.now() - lastToolUpdateFlush));
  toolUpdateFlushTimer = window.setTimeout(flushPendingToolUpserts, wait);
}

function flushPendingStreamUpdates() {
  flushPendingDeltas();
  flushPendingToolUpserts();
}

function applyOp(op: UpdateOp) {
  if (op.t === "usage") {
    // 本轮进行中的实时累计用量；按会话缓存，切换回来无需等待下一次工具/usage 事件。
    if (state.currentId) liveUsageByThread.set(state.currentId, op.usage);
    setState("liveUsage", op.usage);
    return;
  }
  if (op.t === "plan") {
    flushPendingStreamUpdates();
    setState("plan", op.plan);
    return;
  }
  if (op.t === "proposed_plan") {
    flushPendingStreamUpdates();
    setState("proposedPlan", op.text);
    return;
  }
  if (op.t === "mode") {
    flushPendingStreamUpdates();
    // 后端偶发上报原生 id（bypass/agent/plan…），一律归一成 Build
    const mode = normalizeUnifiedMode(op.mode) ?? "build";
    setState("mode", mode);
    lastUsed.setMode(state.agentKind, mode);
    return;
  }
  if (op.t === "delta") {
    queueDelta(op);
    return;
  }
  if (op.t === "remove") {
    flushPendingStreamUpdates();
    setState(
      "items",
      state.items.filter((item) => item.id !== op.itemId),
    );
    return;
  }
  // upsert
  if (op.item.type === "tool") {
    queueToolItem(op.item);
    if (op.item.status !== "pending" && op.item.status !== "in_progress") {
      flushPendingStreamUpdates();
    }
    return;
  }
  flushPendingStreamUpdates();
  applyUpsert(op.item);
}

let initialized = false;

type OptionsEvent =
  | ModelOptions
  | {
      agentKind: AgentKind;
      options: ModelOptions;
    };

type CommandsEvent = {
  agentKind: AgentKind;
  commands: unknown;
};

function firstString(...values: unknown[]): string | undefined {
  return values.find((v): v is string => typeof v === "string");
}

function normalizeSlashCommand(raw: unknown): SlashCommand | null {
  if (typeof raw === "string") {
    const name = raw.replace(/^\/+/, "").trim();
    return name ? { name } : null;
  }
  if (!raw || typeof raw !== "object") return null;
  const obj = raw as Record<string, unknown>;
  const name = firstString(obj.name, obj.command, obj.id, obj.title) ?? "";
  const cleanName = name.replace(/^\/+/, "").trim();
  if (!cleanName) return null;
  const description = firstString(obj.description, obj.summary);
  const kind = firstString(obj.kind, obj.type);
  const input = firstString(obj.input, obj.insertText);
  return { name: cleanName, description, kind, input };
}

function normalizeSlashCommands(commands: unknown): SlashCommand[] {
  const values = Array.isArray(commands) ? commands : [];
  return values
    .map(normalizeSlashCommand)
    .filter((c): c is SlashCommand => !!c);
}

const BROWSER_SLASH_COMMANDS: SlashCommand[] = [
  { name: "browser", description: "进入持续浏览器调试模式（Playwright）", kind: "builtin", input: "/browser " },
  { name: "browser-exit", description: "退出浏览器调试模式", kind: "builtin", input: "/browser-exit" },
];

export async function refreshSlashCommands(agentKind: AgentKind) {
  try {
    const commands = await api.getSlashCommands(agentKind);
    const list = normalizeSlashCommands(commands);
    // 内置 /browser 调试命令对 Lyra 与 ACP 后端可用（后端支持 MCP 注入时生效）。
    if (["lyra", "devin", "codebuddy"].includes(agentKind)) {
      for (const cmd of BROWSER_SLASH_COMMANDS) {
        if (!list.some((c) => c.name === cmd.name)) list.push(cmd);
      }
    }
    setState("slashCommands", agentKind, list);
  } catch (err) {
    console.warn(`拉取 ${agentKind} 斜杠命令失败`, err);
  }
}

/** 升级重启的会话恢复是否已有结论（恢复完成或确认无需恢复）。启动签名据此等待目标视图稳定。 */
export const [restoreSettled, setRestoreSettled] = createSignal(false);

export async function initStore() {
  if (initialized) return;
  initialized = true;

  // 必须先监听模型更新，再读取 settings 触发 ensureModelOptions；否则缓存命中后的后台
  // 重验可能在其余监听串行注册期间完成，磁盘已更新但当前窗口仍停在旧列表。
  await listen<OptionsEvent>("acp:options", (e) => {
    const payload = e.payload;
    if ("options" in payload && "agentKind" in payload) {
      setModelOptions(payload.agentKind, payload.options);
      syncLyraConfigDefaultModel(payload.agentKind, payload.options);
      const model = lastUsed.model(payload.agentKind);
      const name = modelChoices(payload.agentKind).find((c) => c.value === model)?.name;
      if (model && name) lastUsed.setModelName(payload.agentKind, name);
    } else {
      setModelOptions("devin", payload as ModelOptions);
      syncLyraConfigDefaultModel("devin", payload as ModelOptions);
    }
  });

  // settings 是本地快照且不依赖事件监听：先并行拉取并尽快写进 store。
  // 后面的 listen 仍照常尽早注册；但不会再因为逐个 await listen 而拖慢设置页回显。
  const settingsReady = api.getSettings().then((settings) => {
    let resolvedSettings = settings;
    if (settings.modelFavorites.length > 0) {
      setModelFavoriteIds(settings.modelFavorites);
      localStorage.setItem(MODEL_FAVORITES_KEY, JSON.stringify(settings.modelFavorites));
    } else if (modelFavoriteIds().length > 0) {
      resolvedSettings = { ...settings, modelFavorites: modelFavoriteIds() };
      void api.setSettings(resolvedSettings).catch(() => {});
    }
    const preferredAgent = lastUsed.agentKind();
    const initialAgent = agentEnabled(resolvedSettings, preferredAgent)
      ? preferredAgent
      : (ALL_AGENT_KINDS.find((kind) => agentEnabled(resolvedSettings, kind)) ?? preferredAgent);
    if (isThemePref(resolvedSettings.theme) && resolvedSettings.theme !== state.theme) {
      setTheme(resolvedSettings.theme);
    }
    setState({ settings: resolvedSettings, agentKind: initialAgent });
    // 后端先返回上次缓存，并在进程内只后台重验一次；CodeBuddy 即使不是当前后端也要
    // 静默同步云端动态模型，选择器打开时继续展示旧列表，不清空、不阻塞。
    void ensureModelOptions(initialAgent);
    if (initialAgent !== "codebuddy" && agentEnabled(resolvedSettings, "codebuddy")) {
      void ensureModelOptions("codebuddy");
    }
    return { settings: resolvedSettings, initialAgent };
  }).catch((error: unknown) => {
    // 设置读取失败也不能让窗口永久隐藏；此时沿用 localStorage 的首帧主题。
    setRestoreSettled(true);
    void api.showMainWindow().catch(() => {});
    return { error };
  });

  await listen<{ threadId: string; op?: UpdateOp; ops?: UpdateOp[] }>("acp:update", (e) => {
    const ops = e.payload.ops ?? (e.payload.op ? [e.payload.op] : []);
    // 后台会话的 usage 也要保留；否则切回运行中的会话会先显示 0，直到下一次上报。
    for (const op of ops) {
      if (op.t === "usage") liveUsageByThread.set(e.payload.threadId, op.usage);
    }
    if (e.payload.threadId !== state.currentId) {
      if (threadSnapshots.has(e.payload.threadId)) staleThreadSnapshots.add(e.payload.threadId);
      return;
    }
    // 切换会话加载快照期间忽略增量：此刻 items 还是旧会话的，getThread 快照会包含
    // 已落库的全部内容，加载完成（loadingThread=false）后再应用后续实时增量。
    // mode / proposed_plan / plan 是低频关键状态，加载中也要应用，否则 agent 切到 Plan
    // 时选择器与「实施此计划」按钮会对不齐。
    const apply = (op: UpdateOp) => {
      if (
        state.loadingThread &&
        op.t !== "mode" &&
        op.t !== "proposed_plan" &&
        op.t !== "plan"
      ) {
        return;
      }
      applyOp(op);
    };
    if (ops.length > 1) {
      batch(() => {
        for (const op of ops) apply(op);
      });
    } else if (ops[0]) {
      apply(ops[0]);
    }
  });

  await listen<{ threadId: string; cwd: string }>("thread:cwd-changed", (e) => {
    const { threadId, cwd } = e.payload;
    const cached = threadSnapshots.peek(threadId);
    if (cached) rememberThreadSnapshot({ ...cached, cwd });
    setState("threads", (thread) => thread.id === threadId, "cwd", cwd);
    if (state.currentId === threadId) setState("cwd", cwd);
  });

  await listen<TurnEvent>("acp:turn", (e) => {
    const threadId = e.payload.threadId;
    const wasRunning = !!state.running[threadId];
    runningEventVersions.set(threadId, (runningEventVersions.get(threadId) ?? 0) + 1);
    optimisticRunningThreads.delete(threadId);
    setState("running", threadId, e.payload.running);
    if (threadId !== state.currentId && threadSnapshots.has(threadId)) {
      staleThreadSnapshots.add(threadId);
    }
    if (e.payload.running) {
      // 只在新一轮开始时丢弃上一轮残留；重复 running 事件不能覆盖本轮已收到的 usage。
      if (!wasRunning) {
        liveUsageByThread.delete(threadId);
        if (threadId === state.currentId) setState("liveUsage", null);
      }
      // 非 store.sendPrompt 入口（远程、后台重发等）开始 turn 时，重新挂上 Fire 跟踪。
      resumeFireRelay(e.payload.threadId);
      handleWorkflowTurnStart(e.payload.threadId);
    } else {
      // 轮次正常收尾且该会话未打开 → 标记未读，提醒回看结论
      const completedNormally =
        e.payload.stopReason === "end_turn" || e.payload.stopReason === "max_turn_requests";
      if (completedNormally && threadId !== state.currentId) {
        setState("unreadTurns", threadId, (state.unreadTurns[threadId] ?? 0) + 1);
      }
      // 轮次结束的兜底清理：正常路径下 Turn upsert 已清零，这里覆盖异常收尾。
      liveUsageByThread.delete(threadId);
      if (threadId === state.currentId) setState("liveUsage", null);
      if (pendingSetupConfigRefresh.delete(threadId)) {
        void api.refreshLyraConfig().catch((error) =>
          console.error("Refresh Lyra config after /setup failed", error),
        );
      }
      if (fireRelaySteps.has(e.payload.threadId)) {
        const reason = e.payload.stopReason;
        const manuallyInterrupted = reason === "cancelled" || reason === "force_cancelled";
        const completedNormally = reason === "end_turn" || reason === "max_turn_requests";
        // 只有明确正常收尾才进入判断。网络、进程或模型错误均暂停在当前阶段，
        // 用户补充提示或发送“继续”后，会从这一阶段恢复完整 Fire 流程。
        const action = completedNormally
          ? advanceFireRelay(e.payload.threadId)
          : suspendFireRelay(e.payload.threadId, manuallyInterrupted);
        void action.catch((error) => console.error("Fire relay failed", error));
      }
      // 通用工作流（/run）与 Fire 互斥：非 Fire 会话才会被其接管。
      handleWorkflowTurnEnd(e.payload.threadId, e.payload.stopReason);
      if (pendingHardDesign.has(threadId)) {
        const reason = e.payload.stopReason;
        const completedNormally = reason === "end_turn" || reason === "max_turn_requests";
        if (completedNormally) {
          void finalizeHardDesign(threadId).catch((error) =>
            console.error("Hard workflow design failed", error),
          );
        }
      }
      if (
        e.payload.threadId === state.currentId &&
        state.items.some((item) => item.id < 0)
      ) {
        // 后台 restore 被取消或自动重发失败：清掉尚未落库的乐观消息。
        void openThread(e.payload.threadId);
      }
    }
  });

  await listen<{ threadId: string; text: string }>("fire:start", (e) => {
    void handleFireStart(e.payload.threadId, e.payload.text).catch((error) =>
      console.error("Fire start failed", error),
    );
  });

  await listen<{ threadId: string; text: string; images?: PromptImage[] }>(
    "remote-prompt:dispatch",
    (e) => {
      void sendPromptTo(e.payload.threadId, e.payload.text, e.payload.images ?? []).catch((error) =>
        console.error("Remote prompt dispatch failed", error),
      );
    },
  );

  await listen<PermissionRequest>("acp:permission", (e) => {
    setState("permissions", state.permissions.length, e.payload);
  });

  await listen<{ requestKey: string }>("acp:permission-resolved", (e) => {
    setState(
      "permissions",
      state.permissions.filter((p) => p.requestKey !== e.payload.requestKey),
    );
  });

  await listen<Status>("acp:status", (e) => {
    setState({ connected: e.payload.connected, agent: e.payload.agent });
  });

  await listen<CommandsEvent>("acp:commands", (e) => {
    setState("slashCommands", e.payload.agentKind, normalizeSlashCommands(e.payload.commands));
  });

  // 后端可用性只用于设置页的 CLI 缺失提示；选择器是否展示完全由启用开关决定。
  await listen<{ availability: Record<string, boolean> }>("backends:availability", (e) => {
    setState("backendAvailability", reconcile(e.payload.availability ?? {}));
  });

  await listen<string>("acp:log", (e) => {
    setState(
      "logs",
      produce((logs) => {
        logs.push(e.payload);
        if (logs.length > 500) logs.splice(0, logs.length - 500);
      }),
    );
  });

  // 自动更新：检测 + 静默下载暂存改由后端 tokio 定时器负责（每 10 分钟，不只启动时），
  // 避免 WebView 计时器在窗口最小化/隐藏时被节流，导致「只有启动才检测、角标不出现」。
  // 前端只负责响应事件并展示角标。
  await listen<UpdateProgress>("update:progress", (e) => {
    setState("updateProgress", e.payload);
  });
  // 后端暂存就绪 → 显示左上角「可更新」角标，并填充更新弹窗信息
  await listen<UpdateInfo>("update:available", (e) => {
    setState("update", { ...e.payload, staged: true });
    setState("updateStaging", false);
  });
  // 空闲（无会话/无任务）+ 新版本已下载好 → 后端主动请求弹窗，让用户选择是否现在更新
  await listen<UpdateInfo>("update:prompt", (e) => {
    setState("update", { ...e.payload, staged: true });
    setState("updateStaging", false);
    setState("updatePromptAt", Date.now());
  });
  // 启动即反映「已暂存好」的更新，让角标立刻出现（新版本的下载交给后端静默处理）
  void api
    .checkUpdate()
    .then((info) => {
      if (info.hasUpdate && info.staged) setState("update", { ...info, staged: true });
    })
    .catch(() => {
      // 网络不可用等场景静默失败，后端定时器会按周期重试
    });

  await listen<{ threadId: string }>("threads:title-generated", (e) => {
    const id = e.payload.threadId;
    setState("titleTyping", id, true);
    window.setTimeout(() => {
      setState("titleTyping", id, false);
    }, 3000);
  });

  await listen("threads:changed", () => {
    // 标题可能由首条消息生成：直接用列表 meta 同步，不再 getThread 全量拉当前会话
    // （那会把整段历史 items 走一遍 IPC 序列化，长会话时每轮结束都白搬几 MB）
    void refreshThreads().then(() => {
      const id = state.currentId;
      if (!id) return;
      const meta = state.threads.find((t) => t.id === id);
      if (meta && meta.title !== state.title) setState("title", meta.title);
    });
    // 项目列表由后端合并会话目录生成，会话增删后同步刷新
    void refreshProjects();
  });

  // worktree 删除等操作导致项目列表变化
  await listen("projects:changed", () => {
    void refreshProjects();
  });

  await listen("clues:changed", () => {
    void refreshClueGroups();
  });

  await listen<{ cardId: string }>("clues:mention-open", (e) => {
    openClueCard(e.payload.cardId);
  });

  await listen<{ cardId: string }>("clues:mentioned", (e) => {
    const cardId = e.payload.cardId;
    if (!cardId || state.unreadClueMentions.includes(cardId)) return;
    setState("unreadClueMentions", (ids) => [...ids, cardId]);
  });

  // 系统通知点击：跳转到对应会话
  await listen<{ threadId: string }>("acp:notify-open", (e) => {
    void openThread(e.payload.threadId);
  });

  // 漫游快照重同步（重连/轮次结束自愈）：用 reconcile 按 id 合并，保留未变条目的
  // DOM 与滚动位置、思考/工具展开状态，避免整段重渲染导致的闪烁与跳动。
  await listen<{ threadId: string }>("acp:reload", (e) => {
    const id = e.payload.threadId;
    if (state.currentId !== id) return;
    void api.getThread(id).then((t) => {
      if (state.currentId !== id) return;
      flushPendingStreamUpdates();
      setState("items", reconcile(t.items, { key: "id" }));
      setState({
        plan: (t.plan as PlanEntry[] | null) ?? null,
        title: t.title,
      });
    });
  });

  // 团队/漫游中转站事件
  await listen<RelayStatus>("relay:status", (e) => {
    setState("relay", e.payload);
    if (e.payload.connected) {
      void refreshInbox();
      void refreshWorkflowInbox();
      // 重连后强制校准：离线期间对端可能已调整共享模型，旧 peerModels 不能继续复用。
      preloadPeerModels(true);
    }
  });
  await listen<{ peers: Peer[] } | Peer[]>("relay:peers", (e) => {
    setState("peers", normalizePeers(e.payload));
    // 名单变化（有人上线/重连）即强制刷新，避免继续复用该成员断线前的旧模型列表。
    preloadPeerModels(true);
  });
  // 漫游：对端回传其可选模型/模式，按 token 缓存供选择器使用
  await listen<{
    peer: string;
    backends: AgentKind[];
    options: PeerModels["options"];
    sharedOptions: PeerModels["sharedOptions"];
  }>(
    "relay:peer-models",
    (e) => {
      const { peer, backends, options, sharedOptions } = e.payload;
      if (!peer) return;
      setState("peerModels", peer, {
        backends: Array.isArray(backends) ? backends : [],
        options: options ?? {},
        sharedOptions: sharedOptions ?? {},
      });
    },
  );
  // 漫游：对端回传某目录的本地分支列表，按「token+目录」缓存供 worktree 下拉使用
  await listen<{ peer: string; folder: string; current: string; branches: string[] }>(
    "relay:peer-branches",
    (e) => {
      const { peer, folder, current, branches } = e.payload;
      if (!peer) return;
      setState("peerBranches", peerBranchKey(peer, folder), {
        current: current ?? "",
        branches: Array.isArray(branches) ? branches : [],
      });
    },
  );
  await listen<IncomingShare[]>("relay:inbox", (e) => {
    // 漫游召回的快照到达时自动弹出收件箱，用户直接选项目接收
    const known = new Set(state.inbox.map((s) => s.id));
    const hasNewRecall = e.payload.some((s) => s.recall && !known.has(s.id));
    setState("inbox", e.payload);
    if (hasNewRecall) setState("inboxPromptAt", Date.now());
  });
  // 队友分享的工作流到达：进入工作流收件箱，在「工作流」页接收
  await listen<IncomingWorkflowShare[]>("relay:workflow-inbox", (e) => {
    setState("workflowInbox", e.payload);
  });
  // 本地 worktree 后台创建就绪：切到 worktree 的 cwd 已由后端回写，这里补发暂存的首条提示词
  await listen<{ threadId: string }>("acp:worktree-ready", (e) => {
    const id = e.payload.threadId;
    void refreshThreads();
    if (state.currentId === id) {
      void api.getThread(id).then((t) => {
        if (state.currentId === id) setState("cwd", t.cwd);
      });
    }
    flushWorktreePrompt(id);
  });
  // 本地 worktree 后台创建失败：丢弃暂存提示词（会话里已有错误系统消息）
  await listen<{ threadId: string; error?: string }>("acp:worktree-failed", (e) => {
    pendingWorktreePrompts.delete(e.payload.threadId);
    void refreshThreads();
  });
  // host 侧：收到漫游请求，入队等本机用户在弹框里确认
  await listen<IncomingRoamRequest>("relay:roam-request", (e) => {
    setState("incomingRoams", (prev) => [
      ...prev.filter((r) => r.reqId !== e.payload.reqId),
      e.payload,
    ]);
  });
  await listen<QuotaRoamingProgress>("relay:quota-progress", (e) => {
    setState("quotaRoamingProgress", e.payload);
  });

  // settingsReady 在 initStore 开头已经启动并会尽快 setState；这里 await 只是拿到值供后续
  // 主题迁移、团队刷新、模型预拉等初始化步骤继续使用。
  const settingsResult = await settingsReady;
  if ("error" in settingsResult) throw settingsResult.error;
  const { settings, initialAgent } = settingsResult;

  // 后端可用性兜底拉取：启动检测很快，事件可能在前端监听就绪前已 emit 过；
  // 结果仅用于设置页提示，不参与选择器过滤。
  void api
    .getBackendAvailability()
    .then((a) => {
      if (Object.keys(a).length > 0) setState("backendAvailability", reconcile(a));
    })
    .catch(() => {
      // 拉取失败按「全部可用」处理，不影响使用
    });

  // 主题以后端 settings.json 为准；后端未设置（老版本/首次升级）时，
  // 把当前 localStorage 里的偏好迁移上去，使其成为可靠真相来源。
  if (!isThemePref(settings.theme)) {
    persistThemeToBackend(state.theme);
  }

  // 团队/漫游：settings 一就绪就立刻刷新中转状态并读取本地 presence 缓存；
  // 在线名单随后完全由 /v2/events SSE 的 presence 首包和变更推送维护，不再轮询 /v2/peers。
  void refreshRelayStatus();
  void api
    .getRelayPeers()
    .then((peers) => {
      setState("peers", normalizePeers(peers));
      preloadPeerModels();
    })
    .catch(() => {});
  void refreshInbox();
  void refreshWorkflowInbox();
  // 成就后台预拉：有新成就时侧栏入口直接亮角标，不必等用户打开成就页
  void refreshAchievements();
  void refreshRoamingFolders();
  void refreshClueGroups();

  // 会话/项目列表也是快的本地读取；升级重启后的会话恢复依赖 threads 已就绪，故这里 await 等它俩。
  await Promise.all([refreshThreads(), refreshProjects()]);

  // agent 连接状态与日志：getStatus 可能因 prewarm 抢连接锁而慢，单独异步拉取，绝不能再放进上面
  // 的关键路径阻塞 settings/团队状态/会话列表。
  void api
    .getStatus()
    .then((status) => setState({ connected: status.connected, agent: status.agent }))
    .catch(() => {
      // 连接状态拉取失败不影响使用，后端连上后会通过 acp:status 事件补正
    });
  void api.getLogs().then((logs) => setState("logs", logs)).catch(() => {});
  // 其他后端在用户切换或打开模型选择器时按需加载。
  void refreshSlashCommands(initialAgent);

  // 额度：启动拉取一次，之后每 10 分钟刷新；模型费用基本不变，失败时随额度周期重试
  void refreshQuota();
  void refreshModelCosts();
  setInterval(() => {
    void refreshQuota();
    if (!state.modelCosts) void refreshModelCosts();
  }, 10 * 60 * 1000);

  // 漫游模型后台静默「更新」：每 5 分钟强制刷新一轮在线队友的模型列表，
  // 让对端后端配置变化（启用/关闭某后端、换模型）能被及时同步，而无需用户手动重试。
  setInterval(() => preloadPeerModels(true), 5 * 60 * 1000);

  // 升级（手动/静默）重启后：自动恢复打开升级前正在查看的会话。
  // 普通启动时后端返回 null，不会恢复任何会话。
  void api
    .takeRestoreThread()
    .then(async (id) => {
      if (id && !state.currentId && state.threads.some((t) => t.id === id)) {
        await openThread(id).catch(() => {});
      }
    })
    .catch(() => {})
    .finally(() => {
      // 目标页面已经稳定后再显示窗口，避免升级重启时先露出新会话页。
      setRestoreSettled(true);
      void api.showMainWindow().catch(() => {});
    });

  // 监听用户交互并节流上报活动，供后端静默升级判断「一段时间没有操作」
  initActivityTracking();
}
