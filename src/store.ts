import { listen } from "@tauri-apps/api/event";
import { batch, createSignal } from "solid-js";
import { createStore, produce, reconcile } from "solid-js/store";
import { api } from "./ipc";
import type {
  AgentKind,
  BranchList,
  CaptureClueResult,
  CliOperationProgress,
  ClueCard,
  ClueNodeGroup,
  Decision,
  EffortChoice,
  Employee,
  EmployeeTask,
  IncomingRoamRequest,
  IncomingShare,
  Item,
  Mark,
  ModelChoice,
  ModelCost,
  ModelOptions,
  ModeChoice,
  Peer,
  PeerModels,
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
import { firstWakeDoPairForThread } from "./threadDisplay";
import { isScratch } from "./utils";

/** 界面皮肤：水墨夜色 / 宣纸亮色 */
export type ThemePref = "ink-dark" | "ink-light";

const THEME_KEY = "fd:theme";

function readThemePref(): ThemePref {
  return localStorage.getItem(THEME_KEY) === "ink-light" ? "ink-light" : "ink-dark";
}

function applyThemeToDom(theme: ThemePref) {
  document.documentElement.dataset.theme = theme;
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
  title: string;
  /** 当前线程的模型/模式（"" = 默认） */
  agentKind: AgentKind;
  model: string;
  mode: string;
  reasoningEffort: string;
  /** 当前打开线程若是漫游 guest，其对端（host）token；否则 null。用于取对端模型列表 */
  roamingPeer: string | null;
  running: Record<string, boolean>;
  permissions: PermissionRequest[];
  connected: boolean;
  agent: Status["agent"];
  settings: Settings | null;
  modelOptions: Record<AgentKind, ModelOptions | null>;
  logs: string[];
  loadingThread: boolean;
  quota: Quota | null;
  /** 模型费用信息（modelUid -> 倍率/厂商/视觉），拉取失败时为 null */
  modelCosts: Record<string, ModelCost> | null;
  /** 已静默下载好、可重启更新的版本信息（无则为 null） */
  update: UpdateInfo | null;
  /** 正在后台静默下载更新 */
  updateStaging: boolean;
  /** 空闲时后端请求弹出更新对话框的时间戳（0 = 未请求）；变化即触发弹窗 */
  updatePromptAt: number;
  /** 当前 agent 暴露的斜杠命令。Devin 来自 ACP，Codex 来自本机 Codex skills。 */
  slashCommands: Record<AgentKind, SlashCommand[]>;
  /** 更新下载/安装进度 */
  updateProgress: UpdateProgress | null;
  /** CLI 安装/升级进度；设置页与额度租借前置安装共用。 */
  cliOperationProgress: CliOperationProgress | null;
  /** 团队/漫游中转站状态 */
  relay: RelayStatus;
  /** 在线名单（团队/漫游） */
  peers: Peer[];
  /** 漫游：各对端（host）回传的可选模型/模式，按对端 token 缓存 */
  peerModels: Record<string, PeerModels>;
  /** worktree：各来源的本地分支列表，key 为 `${peer}:${folder}`（本地会话不走这里） */
  peerBranches: Record<string, BranchList>;
  /** 收到的待接收分享 */
  inbox: IncomingShare[];
  /** 请求自动弹出收件箱的时间戳（漫游召回快照到达时置位；0 = 无请求） */
  inboxPromptAt: number;
  /** host 侧：待本机确认的漫游请求队列 */
  incomingRoams: IncomingRoamRequest[];
  /** 借用方：等待授权、安装 CLI、准备隔离凭证的进度。 */
  quotaRoamingProgress: QuotaRoamingProgress | null;
  /** 本机允许漫游的目录 */
  roamingFolders: string[];
  /** 手动展开的详情（工具调用/轮次折叠/思考过程），key 为 item id；切换线程时清空 */
  expanded: Record<string, boolean>;
  titleTyping: Record<string, boolean>;
  /** 主区域视图（currentId 非空时优先显示会话，与本字段无关） */
  view: "home" | "clues" | "employees" | "workbench";
  /** 证据链的隐藏节点组；界面只渲染其中的 ClueCard。 */
  clueGroups: ClueNodeGroup[];
  /** 从证据链跳到新会话时暂存的根线索。 */
  pendingClueCard: { id: string; title: string } | null;
  /** 系统提醒点击后，请证据链定位到指定卡片。 */
  clueOpenRequest: string | null;
  /** 数字员工列表 */
  employees: Employee[];
  /** 全部员工的任务活动记录（历史/进行中） */
  employeeTasks: EmployeeTask[];
  /** 协作标记账本（全部 scope） */
  marks: Mark[];
  /** 奏折（御书房）：候旨/已批阅 */
  decisions: Decision[];
  /** 当前界面皮肤 */
  theme: ThemePref;
  /** 后端可用性检测结果（agentKind → 是否可用）。空 = 尚未检测完成（按全部可用处理） */
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
    devin: null,
    codex: null,
    codebuddy: null,
    claudecode: null,
    cursor: null,
    opencode: null,
  },
  logs: [],
  loadingThread: false,
  quota: null,
  modelCosts: null,
  update: null,
  updateStaging: false,
  updatePromptAt: 0,
  slashCommands: {
    devin: [],
    codex: [],
    codebuddy: [],
    claudecode: [],
    cursor: [],
    opencode: [],
  },
  updateProgress: null,
  cliOperationProgress: null,
  relay: { enabled: false, connected: false },
  peers: [],
  peerModels: {},
  peerBranches: {},
  inbox: [],
  inboxPromptAt: 0,
  incomingRoams: [],
  quotaRoamingProgress: null,
  roamingFolders: [],
  expanded: {},
  titleTyping: {},
  view: "home",
  clueGroups: [],
  pendingClueCard: null,
  clueOpenRequest: null,
  employees: [],
  employeeTasks: [],
  marks: [],
  decisions: [],
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

export function toggleExpanded(key: number | string, value?: boolean) {
  const k = String(key);
  setState("expanded", k, value ?? !state.expanded[k]);
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
      value: "__nova_auto_value__",
      name: "Auto（按性价比）",
      description: "新会话首次发送前获取性价比第一名，后续固定复用；数据来自 Codex 雷达 codexradar.com",
    },
    {
      value: "__nova_auto_iq__",
      name: "Auto（按智商）",
      description: "新会话首次发送前获取 IQ 第一名，后续固定复用；数据来自 Codex 雷达 codexradar.com",
    },
  ];
  return [...auto, ...choices.filter((choice) => !choice.value.startsWith("__nova_auto_"))];
}

