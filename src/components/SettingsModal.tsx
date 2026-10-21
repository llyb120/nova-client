import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { confirm, message, open as openDialog } from "@tauri-apps/plugin-dialog";
import * as QRCode from "qrcode";
import { createEffect, createMemo, createSignal, For, Index, onCleanup, onMount, Show } from "solid-js";
import { api } from "../ipc";
import AppearanceLayoutSettings from './AppearanceLayoutSettings';
import {
  ALL_AGENT_KINDS,
  checkAndStageUpdate,
  deleteThreads,
  enabledAgentKinds,
  installChromeExtensionWithJianlai,
  refreshQuota,
  refreshRelayStatus,
  setState,
  setTheme,
  state,
} from "../store";
import { agentLabel, isScratch, setFileDropBlocked } from "../utils";
import { ModelPicker } from "./ConfigSelects";
import { IconPlus, IconX } from "./icons";
import { LyraConfigPanel } from "./LyraConfigPanel";
import { ProjectPicker } from "./ProjectPicker";
import {
  encodeModelTarget,
  encodeRoamTarget,
  formatShortcutKeys,
  newSessionShortcutId,
  parseModelTarget,
  parseRoamTarget,
  setShortcutRecordingActive,
  withDefaultSessionShortcuts,
} from "../sessionShortcuts";
import type {
  AgentInstructionTarget,
  PromptTrigger,
  AgentKind,
  CliStatus,
  Peer,
  SessionShortcut,
  SessionShortcutAction,
  Settings,
  SkillInfo,
  WorktreeRecord,
} from "../types";
import { SCENES, scenesReady, type SceneKey, type SceneModels } from "../types";

function threadGroupName(cwd: string): string {
  if (isScratch(cwd)) return "临时会话";
  return cwd.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || cwd;
}

function projectPathKey(path: string): string {
  return path.replace(/\\/g, "/").toLowerCase();
}

function resolveShortcutRoam(target: string): { peer: Peer; folder: string } | null {
  const parsed = parseRoamTarget(target);
  if (!parsed) return null;
  const peer = state.peers.find((item) => item.token === parsed.peerToken);
  return {
    peer:
      peer ??
      ({
        token: parsed.peerToken,
        name: "队友",
        online: false,
        folders: [],
        lastSeen: 0,
      } satisfies Peer),
    folder: parsed.folder,
  };
}

function quotaPercent(value: number): number {
  return Math.max(0, Math.min(100, Math.round(value)));
}

const DEFAULT_RELAY_SERVER = "";
const RELAY_SERVER_PLACEHOLDER = "http://127.0.0.1:8320";

/** 点击后按下组合键录制快捷键。 */
function ShortcutKeyCapture(props: {
  value: string;
  onChange: (keys: string) => void;
}) {
  const [recording, setRecording] = createSignal(false);
  let listener: ((event: KeyboardEvent) => void) | null = null;

  const stopRecording = () => {
    if (!listener) return;
    window.removeEventListener("keydown", listener, true);
    window.removeEventListener("blur", stopRecording);
    listener = null;
    setShortcutRecordingActive(false);
    setRecording(false);
  };

  const startRecording = () => {
    stopRecording();
    setShortcutRecordingActive(true);
    setRecording(true);
    listener = (event: KeyboardEvent) => {
      event.preventDefault();
      event.stopPropagation();
      if (event.key === "Escape") {
        stopRecording();
        return;
      }
      const keys = formatShortcutKeys(event);
      if (!keys) return;
      props.onChange(keys);
      stopRecording();
    };
    window.addEventListener("keydown", listener, true);
    window.addEventListener("blur", stopRecording);
  };

  onCleanup(() => stopRecording());

  return (
    <button
      type="button"
      classList={{
        "btn secondary session-shortcut-keys": true,
        recording: recording(),
      }}
      onClick={() => {
        if (recording()) stopRecording();
        else startRecording();
      }}
      onBlur={stopRecording}
      aria-label="录制快捷键"
      title={recording() ? "按下组合键，Esc 取消" : "点击后按下组合键"}
    >
      {recording() ? "按下按键…" : props.value || "录制按键"}
    </button>
  );
}

/** 后端代理输入框（每个后端进程可单独走代理） */
function ProxyField(props: { value: string; onInput: (v: string) => void }) {
  return (
    <label class="backend-proxy-field">
      <span class="field-label">代理</span>
      <input
        class="field-input"
        value={props.value}
        onInput={(e) => props.onInput(e.currentTarget.value)}
        placeholder="留空直连；如 http://127.0.0.1:10808"
      />
    </label>
  );
}

function CliManager(props: {
  status?: CliStatus;
  loading: boolean;
}) {
  const [copied, setCopied] = createSignal(false);
  const command = () => props.status?.installCommand ?? "";
  const copy = () => {
    if (!command()) return;
    void navigator.clipboard.writeText(command()).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    });
  };
  return (
    <div class="cli-manager">
      <div class="cli-manager-main">
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
      </div>
      <Show when={command()}>
        <div class="cli-install-row">
          <code title={command()}>{command()}</code>
          <button type="button" class="btn secondary cli-copy-btn" onClick={copy}>
            {copied() ? "已复制" : "复制"}
          </button>
        </div>
      </Show>
    </div>
  );
}

export type SettingsTab =
  | "general"
  | "scenes"
  | "meteor"
  | "jianlai"
  | "advanced"
  | "lyra"
  | "backends"
  | "instructions"
  | "appearance"
  | "team"
  | "worktree"
  | "skills"
  | "env"
  | "about";

// 左侧导航按用途分组，同类设置挨在一起；group 为空的项单独成段（如「关于」）。
const TABS: { id: SettingsTab; name: string; group: string }[] = [
  { id: "general", name: "通用", group: "常规" },
  { id: "appearance", name: "外观", group: "常规" },
  { id: "advanced", name: "高级", group: "常规" },
  { id: "env", name: "环境变量", group: "常规" },
  { id: "scenes", name: "场景模型", group: "模型与 Agent" },
  { id: "lyra", name: "Lyra", group: "模型与 Agent" },
  { id: "backends", name: "模型后端", group: "模型与 Agent" },
  { id: "instructions", name: "Agent 配置", group: "模型与 Agent" },
  { id: "skills", name: "Skills", group: "模型与 Agent" },
  { id: "meteor", name: "Meteor", group: "工具与协作" },
  { id: "jianlai", name: "剑来", group: "工具与协作" },
  { id: "worktree", name: "Worktree", group: "工具与协作" },
  { id: "team", name: "团队", group: "工具与协作" },
  { id: "about", name: "关于", group: "" },
];

// 首次运行引导：第 1 步配置后端（含 Lyra），第 2 步配置场景模型。
const ONBOARDING_STEPS: { title: string; tabs: SettingsTab[] }[] = [
  { title: "配置模型后端", tabs: ["lyra", "backends"] },
  { title: "配置场景模型", tabs: ["scenes"] },
];

