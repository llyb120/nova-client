export type AgentKind = "alkaid" | "devin" | "codex" | "codebuddy" | "claudecode" | "cursor" | "opencode";

export interface SlashCommand {
  name: string;
  description?: string;
  kind?: string;
  input?: string;
}

/** worktree 执行信息：会话在为某 git 仓库创建的独立 worktree（分支 + 工作目录）中运行 */
export interface Worktree {
  repo: string;
  path: string;
  branch: string;
}

/** 项目选择器里的一条最近项目；worktree 非空表示该目录是某次会话创建的 git worktree */
export interface ProjectEntry {
  path: string;
  worktree?: { repo: string; branch: string } | null;
}

export interface ClueMention {
  token: string;
  name: string;
}

export interface ClueAttachment {
  name: string;
  mimeType: string;
  /** 粘贴进来的附件以内嵌 base64 保存；本机文件也可保留 file:// URI。 */
  data?: string;
  uri?: string;
  size?: number;
}

export interface ClueCardVersion {
  id: string;
  title: string;
  content: string;
  authorName?: string;
  sourceThreadId?: string | null;
  mentions: ClueMention[];
  attachments: ClueAttachment[];
  createdAt: number;
}

export interface ClueComment {
  id: string;
  parentCommentId?: string | null;
  content: string;
  authorToken?: string | null;
  authorName?: string;
  mentions: ClueMention[];
  createdAt: number;
}

export interface ClueCard {
  id: string;
  currentVersionId: string;
  versions: ClueCardVersion[];
  comments: ClueComment[];
  createdAt: number;
  updatedAt: number;
}

/** 内部节点组；界面只把 cards 展示为共享前置线索的平行后续卡片。 */
export interface ClueNodeGroup {
  id: string;
  parentCardIds: string[];
  cards: ClueCard[];
  createdAt: number;
  updatedAt: number;
}

export interface ClueContextCard {
  cardId: string;
  versionId: string;
  title: string;
  content: string;
  parentCardIds: string[];
}

export interface ClueContextSnapshot {
  rootCardId: string;
  cards: ClueContextCard[];
  renderedContext: string;
  createdAt: number;
}

export interface CaptureClueResult {
  group: ClueNodeGroup;
  card: ClueCard;
}

/** 线索 AI 总结：高级分享模型产出的标题与内容 */
export interface ClueAiSummary {
  title: string;
  content: string;
}

export interface ThreadMeta {
  id: string;
  title: string;
  cwd: string;
  agentKind: AgentKind;
  model?: string | null;
  createdAt: number;
  updatedAt: number;
  running: boolean;
  /** 临时会话：程序关闭时自动删除 */
  ephemeral?: boolean;
  /** 用户星标：在所在项目内置顶 */
  starred: boolean;
  /** 漫游角色：host = 我替别人执行；guest = 在别人机器上执行、本机只接收 */
  roamingRole?: string | null;
  /** 漫游对端展示名 */
  roamingPeerName?: string | null;
  /** 额度租借提供方展示名 */
  quotaPeerName?: string | null;
  /** 非空：该会话在独立 git worktree 中执行 */
  worktree?: Worktree | null;
  /** 非空：该会话由数字员工后台产生，不在左侧历史列表展示，仅点开查看 */
  employeeId?: string | null;
  /** Mind 自整理会话：默认不进入数字员工左侧会话列表 */
  mindThread?: boolean;
  /** 会话树父节点：预检会话后的开发子会话会指向预检会话 */
  parentThreadId?: string | null;
  /** 当前会话在证据链中的线索位置 */
  activeClueCardId?: string | null;
}

/** 用户随 prompt 带上的附件。图片可带 base64，普通文件走 file:// resource_link。 */
export interface PromptImage {
  name: string;
  mimeType: string;
  data?: string;
  uri?: string;
  size?: number;
}

export interface UserItem {
  type: "user";
  id: number;
  text: string;
  ts: number;
  images?: PromptImage[];
}

export interface AssistantItem {
  type: "assistant";
  id: number;
  text: string;
  ts: number;
}

export interface ThoughtItem {
  type: "thought";
  id: number;
  text: string;
  ts: number;
}