/** 在可选列表中解析应使用的模型。
 *  优先 preferred；项目模型即使暂不在列表也必须保留，避免异步加载时被全局 lastUsed 覆盖；
 *  仅 preferred 为空时才回退全局 lastUsed / 第一项（跳过 Cursor「Auto」这类 value="" 入口）。 */
export function resolveAvailableModel(
  agentKind: AgentKind,
  preferred: string,
  source?: ModelOptions | null,
): string {
  const choices = modelChoices(agentKind, source);
  if (choices.length === 0) return preferred;
  if (preferred && choices.some((c) => c.value === preferred)) return preferred;
  // 有明确选择但不在当前列表：保留，避免项目模型在列表不全/中间态被全局最近模型覆盖。
  if (preferred) return preferred;
  const previous = lastUsed.globalModel(agentKind);
  if (previous && choices.some((c) => c.value === previous)) return previous;
  return choices.find((c) => c.value)?.value ?? choices[0]?.value ?? "";
}

/** 统一会话模式：全部后端只暴露 Build（放开全部权限执行）/ Plan（只规划不执行）两种，
 *  发送时由 Rust 侧翻译成各后端的真实模式 id（bypass / bypassPermissions / agent / build …）。 */
export const UNIFIED_MODES: ModeChoice[] = [
  { id: "build", name: "Build" },
  { id: "plan", name: "Plan" },
];

/** 可选会话模式列表。所有后端（含漫游对端）统一返回 Build / Plan。
 *  参数保留是为了兼容既有调用点（历史上按后端上报列表区分）。 */
export function modeChoices(
  _agentKind: AgentKind = state.agentKind,
  _source?: ModelOptions | null,
): ModeChoice[] {
  return UNIFIED_MODES;
}

