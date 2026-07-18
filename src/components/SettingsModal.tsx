import { getVersion } from "@tauri-apps/api/app";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { confirm, message, open as openDialog } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { api } from "../ipc";
import {
  checkAndStageUpdate,
  deleteThreads,
  enabledAgentKinds,
  ensureModelOptions,
  modelChoices,
  normalizeUnifiedMode,
  refreshRelayStatus,
  setState,
  setTheme,
  state,
} from "../store";
import { agentLabel, isScratch, setFileDropBlocked } from "../utils";
import { ModelPicker } from "./ConfigSelects";
import { IconX } from "./icons";
import type {
  AgentInstructionTarget,
  AgentKind,
  CliStatus,
  Settings,
  SkillInfo,
  WorktreeRecord,
} from "../types";

function threadGroupName(cwd: string): string {
  if (isScratch(cwd)) return "临时会话";
  return cwd.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || cwd;
}

function projectPathKey(path: string): string {
  return path.replace(/\\/g, "/").toLowerCase();
}

const DEFAULT_RELAY_SERVER = "";
const RELAY_SERVER_PLACEHOLDER = "http://127.0.0.1:8320";

/** 统一会话模式（与 store.UNIFIED_MODES 一致，带说明文案） */
const UNIFIED_MODE_OPTIONS = [
  { id: "build", name: "Build（放开全部权限，自动执行）" },
  { id: "plan", name: "Plan（只规划不执行）" },
];

/** 后端代理输入框（每个后端进程可单独走代理） */
function ProxyField(props: { value: string; onInput: (v: string) => void }) {
  return (
    <label class="backend-proxy-field">
      <span class="field-label">代理</span>
      <input
        class="field-input"
        value={props.value}
        onInput={(e) => props.onInput(e.currentTarget.value)}
        placeholder="http://127.0.0.1:10808"
      />
    </label>
  );
}

function CliManager(props: {
  status?: CliStatus;
  loading: boolean;
  busy: boolean;
  upgrading: boolean;
  message?: string;
  onUpgrade: () => void;
}) {
  return (
    <div class="cli-manager">
      <div class="cli-manager-info">
        <span class="field-label">对应 CLI</span>
        <span class="cli-manager-name">{props.status?.cliName ?? "检测中…"}</span>
        <span
          classList={{
            "cli-manager-version": true,
            missing: props.status?.installed === false,
          }}
          title={props.status?.detail || ""}
        >
          {props.loading ? "正在读取版本…" : (props.status?.version ?? "尚未检测")}
        </span>
      </div>
      <button
        type="button"
        class="btn secondary cli-upgrade-btn"
        disabled={props.loading || props.busy || props.status?.upgradeSupported !== true}
        onClick={props.onUpgrade}
      >
        {props.upgrading
          ? (props.status?.installed === false ? "安装中…" : "升级中…")
          : (props.status?.installed === false ? "一键安装" : "一键升级")}
      </button>
      <Show when={props.message}>
        <span class={`cli-manager-message ${props.message?.includes("失败") ? "bad" : "ok"}`}>
          {props.message}
        </span>
      </Show>
      <Show when={!props.message && !props.loading && props.status?.upgradeSupported === false}>
        <span class="cli-manager-message bad" title={props.status?.detail}>
          {props.status?.detail}
        </span>
      </Show>
    </div>
  );
}

type SettingsTab =
  | "general"
  | "advanced"
  | "backends"
  | "instructions"
  | "appearance"
  | "team"
  | "memory"
  | "worktree"
  | "skills"
  | "about";

const TABS: { id: SettingsTab; name: string }[] = [
  { id: "general", name: "通用" },
  { id: "advanced", name: "高级" },
  { id: "backends", name: "模型后端" },
  { id: "instructions", name: "Agent 配置" },
  { id: "appearance", name: "外观" },
  { id: "team", name: "团队" },
  { id: "memory", name: "记忆检索" },
  { id: "worktree", name: "Worktree" },
  { id: "skills", name: "Skills" },
  { id: "about", name: "关于" },
];

