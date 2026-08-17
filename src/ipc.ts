import { invoke } from "@tauri-apps/api/core";
import type { ExclusiveChatIdentity } from "./components/ExclusiveChatMark";
import type {
  Achievement,
  AgentKind,
  BranchList,
  CaptureClueResult,
  CliStatus,
  ClueAiSummary,
  ClueAttachment,
  ClueContextSnapshot,
  ClueNodeGroup,
  ExperienceOverview,
  GlobalAgentInstructions,
  IncomingShare,
  IncomingWorkflowShare,
  ModelCost,
  ModelOptions,
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
  TimeMachinePrompt,
  TimeMachineRestoreResult,
  TimeMachineTimeline,
  TimeMachineTrainingDigest,
  UpdateInfo,
  WorktreeRecord,
} from "./types";
import type { WorkflowDef } from "./workflow/types";

function fileUriPath(uri: string) {
  const path = decodeURI(uri.replace(/^file:\/\//, ""));
  return /^\/[A-Za-z]:\//.test(path) ? path.slice(1) : path;
}

export const api = {
  listThreads: () => invoke<ThreadMeta[]>("list_threads"),
  getThread: (threadId: string) => invoke<Thread>("get_thread", { threadId }),
  createTimeMachineCheckpoint: (threadId: string) =>
    invoke<TimeMachineTimeline>("create_time_machine_checkpoint", { threadId }),
  getTimeMachineTimeline: (threadId: string) =>
    invoke<TimeMachineTimeline | null>("get_time_machine_timeline", { threadId }),
  getTimeMachineTrainingDigest: (threadId: string) =>
    invoke<TimeMachineTrainingDigest>("get_time_machine_training_digest", { threadId }),
  setTimeMachineCheckpointOutcome: (
    threadId: string,
    checkpointId: string,
    outcome: string | null,
  ) =>
    invoke<TimeMachineTimeline>("set_time_machine_checkpoint_outcome", {
      threadId,
      checkpointId,
      outcome,
    }),
  getTimeMachineCheckpointPreview: (threadId: string, checkpointId: string) =>
    invoke<Thread>("get_time_machine_checkpoint_preview", { threadId, checkpointId }),
  restoreTimeMachineCheckpoint: (threadId: string, checkpointId: string) =>
    invoke<TimeMachineRestoreResult>("restore_time_machine_checkpoint", { threadId, checkpointId }),
  deleteTimeMachineContext: (threadId: string, prompts: TimeMachinePrompt[]) =>
    invoke<TimeMachineRestoreResult>("delete_time_machine_context", { threadId, prompts }),
  listProjects: () => invoke<ProjectEntry[]>("list_projects"),
  removeProject: (cwd: string) => invoke<void>("remove_project", { cwd }),
  prewarm: (cwd: string, agentKind: AgentKind, model?: string | null, mode?: string | null) =>
    invoke<void>("prewarm", { cwd, agentKind, model, mode }),
  /** 工作流提示词模式连线判断（轻量模型）；返回选中的连线 id，无法判断返回空串 */
  judgeWorkflowRoute: (conclusion: string, options: { id: string; label: string }[]) =>
    invoke<string>("judge_workflow_route", { conclusion, options }),
  scratchDir: () => invoke<string>("scratch_dir"),
  directoryExists: (path: string) => invoke<boolean>("directory_exists", { path }),
  clipboardFilePaths: () => invoke<string[]>("clipboard_file_paths"),
  getQuota: () => invoke<Quota>("get_quota"),
  getModelCosts: () => invoke<Record<string, ModelCost>>("get_model_costs"),
  checkUpdate: () => invoke<UpdateInfo>("check_update"),
  downloadStagedUpdate: () =>
    invoke<{ ready: boolean; hasUpdate?: boolean; version?: string }>("download_staged_update"),
  applyStagedUpdate: () => invoke<void>("apply_staged_update"),
  reportActivity: (threadId: string | null) =>
    invoke<void>("report_activity", { threadId }),
  showMainWindow: () => invoke<void>("show_main_window"),
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
    parentThreadId: string | null = null,
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
      parentThreadId,
    }),
  createStageThread: (sourceThreadId: string, stageIndex = 0, inheritSourceModel = false) =>
    invoke<Thread>("create_stage_thread", { sourceThreadId, stageIndex, inheritSourceModel }),
  listClueGroups: (space: "personal" | "team" = "personal") =>
    invoke<ClueNodeGroup[]>("list_clue_groups", { space }),
  getClueContext: (cardId: string) =>
    invoke<ClueContextSnapshot>("get_clue_context", { cardId }),
  captureClue: (
    threadId: string | null,
    title: string,
    content: string,
    placement: "update" | "parallel" | "new",
    targetCardId: string | null,
    mentionTokens: string[],
    attachments: ClueAttachment[],
    space: "personal" | "team" = "personal",
  ) =>
    invoke<CaptureClueResult>("capture_clue", {
      threadId,
      title,
      content,
      placement,
      targetCardId,
      mentionTokens,
      attachments,
      space,
    }),
  addClueComment: (
    cardId: string,
    content: string,
    parentCommentId: string | null,
    mentionTokens: string[],
    space: "personal" | "team" = "personal",
  ) =>
    invoke<void>("add_clue_comment", {
      cardId,
      content,
      parentCommentId,
      mentionTokens,
      space,
    }),
  associateClues: (beforeCardId: string, afterCardId: string, space: "personal" | "team") =>
    invoke<ClueNodeGroup>("associate_clues", { beforeCardId, afterCardId, space }),
  disassociateClues: (beforeCardId: string, afterCardId: string, space: "personal" | "team") =>
    invoke<ClueNodeGroup>("disassociate_clues", { beforeCardId, afterCardId, space }),
  splitClue: (cardId: string, space: "personal" | "team") =>
    invoke<ClueNodeGroup>("split_clue", { cardId, space }),
  stackClues: (cardIds: string[], space: "personal" | "team") =>
    invoke<ClueNodeGroup>("stack_clues", { cardIds, space }),
  deleteClue: (cardId: string, space: "personal" | "team") =>
    invoke<void>("delete_clue", { cardId, space }),
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
  openClueAttachment: (attachment: ClueAttachment) =>
    invoke<void>("open_clue_attachment", {
      name: attachment.name,
      data: attachment.data ?? null,
      path: attachment.uri ? fileUriPath(attachment.uri) : null,
    }),
  /** 读取本地文件为内嵌（base64）线索附件。 */
  readLocalAttachment: (path: string) =>
    invoke<ClueAttachment>("read_local_attachment", { path }),
  /** 把线索附件保存（下载）到指定路径。 */
  saveClueAttachment: (attachment: ClueAttachment, target: string) =>
    invoke<void>("save_clue_attachment", {
      data: attachment.data ?? null,
      path: attachment.uri ? fileUriPath(attachment.uri) : null,
      target,
    }),
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
  /** 设置页手动刷新 Vega 本地配置：重读 config.jsonc 并后台重拉模型列表。 */
  refreshAlkaidConfig: () => invoke<void>("refresh_alkaid_config"),
  getSlashCommands: (agentKind: AgentKind) =>
    invoke<SlashCommand[]>("get_slash_commands", { agentKind }),
  renameThread: (threadId: string, title: string) =>
    invoke<void>("rename_thread", { threadId, title }),
  /** 让模型按节点任务生成会话标题（仅用于工作流阶段会话，前缀由后端保留）。 */
  generateThreadTitle: (threadId: string, prompt: string) =>
    invoke<void>("generate_thread_title", { threadId, prompt }),
  notifyFireDone: (threadId: string, success: boolean) =>
    invoke<void>("notify_fire_done", { threadId, success }),
  notifyWorkflowDone: (threadId: string, success: boolean) =>
    invoke<void>("notify_workflow_done", { threadId, success }),
  /** 向后端追加一条系统提示（工作流预览等由前端渲染的结构化内容）。 */
  pushSystemItem: (threadId: string, text: string, level = "info") =>
    invoke<void>("push_system_item", { threadId, text, level }),
  setPromptQueuePending: (threadId: string, pending: boolean) =>
    invoke<void>("set_prompt_queue_pending", { threadId, pending }),
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
  refreshEnvironmentVariables: () => invoke<number>("refresh_environment_variables"),
  getGlobalAgentInstructions: () =>
    invoke<GlobalAgentInstructions>("get_global_agent_instructions"),
  setGlobalAgentInstructions: (content: string, enabledAgentKinds: AgentKind[]) =>
    invoke<GlobalAgentInstructions>("set_global_agent_instructions", { content, enabledAgentKinds }),
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
  listAchievements: () => invoke<Achievement[]>("list_achievements"),
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
  // 工作流分享：把工作流定义定向发给队友 / 接收队友分享的工作流
  /** 共享/分享工作流：to 为空串 = 共享给全组在线队友；返回送达的队友数。 */
  shareWorkflow: (workflow: WorkflowDef, to: string) =>
    invoke<number>("share_workflow", { workflow, to }),
  revokeWorkflow: (workflowId: string) =>
    invoke<number>("revoke_workflow", { workflowId }),
  getRelayWorkflowInbox: () => invoke<IncomingWorkflowShare[]>("get_relay_workflow_inbox"),
  acceptRelayWorkflowShare: (id: string) =>
    invoke<WorkflowDef>("accept_relay_workflow_share", { id }),
  declineRelayWorkflowShare: (id: string) =>
    invoke<void>("decline_relay_workflow_share", { id }),
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

  // 猎户座（经验训练会话与普通会话隔离）
  listExperiences: (cwd: string) => invoke<ExperienceOverview>("list_experiences", { cwd }),
  feedbackExperience: (cwd: string, experienceId: string, reward: number) =>
    invoke<{ updated: number; reward: number }>("feedback_experience", { cwd, experienceId, reward }),
  deleteExperience: (cwd: string, experienceId: string) =>
    invoke<{ deleted: number }>("delete_experience", { cwd, experienceId }),
  evolveExperiences: (cwd: string) => invoke<{ generation: number; created: number; reviewed: number; rejected: number; crossed: number; mutated: number; migrated: number; quarantined: number }>("evolve_experiences", { cwd }),
  trainExperience: (cwd: string) => invoke<{ trained: boolean; learned?: number; reason?: string; sessionId?: string }>("train_experience", { cwd }),

  // Skills（集中管理 ~/.nova/skills，启动后端时软链接到各 agent 全局目录）
  listSkills: () => invoke<SkillInfo[]>("list_skills"),
  getSkillsDir: () => invoke<string>("get_skills_dir"),
  installSkill: (path: string) => invoke<SkillInfo>("install_skill", { path }),
  removeSkill: (name: string) => invoke<void>("remove_skill", { name }),
  syncSkills: () => invoke<void>("sync_skills"),

};