export interface ToolItem {
  type: "tool";
  id: number;
  ts: number;
  toolCallId: string;
  title: string;
  kind: string;
  status: string;
  content: ToolContent[];
  locations: { path?: string; line?: number }[];
  rawInput?: unknown;
  rawOutput?: unknown;
}

export interface SystemItem {
  type: "system";
  id: number;
  text: string;
  level: string;
  ts: number;
}

/** 轮次结束标记：耗时 + token 用量 */
export interface TurnItem {
  type: "turn";
  id: number;
  ts: number;
  durationMs: number;
  totalTokens?: number | null;
  inputTokens?: number | null;
  outputTokens?: number | null;
  /** 后端支持时提供缓存读取/写入 token 明细 */
  cacheReadTokens?: number | null;
  cacheWriteTokens?: number | null;
  /** Auto 模式在本轮实际使用的模型及推理档位 */
  actualModel?: string | null;
  stopReason: string;
}

export type Item = UserItem | AssistantItem | ThoughtItem | ToolItem | SystemItem | TurnItem;

export type ToolContent =
  | { type: "content"; content: { type: string; text?: string; [k: string]: unknown } }
  | { type: "diff"; path: string; oldText?: string | null; newText: string }
  | { type: string; [k: string]: unknown };

export interface PlanEntry {
  content: string;
  priority?: string;
  status: string;
}

export interface Thread {
  id: string;
  title: string;
  cwd: string;
  agentKind: AgentKind;
  acpSessionId?: string | null;
  model?: string | null;
  mode?: string | null;
  reasoningEffort?: string | null;
  /** Auto 模式首次查询后缓存的实际模型 value */
  autoRoutedModel?: string | null;
  autoRouteSelection?: string | null;
  /** Auto 模式实际模型的展示名称 */
  autoRoutedLabel?: string | null;
  ephemeral?: boolean;
  starred?: boolean;
  roamingRole?: string | null;
  roamingPeer?: string | null;
  roamingPeerName?: string | null;
  roamingRemoteId?: string | null;
  /** 非空：本机会话使用该在线队友临时授权的额度 */
  quotaPeer?: string | null;
  quotaPeerName?: string | null;
  /** 非空：该会话在独立 git worktree 中执行（cwd 已指向该 worktree 工作目录） */
  worktree?: Worktree | null;
  /** 非空：该会话由数字员工后台产生 */
  employeeId?: string | null;
  /** Mind 自整理会话：默认不进入数字员工左侧会话列表 */
  mindThread?: boolean;
  /** 会话树父节点：预检会话后的开发子会话会指向预检会话 */
  parentThreadId?: string | null;
  activeClueCardId?: string | null;
  clueContext?: ClueContextSnapshot | null;
  createdAt: number;
  updatedAt: number;
  items: Item[];
  plan?: PlanEntry[] | null;
}

export interface ModelChoice {
  value: string;
  name: string;
  /** 选项描述；CodeBuddy 在此下发费用（积分倍率），如 "x0.79 credits" */
  description?: string;
  /** devin 在选项上附带的元数据，如 cognition.ai/supportsImages */
  _meta?: Record<string, unknown>;
}

export interface EffortChoice {
  value: string;
  name: string;
  description?: string;
}

export interface ModeChoice {
  id: string;
  name: string;
}

/** devin 返回的可用模型与会话模式（来自 session/new 的 configOptions） */
export interface ModelOptions {
  configOptions:
    | {
        id: string;
        name?: string;
        currentValue?: string;
        options?: { value: string; name: string; description?: string; _meta?: Record<string, unknown> }[];
      }[]
    | null;
  modes: {
    currentModeId?: string;
    availableModes?: { id: string; name: string }[];
  } | null;
}

export interface PermissionOption {
  optionId: string;
  name: string;
  kind: string;
}

export interface QuestionInfo {
  header: string;
  question: string;
  options: Array<{ label: string; description: string }>;
  multiple?: boolean;
  custom?: boolean;
}

export interface PermissionRequest {
  threadId: string;
  agentKind?: AgentKind;
  requestKey: string;
  toolCall: {
    title?: string;
    kind?: string;
    rawInput?: unknown;
    content?: ToolContent[];
    [k: string]: unknown;
  } | null;
  options: PermissionOption[];
  questions?: QuestionInfo[];
}