export function SettingsModal(props: { onClose: () => void }) {
  const s = state.settings;
  const [tab, setTab] = createSignal<SettingsTab>("general");
  const [devinPath, setDevinPath] = createSignal(s?.devinPath ?? "devin");
  const [acpArgs, setAcpArgs] = createSignal(s?.acpArgs ?? "acp");
  const [codebuddyPath, setCodebuddyPath] = createSignal(s?.codebuddyPath ?? "codebuddy");
  const [claudecodePath, setClaudecodePath] = createSignal(s?.claudecodePath ?? "claude");
  const [cursorPath, setCursorPath] = createSignal(s?.cursorPath ?? "cursor-agent");
  const [opencodePath, setOpencodePath] = createSignal(s?.opencodePath ?? "opencode");
  const [codexPath, setCodexPath] = createSignal(s?.codexPath ?? "codex");
  const [codexProxy, setCodexProxy] = createSignal(s?.codexProxy ?? "");
  const [windowsShellShimEnabled, setWindowsShellShimEnabled] = createSignal(
    s?.windowsShellShimEnabled ?? false,
  );
  const [devinProxy, setDevinProxy] = createSignal(s?.devinProxy ?? "");
  const [codebuddyProxy, setCodebuddyProxy] = createSignal(s?.codebuddyProxy ?? "");
  const [claudecodeProxy, setClaudecodeProxy] = createSignal(s?.claudecodeProxy ?? "");
  const [claudecodeSdkApiKey, setClaudecodeSdkApiKey] = createSignal(s?.claudecodeSdkApiKey ?? "");
  const [cursorProxy, setCursorProxy] = createSignal(s?.cursorProxy ?? "");
  const [cursorSdkApiKey, setCursorSdkApiKey] = createSignal(s?.cursorSdkApiKey ?? "");
  const [opencodeProxy, setOpencodeProxy] = createSignal(s?.opencodeProxy ?? "");
  const [devinEnabled, setDevinEnabled] = createSignal(s?.devinEnabled !== false);
  const [codexEnabled, setCodexEnabled] = createSignal(s?.codexEnabled !== false);
  const [codebuddyEnabled, setCodebuddyEnabled] = createSignal(s?.codebuddyEnabled !== false);
  const [claudecodeEnabled, setClaudecodeEnabled] = createSignal(s?.claudecodeEnabled !== false);
  const [cursorEnabled, setCursorEnabled] = createSignal(s?.cursorEnabled !== false);
  const [opencodeEnabled, setOpencodeEnabled] = createSignal(s?.opencodeEnabled !== false);
  // 旧值（bypass 等）归一到统一模式 build/plan
  const [defaultMode, setDefaultMode] = createSignal(
    normalizeUnifiedMode(s?.defaultMode) ?? s?.defaultMode ?? "",
  );
  const [titleAgent, setTitleAgent] = createSignal<AgentKind>(
    (s?.titleModelAgent as AgentKind) || "devin",
  );
  const [titleModel, setTitleModel] = createSignal(s?.titleModel ?? "swe-1-6");
  const [shareAgent, setShareAgent] = createSignal<AgentKind>(
    (s?.shareModelAgent as AgentKind) || "devin",
  );
  const [shareModel, setShareModel] = createSignal(s?.shareModel ?? "swe-1.6");
  const [editor, setEditor] = createSignal(s?.editor ?? "code");
  const [sessionAutoCleanupEnabled, setSessionAutoCleanupEnabled] = createSignal(
    s?.sessionAutoCleanupEnabled ?? false,
  );
  const [sessionAutoCleanupHours, setSessionAutoCleanupHours] = createSignal(
    s?.sessionAutoCleanupHours ?? 24 * 30,
  );
  const [historyDisplayMode, setHistoryDisplayMode] = createSignal<"project" | "time">(
    s?.historyDisplayMode === "time" ? "time" : "project",
  );
  // server 留空回退默认地址；这里也预填，避免误存成空导致团队/漫游被静默关闭
  const [relayServer, setRelayServer] = createSignal(s?.relayServer || DEFAULT_RELAY_SERVER);
  const [relayToken, setRelayToken] = createSignal(s?.relayToken ?? "");
  const [relayGroups, setRelayGroups] = createSignal(s?.relayGroups ?? "");
  const [remoteControlEnabled, setRemoteControlEnabled] = createSignal(
    s?.remoteControlEnabled ?? false,
  );
  const [quotaSharedModels, setQuotaSharedModels] = createSignal<string[]>(s?.quotaSharedModels ?? []);
  const [roamingFolders, setRoamingFolders] = createSignal<string[]>(state.roamingFolders);
  const [roamingFoldersLoading, setRoamingFoldersLoading] = createSignal(false);
  const [globalInstructions, setGlobalInstructions] = createSignal("");
  const [globalInstructionsPath, setGlobalInstructionsPath] = createSignal("");
  const [globalInstructionTargets, setGlobalInstructionTargets] = createSignal<
    AgentInstructionTarget[]
  >([]);
  const [globalInstructionsLoading, setGlobalInstructionsLoading] = createSignal(false);
  const [globalInstructionsBusy, setGlobalInstructionsBusy] = createSignal(false);
  const [globalInstructionsDirty, setGlobalInstructionsDirty] = createSignal(false);
  const [globalInstructionsMsg, setGlobalInstructionsMsg] = createSignal("");
  const [verifying, setVerifying] = createSignal(false);
  const [verifyMsg, setVerifyMsg] = createSignal("");
  const [showLogs, setShowLogs] = createSignal(false);
  const [saving, setSaving] = createSignal(false);
  const [restarting, setRestarting] = createSignal(false);
  const [restartMsg, setRestartMsg] = createSignal("");
  const [cliStatuses, setCliStatuses] = createSignal<Partial<Record<AgentKind, CliStatus>>>({});
  const [cliLoading, setCliLoading] = createSignal(false);
  const [upgradingCli, setUpgradingCli] = createSignal<AgentKind | null>(null);
  const [cliMessages, setCliMessages] = createSignal<Partial<Record<AgentKind, string>>>({});

  const restartAgents = async () => {
    setRestarting(true);
    setRestartMsg("");
    try {
      await api.restartDevin();
      setRestartMsg("已重启所有 agent 进程");
      setTimeout(() => setRestartMsg(""), 4000);
    } catch (e) {
      setRestartMsg(`重启失败：${String(e)}`);
    } finally {
      setRestarting(false);
    }
  };

  // 至少保留一个启用的后端：是最后一个时不允许关闭
  const enabledCount = () =>
    [
      devinEnabled(),
      codexEnabled(),
      codebuddyEnabled(),
      claudecodeEnabled(),
      cursorEnabled(),
      opencodeEnabled(),
    ].filter(Boolean).length;

  const quotaShareKinds = createMemo<AgentKind[]>(() => {
    const kinds: AgentKind[] = [];
    if (devinEnabled()) kinds.push("devin");
    if (codexEnabled()) kinds.push("codex");
    if (codebuddyEnabled()) kinds.push("codebuddy");
    if (claudecodeEnabled()) kinds.push("claudecode");
    if (cursorEnabled()) kinds.push("cursor");
    if (opencodeEnabled()) kinds.push("opencode");
    return kinds;
  });
  const titleAgentKinds = createMemo(() =>
    enabledAgentKinds().filter((kind) => kind === "devin" || kind === "codex" || kind === "opencode"),
  );

  const quotaShareKey = (kind: AgentKind, model: string) => `${kind}:${model}`;
  const toggleQuotaSharedModel = (kind: AgentKind, model: string, checked: boolean) => {
    const key = quotaShareKey(kind, model);
    setQuotaSharedModels((current) => {
      if (!checked) return current.filter((item) => item !== key);
      if (current.includes(key)) return current;
      return [...current, key];
    });
  };

  let roamingFoldersLoaded = false;
  const loadRoamingFolders = async () => {
    if (roamingFoldersLoaded || roamingFoldersLoading()) return;
    setRoamingFoldersLoading(true);
    try {
      const folders = await api.listRoamingFolders();
      setRoamingFolders(folders);
      roamingFoldersLoaded = true;
    } finally {
      setRoamingFoldersLoading(false);
    }
  };
  const roamingProjectSelected = (path: string) => {
    const key = projectPathKey(path);
    return roamingFolders().some((folder) => projectPathKey(folder) === key);
  };
  const toggleRoamingProject = (path: string, checked: boolean) => {
    setRoamingFolders((current) => {
      const key = projectPathKey(path);
      const next = current.filter((folder) => projectPathKey(folder) !== key);
      return checked ? [...next, path] : next;
    });
  };
  const selectedRoamingProjectCount = createMemo(
    () => state.projects.filter((project) => roamingProjectSelected(project.path)).length,
  );

  let globalInstructionsLoaded = false;
  const loadGlobalInstructions = async () => {
    if (globalInstructionsLoaded || globalInstructionsLoading()) return;
    setGlobalInstructionsLoading(true);
    setGlobalInstructionsMsg("");
    try {
      const config = await api.getGlobalAgentInstructions();
      setGlobalInstructions(config.content);
      setGlobalInstructionsPath(config.path);
      setGlobalInstructionTargets(config.targets);
      setGlobalInstructionsDirty(false);
      globalInstructionsLoaded = true;
    } catch (error) {
      setGlobalInstructionsMsg(`加载失败：${String(error)}`);
    } finally {
      setGlobalInstructionsLoading(false);
    }
  };
  const syncGlobalInstructions = async () => {
    setGlobalInstructionsBusy(true);
    setGlobalInstructionsMsg("");
    try {
      const config = await api.setGlobalAgentInstructions(globalInstructions());
      setGlobalInstructions(config.content);
      setGlobalInstructionsPath(config.path);
      setGlobalInstructionTargets(config.targets);
      setGlobalInstructionsDirty(false);
      const conflicts = config.targets.filter(
        (target) => target.status === "conflict" || target.status === "error",
      ).length;
      setGlobalInstructionsMsg(
        conflicts > 0
          ? `已同步，其余 ${conflicts} 个冲突入口未覆盖，请检查下方状态。`
          : "已同步到所有后端；正在运行的 Agent 重启后读取新配置。",
      );
      return config;
    } finally {
      setGlobalInstructionsBusy(false);
    }
  };

  // 后端可用性检测结果：false = 已检测且未找到 CLI（卡片上提示，仍可手动改路径）
  const backendMissing = (kind: string) => state.backendAvailability[kind] === false;

  const ensureRelayToken = () => {
    const token = relayToken().trim();
    if (!state.settings?.relayToken.trim() && token && !token.includes("/")) {
      const generated = `${token}/${crypto.randomUUID()}`;
      setRelayToken(generated);
      return generated;
    }
    return token;
  };

  const verifyRelay = async () => {
    setVerifying(true);
    setVerifyMsg("");
    try {
      const online = await api.verifyRelay(
        relayServer().trim(),
        ensureRelayToken(),
        relayGroups().trim(),
      );
      setVerifyMsg(`连接正常 ✓ 本群组在线 ${online} 人`);
    } catch (e) {
      setVerifyMsg(`✗ ${String(e)}`);
    } finally {
      setVerifying(false);
    }
  };

  // 版本与更新
  const [version, setVersion] = createSignal("");
  const [checking, setChecking] = createSignal(false);
  const [checkResult, setCheckResult] = createSignal("");
  onMount(() => void getVersion().then(setVersion));
  const checkNow = async () => {
    setChecking(true);
    setCheckResult("");
    try {
      setCheckResult(await checkAndStageUpdate());
    } catch (e) {
      setCheckResult(String(e));
    } finally {
      setChecking(false);
    }
  };

  const draftSettings = (): Settings => ({
    devinPath: devinPath().trim() || "devin",
    acpArgs: acpArgs().trim() || "acp",
    codebuddyPath: codebuddyPath().trim() || "codebuddy",
    claudecodePath: claudecodePath().trim() || "claude",
    cursorPath: cursorPath().trim() || "cursor-agent",
    opencodePath: opencodePath().trim() || "opencode",
    codexPath: codexPath().trim() || "codex",
    codexProxy: codexProxy().trim(),
    windowsShellShimEnabled: windowsShellShimEnabled(),
    devinProxy: devinProxy().trim(),
    codebuddyProxy: codebuddyProxy().trim(),
    claudecodeProxy: claudecodeProxy().trim(),
    claudecodeSdkApiKey: claudecodeSdkApiKey().trim(),
    cursorProxy: cursorProxy().trim(),
    cursorSdkApiKey: cursorSdkApiKey().trim(),
    opencodeProxy: opencodeProxy().trim(),
    defaultMode: defaultMode(),
    titleModelAgent: titleAgent(),
    titleModel: titleModel().trim(),
    shareModelAgent: shareAgent(),
    shareModel: shareModel().trim(),
    editor: editor().trim() || "code",
    theme: state.theme,
    relayServer: relayServer().trim(),
    relayToken: relayToken().trim(),
    relayGroups: relayGroups().trim(),
    remoteControlEnabled: remoteControlEnabled(),
    quotaSharedModels: quotaSharedModels(),
    devinEnabled: devinEnabled(),
    codexEnabled: codexEnabled(),
    codebuddyEnabled: codebuddyEnabled(),
    claudecodeEnabled: claudecodeEnabled(),
    cursorEnabled: cursorEnabled(),
    opencodeEnabled: opencodeEnabled(),
    codexIntegration: "sdk",
    codebuddyIntegration: "sdk",
    claudecodeIntegration: "sdk",
    cursorIntegration: "sdk",
    opencodeIntegration: "sdk",
    worktreeDir: worktreeDir().trim(),
    sessionAutoCleanupEnabled: sessionAutoCleanupEnabled(),
    sessionAutoCleanupHours: Math.max(1, Math.floor(sessionAutoCleanupHours() || 24 * 30)),
    historyDisplayMode: historyDisplayMode(),
    semanticEnabled: semanticEnabled(),
    embedEndpoint: embedEndpoint().trim(),
    embedModel: embedModel().trim(),
    embedApiKey: embedApiKey().trim(),
  });

  const refreshCliStatuses = async () => {
    setCliLoading(true);
    try {
      const statuses = await api.getCliStatuses(draftSettings());
      const next: Partial<Record<AgentKind, CliStatus>> = {};
      for (const status of statuses) next[status.agentKind] = status;
      setCliStatuses(next);
    } finally {
      setCliLoading(false);
    }
  };

  const upgradeCli = async (kind: AgentKind) => {
    const wasInstalled = cliStatuses()[kind]?.installed !== false;
    setUpgradingCli(kind);
    setCliMessages((prev) => ({ ...prev, [kind]: "" }));
    try {
      const operationId = typeof globalThis.crypto?.randomUUID === "function"
        ? globalThis.crypto.randomUUID()
        : `${Date.now()}-${Math.random().toString(36).slice(2)}`;
      const status = await api.upgradeCli(kind, draftSettings(), operationId);
      setCliStatuses((prev) => ({ ...prev, [kind]: status }));
      setCliMessages((prev) => ({
        ...prev,
        [kind]: `${wasInstalled ? "已更新" : "已安装"}到 ${status.version}`,
      }));
    } catch (e) {
      const cancelled = String(e).includes("CLI 操作已取消");
      setCliMessages((prev) => ({
        ...prev,
        [kind]: cancelled ? "操作已取消" : `${wasInstalled ? "升级" : "安装"}失败：${String(e)}`,
      }));
    } finally {
      setUpgradingCli(null);
    }
  };

  let cliStatusesLoaded = false;
  createEffect(() => {
    if (tab() === "backends" && !cliStatusesLoaded) {
      cliStatusesLoaded = true;
      void refreshCliStatuses();
    }
  });

  createEffect(() => {
    if (tab() !== "team") return;
    void loadRoamingFolders();
    for (const kind of quotaShareKinds()) void ensureModelOptions(kind);
  });

  createEffect(() => {
    if (tab() === "instructions") void loadGlobalInstructions();
  });

  // 会话批量管理
  const [managing, setManaging] = createSignal(false);
  const [sel, setSel] = createSignal<Record<string, boolean>>({});
  const [deleting, setDeleting] = createSignal(false);
  const deletable = createMemo(() => state.threads.filter((t) => !state.running[t.id]));
  const selectedIds = createMemo(() =>
    deletable()
      .filter((t) => sel()[t.id])
      .map((t) => t.id),
  );
  const allSelected = createMemo(
    () => deletable().length > 0 && deletable().every((t) => sel()[t.id]),
  );
  const toggleAll = () => {
    const on = !allSelected();
    const next: Record<string, boolean> = {};
    if (on) for (const t of deletable()) next[t.id] = true;
    setSel(next);
  };
  const removeSelected = async () => {
    const ids = selectedIds();
    if (ids.length === 0) return;
    const ok = await confirm(`删除选中的 ${ids.length} 个会话？聊天记录将一并删除。`, {
      title: "批量删除会话",
      kind: "warning",
    });
    if (!ok) return;
    setDeleting(true);
    try {
      await deleteThreads(ids);
      setSel({});
    } finally {
      setDeleting(false);
    }
  };

  // 统一模式：全后端只有 Build / Plan 两种（Rust 侧翻译成各后端真实模式 id）
  const modes = () => UNIFIED_MODE_OPTIONS;

  // worktree 管理
  const [worktreeDir, setWorktreeDir] = createSignal(s?.worktreeDir ?? "");
  // skills 管理（集中存放 ~/.nova/skills）
  const [skillsDir, setSkillsDir] = createSignal("");
  const [skills, setSkills] = createSignal<SkillInfo[]>([]);
  const [skillsLoading, setSkillsLoading] = createSignal(false);
  const [skillsBusy, setSkillsBusy] = createSignal(false);
  const [skillsDragging, setSkillsDragging] = createSignal(false);
  const [skillsMsg, setSkillsMsg] = createSignal("");
  const [semanticEnabled, setSemanticEnabled] = createSignal(s?.semanticEnabled ?? false);
  const [embedEndpoint, setEmbedEndpoint] = createSignal(s?.embedEndpoint ?? "http://localhost:11434");
  const [embedModel, setEmbedModel] = createSignal(s?.embedModel ?? "bge-m3");
  const [embedApiKey, setEmbedApiKey] = createSignal(s?.embedApiKey ?? "");
  const [embedBusy, setEmbedBusy] = createSignal(false);
  const [embedMsg, setEmbedMsg] = createSignal("");
  const persistEmbed = async () => {
    const cur = await api.getSettings();
    const next: Settings = {
      ...cur,
      semanticEnabled: semanticEnabled(),
      embedEndpoint: embedEndpoint().trim(),
      embedModel: embedModel().trim(),
      embedApiKey: embedApiKey().trim(),
    };
    await api.setSettings(next);
    setState("settings", next);
  };
  const testEmbed = async () => {
    setEmbedBusy(true);
    setEmbedMsg("正在连接…");
    try {
      await persistEmbed();
      const r = await api.semanticStatus();
      setEmbedMsg(`连接成功，向量维度 ${r.dim}。`);
    } catch (e) {
      setEmbedMsg(`连接失败：${String(e)}`);
    } finally {
      setEmbedBusy(false);
    }
  };
  const pullModel = async () => {
    const m = embedModel().trim();
    if (!m) {
      setEmbedMsg("请先填写模型名。");
      return;
    }
    setEmbedBusy(true);
    setEmbedMsg(`正在下载模型 ${m}…（体积较大，请耐心等待，勿关闭）`);
    try {
      await persistEmbed();
      await api.semanticPull(m);
      setEmbedMsg("模型下载完成，可点「测试连接」验证。");
    } catch (e) {
      setEmbedMsg(`下载失败：${String(e)}`);
    } finally {
      setEmbedBusy(false);
    }
  };
  const [worktrees, setWorktrees] = createSignal<WorktreeRecord[]>([]);
  const [wtLoading, setWtLoading] = createSignal(false);
  const [wtDelBranch, setWtDelBranch] = createSignal<Record<string, boolean>>({});
  const refreshWorktrees = async () => {
    setWtLoading(true);
    try {
      setWorktrees(await api.listWorktrees());
    } finally {
      setWtLoading(false);
    }
  };
  // 进入 Worktree 页时拉取一次列表
  createEffect(() => {
    if (tab() === "worktree") void refreshWorktrees();
  });
  const pickWorktreeDir = async () => {
    const dir = await openDialog({ directory: true, title: "选择 worktree 根目录" });
    if (typeof dir === "string" && dir) setWorktreeDir(dir);
  };
  const removeWt = async (w: WorktreeRecord) => {
    // 直接检出用户已有分支的 worktree 没有「删分支」可言（后端也会强制忽略）
    const del = !!wtDelBranch()[w.id] && w.ownedBranch !== false;
    const ok = await confirm(
      del
        ? `移除 worktree「${w.branch}」并删除该分支？分支上未合并/未推送的提交会一并丢失，属于该目录的会话历史也会一起删除。`
        : `移除 worktree「${w.branch}」的工作目录？分支保留，未提交的改动会丢弃，属于该目录的会话历史也会一起删除。`,
      { title: "移除 worktree", kind: "warning" },
    );
    if (!ok) return;
    try {
      await api.removeWorktree(w.id, del);
    } catch (e) {
      await message(String(e), { kind: "error" });
    } finally {
      await refreshWorktrees();
    }
  };

  const refreshSkills = async () => {
    setSkillsLoading(true);
    try {
      const [dir, list] = await Promise.all([api.getSkillsDir(), api.listSkills()]);
      setSkillsDir(dir);
      setSkills(list);
    } catch (e) {
      setSkillsMsg(`加载失败：${String(e)}`);
    } finally {
      setSkillsLoading(false);
    }
  };
  createEffect(() => {
    if (tab() === "skills") void refreshSkills();
  });

  const installSkillPaths = async (paths: string[]) => {
    if (paths.length === 0) return;
    setSkillsBusy(true);
    setSkillsMsg("");
    const okNames: string[] = [];
    const errors: string[] = [];
    for (const path of paths) {
      try {
        const info = await api.installSkill(path);
        okNames.push(info.name);
      } catch (e) {
        errors.push(`${path.split(/[\\/]/).pop()}: ${String(e)}`);
      }
    }
    await refreshSkills();
    setSkillsBusy(false);
    if (okNames.length > 0) {
      setSkillsMsg(`已安装：${okNames.join("、")}（已同步到各后端）`);
    }
    if (errors.length > 0) {
      setSkillsMsg((prev) => (prev ? `${prev}；` : "") + errors.join("；"));
    }
  };

  const pickSkillZip = async () => {
    const selected = await openDialog({
      multiple: true,
      title: "选择 skill zip 或文件夹",
      filters: [{ name: "Skill 包", extensions: ["zip"] }],
    });
    const paths = Array.isArray(selected) ? selected : selected ? [selected] : [];
    await installSkillPaths(paths.filter((p): p is string => typeof p === "string"));
  };

  const pickSkillFolder = async () => {
    const dir = await openDialog({ directory: true, title: "选择 skill 文件夹（含 SKILL.md）" });
    if (typeof dir === "string" && dir) await installSkillPaths([dir]);
  };

  const removeSkillItem = async (sk: SkillInfo) => {
    const ok = await confirm(`删除 skill「${sk.name}」？各后端中的对应快捷方式也会移除。`, {
      title: "删除 skill",
      kind: "warning",
    });
    if (!ok) return;
    setSkillsBusy(true);
    try {
      await api.removeSkill(sk.name);
      setSkillsMsg(`已删除：${sk.name}`);
      await refreshSkills();
    } catch (e) {
      await message(String(e), { kind: "error" });
    } finally {
      setSkillsBusy(false);
    }
  };

  const openSkillsDir = async () => {
    const dir = skillsDir() || (await api.getSkillsDir());
    if (!dir) return;
    try {
      await api.openInExplorer(dir);
    } catch (e) {
      await message(String(e), { kind: "error" });
    }
  };

  const resyncSkills = async () => {
    setSkillsBusy(true);
    try {
      await api.syncSkills();
      setSkillsMsg("已重新同步到各后端全局 skills 目录");
    } catch (e) {
      setSkillsMsg(`同步失败：${String(e)}`);
    } finally {
      setSkillsBusy(false);
    }
  };

  // 设置弹层打开期间屏蔽聊天区拖放；Skills 页接管 zip/文件夹拖入
  onMount(() => {
    setFileDropBlocked(true);
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    try {
      void getCurrentWebview()
        .onDragDropEvent((event) => {
          if (tab() !== "skills") {
            if (event.payload.type === "drop" || event.payload.type === "leave") {
              setSkillsDragging(false);
            }
            return;
          }
          if (event.payload.type === "enter" || event.payload.type === "over") {
            setSkillsDragging(true);
          } else if (event.payload.type === "drop") {
            setSkillsDragging(false);
            void installSkillPaths(event.payload.paths);
          } else {
            setSkillsDragging(false);
          }
        })
        .then((fn) => {
          if (cancelled) fn();
          else unlisten = fn;
        })
        .catch(() => setSkillsDragging(false));
    } catch {
      setSkillsDragging(false);
    }
    onCleanup(() => {
      cancelled = true;
      unlisten?.();
      setFileDropBlocked(false);
    });
  });

  const save = async () => {
    setSaving(true);
    const settings = draftSettings();
    settings.relayToken = ensureRelayToken();
    const shellShimChanged =
      settings.windowsShellShimEnabled !== (state.settings?.windowsShellShimEnabled ?? false);
    try {
      await api.setSettings(settings);
      setState("settings", settings);
      if (roamingFoldersLoaded) {
        const folders = await api.setRoamingFolders(roamingFolders());
        setRoamingFolders(folders);
        setState("roamingFolders", folders);
      }
      if (globalInstructionsLoaded && globalInstructionsDirty()) {
        await syncGlobalInstructions();
      }
      // 中转站配置可能变化，稍后刷新连接状态
      setTimeout(() => void refreshRelayStatus(), 800);
      if (shellShimChanged) {
        await message("Windows 启动 shim 设置已保存，重启 Nova 后生效。", {
          title: "需要重启 Nova",
          kind: "info",
        });
      }
      props.onClose();
    } catch (error) {
      await message(String(error), { title: "保存设置失败", kind: "error" });
    } finally {
      setSaving(false);
    }
  };

  return (
    <div class="modal-backdrop">
      <div class="modal settings-modal">
        <div class="modal-head">
          <span>设置</span>
          <button class="icon-btn" onClick={props.onClose}>
            <IconX size={16} />
          </button>
        </div>

        <div class="settings-tabs">
          <For each={TABS}>
            {(t) => (
              <button
                type="button"
                classList={{ "settings-tab": true, active: tab() === t.id }}
                onClick={() => setTab(t.id)}
              >
                {t.name}
              </button>
            )}
          </For>
        </div>

        <div class="modal-body">
          {/* ===== 通用 ===== */}
          <Show when={tab() === "general"}>
            <section class="settings-group">
              <h3 class="settings-group-title">会话</h3>
              <label class="field">
                <span class="field-label">新会话默认模式</span>
                <select
                  class="field-input"
                  value={defaultMode()}
                  onChange={(e) => setDefaultMode(e.currentTarget.value)}
                >
                  <option value="">跟随 agent 默认</option>
                  <For each={modes()}>{(m) => <option value={m.id}>{m.name}</option>}</For>
                </select>
                <span class="field-hint">
                  新建会话未手动选择模式时使用。Build 等价原 Bypass Permissions（全部自动批准）。
                </span>
              </label>

              <label class="field">
                <span class="field-label">会话历史展示方式</span>
                <select
                  class="field-input"
                  value={historyDisplayMode()}
                  onChange={(e) =>
                    setHistoryDisplayMode(e.currentTarget.value === "time" ? "time" : "project")
                  }
                >
                  <option value="project">按项目</option>
                  <option value="time">按时间</option>
                </select>
                <span class="field-hint">
                  按时间会将普通会话按最近更新时间排列，并标出项目和模型。
                </span>
              </label>

              <label class="field">
                <span class="field-label">编辑器命令</span>
                <input
                  class="field-input"
                  value={editor()}
                  onInput={(e) => setEditor(e.currentTarget.value)}
                  placeholder="code"
                />
                <span class="field-hint">
                  点击文件路径时用它打开，如 cursor / code / zed / windsurf（需在
                  PATH 中）。正式项目会连同项目目录一起打开，临时会话只打开文件。
                </span>
              </label>

              <div class="field">
                <span class="field-label">自动清理过期会话</span>
                <label class="backend-switch">
                  <input
                    type="checkbox"
                    checked={sessionAutoCleanupEnabled()}
                    onChange={(e) => setSessionAutoCleanupEnabled(e.currentTarget.checked)}
                  />
                  <span>启用</span>
                </label>
                <span class="field-hint">
                  启用后会在启动时及之后每小时检查一次，运行中的会话不会被清理。
                </span>
              </div>

              <label class="field">
                <span class="field-label">会话保留时间（小时）</span>
                <input
                  class="field-input"
                  type="number"
                  min="1"
                  step="1"
                  value={sessionAutoCleanupHours()}
                  onInput={(e) => setSessionAutoCleanupHours(Number(e.currentTarget.value))}
                />
                <span class="field-hint">
                  仅清理普通会话；超时后先移入回收站，保留同样时长才彻底删除。
                </span>
              </label>
            </section>

            <section class="settings-group">
              <h3 class="settings-group-title">标题与分享模型</h3>
              <p class="settings-group-desc">
                与新建会话同款的模型选择器，可任选任意已启用后端下的模型，不再限定
                Devin。
              </p>
              <div class="field">
                <span class="field-label">标题生成模型</span>
                <ModelPicker
                  agentKind={titleAgent()}
                  agentKinds={titleAgentKinds()}
                  model={titleModel()}
                  onPickModel={(a, m) => {
                    setTitleAgent(a);
                    setTitleModel(m);
                  }}
                  prefix="标题模型"
                  title="标题生成模型"
                  portal
                />
                <span class="field-hint">
                  自动为新会话生成标题时所用的轻量模型。标题统一在所选后端生成；选
                  Codex 或该后端不可用时，自动回退到会话所在后端。
                </span>
              </div>

              <div class="field">
                <span class="field-label">高级分享处理模型</span>
                <ModelPicker
                  agentKind={shareAgent()}
                  agentKinds={enabledAgentKinds()}
                  model={shareModel()}
                  onPickModel={(a, m) => {
                    setShareAgent(a);
                    setShareModel(m);
                  }}
                  prefix="分享模型"
                  title="高级分享处理模型"
                  portal
                />
                <span class="field-hint">
                  高级分享「按提示词处理/总结会话」时的默认后端与模型。分享时仍可临时改选。
                </span>
              </div>
            </section>

            <section class="settings-group">
              <h3 class="settings-group-title">更新</h3>
              <div class="field">
                <span class="field-label">自动升级</span>
                <span class="field-hint">
                  新版本会在后台自动下载好，并在空闲时间（没有任何会话或任务在运行）弹窗提示你选择是否现在更新，不会强制静默重启。
                </span>
              </div>
            </section>
          </Show>

          {/* ===== 高级 ===== */}
          <Show when={tab() === "advanced"}>
            <section class="settings-group">
              <h3 class="settings-group-title">Windows 启动</h3>
              <div class="field">
                <span class="field-label">Windows shell 启动 shim</span>
                <label class="backend-switch">
                  <input
                    type="checkbox"
                    checked={windowsShellShimEnabled()}
                    onChange={(e) => setWindowsShellShimEnabled(e.currentTarget.checked)}
                  />
                  <span>启用</span>
                </label>
                <span class="field-hint">
                  为 agent 子进程的 cmd、PowerShell 和 pwsh 使用无窗口 shim，减少控制台闪现。默认关闭；保存后重启 Nova 生效。
                </span>
              </div>
            </section>
          </Show>

          {/* ===== 模型后端 ===== */}
          <Show when={tab() === "backends"}>
            <p class="field-hint">
              每个后端可单独启用/关闭并配置启动方式。关闭的后端不会出现在新建/切换会话的后端列表里（历史会话仍可打开查看）。
            </p>

            <div class="backend-card">
              <div class="backend-card-head">
                <span class={`agent-badge devin`}>{agentLabel("devin")}</span>
                <Show when={backendMissing("devin")}>
                  <span class="backend-missing">未检测到 CLI</span>
                </Show>
                <label class="backend-switch">
                  <input
                    type="checkbox"
                    checked={devinEnabled()}
                    disabled={devinEnabled() && enabledCount() === 1}
                    onChange={(e) => setDevinEnabled(e.currentTarget.checked)}
                  />
                  <span>启用</span>
                </label>
              </div>
              <CliManager
                status={cliStatuses().devin}
                loading={cliLoading()}
                busy={upgradingCli() !== null}
                upgrading={upgradingCli() === "devin"}
                message={cliMessages().devin}
                onUpgrade={() => void upgradeCli("devin")}
              />
              <div class="backend-config-row">
                <span class="fixed-integration">ACP</span>
                <div class="backend-fields">
                  <label class="backend-field">
                    <span class="field-label">可执行文件</span>
                    <input class="field-input" value={devinPath()} onInput={(e) => setDevinPath(e.currentTarget.value)} placeholder="devin" />
                  </label>
                  <label class="backend-field">
                    <span class="field-label">启动参数</span>
                    <input class="field-input" value={acpArgs()} onInput={(e) => setAcpArgs(e.currentTarget.value)} placeholder="acp" />
                  </label>
                </div>
              </div>
              <ProxyField value={devinProxy()} onInput={setDevinProxy} />
            </div>

            <div class="backend-card">
              <div class="backend-card-head">
                <span class={`agent-badge codebuddy`}>{agentLabel("codebuddy")}</span>
                <Show when={backendMissing("codebuddy")}>
                  <span class="backend-missing">未检测到 CLI</span>
                </Show>
                <label class="backend-switch">
                  <input
                    type="checkbox"
                    checked={codebuddyEnabled()}
                    disabled={codebuddyEnabled() && enabledCount() === 1}
                    onChange={(e) => setCodebuddyEnabled(e.currentTarget.checked)}
                  />
                  <span>启用</span>
                </label>
              </div>
              <CliManager
                status={cliStatuses().codebuddy}
                loading={cliLoading()}
                busy={upgradingCli() !== null}
                upgrading={upgradingCli() === "codebuddy"}
                message={cliMessages().codebuddy}
                onUpgrade={() => void upgradeCli("codebuddy")}
              />
              <div class="backend-config-row">
                <span class="fixed-integration">SDK</span>
                <div class="backend-fields">
                  <label class="backend-field">
                    <span class="field-label">可执行文件</span>
                    <input class="field-input" value={codebuddyPath()} onInput={(e) => setCodebuddyPath(e.currentTarget.value)} placeholder="codebuddy" />
                  </label>
                </div>
              </div>
              <ProxyField value={codebuddyProxy()} onInput={setCodebuddyProxy} />
            </div>

            <div class="backend-card">
              <div class="backend-card-head">
                <span class={`agent-badge claudecode`}>{agentLabel("claudecode")}</span>
                <Show when={backendMissing("claudecode")}>
                  <span class="backend-missing">未检测到 CLI</span>
                </Show>
                <label class="backend-switch">
                  <input
                    type="checkbox"
                    checked={claudecodeEnabled()}
                    disabled={claudecodeEnabled() && enabledCount() === 1}
                    onChange={(e) => setClaudecodeEnabled(e.currentTarget.checked)}
                  />
                  <span>启用</span>
                </label>
              </div>
              <CliManager
                status={cliStatuses().claudecode}
                loading={cliLoading()}
                busy={upgradingCli() !== null}
                upgrading={upgradingCli() === "claudecode"}
                message={cliMessages().claudecode}
                onUpgrade={() => void upgradeCli("claudecode")}
              />
              <div class="backend-config-row">
                <span class="fixed-integration">SDK</span>
                <div class="backend-fields">
                  <label class="backend-field">
                    <span class="field-label">可执行文件</span>
                    <input class="field-input" value={claudecodePath()} onInput={(e) => setClaudecodePath(e.currentTarget.value)} placeholder="claude" />
                  </label>
                  <label class="backend-field backend-field-wide">
                    <span class="field-label">Anthropic API Key</span>
                    <input class="field-input" value={claudecodeSdkApiKey()} onInput={(e) => setClaudecodeSdkApiKey(e.currentTarget.value)} placeholder="留空使用环境/provider 凭据" />
                  </label>
                </div>
              </div>
              <ProxyField value={claudecodeProxy()} onInput={setClaudecodeProxy} />
            </div>

            <div class="backend-card">
              <div class="backend-card-head">
                <span class={`agent-badge codex`}>{agentLabel("codex")}</span>
                <Show when={backendMissing("codex")}>
                  <span class="backend-missing">未检测到 CLI</span>
                </Show>
                <label class="backend-switch">
                  <input
                    type="checkbox"
                    checked={codexEnabled()}
                    disabled={codexEnabled() && enabledCount() === 1}
                    onChange={(e) => setCodexEnabled(e.currentTarget.checked)}
                  />
                  <span>启用</span>
                </label>
              </div>
              <CliManager
                status={cliStatuses().codex}
                loading={cliLoading()}
                busy={upgradingCli() !== null}
                upgrading={upgradingCli() === "codex"}
                message={cliMessages().codex}
                onUpgrade={() => void upgradeCli("codex")}
              />
              <div class="backend-config-row">
                <span class="fixed-integration">SDK</span>
                <div class="backend-fields">
                  <label class="backend-field">
                    <span class="field-label">可执行文件</span>
                    <input class="field-input" value={codexPath()} onInput={(e) => setCodexPath(e.currentTarget.value)} placeholder="codex" />
                  </label>
                </div>
              </div>
              <ProxyField value={codexProxy()} onInput={setCodexProxy} />
            </div>

            <div class="backend-card">
              <div class="backend-card-head">
                <span class={`agent-badge cursor`}>{agentLabel("cursor")}</span>
                <Show when={backendMissing("cursor")}>
                  <span class="backend-missing">未检测到 CLI</span>
                </Show>
                <label class="backend-switch">
                  <input
                    type="checkbox"
                    checked={cursorEnabled()}
                    disabled={cursorEnabled() && enabledCount() === 1}
                    onChange={(e) => setCursorEnabled(e.currentTarget.checked)}
                  />
                  <span>启用</span>
                </label>
              </div>
              <CliManager
                status={cliStatuses().cursor}
                loading={cliLoading()}
                busy={upgradingCli() !== null}
                upgrading={upgradingCli() === "cursor"}
                message={cliMessages().cursor}
                onUpgrade={() => void upgradeCli("cursor")}
              />
              <div class="backend-config-row">
                <span class="fixed-integration">SDK</span>
                <div class="backend-fields">
                  <label class="backend-field">
                    <span class="field-label">可执行文件</span>
                    <input class="field-input" value={cursorPath()} onInput={(e) => setCursorPath(e.currentTarget.value)} placeholder="cursor-agent" />
                  </label>
                  <label class="backend-field backend-field-wide">
                    <span class="field-label">Cursor API Key</span>
                    <input class="field-input" value={cursorSdkApiKey()} onInput={(e) => setCursorSdkApiKey(e.currentTarget.value)} placeholder="留空使用 CURSOR_API_KEY" />
                  </label>
                </div>
              </div>
              <ProxyField value={cursorProxy()} onInput={setCursorProxy} />
            </div>

            <div class="backend-card">
              <div class="backend-card-head">
                <span class={`agent-badge opencode`}>{agentLabel("opencode")}</span>
                <Show when={backendMissing("opencode")}>
                  <span class="backend-missing">未检测到 CLI</span>
                </Show>
                <label class="backend-switch">
                  <input
                    type="checkbox"
                    checked={opencodeEnabled()}
                    disabled={opencodeEnabled() && enabledCount() === 1}
                    onChange={(e) => setOpencodeEnabled(e.currentTarget.checked)}
                  />
                  <span>启用</span>
                </label>
              </div>
              <CliManager
                status={cliStatuses().opencode}
                loading={cliLoading()}
                busy={upgradingCli() !== null}
                upgrading={upgradingCli() === "opencode"}
                message={cliMessages().opencode}
                onUpgrade={() => void upgradeCli("opencode")}
              />
              <div class="backend-config-row">
                <span class="fixed-integration">SDK</span>
                <div class="backend-fields">
                  <label class="backend-field">
                    <span class="field-label">可执行文件</span>
                    <input class="field-input" value={opencodePath()} onInput={(e) => setOpencodePath(e.currentTarget.value)} placeholder="opencode" />
                  </label>
                </div>
              </div>
              <ProxyField value={opencodeProxy()} onInput={setOpencodeProxy} />
            </div>

            <p class="field-hint">
              修改后端配置会重启对应 agent 进程，进行中的会话将被打断（上下文下次发消息时自动恢复）。
              未检测到 CLI 的后端不会出现在新建会话的后端列表里（保存后会自动重新检测）。
            </p>
            <div class="field">
              <button
                class="btn secondary"
                style={{ "align-self": "flex-start" }}
                disabled={restarting()}
                onClick={() => void restartAgents()}
              >
                {restarting() ? "重启中…" : "重启所有 agent 进程"}
              </button>
              <Show when={restartMsg()}>
                <span class={`relay-verify ${restartMsg().startsWith("重启失败") ? "bad" : "ok"}`}>
                  {restartMsg()}
                </span>
              </Show>
              <p class="field-hint">
                任务卡死（如后端网络重试不止）时使用：所有运行中的轮次会立即结束，会话上下文下次发消息时自动恢复。
              </p>
            </div>
          </Show>

          {/* ===== Agent 全局配置 ===== */}
          <Show when={tab() === "instructions"}>
            <div class="field">
              <span class="field-label">集中配置</span>
              <input
                class="field-input"
                value={globalInstructionsPath()}
                readonly
                title={globalInstructionsPath()}
                placeholder={globalInstructionsLoading() ? "加载中…" : "~/.nova/global-agent-instructions.md"}
              />
              <span class="field-hint">
                只维护这一份内容；Nova 会按每个后端的原生规则入口分别适配。已有真实配置文件会保留原内容，只更新 Nova 托管区块。
              </span>
            </div>
            <label class="field">
              <span class="field-label">全局指令</span>
              <textarea
                class="field-input global-agent-instructions"
                value={globalInstructions()}
                disabled={globalInstructionsLoading()}
                onInput={(event) => {
                  setGlobalInstructions(event.currentTarget.value);
                  setGlobalInstructionsDirty(true);
                  setGlobalInstructionsMsg("");
                }}
                placeholder="例如：始终使用中文；修改代码后执行聚焦测试；不要覆盖用户已有改动……"
              />
              <div class="global-agent-actions">
                <button
                  type="button"
                  class="btn secondary"
                  disabled={globalInstructionsLoading() || globalInstructionsBusy()}
                  onClick={() => void syncGlobalInstructions()}
                >
                  {globalInstructionsBusy() ? "同步中…" : "保存并同步到所有后端"}
                </button>
                <Show when={globalInstructionsMsg()}>
                  <span
                    class={`relay-verify ${globalInstructionsMsg().includes("失败") ? "bad" : "ok"}`}
                  >
                    {globalInstructionsMsg()}
                  </span>
                </Show>
              </div>
              <span class="field-hint">
                清空后同步会移除 Nova 创建的托管文件/区块，不会删除各后端原有的其它配置。正在运行的 Agent 需重启后读取新内容。
              </span>
            </label>

            <div class="field">
              <span class="field-label">后端适配状态</span>
              <Show
                when={globalInstructionTargets().length > 0}
                fallback={<div class="sel-empty">{globalInstructionsLoading() ? "加载中…" : "暂无状态"}</div>}
              >
                <div class="wt-list">
                  <For each={globalInstructionTargets()}>
                    {(target) => (
                      <div class="wt-row">
                        <div class="wt-row-main">
                          <span class={`agent-badge ${target.agentKind}`}>{target.label}</span>
                          <span class={`agent-config-status ${target.status}`}>{target.detail}</span>
                        </div>
                        <div class="wt-row-sub">
                          <span class="wt-path" title={target.path}>
                            {target.path}
                          </span>
                        </div>
                      </div>
                    )}
                  </For>
                </div>
              </Show>
            </div>
          </Show>

          {/* ===== 外观 ===== */}
          <Show when={tab() === "appearance"}>
            <div class="field">
              <span class="field-label">界面主题</span>
              <div class="theme-seg">
                <button
                  type="button"
                  classList={{ "theme-seg-btn": true, active: state.theme === "ink-light" }}
                  onClick={() => setTheme("ink-light")}
                >
                  <span class="theme-swatch light" />
                  浅色
                </button>
                <button
                  type="button"
                  classList={{ "theme-seg-btn": true, active: state.theme === "ink-dark" }}
                  onClick={() => setTheme("ink-dark")}
                >
                  <span class="theme-swatch dark" />
                  深色
                </button>
              </div>
              <span class="field-hint">明暗两套主题互为镜像、即点即换，选择会自动记住。</span>
            </div>
          </Show>

          {/* ===== 团队 ===== */}
          <Show when={tab() === "team"}>
            <div class="field">
              <span class="field-label">
                团队 / 漫游中转站
                <Show when={state.settings?.relayToken}>
                  <span class={`relay-state ${state.relay.connected ? "on" : "off"}`}>
                    {state.relay.connected ? "已连接" : "未连接"}
                  </span>
                </Show>
              </span>
              <input
                class="field-input"
                value={relayServer()}
                onInput={(e) => setRelayServer(e.currentTarget.value)}
                placeholder={RELAY_SERVER_PLACEHOLDER}
              />
              <span class="field-hint">中转服务地址，一般用默认即可（留空也会回退到默认）。</span>
            </div>

            <label class="field">
              <span class="field-label">身份 token</span>
              <div class="relay-token-row">
                <input
                  class="field-input"
                  value={relayToken()}
                  onInput={(e) => setRelayToken(e.currentTarget.value)}
                  placeholder="首次使用时填写用户名"
                />
                <button
                  class="btn secondary small"
                  disabled={verifying() || !relayToken().trim()}
                  onClick={() => void verifyRelay()}
                >
                  {verifying() ? "验证中…" : "验证"}
                </button>
              </div>
              <span class="field-hint">
                首次保存会生成“用户名/随机串”作为永久 token；队友只会看到用户名。<b>填了 token 即开启团队/漫游，清空 token 即关闭。</b>
              </span>
              <Show when={verifyMsg()}>
                <span class={`relay-verify ${verifyMsg().startsWith("✗") ? "bad" : "ok"}`}>
                  {verifyMsg()}
                </span>
              </Show>
            </label>

            <label class="field">
              <span class="field-label">群组</span>
              <input
                class="field-input"
                value={relayGroups()}
                onInput={(e) => setRelayGroups(e.currentTarget.value)}
                placeholder="如：backend, infra（逗号或空格分隔，可多个）"
              />
              <span class="field-hint">
                只有<b>相同群组</b>的人才能在在线名单里看到彼此、互相分享/漫游；一个人可归属多个群组。<b>留空 = 默认群组</b>（与其他同样未配置群组的人互相可见）。
              </span>
            </label>

            <div class="field">
              <label
                style={{ display: "flex", "align-items": "center", gap: "8px" }}
              >
                <input
                  type="checkbox"
                  checked={remoteControlEnabled()}
                  onChange={(event) => setRemoteControlEnabled(event.currentTarget.checked)}
                />
                <span>允许 server 端远程控制</span>
              </label>
              <span class="field-hint">
                默认关闭。手动开启并保存后，server 端才可查看会话、读取项目文件或发送远程操作。
              </span>
            </div>

            <div class="field">
              <span class="field-label">允许漫游的项目</span>
              <span class="field-hint">
                只能从你当前已有的本地项目中选择；未列入项目列表的目录不会展示给队友，也不会接受漫游请求。每次请求仍需你在本机确认。
              </span>
              <Show
                when={!roamingFoldersLoading()}
                fallback={<div class="sel-empty">加载中…</div>}
              >
                <Show
                  when={state.projects.length > 0}
                  fallback={<div class="sel-empty">当前没有可共享的本地项目。</div>}
                >
                  <div class="wt-dir-row">
                    <span class="field-hint">
                      已允许 {selectedRoamingProjectCount()} / {state.projects.length} 个项目
                    </span>
                  </div>
                  <div class="wt-list">
                    <For each={state.projects}>
                      {(project) => (
                        <label class="wt-row" title={project.path}>
                          <div class="wt-row-main">
                            <input
                              type="checkbox"
                              checked={roamingProjectSelected(project.path)}
                              onChange={(event) =>
                                toggleRoamingProject(project.path, event.currentTarget.checked)
                              }
                            />
                            <span class="wt-branch">{threadGroupName(project.path)}</span>
                            <Show when={project.worktree}>
                              <span class="wt-tag">⎇ {project.worktree!.branch}</span>
                            </Show>
                          </div>
                          <div class="wt-row-sub">
                            <span class="wt-path">{project.path}</span>
                          </div>
                        </label>
                      )}
                    </For>
                  </div>
                </Show>
              </Show>
            </div>

            <div class="field">
              <span class="field-label">共享模型额度（可多选）</span>
              <span class="field-hint">
                选中的模型会以“我的 Cursor”这类一级分类出现在队友的新会话模型选择器中。首次选择时会安全同步并预热额度租约；取消勾选后，旧缓存中的入口也无法再使用。
              </span>
              <div class="quota-share-models">
                <For each={quotaShareKinds()}>
                  {(kind) => {
                    const choices = () => modelChoices(kind);
                    return (
                      <section class="quota-share-backend">
                        <div class="quota-share-backend-title">
                          <span class={`agent-badge ${kind}`}>{agentLabel(kind)}</span>
                          <span>{choices().length} 个模型</span>
                        </div>
                        <Show
                          when={choices().length > 0}
                          fallback={<div class="field-hint">暂无模型，请先安装并登录对应 CLI。</div>}
                        >
                          <div class="quota-share-options">
                            <For each={choices()}>
                              {(choice) => {
                                const key = quotaShareKey(kind, choice.value);
                                return (
                                  <label class="quota-share-option" title={choice.value}>
                                    <input
                                      type="checkbox"
                                      checked={quotaSharedModels().includes(key)}
                                      onChange={(event) =>
                                        toggleQuotaSharedModel(
                                          kind,
                                          choice.value,
                                          event.currentTarget.checked,
                                        )
                                      }
                                    />
                                    <span>{choice.name}</span>
                                  </label>
                                );
                              }}
                            </For>
                          </div>
                        </Show>
                      </section>
                    );
                  }}
                </For>
              </div>
            </div>
          </Show>

          {/* ===== 记忆检索 ===== */}
          <Show when={tab() === "memory"}>
            <label
              class="field"
              style={{ display: "flex", "flex-direction": "row", "align-items": "center", gap: "8px" }}
            >
              <input
                type="checkbox"
                checked={semanticEnabled()}
                onChange={(e) => setSemanticEnabled(e.currentTarget.checked)}
              />
              <span>启用语义检索（关闭 = 用内置 BM25 关键词检索，零依赖）</span>
            </label>
            <div class="field">
              <span class="field-hint">
                语义检索由「外置 embedding 服务」提供：主程序不内置模型、不增加体积。可本地安装 Ollama 后填模型名并点「下载模型」；也可把服务部署在服务器上（Ollama / TEI / vLLM 等，见 docs/embedding-server.md），本地只在下方填服务器地址即可，无需本地部署；还可填任意 OpenAI 兼容的 /v1/embeddings 服务（含云端）。不配置或连不上时自动回退 BM25，员工记忆检索始终可用。
              </span>
            </div>
            <label class="field">
              <span class="field-label">服务地址</span>
              <input
                class="field-input"
                value={embedEndpoint()}
                onInput={(e) => setEmbedEndpoint(e.currentTarget.value)}
                placeholder="http://localhost:11434"
              />
              <span class="field-hint">OpenAI 兼容服务的 base 地址（无需带 /v1）。可填本地（Ollama 默认 http://localhost:11434），也可填部署在服务器上的地址（如 http://your-server:11434），本地无需装模型。服务器部署见 docs/embedding-server.md。</span>
            </label>
            <label class="field">
              <span class="field-label">模型名</span>
              <input
                class="field-input"
                value={embedModel()}
                onInput={(e) => setEmbedModel(e.currentTarget.value)}
                placeholder="bge-m3 / nomic-embed-text / text-embedding-3-small"
              />
              <span class="field-hint">中文/多语言推荐 bge-m3；轻量可用 nomic-embed-text。换模型后向量会自动重建。</span>
            </label>
            <label class="field">
              <span class="field-label">API Key</span>
              <input
                class="field-input"
                type="password"
                value={embedApiKey()}
                onInput={(e) => setEmbedApiKey(e.currentTarget.value)}
                placeholder="本地服务留空；云端填对应 key"
              />
            </label>
            <div class="wt-dir-row">
              <button class="btn secondary" disabled={embedBusy()} onClick={() => void testEmbed()}>
                测试连接
              </button>
              <button class="btn secondary" disabled={embedBusy()} onClick={() => void pullModel()}>
                下载模型（Ollama）
              </button>
            </div>
            <Show when={embedMsg()}>
              <div class="field-hint">{embedMsg()}</div>
            </Show>
          </Show>

          {/* ===== Worktree ===== */}
          <Show when={tab() === "worktree"}>
            <div class="field">
              <span class="field-label">worktree 根目录</span>
              <div class="wt-dir-row">
                <input
                  class="field-input"
                  value={worktreeDir()}
                  onInput={(e) => setWorktreeDir(e.currentTarget.value)}
                  placeholder="留空 = 应用数据目录下的 worktrees/"
                />
                <button class="btn secondary" onClick={() => void pickWorktreeDir()}>
                  浏览…
                </button>
              </div>
              <span class="field-hint">
                会话开启「在 worktree 中执行」时，在此目录下为其创建独立工作目录（每个 worktree 一个子文件夹）。改动仅对之后新建的 worktree 生效。
              </span>
            </div>

            <div class="field">
              <span class="field-label">
                已创建的 worktree（共 {worktrees().length} 个）
                <button
                  class="link-btn"
                  style={{ "margin-left": "8px" }}
                  onClick={() => void refreshWorktrees()}
                >
                  刷新
                </button>
              </span>
              <span class="field-hint">
                这些工作目录不随会话删除而自动清理，在此手动移除。移除只影响该 worktree，不动主工作区。
              </span>
              <Show
                when={worktrees().length > 0}
                fallback={
                  <div class="sel-empty">{wtLoading() ? "加载中…" : "暂无 worktree"}</div>
                }
              >
                <div class="wt-list">
                  <For each={worktrees()}>
                    {(w) => {
                      const linked = () => state.threads.find((t) => t.id === w.threadId);
                      return (
                        <div class="wt-row">
                          <div class="wt-row-main">
                            <span class="wt-branch" title={w.branch}>
                              ⎇ {w.branch}
                            </span>
                            <span class="wt-repo" title={w.repo}>
                              {threadGroupName(w.repo)}
                            </span>
                            <Show when={w.roaming}>
                              <span class="wt-tag">漫游</span>
                            </Show>
                          </div>
                          <div class="wt-row-sub">
                            <span class="wt-path" title={w.path}>
                              {w.path}
                            </span>
                            <Show
                              when={linked()}
                              fallback={<span class="wt-linked dim">无关联会话</span>}
                            >
                              <span class="wt-linked" title={linked()!.title}>
                                {linked()!.title}
                              </span>
                            </Show>
                          </div>
                          <div class="wt-row-actions">
                            <Show
                              when={w.ownedBranch !== false}
                              fallback={
                                <span class="wt-delbranch dim" title="该 worktree 直接检出的是已有分支，移除时不会删除分支">
                                  已有分支
                                </span>
                              }
                            >
                              <label class="wt-delbranch">
                                <input
                                  type="checkbox"
                                  checked={!!wtDelBranch()[w.id]}
                                  onChange={(e) =>
                                    setWtDelBranch({
                                      ...wtDelBranch(),
                                      [w.id]: e.currentTarget.checked,
                                    })
                                  }
                                />
                                同时删分支
                              </label>
                            </Show>
                            <button class="btn danger small" onClick={() => void removeWt(w)}>
                              移除
                            </button>
                          </div>
                        </div>
                      );
                    }}
                  </For>
                </div>
              </Show>
            </div>
          </Show>

          {/* ===== Skills ===== */}
          <Show when={tab() === "skills"}>
            <div class="field">
              <span class="field-label">集中目录</span>
              <div class="wt-dir-row">
                <input class="field-input" value={skillsDir()} readonly title={skillsDir()} />
                <button class="btn secondary" onClick={() => void openSkillsDir()}>
                  打开
                </button>
                <button class="btn secondary" disabled={skillsBusy()} onClick={() => void resyncSkills()}>
                  同步
                </button>
              </div>
              <span class="field-hint">
                Skill 统一放在 <code>~/.nova/skills</code>。启动各后端时会以软链接（macOS/Linux）或目录联接（Windows）同步到
                Codex / Claude Code / Cursor / OpenCode / agents 的全局 skills 目录，不拷贝文件。
              </span>
            </div>

            <div
              classList={{
                "skills-drop": true,
                "is-dragging": skillsDragging(),
                busy: skillsBusy(),
              }}
            >
              <div class="skills-drop-title">拖入 zip 或 skill 文件夹</div>
              <div class="skills-drop-hint">也可使用下方按钮选择。每个 skill 需包含 SKILL.md。</div>
              <div class="skills-drop-actions">
                <button class="btn secondary" disabled={skillsBusy()} onClick={() => void pickSkillZip()}>
                  上传 zip…
                </button>
                <button class="btn secondary" disabled={skillsBusy()} onClick={() => void pickSkillFolder()}>
                  选择文件夹…
                </button>
                <button class="link-btn" disabled={skillsLoading()} onClick={() => void refreshSkills()}>
                  刷新
                </button>
              </div>
            </div>

            <Show when={skillsMsg()}>
              <div class="field-hint">{skillsMsg()}</div>
            </Show>

            <div class="field">
              <span class="field-label">已安装（共 {skills().length} 个）</span>
              <Show
                when={skills().length > 0}
                fallback={
                  <div class="sel-empty">{skillsLoading() ? "加载中…" : "暂无 skill，拖入或上传开始管理"}</div>
                }
              >
                <div class="wt-list">
                  <For each={skills()}>
                    {(sk) => (
                      <div class="wt-row">
                        <div class="wt-row-main">
                          <span class="wt-branch" title={sk.name}>
                            {sk.name}
                          </span>
                        </div>
                        <div class="wt-row-sub">
                          <span class="wt-path" title={sk.description || sk.path}>
                            {sk.description || sk.path}
                          </span>
                          <button
                            class="btn danger small"
                            disabled={skillsBusy()}
                            onClick={() => void removeSkillItem(sk)}
                          >
                            删除
                          </button>
                        </div>
                      </div>
                    )}
                  </For>
                </div>
              </Show>
            </div>
          </Show>

          {/* ===== 关于 ===== */}
          <Show when={tab() === "about"}>
            <div class="field">
              <span class="field-label">版本</span>
              <div class="version-row">
                <span>Nova v{version() || "…"}</span>
                <button class="link-btn" disabled={checking()} onClick={() => void checkNow()}>
                  {checking() ? "检查中…" : "检查更新"}
                </button>
                <Show when={checkResult()}>
                  <span class="field-hint">{checkResult()}</span>
                </Show>
              </div>
            </div>

            <div class="field">
              <span class="field-label">会话管理（共 {state.threads.length} 个）</span>
              <button
                class="btn secondary"
                style={{ "align-self": "flex-start" }}
                onClick={() => setManaging(!managing())}
              >
                {managing() ? "收起" : "批量管理会话"}
              </button>
              <Show when={managing()}>
                <div>
                  <div class="tm-toolbar">
                    <label>
                      <input type="checkbox" checked={allSelected()} onChange={toggleAll} />
                      全选
                    </label>
                    <span>已选 {selectedIds().length} 个</span>
                    <span class="tm-spacer" />
                    <button
                      class="btn danger small"
                      disabled={selectedIds().length === 0 || deleting()}
                      onClick={() => void removeSelected()}
                    >
                      {deleting() ? "删除中…" : "删除选中"}
                    </button>
                  </div>
                  <div class="tm-list">
                    <For each={state.threads}>
                      {(t) => {
                        const running = () => !!state.running[t.id];
                        return (
                          <label class={`tm-row ${running() ? "disabled" : ""}`}>
                            <input
                              type="checkbox"
                              disabled={running()}
                              checked={!!sel()[t.id]}
                              onChange={(e) =>
                                setSel({ ...sel(), [t.id]: e.currentTarget.checked })
                              }
                            />
                            <span class="tm-title" title={t.title}>
                              {t.title}
                            </span>
                            <span class={`thread-agent ${t.agentKind}`}>
                              {agentLabel(t.agentKind)}
                            </span>
                            <span class="tm-meta" title={t.cwd}>
                              {threadGroupName(t.cwd)}
                            </span>
                            <Show when={running()}>
                              <span class="tm-running">运行中</span>
                            </Show>
                          </label>
                        );
                      }}
                    </For>
                    <Show when={state.threads.length === 0}>
                      <div class="sel-empty">暂无会话</div>
                    </Show>
                  </div>
                </div>
              </Show>
            </div>

            <div class="field">
              <button class="link-btn" onClick={() => setShowLogs(!showLogs())}>
                {showLogs() ? "隐藏 agent 日志" : `查看 agent 日志（${state.logs.length}）`}
              </button>
              <Show when={showLogs()}>
                <pre class="log-view">
                  <For each={state.logs.slice(-200)}>{(line) => <div>{line}</div>}</For>
                </pre>
              </Show>
            </div>
          </Show>
        </div>

        <div class="modal-foot">
          <button class="btn secondary" onClick={props.onClose}>
            取消
          </button>
          <button class="btn primary" disabled={saving()} onClick={() => void save()}>
            {saving() ? "保存中…" : "保存"}
          </button>
        </div>
      </div>
    </div>
  );
}
