import { invoke } from "@tauri-apps/api/core";
import type { ExclusiveChatIdentity } from "./components/ExclusiveChatMark";
import type {
  AgentKind,
  BranchList,
  CaptureClueResult,
  CliStatus,
  ClueAiSummary,
  ClueContextSnapshot,
  ClueNodeGroup,
  Decision,
  Employee,
  EmployeeJournalEntry,
  EmployeeTask,
  GlobalAgentInstructions,
  IncomingShare,
  Mark,
  MindSnapshot,
  ModelCost,
  ModelOptions,
  Notice,
  Partner,
  Peer,
  ProjectEntry,
  PromptImage,
  Quota,
  RelayStatus,
  RevertChange,
  RevertResult,
  Settings,
  SkillInfo,
  SlashCommand,
  Status,
  Thread,
  ThreadMeta,
  UpdateInfo,
  WorkHours,
  WorktreeRecord,
} from "./types";

export const api = {
  listThreads: () => invoke<ThreadMeta[]>("list_threads"),
  getThread: (threadId: string) => invoke<Thread>("get_thread", { threadId }),
  listProjects: () => invoke<ProjectEntry[]>("list_projects"),
  removeProject: (cwd: string) => invoke<void>("remove_project", { cwd }),
  prewarm: (cwd: string, agentKind: AgentKind, model?: string | null, mode?: string | null) =>
    invoke<void>("prewarm", { cwd, agentKind, model, mode }),
  scratchDir: () => invoke<string>("scratch_dir"),
  getQuota: () => invoke<Quota>("get_quota"),
  getModelCosts: () => invoke<Record<string, ModelCost>>("get_model_costs"),
  checkUpdate: () => invoke<UpdateInfo>("check_update"),
  downloadStagedUpdate: () =>
    invoke<{ ready: boolean; hasUpdate?: boolean; version?: string }>("download_staged_update"),
  applyStagedUpdate: () => invoke<void>("apply_staged_update"),
  reportActivity: (threadId: string | null) =>
    invoke<void>("report_activity", { threadId }),
  takeRestoreThread: () => invoke<string | null>("take_restore_thread"),
  signaturePending: () => invoke<ExclusiveChatIdentity | null>("signature_pending"),
  createThread: (
    cwd: string,
    agentKind: AgentKind,
    model: string | null,
    mode: string | null,
    reasoningEffort: string | null,
    ephemeral = false,
    worktree = false,
    worktreeBranch: string | null = null,
    worktreeBase: string | null = null,
    clueCardId: string | null = null,
  ) =>
    invoke<Thread>("create_thread", {
      cwd,
      agentKind,
      model,
      mode,
      reasoningEffort,
      ephemeral,
      worktree,
      worktreeBranch,
      worktreeBase,
      clueCardId,
    }),
  listClueGroups: () => invoke<ClueNodeGroup[]>("list_clue_groups"),
  getClueContext: (cardId: string) =>
    invoke<ClueContextSnapshot>("get_clue_context", { cardId }),
  captureClue: (
    threadId: string | null,
    title: string,
    content: string,
    placement: "update" | "parallel" | "new",
    targetCardId: string | null,
    mentionTokens: string[],
  ) =>
    invoke<CaptureClueResult>("capture_clue", {
      threadId,
      title,
      content,
      placement,
      targetCardId,
      mentionTokens,
    }),
  addClueComment: (
    cardId: string,
    content: string,
    parentCommentId: string | null,
    mentionTokens: string[],
  ) =>
    invoke<void>("add_clue_comment", {
      cardId,
      content,
      parentCommentId,
      mentionTokens,
    }),
  associateClues: (beforeCardId: string, afterCardId: string) =>
    invoke<ClueNodeGroup>("associate_clues", { beforeCardId, afterCardId }),
  disassociateClues: (beforeCardId: string, afterCardId: string) =>
    invoke<ClueNodeGroup>("disassociate_clues", { beforeCardId, afterCardId }),
  splitClue: (cardId: string) => invoke<ClueNodeGroup>("split_clue", { cardId }),
  stackClues: (cardIds: string[]) => invoke<ClueNodeGroup>("stack_clues", { cardIds }),
  deleteClue: (cardId: string) => invoke<void>("delete_clue", { cardId }),
  deleteThread: (threadId: string) => invoke<void>("delete_thread", { threadId }),
  deleteThreads: (threadIds: string[]) => invoke<number>("delete_threads", { threadIds }),
  deleteProjectThreads: (threadIds: string[]) =>
    invoke<number>("delete_project_threads", { threadIds }),
  openInExplorer: (path: string) => invoke<void>("open_in_explorer", { path }),
  openInTerminal: (path: string) => invoke<void>("open_in_terminal", { path }),
  openUrl: (url: string) => invoke<void>("open_url", { url }),
  openInEditor: (threadId: string, path: string, line?: number) =>
    invoke<void>("open_in_editor", { threadId, path, line }),
  openFileDefault: (threadId: string, path: string) =>
    invoke<void>("open_file_default", { threadId, path }),
  revertFileChanges: (threadId: string, changes: RevertChange[]) =>
    invoke<RevertResult>("revert_file_changes", { threadId, changes }),
  setThreadModel: (threadId: string, model: string | null) =>
    invoke<void>("set_thread_model", { threadId, model }),
  setThreadMode: (threadId: string, mode: string | null) =>
    invoke<void>("set_thread_mode", { threadId, mode }),
  setThreadReasoningEffort: (threadId: string, reasoningEffort: string | null) =>
    invoke<void>("set_thread_reasoning_effort", { threadId, reasoningEffort }),
  setThreadStarred: (threadId: string, starred: boolean) =>
    invoke<void>("set_thread_starred", { threadId, starred }),
  setThreadAgent: (
    threadId: string,
    agentKind: AgentKind,
    model: string | null,
    mode: string | null,
    reasoningEffort: string | null,
  ) =>
    invoke<void>("set_thread_agent", { threadId, agentKind, model, mode, reasoningEffort }),
  getModelOptions: (agentKind: AgentKind) =>
    invoke<ModelOptions | null>("get_model_options", { agentKind }),
  getSlashCommands: (agentKind: AgentKind) =>
    invoke<SlashCommand[]>("get_slash_commands", { agentKind }),
  renameThread: (threadId: string, title: string) =>
    invoke<void>("rename_thread", { threadId, title }),
  sendPrompt: (threadId: string, text: string, images: PromptImage[] = []) =>
    invoke<void>("send_prompt", { threadId, text, images }),
  truncateThread: (
    threadId: string,
    itemId: number,
    text?: string,
    images: PromptImage[] = [],
  ) => invoke<void>("truncate_thread", { threadId, itemId, text, images }),
  cancelTurn: (threadId: string, stopReason?: string | null, deleteWork = false) =>
    invoke<void>("cancel_turn", { threadId, stopReason: stopReason ?? null, deleteWork }),
  compactThread: (threadId: string) => invoke<void>("compact_thread", { threadId }),
  respondPermission: (requestKey: string, optionId: string) =>
    invoke<void>("respond_permission", { requestKey, optionId }),
  getSettings: () => invoke<Settings>("get_settings"),
  setSettings: (settings: Settings) => invoke<void>("set_settings", { settings }),
  getGlobalAgentInstructions: () =>
    invoke<GlobalAgentInstructions>("get_global_agent_instructions"),
  setGlobalAgentInstructions: (content: string) =>
    invoke<GlobalAgentInstructions>("set_global_agent_instructions", { content }),
  /** 后端可用性检测结果（agentKind → 是否可用）；空 map = 尚未检测完成 */
  getBackendAvailability: () =>
    invoke<Record<string, boolean>>("get_backend_availability"),
  getCliStatuses: (settings: Settings) =>
    invoke<CliStatus[]>("get_cli_statuses", { settings }),
  upgradeCli: (agentKind: AgentKind, settings: Settings, operationId: string) =>
    invoke<CliStatus>("upgrade_cli", { agentKind, settings, operationId }),
  cancelCliOperation: (operationId: string) =>
    invoke<boolean>("cancel_cli_operation", { operationId }),
  restartDevin: () => invoke<void>("restart_devin"),
  getStatus: () => invoke<Status>("get_status"),
  getLogs: () => invoke<string[]>("get_logs"),

  // 团队分享 / 漫游
  getRelayStatus: () => invoke<RelayStatus>("get_relay_status"),
  verifyRelay: (server: string, token: string, groups: string) =>
    invoke<number>("verify_relay", { server, token, groups }),
  getRelayPeers: () => invoke<{ peers: Peer[] } | Peer[]>("get_relay_peers"),
  // 联网兜底刷新：直接查服务端 roster（不依赖 SSE presence 推送），自愈丢失的在线名单
  refreshRelayPeers: () => invoke<{ peers: Peer[] } | Peer[]>("refresh_relay_peers"),
  getRelayInbox: () => invoke<IncomingShare[]>("get_relay_inbox"),
  shareThread: (threadId: string, to: string) =>
    invoke<void>("share_thread", { threadId, to }),
  advancedShare: (
    threadId: string,
    to: string,
    prompt: string,
    agent: string | null,
    model: string | null,
  ) => invoke<Thread>("advanced_share", { threadId, to, prompt, agent, model }),
  summarizeClue: (threadId: string) => invoke<ClueAiSummary>("summarize_clue", { threadId }),
  acceptShare: (id: string, cwd: string, ephemeral = false) =>
    invoke<string>("accept_share", { id, cwd, ephemeral }),
  declineShare: (id: string) => invoke<void>("decline_share", { id }),
  listRoamingFolders: () => invoke<string[]>("list_roaming_folders"),
  isFolderRoaming: (cwd: string) => invoke<boolean>("is_folder_roaming", { cwd }),
  setFolderRoaming: (cwd: string, allowed: boolean) =>
    invoke<boolean>("set_folder_roaming", { cwd, allowed }),
  setRoamingFolders: (folders: string[]) =>
    invoke<string[]>("set_roaming_folders", { folders }),
  createRoamingThread: (
    peerToken: string,
    peerName: string,
    folder: string,
    agentKind: AgentKind,
    model: string | null,
    mode: string | null,
    firstPrompt: string | null,
    clueCardId: string | null = null,
    worktree = false,
    worktreeBranch: string | null = null,
    worktreeBase: string | null = null,
  ) =>
    invoke<Thread>("create_roaming_thread", {
      peerToken,
      peerName,
      folder,
      agentKind,
      model,
      mode,
      firstPrompt,
      clueCardId,
      worktree,
      worktreeBranch,
      worktreeBase,
    }),
  respondRoamRequest: (
    reqId: string,
    accept: boolean,
    changes: {
      prompt: string;
      folder: string;
      model: string;
      mode: string;
      worktree: boolean;
      worktreeBranch: string;
      worktreeBase: string;
    },
  ) => invoke<void>("respond_roam_request", { reqId, accept, ...changes }),
  createQuotaThread: (
    peerToken: string,
    peerName: string,
    cwd: string,
    agentKind: AgentKind,
    model: string | null,
    mode: string | null,
    clueCardId: string | null,
    operationId: string,
  ) =>
    invoke<Thread>("create_quota_thread", {
      peerToken,
      peerName,
      cwd,
      agentKind,
      model,
      mode,
      clueCardId,
      operationId,
    }),
  cancelQuotaRoaming: (operationId: string) =>
    invoke<boolean>("cancel_quota_roaming", { operationId }),
  prepareQuotaLease: (peerToken: string, agentKind: AgentKind, model: string) =>
    invoke<void>("prepare_quota_lease", { peerToken, agentKind, model }),
  /** guest：召回漫游会话（host 自动把完整快照 Flow 回来，去收件箱选项目接收） */
  recallRoamingThread: (threadId: string) =>
    invoke<void>("recall_roaming_thread", { threadId }),
  requestPeerModels: (peerToken: string) =>
    invoke<void>("request_peer_models", { peerToken }),

  // worktree（独立工作目录执行）
  isGitRepo: (path: string) => invoke<boolean>("is_git_repo", { path }),
  listBranches: (path: string) => invoke<BranchList>("list_branches", { path }),
  requestPeerBranches: (peerToken: string, folder: string) =>
    invoke<void>("request_peer_branches", { peerToken, folder }),
  listWorktrees: () => invoke<WorktreeRecord[]>("list_worktrees"),
  removeWorktree: (id: string, deleteBranch: boolean) =>
    invoke<void>("remove_worktree", { id, deleteBranch }),
  /** 把 worktree 会话的分支合并到目标分支；返回 "merged" 或 "conflict"（冲突已交给该会话的 AI 解决） */
  mergeWorktreeThread: (threadId: string, targetBranch: string) =>
    invoke<"merged" | "conflict">("merge_worktree_thread", { threadId, targetBranch }),

  // Skills（集中管理 ~/.nova/skills，启动后端时软链接到各 agent 全局目录）
  listSkills: () => invoke<SkillInfo[]>("list_skills"),
  getSkillsDir: () => invoke<string>("get_skills_dir"),
  installSkill: (path: string) => invoke<SkillInfo>("install_skill", { path }),
  removeSkill: (name: string) => invoke<void>("remove_skill", { name }),
  syncSkills: () => invoke<void>("sync_skills"),

  // 数字员工
  listEmployees: () => invoke<Employee[]>("list_employees"),
  createEmployee: (p: {
    name: string;
    agentKind: AgentKind;
    model: string | null;
    heartbeatAgentKind: AgentKind;
    heartbeatModel: string | null;
    mindAgentKind: AgentKind;
    mindModel: string | null;
    mode: string | null;
    charter: string;
    cwd: string;
    heartbeatEnabled: boolean;
    heartbeatSecs: number;
    workHours: WorkHours | null;
    enabled: boolean;
    allowWorktree: boolean;
    directive: string;
    markScope: string;
    sharedLedger: boolean;
    partners: Partner[];
  }) => invoke<Employee>("create_employee", p),
  updateEmployee: (employee: Employee) => invoke<void>("update_employee", { employee }),
  deleteEmployee: (id: string) => invoke<void>("delete_employee", { id }),
  setEmployeeEnabled: (id: string, enabled: boolean) =>
    invoke<void>("set_employee_enabled", { id, enabled }),
  getEmployeeMind: (id: string) => invoke<MindSnapshot>("get_employee_mind", { id }),
  setEmployeeMindEnabled: (id: string, enabled: boolean) =>
    invoke<void>("set_employee_mind_enabled", { id, enabled }),
  resumeEmployeeMind: (id: string) => invoke<void>("resume_employee_mind", { id }),
  runEmployeeNow: (id: string) => invoke<void>("run_employee_now", { id }),
  listEmployeeTasks: () => invoke<EmployeeTask[]>("list_employee_tasks"),
  deleteTask: (id: string) => invoke<void>("delete_task", { id }),
  /** 交办：把一个具体单子登记到该员工账本的「待处理」（无需标题，直接填内容），员工唤起后自行侦察认领 */
  registerLedgerItem: (employeeId: string, content: string, images: PromptImage[] = []) =>
    invoke<void>("register_ledger_item", { employeeId, title: "", brief: content, images }),
  /** 普通会话临时交给数字员工：界面只记录用户原文，内部走 Wake → Do。 */
  delegateEmployeeWork: (
    threadId: string,
    employeeId: string,
    content: string,
    images: PromptImage[] = [],
  ) => invoke<void>("delegate_employee_work", { threadId, employeeId, content, images }),

  // 御书房（员工上奏、主管朱批准奏；dismiss = 留中不发）—— 底层 Notice 广播
  listDecisions: () => invoke<Decision[]>("list_decisions"),
  listNotices: () => invoke<Notice[]>("list_notices"),
  resolveDecision: (id: string, answer: string) =>
    invoke<void>("resolve_decision", { id, answer }),
  rejectDecision: (id: string, answer: string) =>
    invoke<void>("reject_decision", { id, answer }),
  dismissDecision: (id: string) => invoke<void>("dismiss_decision", { id }),
  /** 物理删除奏折/汇报记录（含已批阅） */
  deleteDecision: (id: string) => invoke<void>("delete_decision", { id }),
  /** 完工汇报点已读归档（不批示、不唤醒员工） */
  readReport: (id: string) => invoke<void>("read_report", { id }),
  /** 完工汇报批阅归档；批示只供 Dream 学习，不重新唤醒员工 */
  reviewReport: (id: string, answer: string) =>
    invoke<void>("review_report", { id, answer }),
  getEmployeeMemory: (id: string) =>
    invoke<EmployeeJournalEntry[]>("get_employee_memory", { id }),
  addEmployeeMemory: (id: string, title: string, summary: string, pinned: boolean) =>
    invoke<EmployeeJournalEntry>("add_employee_memory", { id, title, summary, pinned }),
  updateEmployeeMemory: (id: string, ts: number, summary: string) =>
    invoke<void>("update_employee_memory", { id, ts, summary }),
  deleteEmployeeMemory: (id: string, ts: number) =>
    invoke<void>("delete_employee_memory", { id, ts }),
  setEmployeeMemoryPinned: (id: string, ts: number, pinned: boolean) =>
    invoke<void>("set_employee_memory_pinned", { id, ts, pinned }),
  setEmployeeMemoryFeedback: (id: string, ts: number, feedback: number) =>
    invoke<void>("set_employee_memory_feedback", { id, ts, feedback }),

  // 协作标记账本
  listMarks: (scope?: string | null) => invoke<Mark[]>("list_marks", { scope: scope ?? null }),
  releaseMark: (scope: string, key: string) => invoke<void>("release_mark", { scope, key }),
  resetMark: (scope: string, key: string) => invoke<void>("reset_mark", { scope, key }),
  setMark: (scope: string, key: string, status: string, note?: string | null) =>
    invoke<void>("set_mark", { scope, key, status, note: note ?? null }),

  // 共享账本（跨机器，经中转站中心仲裁）
  listSharedMarks: (scope: string) => invoke<Mark[]>("list_shared_marks", { scope }),
  releaseSharedMark: (scope: string, key: string) =>
    invoke<void>("release_shared_mark", { scope, key }),
  resetSharedMark: (scope: string, key: string, threadId?: string | null) =>
    invoke<void>("reset_shared_mark", { scope, key, threadId: threadId ?? null }),
  setSharedMark: (scope: string, key: string, status: string, note?: string | null) =>
    invoke<void>("set_shared_mark", { scope, key, status, note: note ?? null }),

  // 语义检索（外置 embedding 引擎）
  semanticStatus: () => invoke<{ ok: boolean; dim: number }>("semantic_status"),
  semanticPull: (model?: string | null) => invoke<void>("semantic_pull", { model: model ?? null }),
  semanticRebuild: (employeeId?: string | null) =>
    invoke<void>("semantic_rebuild", { employeeId: employeeId ?? null }),
};