/** 旧模式值 → 统一模式 id。识别不了（如 accept-edits / ask）返回 undefined，由调用方回退。 */
export function normalizeUnifiedMode(m?: string | null): "build" | "plan" | undefined {
  if (!m) return undefined;
  if (m.toLowerCase() === "plan") return "plan";
  if (["build", "bypass", "bypassPermissions", "agent", "dontAsk", "fullAccess"].includes(m)) {
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

export async function ensureModelOptions(agentKind: AgentKind) {
  if (state.modelOptions[agentKind] || modelOptionsLoading.has(agentKind)) return;
  modelOptionsLoading.add(agentKind);
  try {
    const opts = await api.getModelOptions(agentKind);
    setState("modelOptions", agentKind, opts);
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

/** 「已启用 且 本机检测可用」的模型后端（按固定顺序）——新建/切换会话的后端列表只显示这些。
 *  可用性由后端启动后并发检测（backends:availability），尚未出结果（无该键）时按可用处理。
 *  settings 未加载前必须返回空，避免组件抢先探测全部后端并拉起已关闭的进程。 */
export function enabledAgentKinds(): AgentKind[] {
  const s = state.settings;
  if (!s) return [];
  const avail = state.backendAvailability;
  return ALL_AGENT_KINDS.filter((k) => agentEnabled(s, k) && avail[k] !== false);
}

/** 把 agentKind 收敛到「已启用」集合：已启用则原样返回，否则回退到第一个启用项 */
export function resolveEnabledAgentKind(kind: AgentKind): AgentKind {
  const list = enabledAgentKinds();
  return list.includes(kind) ? kind : (list[0] ?? kind);
}

export async function refreshThreads() {
  const threads = await api.listThreads();
  // 按 id reconcile 而非整体替换：保留未变线程的对象身份，
  // 避免 <For> 重建整个列表 DOM 导致侧边栏滚动位置被重置
  setState("threads", reconcile(threads, { key: "id" }));
  const running: Record<string, boolean> = {};
  for (const t of threads) running[t.id] = t.running;
  setState("running", running);
  for (const thread of threads.filter((thread) => thread.running).slice(0, THREAD_SNAPSHOT_LIMIT)) {
    preloadThreadSnapshot(thread.id);
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

export async function refreshPeers() {
  try {
    // 走联网刷新：直接查服务端 roster，不依赖 SSE presence 推送。
    // 此前读的是本地缓存（只被 SSE presence 更新），一旦某次 presence 推送丢失，
    // 在线名单会长时间停在旧状态（表现为「别人看不到你 / 少一个人」）；也能在 SSE 尚未
    // 连上时就先显示名单，加快入网体感。
    setState("peers", normalizePeers(await api.refreshRelayPeers()));
  } catch {
    // 忽略（保留上次名单）
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

export async function refreshRoamingFolders() {
  try {
    setState("roamingFolders", await api.listRoamingFolders());
  } catch {
    // 忽略
  }
}

// ===== 数字员工 =====

/** 切换主区域视图。不影响已打开的会话（currentId 优先）。 */
export function setView(view: "home" | "clues" | "employees" | "workbench") {
  setState("view", view);
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
  const groups = await api.listClueGroups();
  setState("clueGroups", reconcile(groups));
}

export async function captureClue(
  threadId: string | null,
  title: string,
  content: string,
  placement: "update" | "parallel" | "new",
  targetCardId: string | null,
  mentionTokens: string[] = [],
): Promise<CaptureClueResult> {
  const result = await api.captureClue(
    threadId,
    title,
    content,
    placement,
    targetCardId,
    mentionTokens,
  );
  await Promise.all([refreshClueGroups(), refreshThreads()]);
  return result;
}

export async function addClueComment(
  cardId: string,
  content: string,
  parentCommentId: string | null,
  mentionTokens: string[] = [],
) {
  await api.addClueComment(cardId, content, parentCommentId, mentionTokens);
  await refreshClueGroups();
}

/** 用高级分享模型总结会话，供线索表单填入 */
export async function summarizeClue(threadId: string) {
  return api.summarizeClue(threadId);
}

export async function associateClues(beforeCardId: string, afterCardId: string) {
  await api.associateClues(beforeCardId, afterCardId);
  await refreshClueGroups();
}

export async function disassociateClues(beforeCardId: string, afterCardId: string) {
  await api.disassociateClues(beforeCardId, afterCardId);
  await refreshClueGroups();
}

export async function splitClue(cardId: string) {
  await api.splitClue(cardId);
  await refreshClueGroups();
}

export async function stackClues(cardIds: string[]) {
  await api.stackClues(cardIds);
  await refreshClueGroups();
}

export async function deleteClue(cardId: string) {
  await api.deleteClue(cardId);
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

export function openClueCard(cardId: string) {
  if (!cardId) return;
  setView("clues");
  closeThread();
  setState("clueOpenRequest", cardId);
  void refreshClueGroups();
}

export function clearClueOpenRequest(cardId: string) {
  if (state.clueOpenRequest === cardId) setState("clueOpenRequest", null);
}

export async function refreshEmployees() {
  try {
    setState("employees", await api.listEmployees());
  } catch {
    // 忽略（保留上次数据）
  }
}

export async function refreshEmployeeTasks() {
  try {
    setState("employeeTasks", await api.listEmployeeTasks());
  } catch {
    // 忽略
  }
}

export async function refreshMarks() {
  try {
    setState("marks", await api.listMarks());
  } catch {
    // 忽略
  }
}

export async function refreshDecisions() {
  try {
    setState("decisions", await api.listDecisions());
  } catch {
    // 忽略
  }
}

/** 候旨（待主管朱批）+ 未读完工汇报的数量（御书房入口的未决 badge） */
export function pendingDecisionCount(): number {
  return state.decisions.filter((d) => d.status === "pending" || d.status === "report").length;
}

/** 在线的其他人（排除自己）。漫游只能选择对方已共享（上报）的目录，不再支持手输路径；
 *  没有共享目录的队友会在下拉里提示「对方暂未共享可漫游的项目」。 */
export function roamingPeers(): Peer[] {
  const me = state.settings?.relayToken ?? "";
  return state.peers.filter((p) => p.online && p.token !== me);
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

/** 本机目录执行，临时使用在线队友授权的后端额度。 */
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
  setState("running", t.id, false);
  reportActivity(true);
  void refreshThreads();
  ensurePeerModels(peer.token);
  return t.id;
}

export function clearQuotaRoamingProgress() {
  setState("quotaRoamingProgress", null);
}

const THREAD_SNAPSHOT_LIMIT = 2;
const threadSnapshots = new Map<string, Thread>();
const loadingThreadSnapshots = new Set<string>();

function rememberThreadSnapshot(thread: Thread) {
  threadSnapshots.delete(thread.id);
  threadSnapshots.set(thread.id, thread);
  while (threadSnapshots.size > THREAD_SNAPSHOT_LIMIT) {
    const oldest = threadSnapshots.keys().next().value;
    if (!oldest) break;
    threadSnapshots.delete(oldest);
  }
}

function getThreadSnapshot(id: string): Thread | undefined {
  const thread = threadSnapshots.get(id);
  if (!thread) return undefined;
  threadSnapshots.delete(id);
  threadSnapshots.set(id, thread);
  return thread;
}

function preloadThreadSnapshot(id: string) {
  if (id === state.currentId || threadSnapshots.has(id) || loadingThreadSnapshots.has(id)) return;
  loadingThreadSnapshots.add(id);
  void api
    .getThread(id)
    .then(rememberThreadSnapshot)
    .catch(() => {})
    .finally(() => loadingThreadSnapshots.delete(id));
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
  });
}

/** Plan 模式且已结束的会话：从最后一轮助手正文恢复「实施此计划」按钮 */
function recoverProposedPlan(thread: Thread): string | null {
  if (normalizeUnifiedMode(thread.mode) !== "plan") return null;
  if (state.running[thread.id]) return null;
  let lastAssistant: string | null = null;
  for (let i = thread.items.length - 1; i >= 0; i--) {
    const item = thread.items[i];
    if (item.type === "turn") continue;
    if (item.type === "user") break;
    if (item.type === "assistant" && item.text.trim()) {
      lastAssistant = item.text;
      break;
    }
  }
  return lastAssistant;
}

export async function openThread(id: string) {
  const pair = firstWakeDoPairForThread(state.threads, id);
  if (pair?.wake.id === id && pair.doThread) id = pair.doThread.id;
  const switching = state.currentId !== id;
  if (switching) {
    discardPendingStreamUpdates();
    setState("expanded", reconcile({}));
  } else {
    flushPendingStreamUpdates();
  }
  const cached = getThreadSnapshot(id);
  if (cached) {
    showThreadSnapshot(cached, true);
  } else {
    const meta = state.threads.find((thread) => thread.id === id);
    setState({
      currentId: id,
      items: [],
      plan: null,
      proposedPlan: null,
      cwd: meta?.cwd ?? "",
      title: meta?.title ?? "",
      agentKind: meta?.agentKind ?? state.agentKind,
      model: "",
      mode: "",
      reasoningEffort: "",
      roamingPeer: null,
      loadingThread: true,
    });
  }
  try {
    // 先把「当前查看会话」上报后端，再拉快照。后端 emit_update 按 active_thread 门控，
    // 只有该会话被标记为前台后才会向本 WebView 推增量——放在 getThread 之前，能保证
    // 快照之后产生的流式增量都会被推来，不漏。加载期间 acp:update 监听按 loadingThread
    // 忽略增量（见 initStore），避免它们打到尚未替换的旧 items 上。
    await api.reportActivity(id);
    lastActivityReport = Date.now();
    if (state.currentId !== id) return;
    const t = await api.getThread(id);
    // 防止异步竞态：用户可能已切走
    if (state.currentId !== id) return;
    rememberThreadSnapshot(t);
    const agentKind = t.agentKind ?? "devin";
    // 只有本地 createThread 成功后才更新模型偏好；打开、恢复、会话内切换和漫游只同步 UI。
    showThreadSnapshot(t, false, !!cached);
    // 漫游 / 额度租借：拉取对端模型列表；普通本地会话才探测本机后端。
    const roamingPeer =
      t.roamingRole === "guest" ? t.roamingPeer ?? null : t.quotaPeer ?? null;
    if (roamingPeer) ensurePeerModels(roamingPeer);
    else void ensureModelOptions(agentKind);
    // 活动已在 getThread 之前上报（见开头），此处无需重复
  } catch {
    setState({ loadingThread: false });
  }
}

export function closeThread() {
  discardPendingStreamUpdates();
  setState("expanded", reconcile({}));
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
  // 本地 createThread 是唯一写入模型偏好的入口；漫游和额度租借走各自创建函数，不会到这里。
  lastUsed.setAgentKind(storedAgentKind);
  lastUsed.setModel(agentKind, model, cwd);
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

function projectStorageKey(cwd?: string | null): string | null {
  const key = cwd?.trim();
  return key ? encodeURIComponent(key) : null;
}

/** 记住最近一次选择的模型/模式，作为新会话默认值 */
export const lastUsed = {
  agentKind: (): AgentKind =>
    (localStorage.getItem("fd:lastAgentKind") as AgentKind | null) ?? "devin",
  setAgentKind: (v: AgentKind) => localStorage.setItem("fd:lastAgentKind", v),
  globalModel: (agentKind: AgentKind = "devin") =>
    localStorage.getItem(`fd:${agentKind}:lastModel`) ??
    (agentKind === "devin" ? localStorage.getItem("fd:lastModel") ?? "" : ""),
  model: (agentKind: AgentKind = "devin", cwd?: string | null) =>
    (projectStorageKey(cwd)
      ? localStorage.getItem(`fd:${agentKind}:project:${projectStorageKey(cwd)}:lastModel`)
      : null) ??
    lastUsed.globalModel(agentKind),
  /** 与 lastModel 成对保存的友好名；选项未到时触发器先显示它，避免闪裸 id */
  modelName: (agentKind: AgentKind = "devin", cwd?: string | null) =>
    (projectStorageKey(cwd)
      ? localStorage.getItem(`fd:${agentKind}:project:${projectStorageKey(cwd)}:lastModelName`)
      : null) ??
    localStorage.getItem(`fd:${agentKind}:lastModelName`) ??
    (agentKind === "devin" ? localStorage.getItem("fd:lastModelName") ?? "" : ""),
  mode: (agentKind: AgentKind = "devin") =>
    localStorage.getItem(`fd:${agentKind}:lastMode`) ??
    (agentKind === "devin" ? localStorage.getItem("fd:lastMode") ?? "" : ""),
  reasoningEffort: (agentKind: AgentKind = "codex") =>
    localStorage.getItem(`fd:${agentKind}:lastReasoningEffort`) ?? "",
  setModel: (agentKind: AgentKind, v: string, cwd?: string | null, name?: string | null) => {
    const key = projectStorageKey(cwd);
    const prevModel = lastUsed.model(agentKind, cwd);
    const prevName = lastUsed.modelName(agentKind, cwd);
    if (key) localStorage.setItem(`fd:${agentKind}:project:${key}:lastModel`, v);
    localStorage.setItem(`fd:${agentKind}:lastModel`, v);
    const resolved =
      name?.trim() ||
      modelChoices(agentKind).find((c) => c.value === v)?.name ||
      (v && v === prevModel ? prevName : "");
    if (resolved) {
      if (key) localStorage.setItem(`fd:${agentKind}:project:${key}:lastModelName`, resolved);
      localStorage.setItem(`fd:${agentKind}:lastModelName`, resolved);
    }
  },
  setModelName: (agentKind: AgentKind, name: string, cwd?: string | null) => {
    const resolved = name.trim();
    if (!resolved) return;
    const key = projectStorageKey(cwd);
    if (key) localStorage.setItem(`fd:${agentKind}:project:${key}:lastModelName`, resolved);
    localStorage.setItem(`fd:${agentKind}:lastModelName`, resolved);
  },
  setMode: (agentKind: AgentKind, v: string) =>
    localStorage.setItem(`fd:${agentKind}:lastMode`, v),
  setReasoningEffort: (agentKind: AgentKind, v: string) =>
    localStorage.setItem(`fd:${agentKind}:lastReasoningEffort`, v),
};

export async function setThreadModel(model: string) {
  const id = state.currentId;
  if (!id) return;
  setState("model", model);
  await api.setThreadModel(id, model || null);
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
  lastUsed.setAgentKind(agentKind);
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

export async function sendPrompt(text: string, images: PromptImage[] = []) {
  const id = state.currentId;
  if (!id || (!text.trim() && images.length === 0)) return;
  const thread = state.threads.find((t) => t.id === id);
  if (thread?.employeeId && !thread.mindThread) {
    await api.registerLedgerItem(thread.employeeId, text, images);
    return;
  }
  // 继续发提示词时：若用户滚在中部，立刻跳到底（无过渡动画）
  bumpChatScrollToBottom();
  setState("proposedPlan", null);
  setState("running", id, true);
  try {
    await api.sendPrompt(id, text, images);
  } catch (e) {
    setState("running", id, false);
    throw e;
  }
}

/** ChatView 订阅：发送新提示词时强制滚到底 */
const [chatScrollToBottomTick, setChatScrollToBottomTick] = createSignal(0);
export function chatScrollToBottomSignal() {
  return chatScrollToBottomTick();
}
function bumpChatScrollToBottom() {
  setChatScrollToBottomTick((n) => n + 1);
}

/** 本地 worktree 会话：worktree 在后台创建，暂存首条提示词，就绪后（acp:worktree-ready）再自动发送 */
const pendingWorktreePrompts = new Map<string, { text: string; images: PromptImage[] }>();
export function stashWorktreePrompt(threadId: string, text: string, images: PromptImage[]) {
  if (!text.trim() && images.length === 0) return;
  pendingWorktreePrompts.set(threadId, { text, images });
}

/** 向指定会话发送（不依赖 currentId），用于 worktree 就绪后补发首条提示词 */
async function sendPromptTo(threadId: string, text: string, images: PromptImage[]) {
  if (!text.trim() && images.length === 0) return;
  if (state.currentId === threadId) bumpChatScrollToBottom();
  setState("running", threadId, true);
  try {
    await api.sendPrompt(threadId, text, images);
  } catch {
    setState("running", threadId, false);
  }
}

/** 编辑历史用户消息并从该处重新开始：界面立即更新，SDK restore/fork 在后端排队完成后再发送。 */
export async function editUserMessage(itemId: number, text: string, images: PromptImage[] = []) {
  const id = state.currentId;
  if (!id || (!text.trim() && images.length === 0)) return;
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
  bumpChatScrollToBottom();
  // 「停止 → 立刻编辑重发」的竞态：后端 cancel 可能尚未完成，truncate 会被
  // 「会话正在运行」校验拒绝，直接抛错会让这次编辑静默丢失（表现为第一次发送失败）。
  // 短暂重试等 cancel 落地，仍失败才抛出。
  for (let attempt = 0; ; attempt++) {
    try {
      await api.truncateThread(id, itemId, text, images);
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
  await api.cancelTurn(id, stopReason, deleteWork);
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
    // 后端偶发上报原生 id（bypass/agent…），归一成 Build/Plan 再进 UI
    const mode = normalizeUnifiedMode(op.mode) ?? op.mode;
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

export async function refreshSlashCommands(agentKind: AgentKind) {
  try {
    const commands = await api.getSlashCommands(agentKind);
    setState("slashCommands", agentKind, normalizeSlashCommands(commands));
  } catch (err) {
    console.warn(`拉取 ${agentKind} 斜杠命令失败`, err);
  }
}

export async function initStore() {
  if (initialized) return;
  initialized = true;

  // settings 是本地快照且不依赖事件监听：先并行拉取并尽快写进 store。
  // 后面的 listen 仍照常尽早注册；但不会再因为逐个 await listen 而拖慢设置页回显。
  const settingsReady = api.getSettings().then((settings) => {
    const preferredAgent = lastUsed.agentKind();
    const initialAgent = agentEnabled(settings, preferredAgent)
      ? preferredAgent
      : (ALL_AGENT_KINDS.find((kind) => agentEnabled(settings, kind)) ?? preferredAgent);
    setState({ settings, agentKind: initialAgent });
    // 后端启动时已把磁盘缓存灌入内存，立即读取，不等待其余事件监听和会话初始化。
    void ensureModelOptions(initialAgent);
    return { settings, initialAgent };
  }).catch((error: unknown) => ({ error }));

  await listen<{ threadId: string; op?: UpdateOp; ops?: UpdateOp[] }>("acp:update", (e) => {
    if (e.payload.threadId !== state.currentId) return;
    const ops = e.payload.ops ?? (e.payload.op ? [e.payload.op] : []);
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

  await listen<TurnEvent>("acp:turn", (e) => {
    setState("running", e.payload.threadId, e.payload.running);
    if (e.payload.running) preloadThreadSnapshot(e.payload.threadId);
    else if (
      e.payload.threadId === state.currentId &&
      state.items.some((item) => item.id < 0)
    ) {
      // 后台 restore 被取消或自动重发失败：清掉尚未落库的乐观消息。
      void openThread(e.payload.threadId);
    }
  });

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

  await listen<OptionsEvent>("acp:options", (e) => {
    const payload = e.payload;
    if ("options" in payload && "agentKind" in payload) {
      setState("modelOptions", payload.agentKind, payload.options);
      const model = lastUsed.model(payload.agentKind);
      const name = modelChoices(payload.agentKind).find((c) => c.value === model)?.name;
      if (model && name) lastUsed.setModelName(payload.agentKind, name);
    } else {
      setState("modelOptions", "devin", payload as ModelOptions);
    }
  });

  await listen<CommandsEvent>("acp:commands", (e) => {
    setState("slashCommands", e.payload.agentKind, normalizeSlashCommands(e.payload.commands));
  });

  // 后端可用性：启动后 / 保存设置后由后端并发检测（解析 PATH，不拉起进程），
  // 结果驱动 enabledAgentKinds() 只显示真正可用的后端
  await listen<{ availability: Record<string, boolean> }>("backends:availability", (e) => {
    setState("backendAvailability", reconcile(e.payload.availability ?? {}));
  });

  await listen<CliOperationProgress>("cli:operation-progress", (e) => {
    setState("cliOperationProgress", e.payload);
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
      const pair = firstWakeDoPairForThread(state.threads, id);
      if (pair?.wake.id === id && pair.doThread) {
        void openThread(id);
        return;
      }
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

  // 数字员工：后端在配置/收件箱变化、心跳执行前后都会 emit，前端据此刷新列表与状态
  await listen("employees:changed", () => {
    void refreshEmployees();
  });
  await listen("tasks:changed", () => {
    void refreshEmployeeTasks();
  });
  await listen("marks:changed", () => {
    void refreshMarks();
  });
  await listen("decisions:changed", () => {
    void refreshDecisions();
  });
  await listen("clues:changed", () => {
    void refreshClueGroups();
  });

  await listen<{ cardId: string }>("clues:mention-open", (e) => {
    openClueCard(e.payload.cardId);
  });

  // 系统通知点击：跳转到对应会话
  await listen<{ threadId: string }>("acp:notify-open", (e) => {
    void openThread(e.payload.threadId);
  });

  // 「员工上奏」系统通知点击：直达御书房批阅
  await listen("decisions:open", () => {
    setState("view", "workbench");
    closeThread();
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
      void refreshPeers();
      void refreshInbox();
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
  // 本地 worktree 后台创建就绪：切到 worktree 的 cwd 已由后端回写，这里补发暂存的首条提示词
  await listen<{ threadId: string }>("acp:worktree-ready", (e) => {
    const id = e.payload.threadId;
    const p = pendingWorktreePrompts.get(id);
    pendingWorktreePrompts.delete(id);
    void refreshThreads();
    if (state.currentId === id) {
      void api.getThread(id).then((t) => {
        if (state.currentId === id) setState("cwd", t.cwd);
      });
    }
    if (p) void sendPromptTo(id, p.text, p.images);
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

  // 后端可用性兜底拉取：启动检测很快，事件可能在前端监听就绪前已 emit 过，这里补一次
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
  if (isThemePref(settings.theme)) {
    if (settings.theme !== state.theme) setTheme(settings.theme);
  } else {
    persistThemeToBackend(state.theme);
  }

  // 团队/漫游：settings 一就绪就立刻刷新中转站状态/名单/收件箱/漫游目录。关键是排在下面较慢的
  // getStatus 之前——此前这几行排在 Promise.all(getStatus) 之后，被 getStatus 拖到数秒后才执行，
  // 表现为「启动后天线图标 / 自己的在线状态好久才出现」。现在紧跟 settings，第一时间点亮。
  void refreshRelayStatus();
  void refreshPeers().then(() => preloadPeerModels());
  void refreshInbox();
  void refreshRoamingFolders();

  // 数字员工：列表 + 任务活动 + 协作账本 + 御书房（纯本地读取，很快）
  void refreshEmployees();
  void refreshEmployeeTasks();
  void refreshMarks();
  void refreshDecisions();
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

  // 在线名单联网兜底：每 30 秒主动查一次服务端 roster。SSE presence 只在有人上下线时推送，
  // 且推送可能因通道瞬时拥塞被丢弃；这个定时器直接联网刷新，确保「别人看不到你 / 少一个人」
  // 最多 30 秒自愈，也能在 SSE 尚未连上的启动初期尽快显示名单。
  setInterval(() => {
    if (state.relay.enabled) void refreshPeers().then(() => preloadPeerModels());
  }, 30 * 1000);

  // 漫游模型后台静默「更新」：每 5 分钟强制刷新一轮在线队友的模型列表，
  // 让对端后端配置变化（启用/关闭某后端、换模型）能被及时同步，而无需用户手动重试。
  setInterval(() => preloadPeerModels(true), 5 * 60 * 1000);

  // 升级（手动/静默）重启后：自动恢复打开升级前正在查看的会话。
  // 普通启动时后端返回 null，不会恢复任何会话。
  void api
    .takeRestoreThread()
    .then((id) => {
      if (id && !state.currentId && state.threads.some((t) => t.id === id)) {
        void openThread(id);
      }
    })
    .catch(() => {
      // 无恢复标记或读取失败时静默停留在主页
    });

  // 监听用户交互并节流上报活动，供后端静默升级判断「一段时间没有操作」
  initActivityTracking();
}