export interface Settings {
  devinPath: string;
  acpArgs: string;
  /** Devin 代理地址（空 = 不代理；下同：注入 HTTP(S)_PROXY 到该后端子进程） */
  devinProxy: string;
  /** CodeBuddy CLI 可执行文件 */
  codebuddyPath: string;
  codebuddyProxy: string;
  /** Claude Code CLI 可执行文件 */
  claudecodePath: string;
  claudecodeProxy: string;
  claudecodeSdkApiKey: string;
  cursorProxy: string;
  /** Cursor CLI 可执行文件 */
  cursorPath: string;
  cursorSdkApiKey: string;
  /** OpenCode CLI 可执行文件，默认 opencode 依赖 PATH */
  opencodePath: string;
  opencodeProxy: string;
  codexPath: string;
  codexProxy: string;
  /** Windows shell 启动 shim（保存后重启应用生效） */
  windowsShellShimEnabled: boolean;
  defaultMode: string;
  /** 自动生成会话标题所用后端（devin/codex/codebuddy/...，空 = devin） */
  titleModelAgent: string;
  /** 自动生成会话标题所用模型（须为 titleModelAgent 后端的模型；空 = 该后端会话默认模型） */
  titleModel: string;
  /** 高级分享「处理/总结」所用后端（devin/codex/codebuddy/...，空 = devin） */
  shareModelAgent: string;
  /** 高级分享「处理/总结」的默认模型（须为 shareModelAgent 后端的模型；空 = Devin 时 swe-1.6） */
  shareModel: string;
  /** 打开文件用的编辑器命令（cursor / code / zed 等） */
  editor: string;
  /** 界面皮肤（ink-dark / ink-light，空 = 未设置） */
  theme: string;
  /** 会话历史展示方式（按项目 / 按时间） */
  historyDisplayMode: "project" | "time";
  /** 团队/漫游中转服务地址（空 = 关闭团队/漫游） */
  relayServer: string;
  /** 团队/漫游身份 token（永久，用以区分每个人） */
  relayToken: string;
  /** 归属的群组（逗号/空格分隔，可多个）；只有相同群组的人能互相看到（空 = 默认群组） */
  relayGroups: string;
  /** 是否允许 server 端远程查看和控制本机会话（默认关闭） */
  remoteControlEnabled: boolean;
  /** 允许同团队成员借用的模型，键格式为 `<agentKind>:<modelId>` */
  quotaSharedModels: string[];
  /** 新建会话模型选择器中收藏的模型，键格式为 `<agentKind>:<modelId>` */
  modelFavorites: string[];
  /** 各模型后端是否启用（关闭后不在新建/切换会话的后端列表里出现） */
  devinEnabled: boolean;
  alkaidEnabled: boolean;
  codexEnabled: boolean;
  codebuddyEnabled: boolean;
  claudecodeEnabled: boolean;
  cursorEnabled: boolean;
  opencodeEnabled: boolean;
  codexIntegration: "sdk";
  codebuddyIntegration: "sdk";
  claudecodeIntegration: "sdk";
  cursorIntegration: "sdk";
  opencodeIntegration: "sdk";
  /** worktree 工作目录根（空 = 应用数据目录下 worktrees/） */
  worktreeDir: string;
  /** 是否自动清理长期未更新的会话 */
  sessionAutoCleanupEnabled: boolean;
  /** 自动清理会话的保留时长（小时） */
  sessionAutoCleanupHours: number;
  /** 语义检索开关（关 = 内置 BM25 关键词检索） */
  semanticEnabled: boolean;
  /** embedding 服务地址（OpenAI 兼容 /v1/embeddings；本地 Ollama 默认 http://localhost:11434） */
  embedEndpoint: string;
  /** embedding 模型名（如 bge-m3 / nomic-embed-text / text-embedding-3-small） */
  embedModel: string;
  /** embedding 服务 API key（本地服务通常留空） */
  embedApiKey: string;
}

export interface AgentInstructionTarget {
  agentKind: AgentKind;
  label: string;
  path: string;
  status: "inactive" | "pending" | "merged" | "managed" | "conflict" | "error";
  detail: string;
  enabled: boolean;
}

export interface GlobalAgentInstructions {
  content: string;
  path: string;
  targets: AgentInstructionTarget[];
}