export function SettingsModal(props: { onClose: () => void; initialTab?: SettingsTab; onboarding?: boolean }) {
  const s = state.settings;
  const [step, setStep] = createSignal(0);
  const [chromeDep, setChromeDep] = createSignal<Awaited<ReturnType<typeof api.chromeDependencyStatus>> | null>(null);
  const [chromeDepBusy, setChromeDepBusy] = createSignal(false);
  const checkChromeDep = async () => {
    setChromeDepBusy(true);
    try {
      setChromeDep(await api.chromeDependencyStatus());
    } catch (error) {
      void message(`检测失败：${String(error)}`, { kind: "error" });
    } finally {
      setChromeDepBusy(false);
    }
  };
  const installChromeDep = async () => {
    setChromeDepBusy(true);
    try {
      await installChromeExtensionWithJianlai(chromeDep() ?? (await api.chromeDependencyStatus()));
      props.onClose();
    } catch (error) {
      void message(`启动安装会话失败：${String(error)}`, { kind: "error" });
    } finally {
      setChromeDepBusy(false);
    }
  };
  const visibleTabs = () =>
    props.onboarding ? TABS.filter((t) => ONBOARDING_STEPS[step()].tabs.includes(t.id)) : TABS;
  const [tab, setTab] = createSignal<SettingsTab>(props.onboarding ? "lyra" : props.initialTab ?? "general");
  const [terminalShell, setTerminalShell] = createSignal(s?.terminalShell ?? "");
  const [terminalArgs, setTerminalArgs] = createSignal((s?.terminalArgs ?? []).join("\n"));
  const [claudePath, setClaudePath] = createSignal(s?.claudePath ?? "claude-agent-acp");
  const [claudeProxy, setClaudeProxy] = createSignal(s?.claudeProxy ?? "");
  const [claudeEnabled, setClaudeEnabled] = createSignal(s?.claudeEnabled === true);
  const [kimiPath, setKimiPath] = createSignal(s?.kimiPath ?? "kimi");
  const [kimiProxy, setKimiProxy] = createSignal(s?.kimiProxy ?? "");
  const [kimiEnabled, setKimiEnabled] = createSignal(s?.kimiEnabled === true);
  const [devinPath, setDevinPath] = createSignal(s?.devinPath ?? "devin");
  const [acpArgs, setAcpArgs] = createSignal(s?.acpArgs ?? "acp");
  const [codebuddyPath, setCodebuddyPath] = createSignal(s?.codebuddyPath ?? "codebuddy");
  const [workbuddyPath, setWorkbuddyPath] = createSignal(s?.workbuddyPath ?? "");
  const [codexPath, setCodexPath] = createSignal(s?.codexPath ?? "codex");
  const [codexProxy, setCodexProxy] = createSignal(s?.codexProxy ?? "");
  const [lyraProxy, setLyraProxy] = createSignal(s?.lyraProxy ?? "");
  const [windowsShellShimEnabled, setWindowsShellShimEnabled] = createSignal(
    s?.windowsShellShimEnabled ?? false,
  );
  const [powershellUtf8Enabled, setPowershellUtf8Enabled] = createSignal(
    s?.powershellUtf8Enabled ?? true,
  );
  const [autoChangeProjectEnabled, setAutoChangeProjectEnabled] = createSignal(
    s?.autoChangeProjectEnabled !== false,
  );
  const [ponytailEnabled, setPonytailEnabled] = createSignal(s?.ponytailEnabled ?? true);
  const [checkpointEnabled, setCheckpointEnabled] = createSignal(
    s?.checkpointEnabled ?? false,
  );
  const [contextRetrievalMode, setContextRetrievalMode] = createSignal<"none" | "fast">(
    s?.contextRetrievalMode === "none" ? "none" : "fast",
  );

  const [devinProxy, setDevinProxy] = createSignal(s?.devinProxy ?? "");
  const [codebuddyProxy, setCodebuddyProxy] = createSignal(s?.codebuddyProxy ?? "");
  const [workbuddyProxy, setWorkbuddyProxy] = createSignal(s?.workbuddyProxy ?? "");
  const [sessionShortcuts, setSessionShortcuts] = createSignal<SessionShortcut[]>(
    (s?.sessionShortcuts ?? []).map((item) => ({ ...item })),
  );
  const [meteorEnabled, setMeteorEnabled] = createSignal(s?.meteorEnabled ?? true);
  const [meteorShortcut, setMeteorShortcut] = createSignal(s?.meteorShortcut ?? "Alt+Alt");
  const [meteorClipboard, setMeteorClipboard] = createSignal(s?.meteorClipboard ?? false);
  const [meteorPasteShortcut, setMeteorPasteShortcut] = createSignal(s?.meteorPasteShortcut ?? "Alt+V");
  const [jianpuShortcut, setJianpuShortcut] = createSignal(s?.jianpuShortcut ?? "Alt+R");
  const [tryingMeteor, setTryingMeteor] = createSignal(false);
  const [customEnvVars, setCustomEnvVars] = createSignal<{ name: string; value: string }[]>(
    Object.entries(s?.customEnvVars ?? {}).map(([name, value]) => ({ name, value })),
  );
  const [devinEnabled, setDevinEnabled] = createSignal(s?.devinEnabled !== false);
  const [lyraEnabled, setLyraEnabled] = createSignal(s?.lyraEnabled !== false);
  const [codexEnabled, setCodexEnabled] = createSignal(s?.codexEnabled !== false);
  const [codebuddyEnabled, setCodebuddyEnabled] = createSignal(s?.codebuddyEnabled !== false);
  const [workbuddyEnabled, setWorkbuddyEnabled] = createSignal(s?.workbuddyEnabled === true);
  // 新会话默认固定 Build；Plan 仅由 /plan 启动，不再提供设置项。
  const [sceneModels, setSceneModels] = createSignal<SceneModels>(
    Object.fromEntries(SCENES.map(({ key }) => [key, (s?.sceneModels?.[key] ?? []).map((t) => ({ ...t }))])) as SceneModels,
  );
  const [editor, setEditor] = createSignal(s?.editor ?? "code");
  const [sessionAutoCleanupEnabled, setSessionAutoCleanupEnabled] = createSignal(
    s?.sessionAutoCleanupEnabled ?? false,
  );
  const [sessionAutoCleanupHours, setSessionAutoCleanupHours] = createSignal(
    s?.sessionAutoCleanupHours ?? 24 * 30,
  );
  const [knowledgeGraphEnabled, setKnowledgeGraphEnabled] = createSignal(s?.knowledgeGraphEnabled ?? false);
  const [jevVisionEnabled, setJevVisionEnabled] = createSignal(s?.jevVisionEnabled ?? true);
  const [jevApiKey, setJevApiKey] = createSignal(s?.jevApiKey ?? "");
  const [jevApiUrl, setJevApiUrl] = createSignal(s?.jevApiUrl ?? "");
  const [jevModel, setJevModel] = createSignal(s?.jevModel ?? "");
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
  const [remoteQr, setRemoteQr] = createSignal<{ url: string; dataUrl: string } | null>(null);
  const [roamingFolders, setRoamingFolders] = createSignal<string[]>(state.roamingFolders);
  const [roamingFoldersLoading, setRoamingFoldersLoading] = createSignal(false);
  // 「添加一行」式共享模型选择器的草稿状态：选完即加入列表并复位为空。
  const [globalInstructions, setGlobalInstructions] = createSignal("");
  const [globalInstructionsPath, setGlobalInstructionsPath] = createSignal("");
  const [globalInstructionTargets, setGlobalInstructionTargets] = createSignal<
    AgentInstructionTarget[]
  >([]);
  const [globalInstructionsLoading, setGlobalInstructionsLoading] = createSignal(false);
  const [globalInstructionsBusy, setGlobalInstructionsBusy] = createSignal(false);
  const [globalInstructionsDirty, setGlobalInstructionsDirty] = createSignal(false);
  const [globalInstructionsMsg, setGlobalInstructionsMsg] = createSignal("");
  const [promptTriggers, setPromptTriggers] = createSignal<PromptTrigger[]>([]);
  const editPromptTrigger = (index: number, patch: Partial<PromptTrigger> | null) => {
    setPromptTriggers((list) =>
      patch ? list.map((item, i) => (i === index ? { ...item, ...patch } : item)) : list.filter((_, i) => i !== index),
    );
    setGlobalInstructionsDirty(true);
    setGlobalInstructionsMsg("");
  };
  const [verifying, setVerifying] = createSignal(false);
  const [verifyMsg, setVerifyMsg] = createSignal("");
  const [showLogs, setShowLogs] = createSignal(false);
  const [saving, setSaving] = createSignal(false);
  const [restarting, setRestarting] = createSignal(false);
  const [restartMsg, setRestartMsg] = createSignal("");
  const [environmentRefreshing, setEnvironmentRefreshing] = createSignal(false);
  const [environmentRefreshMsg, setEnvironmentRefreshMsg] = createSignal("");
  const [environmentRefreshFailed, setEnvironmentRefreshFailed] = createSignal(false);
  const [cliStatuses, setCliStatuses] = createSignal<Partial<Record<AgentKind, CliStatus>>>({});
  const [cliLoading, setCliLoading] = createSignal(false);
  const [quotaRefreshing, setQuotaRefreshing] = createSignal(false);

  const reloadQuota = async () => {
    setQuotaRefreshing(true);
    try {
      await refreshQuota();
    } finally {
      setQuotaRefreshing(false);
    }
  };


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

  const refreshEnvironmentVariables = async () => {
    setEnvironmentRefreshing(true);
    setEnvironmentRefreshMsg("");
    setEnvironmentRefreshFailed(false);
    try {
      const count = await api.refreshEnvironmentVariables();
      setEnvironmentRefreshMsg(`已刷新 ${count} 个环境变量`);
    } catch (e) {
      setEnvironmentRefreshFailed(true);
      setEnvironmentRefreshMsg(`刷新失败：${String(e)}`);
    } finally {
      setEnvironmentRefreshing(false);
    }
  };

  // 至少保留一个启用的后端：是最后一个时不允许关闭
  const enabledCount = () =>
    [
      devinEnabled(),
      kimiEnabled(),
      claudeEnabled(),
      lyraEnabled(),
      codexEnabled(),
      codebuddyEnabled(),
      workbuddyEnabled(),
    ].filter(Boolean).length;

  const sceneAgentKinds = createMemo(() => ALL_AGENT_KINDS.filter((kind) => ({
    lyra: lyraEnabled(), devin: devinEnabled(), claude: claudeEnabled(), kimi: kimiEnabled(),
    codex: codexEnabled(), codebuddy: codebuddyEnabled(), workbuddy: workbuddyEnabled(), unsupported: false,
  })[kind]));

  const sceneRowKinds = (kind: AgentKind): AgentKind[] =>
    sceneAgentKinds().includes(kind) ? sceneAgentKinds() : [...sceneAgentKinds(), kind];
  const editScene = (key: SceneKey, edit: (list: SceneModels[SceneKey]) => SceneModels[SceneKey]) =>
    setSceneModels((current) => ({ ...current, [key]: edit(current[key]) }));
  const moveSceneModel = (key: SceneKey, index: number, delta: number) =>
    editScene(key, (list) => {
      const next = [...list];
      const target = index + delta;
      if (target < 0 || target >= next.length) return list;
      [next[index], next[target]] = [next[target], next[index]];
      return next;
    });
  const updateSessionShortcut = (
    index: number,
    patch: Partial<Pick<SessionShortcut, "keys" | "action" | "target">>,
  ) => {
    setSessionShortcuts((current) =>
      current.map((item, itemIndex) => (itemIndex === index ? { ...item, ...patch } : item)),
    );
  };
  const addSessionShortcut = () => {
    setSessionShortcuts((current) => [
      ...current,
      {
        id: newSessionShortcutId(),
        keys: "",
        action: "selectModel",
        target: "",
      },
    ]);
  };
  const removeSessionShortcut = (index: number) => {
    setSessionShortcuts((current) => current.filter((_, itemIndex) => itemIndex !== index));
  };
  const draftSessionShortcuts = (): SessionShortcut[] =>
    sessionShortcuts()
      .map((item): SessionShortcut => ({
        id: item.id || newSessionShortcutId(),
        keys: item.keys.trim(),
        action:
          item.action === "selectProject"
            ? "selectProject"
            : item.action === "newSession"
              ? "newSession"
              : item.action === "openUnread"
                ? "openUnread"
                : item.action === "toggleTerminal" ? "toggleTerminal"
                : item.action === "insertText"
                ? "insertText"
                : "selectModel",
        target: item.target.trim(),
      }))
      .filter((item) => {
        if (!item.keys) return false;
        if (item.action === "newSession" || item.action === "openUnread" || item.action === "toggleTerminal") return true;
        return item.target.length > 0;
      });

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
  const addRoamingFolder = (path: string) => {
    if (!path) return;
    setRoamingFolders((current) => {
      const key = projectPathKey(path);
      return current.some((folder) => projectPathKey(folder) === key) ? current : [...current, path];
    });
  };
  const replaceRoamingFolder = (oldPath: string, newPath: string) => {
    if (!newPath) return;
    const oldKey = projectPathKey(oldPath);
    const newKey = projectPathKey(newPath);
    if (oldKey === newKey) return;
    setRoamingFolders((current) => {
      if (current.some((folder) => projectPathKey(folder) === newKey)) {
        // 目标已在列表中：移除旧行避免重复
        return current.filter((folder) => projectPathKey(folder) !== oldKey);
      }
      return current.map((folder) => (projectPathKey(folder) === oldKey ? newPath : folder));
    });
  };
  const removeRoamingFolderAt = (index: number) => {
    setRoamingFolders((current) => current.filter((_, i) => i !== index));
  };
  /** 本地项目全部已共享时隐藏添加入口，避免重复选择无反馈 */
  const allProjectsShared = createMemo(
    () =>
      state.projects.length > 0 &&
      state.projects.every((project) =>
        roamingFolders().some((folder) => projectPathKey(folder) === projectPathKey(project.path)),
      ),
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
      setPromptTriggers(config.triggers);
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
      const enabledAgentKinds = globalInstructionTargets()
        .filter((target) => target.enabled)
        .map((target) => target.agentKind);
      const config = await api.setGlobalAgentInstructions(
        globalInstructions(),
        enabledAgentKinds,
        promptTriggers(),
      );
      setGlobalInstructions(config.content);
      setGlobalInstructionsPath(config.path);
      setGlobalInstructionTargets(config.targets);
      setPromptTriggers(config.triggers);
      setGlobalInstructionsDirty(false);
      const conflicts = config.targets.filter(
        (target) => target.status === "conflict" || target.status === "error",
      ).length;
      setGlobalInstructionsMsg(
        conflicts > 0
          ? `已同步，其余 ${conflicts} 个冲突入口未覆盖，请检查下方状态。`
          : "已同步到启用的后端；正在运行的 Agent 重启后读取新配置。",
      );
      return config;
    } finally {
      setGlobalInstructionsBusy(false);
    }
  };

  // 后端可用性检测结果：false = 已检测且未找到 CLI（卡片上提示，仍可手动改路径）
  const backendMissing = (kind: string) => state.backendAvailability[kind] === false;

  const verifyRelay = async () => {
    setVerifying(true);
    setVerifyMsg("");
    try {
      const online = await api.verifyRelay(
        relayServer().trim(),
        relayToken().trim(),
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
  const [updateChannel, setUpdateChannel] = createSignal<"release" | "pre-release">(
    s?.updateChannel === "pre-release" ? "pre-release" : "release",
  );
  onMount(() => void getVersion().then(setVersion));
  const changeUpdateChannel = (channel: "release" | "pre-release") => {
    if (channel === updateChannel()) return;
    // 这里只修改设置草稿；取消弹窗不会切换通道，也不会触发下载。
    setUpdateChannel(channel);
    setCheckResult("");
  };
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
    claudePath: claudePath().trim() || "claude-agent-acp",
    claudeProxy: claudeProxy().trim(),
    claudeEnabled: claudeEnabled(),
    kimiPath: kimiPath().trim() || "kimi",
    kimiProxy: kimiProxy().trim(),
    kimiEnabled: kimiEnabled(),
    devinPath: devinPath().trim() || "devin",
    acpArgs: acpArgs().trim() || "acp",
    codebuddyPath: codebuddyPath().trim() || "codebuddy",
    workbuddyPath: workbuddyPath().trim(),
    codexPath: codexPath().trim() || "codex",
    codexProxy: codexProxy().trim(),
    lyraProxy: lyraProxy().trim(),
    windowsShellShimEnabled: windowsShellShimEnabled(),
    powershellUtf8Enabled: powershellUtf8Enabled(),
    autoChangeProjectEnabled: autoChangeProjectEnabled(),
    ponytailEnabled: ponytailEnabled(),
    checkpointEnabled: checkpointEnabled(),
    contextRetrievalMode: contextRetrievalMode(),

    devinProxy: devinProxy().trim(),
    codebuddyProxy: codebuddyProxy().trim(),
    workbuddyProxy: workbuddyProxy().trim(),
    defaultMode: "build",
    sceneModels: sceneModels(),
    editor: editor().trim() || "code",
    terminalShell: terminalShell().trim(),
    terminalArgs: terminalArgs().split(/\r?\n/).filter(arg => arg.length > 0),
    theme: state.theme,
    relayServer: relayServer().trim(),
    relayToken: relayToken().trim(),
    relayGroups: relayGroups().trim(),
    remoteControlEnabled: remoteControlEnabled(),
    modelFavorites: state.settings?.modelFavorites ?? [],
    sessionShortcuts: draftSessionShortcuts(),
    meteorEnabled: meteorEnabled(),
    meteorShortcut: meteorShortcut().trim(),
    meteorClipboard: meteorClipboard(),
    meteorPasteShortcut: meteorPasteShortcut().trim(),
    jianpuShortcut: jianpuShortcut().trim(),
    devinEnabled: devinEnabled(),
    lyraEnabled: lyraEnabled(),
    codexEnabled: codexEnabled(),
    codebuddyEnabled: codebuddyEnabled(),
    workbuddyEnabled: workbuddyEnabled(),
    codexIntegration: "app-server",
    codebuddyIntegration: "acp",
    worktreeDir: worktreeDir().trim(),
    updateChannel: updateChannel(),
    sessionAutoCleanupEnabled: sessionAutoCleanupEnabled(),
    sessionAutoCleanupHours: Math.max(1, Math.floor(sessionAutoCleanupHours() || 24 * 30)),
    knowledgeGraphEnabled: knowledgeGraphEnabled(),
    jevVisionEnabled: jevVisionEnabled(),
    jevApiKey: jevApiKey().trim(),
    jevApiUrl: jevApiUrl().trim(),
    jevModel: jevModel().trim(),
    historyDisplayMode: historyDisplayMode(),
    customEnvVars: Object.fromEntries(
      customEnvVars()
        .map((item) => [item.name.trim(), item.value] as const)
        .filter(([name]) => name.length > 0),
    ),
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

  let backendsLoaded = false;
  createEffect(() => {
    if (tab() !== "backends" || backendsLoaded) return;
    backendsLoaded = true;
    void refreshCliStatuses();
    void reloadQuota();
  });

  createEffect(() => {
    if (tab() !== "team") return;
    void loadRoamingFolders();
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

  // worktree 管理
  const [worktreeDir, setWorktreeDir] = createSignal(s?.worktreeDir ?? "");
  // skills 管理（集中存放 ~/.nova/skills）
  const [skillsDir, setSkillsDir] = createSignal("");
  const [skills, setSkills] = createSignal<SkillInfo[]>([]);
  const [skillsLoading, setSkillsLoading] = createSignal(false);
  const [skillsBusy, setSkillsBusy] = createSignal(false);
  const [skillsDragging, setSkillsDragging] = createSignal(false);
  const [skillsMsg, setSkillsMsg] = createSignal("");
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

  let saveLyraConfig: (() => Promise<void>) | undefined;
  // afterSave 仅供引导第 1 步使用：先落盘后端与 Lyra 配置（此时场景尚未配置），让模型列表加载出来。
  const save = async (afterSave?: () => void) => {
    if (!afterSave && !scenesReady(sceneModels())) {
      setTab("scenes");
      await message("请为每个场景至少配置一个模型后再保存。", { title: "场景模型未配置", kind: "error" });
      return;
    }
    const draftedShortcuts = draftSessionShortcuts();
    const jianpuKeys = jianpuShortcut().trim().toLowerCase();
    if (jianpuKeys && [meteorEnabled() ? meteorShortcut() : "", meteorEnabled() ? meteorPasteShortcut() : "", ...withDefaultSessionShortcuts(draftedShortcuts).map((item) => item.keys)]
      .some((keys) => keys.trim().toLowerCase() === jianpuKeys)) {
      setTab("meteor");
      await message("剑谱快捷键与 Meteor、星移或会话快捷键冲突，请修改后再保存。", { title: "保存设置失败", kind: "error" });
      return;
    }
    if (meteorEnabled()) {
      const keys = meteorShortcut().trim().toLowerCase();
      const pasteKeys = meteorPasteShortcut().trim().toLowerCase();
      const sessionKeys = withDefaultSessionShortcuts(draftedShortcuts).map((item) => item.keys.trim().toLowerCase());
      const error = !keys ? "请为 Meteor 录制快捷键，或恢复连按两次 Alt。"
        : sessionKeys.includes(keys)
          ? "Meteor 快捷键与会话快捷键冲突，请修改后再保存。"
          : pasteKeys && (pasteKeys === keys || sessionKeys.includes(pasteKeys))
            ? "星移快捷键与 Meteor 或会话快捷键冲突，请修改后再保存。" : "";
      if (error) {
        setTab("meteor");
        await message(error, { title: "保存设置失败", kind: "error" });
        return;
      }
    }
    const keyCounts = new Map<string, number>();
    for (const item of draftedShortcuts) {
      const key = item.keys.toLowerCase();
      keyCounts.set(key, (keyCounts.get(key) ?? 0) + 1);
    }
    if ([...keyCounts.values()].some((count) => count > 1)) {
      await message("会话快捷键存在重复按键，请修改后再保存。", {
        title: "保存设置失败",
        kind: "error",
      });
      return;
    }
    setSaving(true);
    const settings = draftSettings();
    settings.relayToken = relayToken().trim();
    const updateChannelChanged =
      settings.updateChannel !== (state.settings?.updateChannel ?? "release");
    const shellShimChanged =
      settings.windowsShellShimEnabled !== (state.settings?.windowsShellShimEnabled ?? false);
    try {
      // 先写 Lyra 配置：校验失败时整体中止，不会只保存一半。
      await saveLyraConfig?.();
      await api.setSettings(settings);
      setState("settings", settings);
      // 只有设置成功持久化后才确认通道切换；检查与下载在后台进行，不阻塞保存或关闭弹窗。
      if (updateChannelChanged) {
        void checkAndStageUpdate().catch((error) =>
          console.error("Background update after channel switch failed", error),
        );
      }
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
      (afterSave ?? props.onClose)();
    } catch (error) {
      await message(String(error), { title: "保存设置失败", kind: "error" });
    } finally {
      setSaving(false);
    }
  };

  const openRemoteQr = async () => {
    const server = relayServer().trim().replace(/\/+$/, "");
    const token = relayToken().trim();
    if (!server || !token) {
      await message("请先填写中转站地址和身份 token。", {
        title: "无法生成远控二维码",
        kind: "warning",
      });
      return;
    }
    // 必须用 /remote/（带尾斜杠）：服务端把 /remote 307 到 /remote/ 时不会保留 query，token 会被丢掉。
    const url = `${server}/remote/?token=${encodeURIComponent(token)}`;
    try {
      const dataUrl = await QRCode.toDataURL(url, {
        width: 280,
        margin: 1,
        errorCorrectionLevel: "M",
      });
      setRemoteQr({ url, dataUrl });
    } catch (error) {
      await message(String(error), { title: "生成远控二维码失败", kind: "error" });
    }
  };

  return (
    <div class="modal-backdrop">
      <div class="modal settings-modal">
        <div class="modal-head">
          <span>
            {props.onboarding
              ? `欢迎使用 Nova · 第 ${step() + 1}/${ONBOARDING_STEPS.length} 步：${ONBOARDING_STEPS[step()].title}`
              : "设置"}
          </span>
          <Show when={!props.onboarding}>
            <button class="icon-btn" onClick={props.onClose}>
              <IconX size={16} />
            </button>
          </Show>
        </div>

        <nav class="settings-tabs" aria-label="设置分类">
          <For each={visibleTabs()}>
            {(t, i) => (
              <>
                <Show when={i() === 0 || visibleTabs()[i() - 1].group !== t.group}>
                  <div class="settings-tab-group" classList={{ gap: !t.group }}>{t.group}</div>
                </Show>
                <button
                  type="button"
                  classList={{ "settings-tab": true, active: tab() === t.id }}
                  aria-current={tab() === t.id ? "page" : undefined}
                  onClick={() => setTab(t.id)}
                >
                  {t.name}
                </button>
              </>
            )}
          </For>
        </nav>

        <div class="modal-body">
          <Show when={props.onboarding}>
            <p class="settings-group-desc">
              {step() === 0
                ? "先启用并配置至少一个能提供模型的后端：在 Lyra 中添加模型服务商，或在「模型后端」中启用已安装并登录的 Claude Code、Codex 等。点「下一步」会保存并启动这些后端。"
                : "为每个场景选择至少一个模型。后端刚启动时模型列表可能需要稍等；仍没有可选模型时请返回上一步检查后端配置。以后可在「设置」中修改。"}
            </p>
          </Show>
          <Show when={tab() === "jianlai"}>
            <section class="settings-group">
              <h3 class="settings-group-title">剑来 · Chrome 依赖</h3>
              <p class="settings-group-desc">
                chrome 工具需要本机安装 Google Chrome 并加载 Nova 自带的 Chrome 扩展。自动安装会用上一次会话的模型在临时目录开启新会话，由剑来操作 Chrome 完成安装并开启无痕模式。
              </p>
              <div class="field">
                <div style={{ display: "flex", gap: "8px", "flex-wrap": "wrap" }}>
                  <button type="button" class="btn secondary" disabled={chromeDepBusy()} onClick={() => void checkChromeDep()}>检测 Chrome 依赖</button>
                  <button type="button" class="btn primary" disabled={chromeDepBusy() || chromeDep()?.chromePath === null} onClick={() => void installChromeDep()}>用剑来自动安装</button>
                </div>
                <Show when={chromeDep()}>
                  {(dep) => (
                    <span class="field-hint">
                      Chrome：{dep().chromePath ?? "未找到，请先安装 Google Chrome"}
                      <br />
                      Nova Chrome 扩展：{dep().connected ? "已连接" : "未连接"}
                      {dep().connected ? `（${dep().incognitoAllowed ? "已允许无痕" : "未允许无痕"}）` : ""}
                      <br />
                      扩展目录：{dep().extensionDir}
                    </span>
                  )}
                </Show>
              </div>
            </section>
          </Show>
          <Show when={tab() === "meteor"}>
            <section class="settings-group">
              <h3 class="settings-group-title">Meteor · 屏幕上的操作助手</h3>
              <p class="settings-group-desc">
                触发后立即显示实时透明标注层，可跨屏添加多个点或框选区域，并分别填写操作要求；输入后直接点击另一处即可自动新增标注。一次提交后 Nova 会在后台新建临时目录会话，桌面应用使用剑来，Chrome 网页使用 chrome，Nova 原生浏览器使用 webview；不切换当前会话。
              </p>
              <div class="field">
                <span class="field-label">启用 Meteor</span>
                <label class="backend-switch">
                  <input type="checkbox" checked={meteorEnabled()} onChange={(e) => setMeteorEnabled(e.currentTarget.checked)} />
                  <span>启用全局触发</span>
                </label>
                <span class="field-hint">同一台电脑运行 dev 实例时，非 dev 实例的 Meteor 自动停用，避免争抢快捷键；dev 退出后恢复。</span>
              </div>
              <div class="field">
                <span class="field-label">全局快捷键</span>
                <div style={{ display: "flex", gap: "8px", "flex-wrap": "wrap" }}>
                  <ShortcutKeyCapture value={meteorShortcut() === "Alt+Alt" ? "连按两次 Alt" : meteorShortcut()} onChange={setMeteorShortcut} />
                  <button type="button" class="btn secondary" onClick={() => setMeteorShortcut("Alt+Alt")}>恢复双 Alt</button>
                </div>
                <span class="field-hint">目前支持 Windows。默认快速按下并松开两次 Alt；点击上方按钮可录制其他组合键（需包含 Ctrl、Alt、Shift 或 Win，以系统全局快捷键注册），Esc 取消录制。若组合键已被其他程序占用会提示注册失败，请换一个。</span>
              </div>
              <div class="field">
                <span class="field-label">星移（StartShift）快捷键</span>
                <div style={{ display: "flex", gap: "8px", "flex-wrap": "wrap" }}>
                  <ShortcutKeyCapture value={meteorPasteShortcut() || "已关闭"} onChange={setMeteorPasteShortcut} />
                  <button type="button" class="btn secondary" onClick={() => setMeteorPasteShortcut("Alt+V")}>恢复 Alt+V</button>
                  <button type="button" class="btn secondary" onClick={() => setMeteorPasteShortcut("")}>关闭</button>
                </div>
                <span class="field-hint">按下后弹出要求输入框，由剑来把剪贴板内容按要求粘贴到当前焦点处；不填要求则自动推断。需为带修饰键的组合键，且不能与 Meteor 或会话快捷键相同。</span>
              </div>
              <div class="field">
                <span class="field-label">剑谱录制快捷键</span>
                <div style={{ display: "flex", gap: "8px", "flex-wrap": "wrap" }}>
                  <ShortcutKeyCapture value={jianpuShortcut() || "已关闭"} onChange={setJianpuShortcut} />
                  <button type="button" class="btn secondary" onClick={() => setJianpuShortcut("Alt+R")}>恢复 Alt+R</button>
                  <button type="button" class="btn secondary" onClick={() => setJianpuShortcut("")}>关闭</button>
                </div>
                <span class="field-hint">目前支持 Windows。按一次开始录制你的桌面操作（暖金边框 + 带录制圆点的箭头指针），再按一次停止；也可在 Butterfly 悬停面板里开始/停止。注册为全局快捷键后，其他程序自己的同名快捷键（如 Word 的 Alt+R）会被占用。录制的打字内容按原文保存在本机。</span>
              </div>
              <div class="field">
                <span class="field-label">最近剪贴板</span>
                <label class="backend-switch">
                  <input type="checkbox" checked={meteorClipboard()} onChange={(e) => setMeteorClipboard(e.currentTarget.checked)} />
                  <span>提交时附加最近的剪贴板</span>
                </label>
                <span class="field-hint">默认关闭。开启后把触发时剪贴板里的文字、图片或文件路径作为参考一并提交，仅取当前一条，不记录历史；关闭时不读取剪贴板。</span>
              </div>
              <p class="settings-group-desc">
                提交时截取所有屏幕并提供完整截图，标注仅提示大致位置，不会裁掉周边或未标注屏幕。目标明确时直接执行，不因未指定逐字措辞或操作步骤而重复询问；只要求草稿时不会发送。请避免在敏感内容可见（或开启剪贴板附加且敏感内容位于剪贴板）时使用。
              </p>
              <button type="button" class="btn secondary" disabled={tryingMeteor()} onClick={async () => {
                setTryingMeteor(true);
                try { await api.openMeteor(); }
                catch (error) { await message(String(error), { title: "Meteor", kind: "error" }); }
                finally { setTryingMeteor(false); }
              }}>{tryingMeteor() ? "正在打开标注层…" : "试用已保存配置"}</button>
              <span class="field-hint">更改快捷键后，请先保存设置再试用；执行模型使用「场景模型 → 轻量化」。</span>
            </section>
          </Show>
          <Show when={tab() === "scenes"}>
            <section class="settings-group">
              <h3 class="settings-group-title">场景模型</h3>
              <p class="settings-group-desc">
                每个场景按顺序使用第一个可用的模型，失效时自动切换到下一个。自动模式会话以轻量化模型开局，由它把整块编码或剑来工作转交给对应场景；轻量化场景还用于标题、摘要、上下文压缩、/stage、Butterfly、Meteor 和数字员工。每个场景至少配置一个模型。
              </p>
              <For each={SCENES}>
                {(scene) => (
                  <div class="field">
                    <span class="field-label">{scene.label}</span>
                    <span class="field-hint">{scene.hint}</span>
                    <div class="share-list">
                      <Index each={sceneModels()[scene.key]}>
                        {(target, index) => (
                          <div class="share-row stage-model-row">
                            <span style={{ width: "28px", "flex-shrink": 0 }}>{index + 1}</span>
                            <ModelPicker
                              agentKind={target().agentKind}
                              agentKinds={sceneRowKinds(target().agentKind)}
                              model={target().model}
                              allowDefault
                              defaultLabel={`${agentLabel(target().agentKind)} 默认模型`}
                              onPickModel={(agentKind, model) =>
                                editScene(scene.key, (list) => list.map((t, i) => (i === index ? { agentKind, model } : t)))
                              }
                              title={`${scene.label}模型 ${index + 1}`}
                              portal
                            />
                            <button type="button" class="icon-btn" title="上移" aria-label="上移" disabled={index === 0}
                              onClick={() => moveSceneModel(scene.key, index, -1)}>↑</button>
                            <button type="button" class="icon-btn share-row-remove" title="移除该模型" aria-label="移除该模型"
                              onClick={() => editScene(scene.key, (list) => list.filter((_, i) => i !== index))}>
                              <IconX size={14} />
                            </button>
                          </div>
                        )}
                      </Index>
                      <Show when={sceneModels()[scene.key].length === 0}>
                        <div class="share-empty">尚未配置，至少需要一个模型。</div>
                      </Show>
                      <Show when={sceneAgentKinds().length > 0}>
                        <div class="share-row share-row-add stage-model-row">
                          <span style={{ width: "28px", "flex-shrink": 0 }}>{sceneModels()[scene.key].length + 1}</span>
                          <ModelPicker
                            agentKind={sceneAgentKinds()[0]}
                            agentKinds={sceneAgentKinds()}
                            model=""
                            allowDefault
                            defaultLabel="选择并添加模型"
                            onPickModel={(agentKind, model) => {
                              if (model) editScene(scene.key, (list) => [...list, { agentKind, model }]);
                            }}
                            title={`添加${scene.label}模型`}
                            portal
                          />
                          <span class="share-row-plus" title="选择模型后自动加入列表">
                            <IconPlus size={13} />
                          </span>
                        </div>
                      </Show>
                    </div>
                  </div>
                )}
              </For>
            </section>
            <section class="settings-group">
              <h3 class="settings-group-title">jev 视觉决策（可选）</h3>
              <p class="settings-group-desc">
                浏览器 run 在本地拿不准控件或结果时，带截图问 jev 选择和核验；启用时同一标签连续 2 次逐步点击/填写后要求改用 run。
                启用后按「中转 → 本地 → 关闭」选择：已配置中转地址和 token 时优先使用中转；否则使用下方本地配置（Key 留空复用 Lyra 的 Command Code），均未配置则不调用 jev。
              </p>
              <label class="backend-switch">
                <input type="checkbox" checked={jevVisionEnabled()} onChange={(e) => setJevVisionEnabled(e.currentTarget.checked)} />
                <span>启用 jev</span>
              </label>
              <Show when={jevVisionEnabled()}>
                <label class="field">
                  <span class="field-label">本地 API Key</span>
                  <input class="field-input" value={jevApiKey()} placeholder="留空复用 Command Code" onInput={(e) => setJevApiKey(e.currentTarget.value)} />
                  <span class="field-hint">下方配置仅在未配置中转时使用。中转请求使用 gzip 压缩、模型 typesafe/jev-1.14.0；本地填写 Key 后默认使用 https://api.typesafe.ai/v1/systemone、模型 jev-1.14.0。</span>
                </label>
                <label class="field">
                  <span class="field-label">API 地址</span>
                  <input class="field-input" value={jevApiUrl()} placeholder="留空使用默认 /systemone 地址" onInput={(e) => setJevApiUrl(e.currentTarget.value)} />
                </label>
                <label class="field">
                  <span class="field-label">模型</span>
                  <input class="field-input" value={jevModel()} placeholder="留空：Command Code 用 typesafe/jev-1.14.0，直连用 jev-1.14.0" onInput={(e) => setJevModel(e.currentTarget.value)} />
                </label>
              </Show>
            </section>
          </Show>
          {/* ===== 通用 ===== */}
          <Show when={tab() === "general"}>
            <section class="settings-group">
              <h3 class="settings-group-title">会话</h3>
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

              <label class="field">
                <span class="field-label">默认终端</span>
                <input class="field-input" value={terminalShell()} onInput={e => setTerminalShell(e.currentTarget.value)} placeholder="系统默认（pwsh.exe / cmd.exe / /bin/bash 等）" />
                <span class="field-hint">填写 shell 程序或绝对路径，不含参数或外层引号；留空使用系统默认。新标签在当前会话或首页已选项目目录启动，修改配置不影响已运行的标签。</span>
              </label>
              <label class="field">
                <span class="field-label">终端启动参数（每行一个）</span>
                <textarea class="field-input" rows={3} value={terminalArgs()} onInput={e => setTerminalArgs(e.currentTarget.value)} placeholder="例如 -NoLogo 或 -l" />
                <span class="field-hint">每行原样作为一个参数，带空格的路径无需引号。配置的是 PowerShell、CMD、Bash、Zsh 等 shell，不是外部终端窗口程序。</span>
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
                  仅清理普通会话；超时后先移入回收站，保留同样时长才彻底删除。周六、周日不计入保留时长，但仍会执行清理检查。
                </span>
              </label>

              <div class="session-shortcut-config">
                <div class="session-shortcut-head">
                  <div class="session-shortcut-copy">
                    <div class="field-label">会话快捷键</div>
                    <div class="field-hint">
                      一键切换项目/模型、快速新会话，或向输入框插入文本。新会话页项目与模型均生效；会话中仅模型切换有效；快速新会话任意页可用；快捷输入仅在会话输入框聚焦时生效。「快速新会话」「打开未读消息」会注册为全局快捷键，程序最小化或失焦时也能触发。默认 Ctrl+`（~ 键）打开或收起终端，可添加「打开 / 收起终端」动作修改按键。
                    </div>
                  </div>
                  <button
                    type="button"
                    class="btn secondary session-shortcut-add"
                    onClick={addSessionShortcut}
                  >
                    添加
                  </button>
                </div>
                <Show
                  when={sessionShortcuts().length > 0}
                  fallback={<div class="session-shortcut-empty">暂无快捷键，点击右上角添加。</div>}
                >
                  <div class="session-shortcut-list">
                    <div class="session-shortcut-cols" aria-hidden="true">
                      <span>按键</span>
                      <span>动作</span>
                      <span>目标</span>
                      <span />
                    </div>
                    <Index each={sessionShortcuts()}>
                      {(item, index) => {
                        const modelTarget = () =>
                          item().action === "selectModel" ? parseModelTarget(item().target) : null;
                        const roamTarget = () =>
                          item().action === "selectProject" ? resolveShortcutRoam(item().target) : null;
                        return (
                          <div class="session-shortcut-row">
                            <ShortcutKeyCapture
                              value={item().keys}
                              onChange={(keys) => updateSessionShortcut(index, { keys })}
                            />
                            <select
                              class="field-input session-shortcut-action"
                              value={item().action}
                              onChange={(event) => {
                                const action = event.currentTarget.value as SessionShortcutAction;
                                const nextAction: SessionShortcutAction =
                                  action === "selectProject"
                                    ? "selectProject"
                                    : action === "newSession"
                                      ? "newSession"
                                      : action === "openUnread"
                                        ? "openUnread"
                                        : action === "toggleTerminal" ? "toggleTerminal"
                                        : action === "insertText"
                                        ? "insertText"
                                        : "selectModel";
                                updateSessionShortcut(index, {
                                  action: nextAction,
                                  target: "",
                                });
                              }}
                              aria-label="快捷键动作"
                            >
                              <option value="selectModel">选择模型</option>
                              <option value="selectProject">选择项目</option>
                              <option value="newSession">快速新会话</option>
                              <option value="openUnread">未读 / 进行中会话</option>
                              <option value="toggleTerminal">打开 / 收起终端</option>
                              <option value="insertText">快捷输入</option>
                            </select>
                            <div class="session-shortcut-target">
                              <Show
                                when={
                                  item().action === "newSession" ||
                                  item().action === "openUnread" ||
                                  item().action === "toggleTerminal"
                                }
                                fallback={
                                  <Show
                                    when={item().action === "insertText"}
                                    fallback={
                                      <Show
                                        when={item().action === "selectProject"}
                                        fallback={
                                          <ModelPicker
                                            agentKind={modelTarget()?.agentKind ?? (enabledAgentKinds()[0] ?? "lyra")}
                                            agentKinds={enabledAgentKinds()}
                                            model={modelTarget()?.model ?? ""}
                                            onPickModel={(agentKind, model) =>
                                              updateSessionShortcut(index, {
                                                target: encodeModelTarget(agentKind, model),
                                              })
                                            }
                                            title="快捷键目标模型"
                                            portal
                                          />
                                        }
                                      >
                                        <ProjectPicker
                                          value={roamTarget() ? "" : item().target}
                                          roam={roamTarget()}
                                          onChange={(path) => updateSessionShortcut(index, { target: path })}
                                          onPickRoaming={(peer, folder) =>
                                            updateSessionShortcut(index, {
                                              target: encodeRoamTarget(peer.token, folder),
                                            })
                                          }
                                          portal
                                        />
                                      </Show>
                                    }
                                  >
                                    <input
                                      class="field-input"
                                      type="text"
                                      value={item().target}
                                      placeholder="要插入的文本"
                                      onInput={(event) =>
                                        updateSessionShortcut(index, { target: event.currentTarget.value })
                                      }
                                      aria-label="快捷输入内容"
                                    />
                                  </Show>
                                }
                              >
                                <div class="session-shortcut-target-none">
                                  {item().action === "openUnread"
                                    ? "优先未读；无未读时循环切换进行中的会话"
                                    : item().action === "toggleTerminal" ? "任意本地页 · 打开 / 收起右侧终端"
                                    : "任意页 · 继承当前目录与模型"}
                                </div>
                              </Show>
                            </div>
                            <button
                              type="button"
                              class="icon-btn session-shortcut-remove"
                              onClick={() => removeSessionShortcut(index)}
                              aria-label="删除会话快捷键"
                              title="删除"
                            >
                              <IconX size={14} />
                            </button>
                          </div>
                        );
                      }}
                    </Index>
                  </div>
                </Show>
              </div>
            </section>

            <section class="settings-group">
              <h3 class="settings-group-title">环境变量</h3>
              <div class="field">
                <span class="field-label">刷新 Windows 环境变量</span>
                <button
                  type="button"
                  class="btn secondary"
                  style={{ "align-self": "flex-start" }}
                  disabled={environmentRefreshing()}
                  onClick={() => void refreshEnvironmentVariables()}
                >
                  {environmentRefreshing() ? "刷新中…" : "刷新环境变量"}
                </button>
                <Show when={environmentRefreshMsg()}>
                  <span class={`relay-verify ${environmentRefreshFailed() ? "bad" : "ok"}`}>
                    {environmentRefreshMsg()}
                  </span>
                </Show>
                <span class="field-hint">
                  从 Windows 注册表重新读取系统和当前用户环境变量，并覆盖 Nova
                  启动时继承的同名变量。之后新启动的 agent 进程会使用新值。
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
              <h3 class="settings-group-title">知识图谱</h3>
              <label class="backend-switch">
                <input type="checkbox" checked={knowledgeGraphEnabled()} onChange={(e) => setKnowledgeGraphEnabled(e.currentTarget.checked)} />
                <span>显示知识图谱</span>
              </label>
              <p class="field-hint">默认关闭。开启后在侧栏显示入口，以向四周展开的操作树查看已记录的路径、下一步分支和可完成的目标。</p>
            </section>
            <section class="settings-group">
              <h3 class="settings-group-title">上下文机制</h3>
              <label class="field">
                <span class="field-label">代码上下文检索</span>
                <select
                  class="field-input"
                  value={contextRetrievalMode()}
                  onChange={(e) =>
                    setContextRetrievalMode(e.currentTarget.value === "none" ? "none" : "fast")
                  }
                >
                  <option value="none">无</option>
                  <option value="fast">FastContext（默认）</option>
                </select>
                <span class="field-hint">
                  FastContext 使用原生常驻服务和增量索引。保存后重启相关 Agent 生效。
                </span>
              </label>

            </section>

            <section class="settings-group">
              <h3 class="settings-group-title">Agent 行为</h3>
              <div class="field">
                <span class="field-label">自动更换项目</span>
                <label class="backend-switch">
                  <input
                    type="checkbox"
                    checked={autoChangeProjectEnabled()}
                    onChange={(e) => setAutoChangeProjectEnabled(e.currentTarget.checked)}
                  />
                  <span>启用</span>
                </label>
                <span class="field-hint">
                  默认开启，支持 Lyra 和 CodeBuddy。切换后会更新会话目录及所属项目；CodeBuddy 会恢复会话并继续任务。关闭后不提供目录切换工具。
                </span>
              </div>
              <div class="field">
                <span class="field-label">ponytail 极简模式</span>
                <label class="backend-switch">
                  <input
                    type="checkbox"
                    checked={ponytailEnabled()}
                    onChange={(e) => setPonytailEnabled(e.currentTarget.checked)}
                  />
                  <span>启用</span>
                </label>
                <span class="field-hint">
                  默认开启。让 Lyra 以“懒惰资深工程师”方式工作：能不写就不写，优先复用现有代码/标准库/原生能力，最小 diff、最少文件。保存后对新会话生效。
                </span>
              </div>
              <div class="field">
                <span class="field-label">PowerShell UTF-8 输出</span>
                <label class="backend-switch">
                  <input
                    type="checkbox"
                    checked={powershellUtf8Enabled()}
                    onChange={(e) => setPowershellUtf8Enabled(e.currentTarget.checked)}
                  />
                  <span>启用</span>
                </label>
                <span class="field-hint">
                  默认开启。Windows 下 Lyra 的 bash 工具按 UTF-8 捕获 PowerShell 输出，修复中文等字符出现「?」或乱码的问题。保存后对新会话生效。
                </span>
              </div>
            </section>

            <section class="settings-group">
              <h3 class="settings-group-title">世界线</h3>
              <div class="field">
                <span class="field-label">启用 Checkpoint</span>
                <label class="backend-switch">
                  <input
                    type="checkbox"
                    checked={checkpointEnabled()}
                    onChange={(e) => setCheckpointEnabled(e.currentTarget.checked)}
                  />
                  <span>启用</span>
                </label>
                <span class="field-hint">
                  开启后，从其他时间线继续对话时会把工作目录文件还原到对应时间点。默认关闭；关闭时只切换会话历史，不修改当前文件。
                </span>
              </div>
            </section>

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

          {/* ===== Lyra（常驻挂载，切换 tab 不丢草稿） ===== */}
          <div class="settings-tab-pane lyra-settings" hidden={tab() !== "lyra"}>
            <div class="lyra-overview">
              <div class="lyra-section-head">
                <div class="lyra-heading">
                  <h3>Lyra</h3>
                  <p class="field-hint">连接模型服务，使用本机 Skills 开始工作。</p>
                </div>
                <label class="backend-switch">
                  <input
                    type="checkbox"
                    checked={lyraEnabled()}
                    disabled={lyraEnabled() && enabledCount() === 1}
                    onChange={(e) => setLyraEnabled(e.currentTarget.checked)}
                  />
                  <span>启用 Lyra</span>
                </label>
              </div>
              <details class="lyra-advanced">
                <summary>网络代理<span class="field-hint">{lyraProxy().trim() ? "已配置" : "默认直连"}</span></summary>
                <div class="lyra-detail-body">
                  <ProxyField value={lyraProxy()} onInput={setLyraProxy} />
                  <span class="field-hint">作为所有 Lyra 服务商的默认代理，可在服务商设置中单独覆盖。</span>
                </div>
              </details>
            </div>

            <LyraConfigPanel onSaver={(fn) => (saveLyraConfig = fn)} />
          </div>

          {/* ===== 模型后端 ===== */}
          <Show when={tab() === "backends"}>
            <p class="field-hint">
              每个后端可单独启用/关闭并配置启动方式。关闭的后端不会出现在新建/切换会话的后端列表里（历史会话仍可打开查看）。
            </p>

            <div class="backend-card">
              <div class="backend-card-head">
                <span class="agent-badge claude">Claude Code</span>
                <span class="fixed-integration">ACP</span>
                <Show when={backendMissing("claude")}><span class="backend-missing">未检测到 ACP adapter</span></Show>
                <label class="backend-switch">
                  <input type="checkbox" checked={claudeEnabled()} disabled={claudeEnabled() && enabledCount() === 1} onChange={(e) => setClaudeEnabled(e.currentTarget.checked)} />
                  <span>启用</span>
                </label>
              </div>
              <CliManager status={cliStatuses().claude} loading={cliLoading()} />
              <div class="backend-fields">
                <label class="backend-field">
                  <span class="field-label">ACP 可执行文件</span>
                  <input class="field-input" value={claudePath()} onInput={(e) => setClaudePath(e.currentTarget.value)} placeholder="claude-agent-acp" />
                </label>
              </div>
              <ProxyField value={claudeProxy()} onInput={setClaudeProxy} />
              <p class="field-hint">需要 Node.js 22+。安装 ACP adapter，并先通过 Claude Code 登录，或在环境变量中配置 ANTHROPIC_API_KEY；此处填写 claude-agent-acp，而不是 claude。Nova 工具通过会话 MCP 自动挂载。</p>
            </div>

            <div class="backend-card">
              <div class="backend-card-head">
                <span class="agent-badge kimi">Kimi Code</span>
                <span class="fixed-integration">ACP</span>
                <Show when={backendMissing("kimi")}><span class="backend-missing">未检测到 CLI</span></Show>
                <label class="backend-switch">
                  <input type="checkbox" checked={kimiEnabled()} disabled={kimiEnabled() && enabledCount() === 1} onChange={(e) => setKimiEnabled(e.currentTarget.checked)} />
                  <span>启用</span>
                </label>
              </div>
              <CliManager status={cliStatuses().kimi} loading={cliLoading()} />
              <div class="backend-fields">
                <label class="backend-field">
                  <span class="field-label">可执行文件</span>
                  <input class="field-input" value={kimiPath()} onInput={(e) => setKimiPath(e.currentTarget.value)} placeholder="kimi" />
                </label>
              </div>
              <ProxyField value={kimiProxy()} onInput={setKimiProxy} />
              <p class="field-hint">安装后先在终端运行 kimi，通过 /login 登录，再启用并保存。Windows 需要 Git for Windows；自定义 Git Bash 路径可在环境变量中设置 KIMI_SHELL_PATH。</p>
            </div>

            <div class="backend-card">
              <div class="backend-card-head">
                <span class={`agent-badge devin`}>{agentLabel("devin")}</span>
                <span class="fixed-integration">ACP</span>
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
              <CliManager status={cliStatuses().devin} loading={cliLoading()} />
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
              <ProxyField value={devinProxy()} onInput={setDevinProxy} />
              <div class="backend-quota-row">
                <span class="field-label">额度</span>
                <Show
                  when={state.quota}
                  fallback={<span class="field-hint">{quotaRefreshing() ? "读取中…" : "暂不可用"}</span>}
                >
                  <span classList={{ "backend-quota-value": true, low: state.quota!.dailyPercent < 20 }}>
                    日 {quotaPercent(state.quota!.dailyPercent)}%
                  </span>
                  <span classList={{ "backend-quota-value": true, low: state.quota!.weeklyPercent < 20 }}>
                    周 {quotaPercent(state.quota!.weeklyPercent)}%
                  </span>
                  <Show when={state.quota!.flexCredits != null}>
                    <span class="backend-quota-value">积分 {state.quota!.flexCredits}</span>
                  </Show>
                </Show>
                <button
                  type="button"
                  class="link-btn backend-quota-refresh"
                  disabled={quotaRefreshing()}
                  onClick={() => void reloadQuota()}
                >
                  {quotaRefreshing() ? "刷新中…" : "刷新"}
                </button>
              </div>
            </div>

            <div class="backend-card">
              <div class="backend-card-head">
                <span class={`agent-badge codebuddy`}>{agentLabel("codebuddy")}</span>
                <span class="fixed-integration">SDK</span>
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
              <CliManager status={cliStatuses().codebuddy} loading={cliLoading()} />
              <div class="backend-fields">
                <label class="backend-field">
                  <span class="field-label">可执行文件</span>
                  <input class="field-input" value={codebuddyPath()} onInput={(e) => setCodebuddyPath(e.currentTarget.value)} placeholder="codebuddy" />
                </label>
              </div>
              <ProxyField value={codebuddyProxy()} onInput={setCodebuddyProxy} />
            </div>

            <div class="backend-card">
              <div class="backend-card-head">
                <span class="agent-badge workbuddy">{agentLabel("workbuddy")}</span>
                <span class="fixed-integration">ACP</span>
                <Show when={backendMissing("workbuddy")}>
                  <span class="backend-missing">未检测到 CLI</span>
                </Show>
                <label class="backend-switch">
                  <input
                    type="checkbox"
                    checked={workbuddyEnabled()}
                    disabled={workbuddyEnabled() && enabledCount() === 1}
                    onChange={(e) => setWorkbuddyEnabled(e.currentTarget.checked)}
                  />
                  <span>启用</span>
                </label>
              </div>
              <CliManager status={cliStatuses().workbuddy} loading={cliLoading()} />
              <div class="backend-fields">
                <label class="backend-field">
                  <span class="field-label">WorkBuddy 路径</span>
                  <input class="field-input" value={workbuddyPath()} onInput={(e) => setWorkbuddyPath(e.currentTarget.value)} placeholder="留空自动查找 WorkBuddy" />
                </label>
              </div>
              <ProxyField value={workbuddyProxy()} onInput={setWorkbuddyProxy} />
              <p class="field-hint">直接启动 WorkBuddy 内置的 CodeBuddy ACP，复用其登录态，无需 WorkBuddy 保持运行；需先在 WorkBuddy 中登录过一次。留空自动查找 Windows 默认安装目录，也可填写安装目录、WorkBuddy.exe 或内置 CLI 路径；不会自动启动桌面程序。当前支持 Windows。</p>
            </div>

            <div class="backend-card">
              <div class="backend-card-head">
                <span class={`agent-badge codex`}>{agentLabel("codex")}</span>
                <span class="fixed-integration">app-server</span>
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
              <CliManager status={cliStatuses().codex} loading={cliLoading()} />
              <div class="backend-fields">
                <label class="backend-field">
                  <span class="field-label">可执行文件</span>
                  <input class="field-input" value={codexPath()} onInput={(e) => setCodexPath(e.currentTarget.value)} placeholder="codex" />
                </label>
              </div>
              <ProxyField value={codexProxy()} onInput={setCodexProxy} />
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
                只维护这一份内容；Nova 会按已启用后端的原生规则入口分别适配。已有真实配置文件会保留原内容，只更新 Nova 托管区块。
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
                  onClick={() => void syncGlobalInstructions().catch((error) => setGlobalInstructionsMsg(`同步失败：${String(error)}`))}
                >
                  {globalInstructionsBusy() ? "同步中…" : "保存并同步"}
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
              <span class="field-label">提示词触发器</span>
              <span class="field-hint">
                类似 skill 的渐进披露：各后端只预载「触发条件」和正文文件路径，条件命中时 Agent 再读取并遵循对应提示词。随「保存并同步」一起写入。
              </span>
              <Index each={promptTriggers()}>
                {(trigger, index) => (
                  <div class="wt-row">
                    <div class="wt-row-main">
                      <input
                        class="field-input"
                        value={trigger().when}
                        aria-label={`触发器 ${index + 1} 的触发条件`}
                        placeholder="触发条件，例如：用户要求写周报或总结本周工作时"
                        disabled={globalInstructionsLoading() || globalInstructionsBusy()}
                        onInput={(event) => editPromptTrigger(index, { when: event.currentTarget.value })}
                      />
                      <button
                        type="button"
                        class="btn secondary"
                        disabled={globalInstructionsLoading() || globalInstructionsBusy()}
                        onClick={() => editPromptTrigger(index, null)}
                      >
                        删除
                      </button>
                    </div>
                    <textarea
                      class="field-input"
                      rows={4}
                      value={trigger().prompt}
                      aria-label={`触发器 ${index + 1} 的提示词`}
                      placeholder="命中后加载的提示词……"
                      disabled={globalInstructionsLoading() || globalInstructionsBusy()}
                      onInput={(event) => editPromptTrigger(index, { prompt: event.currentTarget.value })}
                    />
                  </div>
                )}
              </Index>
              <div class="global-agent-actions">
                <button
                  type="button"
                  class="btn secondary"
                  disabled={globalInstructionsLoading() || globalInstructionsBusy()}
                  onClick={() => {
                    setPromptTriggers((list) => [...list, { when: "", prompt: "" }]);
                    setGlobalInstructionsDirty(true);
                  }}
                >
                  添加触发器
                </button>
                <button
                  type="button"
                  class="btn secondary"
                  disabled={globalInstructionsLoading() || globalInstructionsBusy()}
                  onClick={() => void syncGlobalInstructions().catch((error) => setGlobalInstructionsMsg(`同步失败：${String(error)}`))}
                >
                  {globalInstructionsBusy() ? "同步中…" : "保存并同步"}
                </button>
              </div>
            </div>

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
                          <label class="agent-config-toggle">
                            <input
                              type="checkbox"
                              checked={target.enabled}
                              disabled={globalInstructionsLoading() || globalInstructionsBusy()}
                              onChange={(event) => {
                                const enabled = event.currentTarget.checked;
                                setGlobalInstructionTargets((targets) =>
                                  targets.map((item) =>
                                    item.agentKind === target.agentKind
                                      ? { ...item, enabled, status: "inactive", detail: enabled ? "待同步" : "已取消适配" }
                                      : item,
                                  ),
                                );
                                setGlobalInstructionsDirty(true);
                                setGlobalInstructionsMsg("");
                              }}
                            />
                            <span class={`agent-badge ${target.agentKind}`}>{target.label}</span>
                          </label>
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
            <AppearanceLayoutSettings />
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
                  placeholder="填写服务端配置的 token"
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
                填写服务端为你配置的身份 token，客户端不会修改或自动生成。<b>填了 token 即开启团队/漫游，清空 token 即关闭。</b>
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
              <div class="remote-control-row">
                <label class="remote-control-toggle">
                  <input
                    type="checkbox"
                    checked={remoteControlEnabled()}
                    onChange={(event) => setRemoteControlEnabled(event.currentTarget.checked)}
                  />
                  <span>允许 server 端远程控制</span>
                </label>
                <button
                  type="button"
                  class="btn secondary small"
                  disabled={
                    !remoteControlEnabled() ||
                    !relayServer().trim() ||
                    !relayToken().trim()
                  }
                  onClick={() => void openRemoteQr()}
                >
                  远控二维码
                </button>
              </div>
              <span class="field-hint">
                默认关闭。手动开启并保存后，server 端才可查看会话、读取项目文件或发送远程操作。
              </span>
            </div>

            <div class="field">
              <span class="field-label">允许漫游的项目</span>
              <span class="field-hint">
                每次添加一行，只能选择你自己的本地项目；未列入的目录不会展示给队友，也不会接受漫游请求。每次请求仍需你在本机确认。
              </span>
              <Show
                when={!roamingFoldersLoading()}
                fallback={<div class="sel-empty">加载中…</div>}
              >
                <Show
                  when={state.projects.length > 0}
                  fallback={<div class="sel-empty">当前没有可共享的本地项目。</div>}
                >
                  <div class="share-list">
                    <Index each={roamingFolders()}>
                      {(folder, index) => (
                        <div class="share-row" title={folder()}>
                          <ProjectPicker
                            value={folder()}
                            onChange={(path) => replaceRoamingFolder(folder(), path)}
                            ownOnly
                            portal
                          />
                          <button
                            type="button"
                            class="icon-btn share-row-remove"
                            title="移除该项目"
                            aria-label="移除该项目"
                            onClick={() => removeRoamingFolderAt(index)}
                          >
                            <IconX size={14} />
                          </button>
                        </div>
                      )}
                    </Index>
                    <Show when={roamingFolders().length === 0}>
                      <div class="share-empty">尚未共享任何项目，用下方选择器添加一行。</div>
                    </Show>
                    <Show when={!allProjectsShared()}>
                      <div class="share-row share-row-add">
                        <ProjectPicker
                          value=""
                          onChange={(path) => addRoamingFolder(path)}
                          ownOnly
                          portal
                        />
                        <span class="share-row-plus" title="选择项目后自动加入列表">
                          <IconPlus size={13} />
                        </span>
                      </div>
                    </Show>
                  </div>
                </Show>
              </Show>
            </div>
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
                各后端的全局 skills 目录，不拷贝文件。
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

          {/* ===== 环境变量 ===== */}
          <Show when={tab() === "env"}>
            <section class="settings-group">
              <h3 class="settings-group-title">自定义环境变量</h3>
              <div class="field">
                <span class="field-hint">
                  追加到所有 agent 子进程的环境变量，同名时覆盖用户环境变量。
                </span>
                <div class="session-shortcut-list">
                  {/* Index 按索引追踪：输入时仅更新对应值，不重建行 DOM，避免输入框失焦。 */}
                  <Index each={customEnvVars()}>
                    {(item, index) => (
                      <div class="session-shortcut-row env-var-row">
                        <input
                          class="field-input"
                          style={{ flex: "1" }}
                          placeholder="变量名，如 MY_API_KEY"
                          value={item().name}
                          onInput={(e) =>
                            setCustomEnvVars((list) =>
                              list.map((entry, i) =>
                                i === index ? { ...entry, name: e.currentTarget.value } : entry,
                              ),
                            )
                          }
                        />
                        <input
                          class="field-input"
                          style={{ flex: "2" }}
                          placeholder="变量值"
                          value={item().value}
                          onInput={(e) =>
                            setCustomEnvVars((list) =>
                              list.map((entry, i) =>
                                i === index ? { ...entry, value: e.currentTarget.value } : entry,
                              ),
                            )
                          }
                        />
                        <button
                          type="button"
                          class="btn secondary"
                          onClick={() =>
                            setCustomEnvVars((list) => list.filter((_, i) => i !== index))
                          }
                        >
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
                  onClick={() => setCustomEnvVars((list) => [...list, { name: "", value: "" }])}
                >
                  添加变量
                </button>
              </div>
            </section>

            <section class="settings-group">
              <h3 class="settings-group-title">系统环境变量</h3>
              <div class="field">
                <span class="field-label">刷新 Windows 环境变量</span>
                <button
                  type="button"
                  class="btn secondary"
                  style={{ "align-self": "flex-start" }}
                  disabled={environmentRefreshing()}
                  onClick={() => void refreshEnvironmentVariables()}
                >
                  {environmentRefreshing() ? "刷新中…" : "刷新环境变量"}
                </button>
                <Show when={environmentRefreshMsg()}>
                  <span class={`relay-verify ${environmentRefreshFailed() ? "bad" : "ok"}`}>
                    {environmentRefreshMsg()}
                  </span>
                </Show>
                <span class="field-hint">
                  从 Windows 注册表重新读取系统和当前用户环境变量，并覆盖 Nova
                  启动时继承的同名变量。之后新启动的 agent 进程会使用新值。
                </span>
              </div>
            </section>
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

            <div class="field update-channel-field">
              <span class="field-label">更新通道</span>
              <div class="update-channel-card">
                <div class="update-channel-status">
                  <span class={`update-channel-dot ${updateChannel()}`} aria-hidden="true" />
                  <div>
                    <strong>{updateChannel() === "release" ? "正式版通道" : "预发布版通道"}</strong>
                    <span>{updateChannel() === "release" ? "稳定更新，适合日常使用" : "优先体验新功能，可能包含未完全验证的改动"}</span>
                  </div>
                </div>
                <div class="seg-control update-channel-control" role="radiogroup" aria-label="更新通道">
                  <button
                    type="button"
                    role="radio"
                    aria-checked={updateChannel() === "release"}
                    classList={{ active: updateChannel() === "release" }}
                    disabled={checking()}
                    onClick={() => void changeUpdateChannel("release")}
                  >
                    <span>Release</span>
                    <small>正式版</small>
                  </button>
                  <button
                    type="button"
                    role="radio"
                    aria-checked={updateChannel() === "pre-release"}
                    classList={{ active: updateChannel() === "pre-release" }}
                    disabled={checking()}
                    onClick={() => void changeUpdateChannel("pre-release")}
                  >
                    <span>Pre-release</span>
                    <small>预发布版</small>
                  </button>
                </div>
              </div>
              <span class="field-hint">切换后会立即检查并下载目标通道的最新版，版本号较低时也会切换。</span>
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
          <Show
            when={props.onboarding}
            fallback={
              <>
                <button class="btn secondary" onClick={props.onClose}>
                  取消
                </button>
                <button class="btn primary" disabled={saving()} onClick={() => void save()}>
                  {saving() ? "保存中…" : "保存"}
                </button>
              </>
            }
          >
            <Show
              when={step() > 0}
              fallback={
                <button class="btn primary" disabled={saving()} onClick={() => void save(() => { setStep(1); setTab("scenes"); })}>
                  {saving() ? "保存中…" : "下一步"}
                </button>
              }
            >
              <button class="btn secondary" disabled={saving()} onClick={() => { setStep(0); setTab("lyra"); }}>
                上一步
              </button>
              <button class="btn primary" disabled={saving()} onClick={() => void save()}>
                {saving() ? "保存中…" : "完成"}
              </button>
            </Show>
          </Show>
        </div>
      </div>

      <Show when={remoteQr()}>
        {(qr) => (
          <div class="modal-backdrop remote-qr-backdrop" onClick={() => setRemoteQr(null)}>
            <div class="modal remote-qr-modal" onClick={(event) => event.stopPropagation()}>
              <div class="modal-head">
                <span>远控二维码</span>
                <button class="icon-btn" onClick={() => setRemoteQr(null)}>
                  <IconX size={16} />
                </button>
              </div>
              <div class="modal-body remote-qr-body">
                <img src={qr().dataUrl} alt="远控二维码" />
                <span class="field-hint">使用手机扫码打开中转站远控页面。</span>
                <code class="remote-qr-url">{qr().url}</code>
              </div>
            </div>
          </div>
        )}
      </Show>
    </div>
  );
}
