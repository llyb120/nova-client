import type { WorkflowDef } from "./workflow/types";

export type AgentKind = "alkaid" | "lyra" | "devin" | "codex" | "codebuddy" | "claudecode" | "cursor" | "opencode";

export interface SlashCommand {
  name: string;
  description?: string;
  kind?: string;
  input?: string;
}

export interface StageModelTarget {
  agentKind: AgentKind;
  model: string;
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

/** 线索 AI 总结：轻量级模型（失败时回退原模型）产出的标题与内容 */
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
  /** 猎户座训练会话：仅在猎户座历史展示 */
  experienceThread?: boolean;
  /** 会话树父节点：预检会话后的开发子会话会指向预检会话 */
  parentThreadId?: string | null;
  /** 普通 /stage 引用的源会话；用于导航显示 Stage 自己的会话名。 */
  stageSourceThreadId?: string | null;
  /** 当前会话在证据链中的线索位置 */
  activeClueCardId?: string | null;
}

/** 用户随 prompt 带上的附件。图片可带 base64，普通文件走 file:// resource_link。 */
export interface TimeMachinePrompt {
  id: number;
  text: string;
}

export interface TimeMachineCheckpoint {
  id: string;
  parentId?: string | null;
  sourceThreadId: string;
  title: string;
  createdAt: number;
  changedFiles: number;
  automatic: boolean;
  prompts: TimeMachinePrompt[];
  /** 用户对该分支结局的标记："success" / "failure"，未标记时缺省。 */
  outcome?: string | null;
}

/** 世界线「时光笔记」材料：分支树摘要文件路径 + skills 安装目录。 */
export interface TimeMachineTrainingDigest {
  digestPath: string;
  skillsDir: string;
}

export interface TimeMachineTimeline {
  id: string;
  rootThreadId: string;
  currentCheckpointId?: string | null;
  checkpoints: TimeMachineCheckpoint[];
}

export interface TimeMachineRestoreResult {
  threadId: string;
  timeline: TimeMachineTimeline;
}

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
  /** 最后一次模型请求实际携带的上下文；不同于本轮多次请求的累计输入。 */
  contextTokens?: number | null;
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
  /** 猎户座训练会话：仅在猎户座历史展示 */
  experienceThread?: boolean;
  /** 会话树父节点：预检会话后的开发子会话会指向预检会话 */
  parentThreadId?: string | null;
  /** Stage 会话动态引用的源会话。 */
  stageSourceThreadId?: string | null;
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
  /** 后端还没拿到真实列表时给的占位；拿到之前不要当作已加载缓存住。 */
  pending?: boolean;
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

export interface CursorModelContextRule {
  /** Case-insensitive substring matched against model id, for example `claude-4` or `gpt-5`. */
  prefix: string;
  /** Model context window in tokens. */
  contextWindow: number;
}

export type SessionShortcutAction =
  | "selectProject"
  | "selectModel"
  | "newSession"
  | "insertText"
  /** 内置 Esc 终止，仅运行时使用，不可在设置中配置。 */
  | "stopSession";

/** 会话快捷键：一键切到指定项目或模型、快速新会话、插入文本。 */
export interface SessionShortcut {
  id: string;
  /** 规范化按键，如 Ctrl+1 / Alt+P。 */
  keys: string;
  action: SessionShortcutAction;
  /** 本地项目路径、roam/quota 编码、agentKind:model，或 insertText 的插入内容；newSession 可为空。 */
  target: string;
}