/** 集中管理的 skill（~/.nova/skills） */
export interface SkillInfo {
  name: string;
  description: string;
  path: string;
}

export interface CliStatus {
  agentKind: AgentKind;
  cliName: string;
  installed: boolean;
  version: string;
  upgradeSupported: boolean;
  detail: string;
}

export interface CliOperationProgress {
  operationId: string;
  agentKind: AgentKind;
  action: "安装" | "升级";
  stage: "waiting" | "running" | "verifying" | "completed" | "failed" | "cancelled";
  percent: number;
  message: string;
}

/** worktree「基于分支」下拉的数据：当前分支 + 本地分支列表 */
export interface BranchList {
  current: string;
  branches: string[];
}

/** 一条已创建的 worktree 记录（设置里的 Worktree 面板手动管理用） */
export interface WorktreeRecord {
  id: string;
  repo: string;
  path: string;
  branch: string;
  threadId?: string | null;
  roaming: boolean;
  /** 分支是否由 Nova 新建；false = 直接检出的用户已有分支，移除时不提供「删分支」 */
  ownedBranch: boolean;
  createdAt: number;
}

/** 团队/漫游：一个允许漫游的目录 */
export interface RoamingFolder {
  path: string;
  name: string;
}

/** 漫游：对端（host）回传的可选模型/模式列表，按其已启用的后端归档。
 *  漫游在对方机器上执行，本机的模型对方不一定有，所以选择器用这份数据。 */
export interface PeerModels {
  /** 对端已启用的后端（按 devin → codex → codebuddy 顺序） */
  backends: AgentKind[];
  /** 各后端的模型/模式选项（缺失的后端为 undefined） */
  options: Partial<Record<AgentKind, ModelOptions | null>>;
  /** 对端明确开放额度租借的模型；仅注入新会话模型选择器。 */
  sharedOptions: Partial<Record<AgentKind, ModelOptions | null>>;
}

/** 团队/漫游：在线名单里的一个人 */
export interface Peer {
  token: string;
  name: string;
  online: boolean;
  /** 中转站返回的归属群组；旧服务端可能缺省。 */
  groups?: string[];
  folders: RoamingFolder[];
  lastSeen: number;
}

/** 中转站连接状态 */
export interface RelayStatus {
  enabled: boolean;
  connected: boolean;
}

/** 用户成就（由中转站按 token 前缀授予） */
export interface Achievement {
  id: string;
  title: string;
  description: string;
  /** 徽章样式键，如 founder / pioneer */
  icon: string;
  /** 服务端徽章图 URL（绝对或相对中转站） */
  imageUrl?: string;
  /** 先驱者等编号类成就的用户序号 */
  number?: string;
}

/** host 侧：收到的一条待确认漫游请求 */
export interface IncomingRoamRequest {
  reqId: string;
  from: string;
  fromName: string;
  folder: string;
  folderName: string;
  agentKind: AgentKind;
  /** 该目录在本机是否已存在；不存在时允许后会自动创建 */
  folderExists?: boolean;
  /** 发起人随请求带来的首条提示词（审批展示用，便于判断要执行什么） */
  prompt?: string | null;
  /** 对方要求在 worktree 中执行（host 侧确认框据此提示） */
  worktree?: boolean;
  worktreeBranch?: string | null;
  worktreeBase?: string | null;
  model?: string | null;
  mode?: string | null;
  /** 已有会话授权过期后的单轮续期审批。 */
  continuation?: boolean;
}

export interface QuotaRoamingProgress {
  operationId: string;
  stage: "requesting" | "installing" | "preparing" | "ready";
  message: string;
}

/** 收到的一条分享 */
export interface IncomingShare {
  id: string;
  from: string;
  fromName: string;
  title: string;
  agentKind: AgentKind;
  items: Item[];
  plan?: PlanEntry[] | null;
  activeClueCardId?: string | null;
  /** 漫游召回自动回传的快照（收件箱标注「召回」并自动弹出） */
  recall?: boolean;
  ts: number;
}

/** 撤销一个文件的改动（回滚到本轮编辑前） */
export interface RevertChange {
  path: string;
  /** null 表示文件原本不存在，撤销 = 删除 */
  oldText: string | null;
  /** 期望的当前内容，用于冲突检测 */
  newText: string;
}

export interface RevertResult {
  reverted: string[];
  conflicts: string[];
  errors: string[];
}

export interface Status {
  connected: boolean;
  agent: { name?: string; title?: string; version?: string } | null;
}

/** 模型费用信息（来自 windsurf 后端 GetCliModelConfigs，按 modelUid 索引） */
export interface ModelCost {
  /** 积分倍率；protobuf 省略零值，null 即 0（促销免费） */
  multiplier: number | null;
  provider: string;
  supportsImages: boolean;
  tier: string;
  pricing: string;
  /** token 单价（美元 / 1M tokens）；部分模型（促销/私有）没有 */
  prices: { input?: number; cached?: number; output?: number } | null;
}

/** 更新检查结果 */
export interface UpdateInfo {
  current: string;
  latest?: string;
  hasUpdate: boolean;
  /** 是否已静默下载好、可直接重启更新 */
  staged?: boolean;
  size?: number;
  downloadUrl?: string;
}

/** 更新下载/安装进度（update:progress 事件） */
export interface UpdateProgress {
  phase: "downloading" | "extracting" | "staged" | "applying" | "restarting";
  downloaded: number;
  total: number;
  version?: string;
}

/** devin 剩余额度（来自 windsurf 后端 GetUserStatus） */
export interface Quota {
  plan: string | null;
  dailyPercent: number;
  weeklyPercent: number;
  dailyResetAt: number | null;
  weeklyResetAt: number | null;
  flexCredits: number | null;
}

export type UpdateOp =
  | { t: "upsert"; item: Item }
  | { t: "remove"; itemId: number }
  | { t: "delta"; itemId: number; text: string }
  | { t: "plan"; plan: PlanEntry[] }
  | { t: "proposed_plan"; text: string | null }
  | { t: "mode"; mode: string };

export interface TurnEvent {
  threadId: string;
  running: boolean;
  stopReason?: string | null;
}

/** 讨论伙伴：员工开发中需要协议约定时自动联动的对象（本机同事或跨机队友） */
export interface Partner {
  /** local（本机的另一名数字员工）| remote（同组队友机器上的数字员工） */
  kind: "local" | "remote";
  /** 伙伴数字员工的名字 */
  name: string;
  /** remote 专用：队友在团队里的展示名（经中转站定向投递） */
  peer?: string | null;
}

/** 数字员工：常驻、按心跳像人一样先办批示/续做/领单/找活，遇大事上奏御书房候旨 */
export interface Employee {
  id: string;
  name: string;
  agentKind: AgentKind;
  model?: string | null;
  /** 巡查/心跳会话的后端（可与工作后端不同；留空 = 沿用工作后端） */
  heartbeatAgentKind?: AgentKind | null;
  /** 巡查/心跳专用模型（配合 heartbeatAgentKind；留空 = 沿用工作模型），让心跳用便宜模型 */
  heartbeatModel?: string | null;
  /** Mind 后端（可与工作/巡查不同；留空 = 沿用巡查后端，再沿用工作后端） */
  mindAgentKind?: AgentKind | null;
  /** Mind 专用模型（留空 = 沿用巡查模型，再沿用工作模型） */
  mindModel?: string | null;
  mode?: string | null;
  /** 是否允许员工按任务需要使用独立 git worktree */
  allowWorktree?: boolean;
  /** 岗位说明书（启动提示词）：员工是谁、负责什么、工作规范 */
  charter: string;
  /** @deprecated 兼容旧配置；运行时由普通会话项目或 Wake 动态确定 */
  cwd: string;
  /** 自动心跳开关：false = 不定时巡查/领活，只在御书房交办或朱批时行动；Mind 独立调度 */
  heartbeatEnabled?: boolean;
  /** 心跳周期（秒） */
  heartbeatSecs: number;
  /** 上班时间（可选）：设置后非上班时段自动休眠；null/缺省 = 7×24 在岗 */
  workHours?: WorkHours | null;
  /** 是否在岗（启用后才会被心跳唤醒） */
  enabled: boolean;
  /** 常驻职责：去哪里、找什么样的单子、怎么处理 */
  directive: string;
  /** 协作账本命名空间：同一 scope 的员工共享去重/互斥/接力；空则用私有 scope */
  markScope: string;
  /** 账本是否共享到中转站（同组队友跨机器去重/互斥/接力）；false = 仅本机 */
  sharedLedger: boolean;
  /** 讨论伙伴：开发中需要协议约定时自动联动的其他数字员工 */
  partners: Partner[];
  /** 上次心跳执行时间（ms） */
  lastHeartbeatMs: number;
  /** 旧记忆补种为 Mind 事件的进度时间 */
  mindEventSeededAt?: number;
  /** @deprecated 旧 Mind 配置字段，旧数据读取兼容 */
  learningAgentKind?: AgentKind | null;
  /** @deprecated 旧 Mind 配置字段，旧数据读取兼容 */
  learningModel?: string | null;
  /** @deprecated 旧反思时间，Mind 已改用事件游标 */
  lastReflectMs?: number;
  createdAt: number;
  updatedAt: number;
  /** @deprecated 旧字段，逻辑已不再使用（保留以兼容旧数据） */
  selfDirected?: boolean;
}