/** 从当前会话进入新会话页时暂存的目录/模型，供 HomeView 继承。 */
export interface PendingNewSessionSeed {
  cwd: string;
  agentKind: AgentKind;
  model: string;
  mode: string;
  reasoningEffort: string;
  roam: { peerToken: string; folder: string } | null;
  quotaPeerToken: string | null;
  /** 快捷键触发时，聊天记录中选中的引用文本。 */
  quote: string;
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
  /** 兼容旧配置；Cursor 后端仅使用官方 SDK，不再依赖本机 CLI */
  cursorPath: string;
  cursorSdkApiKey: string;
  /** 是否通过 Cursor 全局 hook 阻止 Task/subagent；默认关闭。 */
  cursorDisableSubagents: boolean;
  /** Cursor 模型 id 包含匹配到上下文窗口的映射；最长匹配串优先。 */
  cursorModelContexts: CursorModelContextRule[];
  /** Vega 上下文机制：default = Reasonix，super = 改造前的超级上下文。 */
  vegaContextMode: "default" | "super";
  /** Cursor 上下文机制：default = Reasonix，super = 改造前的超级上下文。 */
  cursorContextMode: "default" | "super";
  /** OpenCode CLI 可执行文件，默认 opencode 依赖 PATH */
  opencodePath: string;
  opencodeProxy: string;
  codexPath: string;
  codexProxy: string;
  /** Vega provider 代理地址 */
  vegaProxy: string;
  /** Windows shell 启动 shim（保存后重启应用生效） */
  windowsShellShimEnabled: boolean;
  /** 是否允许 Lyra/Vega 自动切换当前项目和工具工作目录；默认开启。 */
  autoChangeProjectEnabled: boolean;
  /** 穿越世界线时间线时是否还原 checkpoint 中的工作目录文件 */
  checkpointEnabled: boolean;
  /** 代码上下文检索模式：无 / FastContext。 */
  contextRetrievalMode: "none" | "fast";
  defaultMode: string;
  /** 标题、快速总结、摘要和压缩等辅助任务所用后端 */
  lightweightModelAgent: string;
  /** 辅助任务所用轻量级模型；失败时回退到任务原模型 */
  lightweightModel: string;
  /** /stage、/stage2 等命令依次使用的模型；/stage 默认取第一项 */
  stageModels: StageModelTarget[];
  /** 打开文件用的编辑器命令（cursor / code / zed 等） */
  editor: string;
  /** 界面皮肤（ink-dark / ink-light，空 = 未设置） */
  theme: string;
  /** 会话历史展示方式（按项目 / 按时间） */
  historyDisplayMode: "project" | "time";
  /** 聊天视图渲染方式（dom / canvas / canvas_qwen；默认 canvas；canvas_qwen 为 GLM canvas 渲染，与默认 canvas 实现相互独立） */
  chatViewRender: "dom" | "canvas" | "canvas_qwen";
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
  /** 会话快捷键：按键一键切换项目或模型 */
  sessionShortcuts: SessionShortcut[];
  /** 各模型后端是否启用（关闭后不在新建/切换会话的后端列表里出现） */
  devinEnabled: boolean;
  vegaEnabled: boolean;
  lyraEnabled: boolean;
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
  /** 更新通道：正式版或预发布版。 */
  updateChannel: "release" | "pre-release";
  /** 是否自动清理长期未更新的会话 */
  sessionAutoCleanupEnabled: boolean;
  /** 自动清理会话的保留时长（小时） */
  sessionAutoCleanupHours: number;
  /** 独立经验库训练；经验不同于客观记忆和必须遵守的守则。开启后 Lyra fast_context 也会并行召回训练知识。 */
  experienceTrainingEnabled: boolean;
  experienceTrainingAgent: AgentKind;
  experienceTrainingModel: string;
  experienceTrainingIntervalMinutes: number;
  experienceEvolutionIntervalMinutes: number;
  experienceExperts: ExperienceExpertConfig[];
}

export interface ExperienceEntry {
  id: string;
  expertId: string;
  kind: "experience" | "memory" | "rule";
  /** 产生该知识的项目标识；按 Git 仓库根归一，同仓库目录和 worktree 相同。 */
  projectId: string;
  /** 产生该知识的 Git 仓库根路径，用于展示和审计。 */
  projectRoot: string;
  /** universal = 泛用；project = 仅当前项目。 */
  knowledgeScope: "universal" | "project";
  trigger: string;
  action: string;
  avoid: string;
  scope: string[];
  sourceThreadIds: string[];
  confidence: number;
  utility: number;
  positiveCount: number;
  negativeCount: number;
  /** 当前用户单票评价；模型反馈仍可累计到计数。 */
  userFeedback: -1 | 0 | 1;
  hitCount: number;
  updatedAt: number;
  status: string;
}

export interface ExperienceTrainingSession {
  id: string;
  createdAt: number;
  agentKind: string;
  model: string;
  expertId: string;
  sourceThreadIds: string[];
  conversation: string;
  output: string;
  status: string;
  error: string;
}

export interface ExperienceExpertRef {
  id: string;
  name: string;
}

export interface ExperienceOverview {
  /** 当前项目根；同一 Git 仓库的 worktree 会归一到主工作树。 */
  projectRoot: string;
  experiences: ExperienceEntry[];
  /** 全部已配置专家；即使尚未产出知识也会返回。 */
  experts: ExperienceExpertRef[];
  lastTrainAt: number;
  trainingCycles: number;
  evolutionGeneration: number;
  training: boolean;
}

export interface ExperienceExpertConfig {
  id: string;
  name: string;
  writeRate: number;
  valueLearningRate: number;
  forgetRate: number;
  mutationRate: number;
  migrationRate: number;
  abstractionLevel: number;
  noveltyPreference: number;
  negativeSensitivity: number;
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

/** 团队分享：队友分享过来的工作流，待接收进本地工作流库 */
export interface IncomingWorkflowShare {
  id: string;
  from: string;
  fromName: string;
  def: WorkflowDef;
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

/** 轮次进行中的实时累计用量（未落库；Turn 落定后由 TurnItem 接管展示） */
export interface LiveUsage {
  totalTokens?: number | null;
  inputTokens?: number | null;
  /** 最后一次模型请求实际携带的上下文；不同于当前轮次累计输入。 */
  contextTokens?: number | null;
  outputTokens?: number | null;
  cacheReadTokens?: number | null;
  cacheWriteTokens?: number | null;
}

export type UpdateOp =
  | { t: "upsert"; item: Item }
  | { t: "remove"; itemId: number }
  | { t: "delta"; itemId: number; text: string }
  | { t: "plan"; plan: PlanEntry[] }
  | { t: "proposed_plan"; text: string | null }
  | { t: "mode"; mode: string }
  | { t: "usage"; usage: LiveUsage };

export interface TurnEvent {
  threadId: string;
  running: boolean;
  stopReason?: string | null;
}