/** 上班时间：员工只在此时段自动干活，时段外自动休眠 */
export interface WorkHours {
  /** 上班时刻 "HH:MM"（24 小时制） */
  start: string;
  /** 下班时刻 "HH:MM"；start==end 视为全天；end<start 视为跨夜班 */
  end: string;
  /** 上班的星期（1=周一 … 7=周日）；留空 = 每天 */
  days: number[];
}

/** 奏折状态：候旨 / 已准奏待领旨 / 已领旨 / 留中不发 / 已驳回 / 单子完结自动撤回 / 完工汇报待处理 / 汇报已阅或已批阅 */
export type DecisionStatus =
  | "pending"
  | "resolved"
  | "consumed"
  | "shelved"
  | "rejected"
  | "withdrawn"
  | "report"
  | "read"
  | "reviewed";

/** 一道奏折：员工推进中拿不准的大事上奏「御书房」；底层已统一为 Notice 广播 */
export interface Decision {
  id: string;
  employeeId: string;
  employeeName: string;
  /** 关联的账本 scope / 单号 */
  scope: string;
  markKey: string;
  taskTitle: string;
  /** 员工上奏时所在的会话 id（可点开查看上下文） */
  threadId?: string | null;
  /** 背景/上下文：员工说明「这是什么、为什么要你定夺、你的选择会带来什么」 */
  brief?: string;
  /** 决策类型：approve|choose|input|priority|other */
  category?: string;
  question: string;
  /** 可选项（员工给出的候选答案，主管可直接选） */
  options: string[];
  blockerSignature?: string;
  proposedAction?: string;
  autoNote?: string;
  /** employee 普通奏折 | wake 开工预检奏折 | mind Mind 自愈或纠错上奏 */
  source?: string;
  status: DecisionStatus;
  /** 主管的答复 */
  answer?: string | null;
  createdAt: number;
  resolvedAt: number;
}

/** 统一协作 Notice（发送方声明处理后 ActionPlan） */
export type NoticeLabel = "decision" | "work" | "discuss" | "report" | "info";
export type NoticeStatus =
  | "pending"
  | "delivered"
  | "handled"
  | "rejected"
  | "withdrawn"
  | "expired";

export interface ActorRef {
  kind: string;
  id?: string | null;
  name?: string | null;
}

export interface NoticeAction {
  type: string;
  [key: string]: unknown;
}

export interface Notice {
  id: string;
  from: ActorRef;
  to: ActorRef;
  label: NoticeLabel | string;
  topic: {
    scope: string;
    markKey: string;
    title: string;
    threadId?: string | null;
  };
  body: {
    brief: string;
    question?: string | null;
    options: { id: string; label: string }[];
  };
  hold?: {
    scope: string;
    key: string;
    ownerEmployeeId: string;
  } | null;
  expect: {
    mode: string;
    onDelivered?: NoticeAction[];
    onHandled?: NoticeAction[];
    onChoice?: Record<string, NoticeAction[]>;
    onReject?: NoticeAction[];
  };
  dedupeKey?: string | null;
  status: NoticeStatus | string;
  response?: {
    by: ActorRef;
    choiceId?: string | null;
    text?: string | null;
    at: number;
  } | null;
  meta?: {
    category?: string;
    source?: string;
    proposedAction?: string;
    autoNote?: string;
  };
  createdAt: number;
  handledAt?: number;
}

/** 员工任务状态：待办 / 执行中 / 完成 / 受阻 */
export type EmployeeTaskStatus = "queued" | "working" | "done" | "blocked";

/** 派给员工的一项任务（收件箱条目） */
export interface EmployeeTask {
  id: string;
  employeeId: string;
  title: string;
  brief: string;
  status: EmployeeTaskStatus;
  /** 来源：user（用户派）| self（自主找活）| handoff:<同事名>（同事派） */
  origin: string;
  /** 执行该任务时创建的会话 id（可点开查看员工干活过程） */
  threadId?: string | null;
  /** 员工完成后的总结 / 受阻原因 */
  result?: string | null;
  createdAt: number;
  updatedAt: number;
}

/** 标记状态：待处理 / 处理中 / 完成 / 失败 */
export type MarkStatus = "open" | "claimed" | "done" | "failed";

/** 协作标记账本条目：只做占用门锁，过程留在会话 */
export interface Mark {
  /** 命名空间 / 看板 */
  scope: string;
  /** 该来源内的唯一标识（如需求单号） */
  key: string;
  title: string;
  status: MarkStatus;
  /** 认领的员工 id / 名字 */
  owner?: string | null;
  ownerName?: string | null;
  /** 旧字段：兼容历史备注，新模型不再把过程写到账本 */
  note: string;
  /** 当前处理会话。过程、判断和接力都在会话里 */
  threadId?: string | null;
  /** 主管交办时随单子带上的图片/文件附件 */
  images?: PromptImage[];
  /** 租约到期时间（ms），claimed 状态超过此刻可被接管；0 = 无租约 */
  leaseUntil: number;
  createdAt: number;
  updatedAt: number;
}

/** 一条记忆、知识、经验守则或工作留痕 */
export interface EmployeeJournalEntry {
  ts: number;
  taskId: string;
  taskTitle: string;
  summary: string;
  /** 置顶（长期注入、不自动截断）：长期知识，或已内化的守则 */
  pinned: boolean;
  /**
   * 条目类型（进化机制标签）：
   * "" 旧工作留痕；"memory" 员工记忆；"knowledge" 员工知识；
   * "knowledge:user|memory:user" 用户手动内容；"lesson" 守则（pinned=false 试行 / true 已内化）；
   * "lesson:challenged|lesson:retired" 受挑战/已退役守则；
   * "outcome:done|blocked|stalled|stopped" 经历留痕；"supervision" 主管批示留痕
   */
  kind?: string;
  /** 守则的实践验证次数（满阈值自动内化） */
  evidence?: number;
  source?: string;
  protected?: boolean;
  confidence?: number;
  lastUsedAt?: number;
  hitCount?: number;
  expiresAt?: number;
  supersededBy?: number | null;
  positiveEvidence?: number;
  negativeEvidence?: number;
  /** -1 = 用户点踩，0 = 未反馈，1 = 用户点赞 */
  userFeedback?: number;
  evidenceTasks?: string[];
}

/** @deprecated Dream 不再产生业务接力，只保留旧数据兼容 */
export interface MindHandoff {
  to: string;
  scope: string;
  key: string;
  title: string;
  brief: string;
}

/** @deprecated Dream 不再向工作提示注入 attention plan，只保留旧数据兼容 */
export interface AttentionPlan {
  version: number;
  focus: string;
  reasons: string[];
  risks: string[];
  rules: string[];
  deferred: string[];
  stopConditions: string[];
  handoff?: MindHandoff | null;
  summary: string;
  sourceEventSeq: number;
  updatedAt: number;
}

export interface MindSnapshot {
  employeeId: string;
  enabled: boolean;
  status: string;
  activeThreadId?: string | null;
  pendingEvents: number;
  snoozedUntil: number;
  nextRetryAt: number;
  consecutiveFailures: number;
  lastRunAt: number;
  lastSummary: string;
  lastError: string;
  /** @deprecated 旧 Dream 工作计划，新模型不再消费 */
  attentionPlan?: AttentionPlan | null;
  memoryEntries: number;
  protectedMemoryEntries: number;
  journalLimit: number;
  managedKnowledgeLimit: number;
}
