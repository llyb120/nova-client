import { getVersion } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { confirm, message, open as openDialog } from "@tauri-apps/plugin-dialog";
import * as QRCode from "qrcode";
import { createEffect, createMemo, createSignal, For, Index, onCleanup, onMount, Show } from "solid-js";
import { api } from "../ipc";
import {
  ALL_AGENT_KINDS,
  checkAndStageUpdate,
  deleteThreads,
  enabledAgentKinds,
  ensureModelOptions,
  preloadPeerModels,
  refreshQuota,
  refreshRelayStatus,
  roamingPeers,
  setState,
  setTheme,
  state,
} from "../store";
import { agentLabel, isScratch, setFileDropBlocked } from "../utils";
import { ModelPicker, type SharedModelSource } from "./ConfigSelects";
import { IconPlus, IconX } from "./icons";
import { ProjectPicker } from "./ProjectPicker";
import {
  encodeModelTarget,
  encodeQuotaModelTarget,
  encodeRoamTarget,
  formatShortcutKeys,
  newSessionShortcutId,
  parseModelTarget,
  parseRoamTarget,
  setShortcutCaptureActive,
} from "../sessionShortcuts";
import type {
  AgentInstructionTarget,
  AgentKind,
  CliOperationProgress,
  CliStatus,
  Peer,
  SessionShortcut,
  SessionShortcutAction,
  Settings,
  SkillInfo,
  WorktreeRecord,
  ExperienceExpertConfig,
} from "../types";

function threadGroupName(cwd: string): string {
  if (isScratch(cwd)) return "临时会话";
  return cwd.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || cwd;
}

function projectPathKey(path: string): string {
  return path.replace(/\\/g, "/").toLowerCase();
}

/** 解析共享模型额度的键 `<agentKind>:<modelId>`；非法键返回 null。 */
function parseQuotaShareKey(key: string): { kind: AgentKind; model: string } | null {
  const i = key.indexOf(":");
  if (i <= 0) return null;
  const kind = key.slice(0, i) as AgentKind;
  if (!ALL_AGENT_KINDS.includes(kind)) return null;
  return { kind, model: key.slice(i + 1) };
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
const CURSOR_CONTEXT_WINDOW_OPTIONS = [
  { value: 16_000, label: "16K tokens" },
  { value: 32_000, label: "32K tokens" },
  { value: 64_000, label: "64K tokens" },
  { value: 100_000, label: "100K tokens" },
  { value: 128_000, label: "128K tokens" },
  { value: 200_000, label: "200K tokens" },
  { value: 256_000, label: "256K tokens" },
  { value: 400_000, label: "400K tokens" },
  { value: 512_000, label: "512K tokens" },
  { value: 1_000_000, label: "1M tokens" },
  { value: 2_000_000, label: "2M tokens" },
];

/** 点击后按下组合键录制快捷键。 */
function ShortcutKeyCapture(props: {
  value: string;
  onChange: (keys: string) => void;
}) {
  const [recording, setRecording] = createSignal(false);
  let listener: ((event: KeyboardEvent) => void) | null = null;

  const stopRecording = () => {
    if (listener) {
      window.removeEventListener("keydown", listener, true);
      listener = null;
    }
    setShortcutCaptureActive(false);
    setRecording(false);
  };

  const startRecording = () => {
    stopRecording();
    setShortcutCaptureActive(true);
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
  progress?: CliOperationProgress;
  output?: string;
  onUpgrade: () => void;
}) {
  const failed = () => props.message?.includes("失败") || props.progress?.stage === "failed";
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
          <span class={`cli-manager-message ${failed() ? "bad" : "ok"}`} title={props.message}>
            {props.message}
          </span>
        </Show>
        <Show when={!props.message && !props.loading && props.status?.upgradeSupported === false}>
          <span class="cli-manager-message bad" title={props.status?.detail}>
            {props.status?.detail}
          </span>
        </Show>
      </div>
      <Show when={props.progress}>
        {(progress) => (
          <div class="cli-operation-progress">
            <div class="cli-progress-track" aria-label={`${progress().action}进度 ${progress().percent}%`}>
              <span style={{ width: `${progress().percent}%` }} />
            </div>
            <span classList={{ "cli-progress-label": true, bad: failed() }}>
              {progress().percent}% · {progress().message}
            </span>
          </div>
        )}
      </Show>
      <Show when={props.output}>
        <details class="cli-shell-output" open={props.upgrading || failed()}>
          <summary>Shell 输出</summary>
          <pre>{props.output}</pre>
        </details>
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
  const [opencodePath, setOpencodePath] = createSignal(s?.opencodePath ?? "opencode");
  const [codexPath, setCodexPath] = createSignal(s?.codexPath ?? "codex");
  const [codexProxy, setCodexProxy] = createSignal(s?.codexProxy ?? "");
  const [vegaProxy, setVegaProxy] = createSignal(s?.vegaProxy ?? "");
  const [windowsShellShimEnabled, setWindowsShellShimEnabled] = createSignal(
    s?.windowsShellShimEnabled ?? false,
  );
  const [autoChangeProjectEnabled, setAutoChangeProjectEnabled] = createSignal(
    s?.autoChangeProjectEnabled !== false,
  );
  const [checkpointEnabled, setCheckpointEnabled] = createSignal(
    s?.checkpointEnabled ?? false,
  );
  const [contextRetrievalMode, setContextRetrievalMode] = createSignal<"none" | "fast">(
    s?.contextRetrievalMode === "none" ? "none" : "fast",
  );

  const [devinProxy, setDevinProxy] = createSignal(s?.devinProxy ?? "");
  const [codebuddyProxy, setCodebuddyProxy] = createSignal(s?.codebuddyProxy ?? "");
  const [claudecodeProxy, setClaudecodeProxy] = createSignal(s?.claudecodeProxy ?? "");
  const [claudecodeSdkApiKey, setClaudecodeSdkApiKey] = createSignal(s?.claudecodeSdkApiKey ?? "");
  const [cursorProxy, setCursorProxy] = createSignal(s?.cursorProxy ?? "");
  const [cursorSdkApiKey, setCursorSdkApiKey] = createSignal(s?.cursorSdkApiKey ?? "");
  const [cursorDisableSubagents, setCursorDisableSubagents] = createSignal(
    s?.cursorDisableSubagents ?? false,
  );
  const [cursorModelContexts, setCursorModelContexts] = createSignal(
    (s?.cursorModelContexts ?? []).map((rule) => ({ ...rule })),
  );
  const [sessionShortcuts, setSessionShortcuts] = createSignal<SessionShortcut[]>(
    (s?.sessionShortcuts ?? [])
      .filter((item) => item.action !== "stopSession")
      .map((item) => ({ ...item })),
  );
  const [vegaContextMode] = createSignal<"default" | "super">(
    s?.vegaContextMode === "super" ? "super" : "default",
  );
  const [cursorContextMode, setCursorContextMode] = createSignal<"default" | "super">(
    s?.cursorContextMode === "super" ? "super" : "default",
  );
  const [opencodeProxy, setOpencodeProxy] = createSignal(s?.opencodeProxy ?? "");
  const [devinEnabled, setDevinEnabled] = createSignal(s?.devinEnabled !== false);
  const [vegaEnabled, setVegaEnabled] = createSignal(s?.vegaEnabled === true);
  const [lyraEnabled, setLyraEnabled] = createSignal(s?.lyraEnabled !== false);
  const [codexEnabled, setCodexEnabled] = createSignal(s?.codexEnabled !== false);
  const [codebuddyEnabled, setCodebuddyEnabled] = createSignal(s?.codebuddyEnabled !== false);
  const [claudecodeEnabled, setClaudecodeEnabled] = createSignal(s?.claudecodeEnabled !== false);
  const [cursorEnabled, setCursorEnabled] = createSignal(s?.cursorEnabled !== false);
  const [opencodeEnabled, setOpencodeEnabled] = createSignal(s?.opencodeEnabled !== false);
  // 新会话默认固定 Build；Plan 仅由 /plan 启动，不再提供设置项。
  const [lightweightAgent, setLightweightAgent] = createSignal<AgentKind>(
    (s?.lightweightModelAgent as AgentKind) || "alkaid",
  );
  const [lightweightModel, setLightweightModel] = createSignal(s?.lightweightModel ?? "");
  const [stageModels, setStageModels] = createSignal<Settings["stageModels"]>(
    (s?.stageModels ?? []).map((target) => ({ ...target })),
  );
  const [draftStageKind, setDraftStageKind] = createSignal<AgentKind | null>(null);
  const [draftStageModel, setDraftStageModel] = createSignal("");
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
  const [chatViewRender, setChatViewRender] = createSignal<"dom" | "canvas" | "canvas_qwen">(
    s?.chatViewRender === "dom"
      ? "dom"
      : s?.chatViewRender === "canvas_qwen"
        ? "canvas_qwen"
        : "canvas",
  );
  // server 留空回退默认地址；这里也预填，避免误存成空导致团队/漫游被静默关闭
  const [relayServer, setRelayServer] = createSignal(s?.relayServer || DEFAULT_RELAY_SERVER);
  const [relayToken, setRelayToken] = createSignal(s?.relayToken ?? "");
  const [relayGroups, setRelayGroups] = createSignal(s?.relayGroups ?? "");
  const [remoteControlEnabled, setRemoteControlEnabled] = createSignal(
    s?.remoteControlEnabled ?? false,
  );
  const [remoteQr, setRemoteQr] = createSignal<{ url: string; dataUrl: string } | null>(null);
  const [quotaSharedModels, setQuotaSharedModels] = createSignal<string[]>(s?.quotaSharedModels ?? []);
  const [roamingFolders, setRoamingFolders] = createSignal<string[]>(state.roamingFolders);
  const [roamingFoldersLoading, setRoamingFoldersLoading] = createSignal(false);
  // 「添加一行」式共享模型选择器的草稿状态：选完即加入列表并复位为空。
  const [draftShareKind, setDraftShareKind] = createSignal<AgentKind | null>(null);
  const [draftShareModel, setDraftShareModel] = createSignal("");
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
  const [environmentRefreshing, setEnvironmentRefreshing] = createSignal(false);
  const [environmentRefreshMsg, setEnvironmentRefreshMsg] = createSignal("");
  const [environmentRefreshFailed, setEnvironmentRefreshFailed] = createSignal(false);
  const [cliStatuses, setCliStatuses] = createSignal<Partial<Record<AgentKind, CliStatus>>>({});
  const [cliLoading, setCliLoading] = createSignal(false);
  const [upgradingCli, setUpgradingCli] = createSignal<AgentKind | null>(null);
  const [cliMessages, setCliMessages] = createSignal<Partial<Record<AgentKind, string>>>({});
  const [cliProgress, setCliProgress] = createSignal<Partial<Record<AgentKind, CliOperationProgress>>>({});
  const [cliOutputs, setCliOutputs] = createSignal<Partial<Record<AgentKind, string>>>({});
  const activeCliOperations: Partial<Record<AgentKind, string>> = {};
  let disposedCliProgressListener = false;
  let stopCliProgressListener: (() => void) | undefined;
  void listen<CliOperationProgress>("cli:operation-progress", (event) => {
    const progress = event.payload;
    if (activeCliOperations[progress.agentKind] !== progress.operationId) return;
    setCliProgress((prev) => ({ ...prev, [progress.agentKind]: progress }));
    const isTicker = progress.stage === "running" && /^正在(安装|升级) .+…$/.test(progress.message);
    if (isTicker) return;
    setCliOutputs((prev) => {
      const current = prev[progress.agentKind] ?? "";
      const next = current ? `${current}\n${progress.message}` : progress.message;
      return { ...prev, [progress.agentKind]: next.slice(-12000) };
    });
  }).then((unlisten) => {
    if (disposedCliProgressListener) unlisten();
    else stopCliProgressListener = unlisten;
  });
  onCleanup(() => {
    disposedCliProgressListener = true;
    stopCliProgressListener?.();
  });
  const [quotaRefreshing, setQuotaRefreshing] = createSignal(false);

  const reloadQuota = async () => {
    setQuotaRefreshing(true);
    try {
      await refreshQuota();
    } finally {
      setQuotaRefreshing(false);
    }
  };

  const [vegaRefreshing, setVegaRefreshing] = createSignal(false);
  const [vegaRefreshMsg, setVegaRefreshMsg] = createSignal("");

  const refreshVegaConfig = async () => {
    setVegaRefreshing(true);
    setVegaRefreshMsg("");
    try {
      await api.refreshAlkaidConfig();
      setVegaRefreshMsg("已重载配置，模型列表刷新中…");
      setTimeout(() => setVegaRefreshMsg(""), 4000);
    } catch (e) {
      setVegaRefreshMsg(`刷新失败：${String(e)}`);
    } finally {
      setVegaRefreshing(false);
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
      vegaEnabled(),
      lyraEnabled(),
      codexEnabled(),
      codebuddyEnabled(),
      claudecodeEnabled(),
      cursorEnabled(),
      opencodeEnabled(),
    ].filter(Boolean).length;

  const quotaShareKinds = createMemo<AgentKind[]>(() => {
    const kinds: AgentKind[] = [];
    if (vegaEnabled()) kinds.push("alkaid");
    if (lyraEnabled()) kinds.push("lyra");
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
  const addQuotaSharedModel = (kind: AgentKind, model: string) => {
    if (!model) return;
    const key = quotaShareKey(kind, model);
    setQuotaSharedModels((current) => (current.includes(key) ? current : [...current, key]));
    // 记住上次选择的后端，连续添加时不用每次重新选后端
    setDraftShareKind(kind);
    setDraftShareModel("");
  };
  const replaceQuotaSharedModel = (oldKey: string, kind: AgentKind, model: string) => {
    const key = quotaShareKey(kind, model);
    if (key === oldKey) return;
    setQuotaSharedModels((current) => {
      // 目标已在列表中：移除旧行避免重复
      if (current.includes(key)) return current.filter((item) => item !== oldKey);
      return current.map((item) => (item === oldKey ? key : item));
    });
  };
  const removeQuotaSharedModelAt = (index: number) => {
    setQuotaSharedModels((current) => current.filter((_, i) => i !== index));
  };
  /** 行选择器的后端列表：已启用后端 + 该行自身的后端（后端被停用后旧行仍可展示/移除） */
  const quotaRowKinds = (kind: AgentKind): AgentKind[] =>
    quotaShareKinds().includes(kind) ? quotaShareKinds() : [...quotaShareKinds(), kind];
  const stageRowKinds = (kind: AgentKind): AgentKind[] => {
    const enabled = enabledAgentKinds();
    return enabled.includes(kind) ? enabled : [...enabled, kind];
  };
  const addStageModel = (agentKind: AgentKind, model: string) => {
    if (!model) return;
    setStageModels((current) => [...current, { agentKind, model }]);
    setDraftStageKind(agentKind);
    setDraftStageModel("");
  };
  const replaceStageModel = (index: number, agentKind: AgentKind, model: string) => {
    setStageModels((current) =>
      current.map((target, targetIndex) =>
        targetIndex === index ? { agentKind, model } : target,
      ),
    );
  };
  const removeStageModelAt = (index: number) => {
    setStageModels((current) => current.filter((_, targetIndex) => targetIndex !== index));
  };
  const updateCursorModelContext = (index: number, patch: { prefix?: string; contextWindow?: number }) => {
    setCursorModelContexts((current) => current.map((rule, ruleIndex) =>
      ruleIndex === index ? { ...rule, ...patch } : rule));
  };
  const addCursorModelContext = () => {
    setCursorModelContexts((current) => [...current, { prefix: "", contextWindow: 128_000 }]);
  };
  const removeCursorModelContext = (index: number) => {
    setCursorModelContexts((current) => current.filter((_, ruleIndex) => ruleIndex !== index));
  };
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
              : item.action === "insertText"
                ? "insertText"
                : "selectModel",
        target: item.target.trim(),
      }))
      .filter((item) => {
        if (!item.keys) return false;
        if (item.action === "stopSession") return false;
        if (item.action === "newSession") return true;
        return item.target.length > 0;
      });

  const shortcutSharedModels = createMemo<SharedModelSource[]>(() =>
    roamingPeers()
      .map((peer) => ({
        peer: { token: peer.token, name: peer.name },
        options: state.peerModels[peer.token]?.sharedOptions ?? {},
      }))
      .filter((source) => Object.keys(source.options).length > 0),
  );

  createEffect(() => {
    if (sessionShortcuts().length === 0) return;
    preloadPeerModels();
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
      const config = await api.setGlobalAgentInstructions(globalInstructions(), enabledAgentKinds);
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
  const changeUpdateChannel = async (channel: "release" | "pre-release") => {
    if (channel === updateChannel() || checking()) return;
    const cur = await api.getSettings();
    const next = { ...cur, updateChannel: channel };
    await api.setSettings(next);
    setState("settings", next);
    setUpdateChannel(channel);
    setCheckResult("");
    await checkNow();
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
    devinPath: devinPath().trim() || "devin",
    acpArgs: acpArgs().trim() || "acp",
    codebuddyPath: codebuddyPath().trim() || "codebuddy",
    claudecodePath: claudecodePath().trim() || "claude",
    // Cursor 仅走官方 SDK，不再依赖本机 cursor-agent；保留字段兼容旧配置。
    cursorPath: s?.cursorPath?.trim() || "cursor-agent",
    opencodePath: opencodePath().trim() || "opencode",
    codexPath: codexPath().trim() || "codex",
    codexProxy: codexProxy().trim(),
    vegaProxy: vegaProxy().trim(),
    windowsShellShimEnabled: windowsShellShimEnabled(),
    autoChangeProjectEnabled: autoChangeProjectEnabled(),
    checkpointEnabled: checkpointEnabled(),
    contextRetrievalMode: contextRetrievalMode(),

    devinProxy: devinProxy().trim(),
    codebuddyProxy: codebuddyProxy().trim(),
    claudecodeProxy: claudecodeProxy().trim(),
    claudecodeSdkApiKey: claudecodeSdkApiKey().trim(),
    cursorProxy: cursorProxy().trim(),
    cursorSdkApiKey: cursorSdkApiKey().trim(),
    cursorDisableSubagents: cursorDisableSubagents(),
    cursorModelContexts: cursorModelContexts()
      .map((rule) => ({ prefix: rule.prefix.trim(), contextWindow: rule.contextWindow }))
      .filter((rule) => rule.prefix.length > 0),
    vegaContextMode: vegaContextMode(),
    cursorContextMode: cursorContextMode(),
    opencodeProxy: opencodeProxy().trim(),
    defaultMode: "build",
    lightweightModelAgent: lightweightAgent(),
    lightweightModel: lightweightModel().trim(),
    stageModels: stageModels()
      .map((target) => ({ agentKind: target.agentKind, model: target.model.trim() }))
      .filter((target) => target.model.length > 0),
    editor: editor().trim() || "code",
    theme: state.theme,
    relayServer: relayServer().trim(),
    relayToken: relayToken().trim(),
    relayGroups: relayGroups().trim(),
    remoteControlEnabled: remoteControlEnabled(),
    quotaSharedModels: quotaSharedModels(),
    modelFavorites: state.settings?.modelFavorites ?? [],
    sessionShortcuts: draftSessionShortcuts(),
    devinEnabled: devinEnabled(),
    vegaEnabled: vegaEnabled(),
    lyraEnabled: lyraEnabled(),
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
    updateChannel: updateChannel(),
    sessionAutoCleanupEnabled: sessionAutoCleanupEnabled(),
    sessionAutoCleanupHours: Math.max(1, Math.floor(sessionAutoCleanupHours() || 24 * 30)),
    historyDisplayMode: historyDisplayMode(),
    chatViewRender: chatViewRender(),
    experienceTrainingEnabled: experienceTrainingEnabled(),
    experienceTrainingAgent: experienceTrainingAgent(),
    experienceTrainingModel: experienceTrainingModel().trim(),
    experienceTrainingIntervalMinutes: Math.max(5, Math.floor(experienceTrainingIntervalMinutes() || 30)),
    experienceEvolutionIntervalMinutes: Math.max(10, Math.floor(experienceEvolutionIntervalMinutes() || 720)),
    experienceExperts: experienceExperts(),
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
    const operationId = typeof globalThis.crypto?.randomUUID === "function"
      ? globalThis.crypto.randomUUID()
      : `${Date.now()}-${Math.random().toString(36).slice(2)}`;
    activeCliOperations[kind] = operationId;
    setUpgradingCli(kind);
    setCliMessages((prev) => ({ ...prev, [kind]: "" }));
    setCliProgress((prev) => ({ ...prev, [kind]: undefined }));
    setCliOutputs((prev) => ({ ...prev, [kind]: "" }));
    try {
      const status = await api.upgradeCli(kind, draftSettings(), operationId);
      setCliStatuses((prev) => ({ ...prev, [kind]: status }));
      setCliMessages((prev) => ({
        ...prev,
        [kind]: `${wasInstalled ? "更新" : "安装"}成功：${status.version}`,
      }));
    } catch (e) {
      const cancelled = String(e).includes("CLI 操作已取消");
      setCliMessages((prev) => ({
        ...prev,
        [kind]: cancelled ? "操作已取消" : `${wasInstalled ? "升级" : "安装"}失败：${String(e)}`,
      }));
    } finally {
      delete activeCliOperations[kind];
      setUpgradingCli(null);
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

  // worktree 管理
  const [worktreeDir, setWorktreeDir] = createSignal(s?.worktreeDir ?? "");
  // skills 管理（集中存放 ~/.nova/skills）
  const [skillsDir, setSkillsDir] = createSignal("");
  const [skills, setSkills] = createSignal<SkillInfo[]>([]);
  const [skillsLoading, setSkillsLoading] = createSignal(false);
  const [skillsBusy, setSkillsBusy] = createSignal(false);
  const [skillsDragging, setSkillsDragging] = createSignal(false);
  const [skillsMsg, setSkillsMsg] = createSignal("");
  const [experienceTrainingEnabled, setExperienceTrainingEnabled] = createSignal(s?.experienceTrainingEnabled ?? false);
  const trainingAgentKinds = () => enabledAgentKinds().filter((kind) => kind !== "devin" && kind !== "opencode");
  const [experienceTrainingAgent, setExperienceTrainingAgent] = createSignal<AgentKind>(s?.experienceTrainingAgent ?? "lyra");
  const [experienceTrainingModel, setExperienceTrainingModel] = createSignal(s?.experienceTrainingModel ?? "");
  const [experienceTrainingIntervalMinutes, setExperienceTrainingIntervalMinutes] = createSignal(s?.experienceTrainingIntervalMinutes ?? 30);
  const [experienceEvolutionIntervalMinutes, setExperienceEvolutionIntervalMinutes] = createSignal(s?.experienceEvolutionIntervalMinutes ?? 720);
  const [experienceExperts, setExperienceExperts] = createSignal(s?.experienceExperts ?? []);
  const updateExperienceExpert = (index: number, key: string, value: number) => {
    setExperienceExperts((current) => current.map((expert, i) => i === index ? { ...expert, [key]: Math.max(0, Math.min(key === "negativeSensitivity" ? 3 : 1, value || 0)) } : expert));
  };
  const BASE_EXPERIENCE_EXPERTS: ExperienceExpertConfig[] = [
    { id: "fast", name: "参宿一", writeRate: 0.90, valueLearningRate: 0.50, forgetRate: 0.08, mutationRate: 0.25, migrationRate: 0.05, abstractionLevel: 0.35, noveltyPreference: 0.40, negativeSensitivity: 1.0 },
    { id: "slow", name: "参宿七", writeRate: 0.30, valueLearningRate: 0.10, forgetRate: 0.005, mutationRate: 0.03, migrationRate: 0.05, abstractionLevel: 0.55, noveltyPreference: 0.20, negativeSensitivity: 0.7 },
    { id: "concrete", name: "参宿二", writeRate: 0.70, valueLearningRate: 0.30, forgetRate: 0.03, mutationRate: 0.10, migrationRate: 0.10, abstractionLevel: 0.20, noveltyPreference: 0.30, negativeSensitivity: 1.0 },
    { id: "abstract", name: "参宿四", writeRate: 0.50, valueLearningRate: 0.20, forgetRate: 0.015, mutationRate: 0.12, migrationRate: 0.10, abstractionLevel: 0.85, noveltyPreference: 0.45, negativeSensitivity: 0.8 },
    { id: "negative", name: "参宿五", writeRate: 0.60, valueLearningRate: 0.45, forgetRate: 0.04, mutationRate: 0.15, migrationRate: 0.08, abstractionLevel: 0.45, noveltyPreference: 0.35, negativeSensitivity: 1.5 },
    { id: "novel", name: "参宿六", writeRate: 0.80, valueLearningRate: 0.35, forgetRate: 0.06, mutationRate: 0.40, migrationRate: 0.05, abstractionLevel: 0.60, noveltyPreference: 0.85, negativeSensitivity: 1.0 },
  ];
  const resetExperienceExperts = () => setExperienceExperts(BASE_EXPERIENCE_EXPERTS.map((expert) => ({ ...expert })));
  const persistExperience = async () => {
    const cur = await api.getSettings();
    const next: Settings = {
      ...cur,
      experienceTrainingEnabled: experienceTrainingEnabled(),
      experienceTrainingAgent: experienceTrainingAgent(),
      experienceTrainingModel: experienceTrainingModel().trim(),
      experienceTrainingIntervalMinutes: Math.max(5, Math.floor(experienceTrainingIntervalMinutes() || 30)),
      experienceEvolutionIntervalMinutes: Math.max(10, Math.floor(experienceEvolutionIntervalMinutes() || 720)),
      experienceExperts: experienceExperts(),
    };
    await api.setSettings(next);
    setState("settings", next);
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
    const draftedShortcuts = draftSessionShortcuts();
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
                  仅清理普通会话；超时后先移入回收站，保留同样时长才彻底删除。周六、周日不计入保留时长，但仍会执行清理检查。
                </span>
              </label>

              <div class="session-shortcut-config">
                <div class="session-shortcut-head">
                  <div class="session-shortcut-copy">
                    <div class="field-label">会话快捷键</div>
                    <div class="field-hint">
                      一键切换项目/模型、快速新会话、终止当前回合，或向输入框插入文本。新会话页项目与模型均生效；会话中仅模型切换与终止回合有效；快速新会话任意页可用；快捷输入仅在会话输入框聚焦时生效。默认 Esc 终止当前回合。
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
                              <option value="insertText">快捷输入</option>
                            </select>
                            <div class="session-shortcut-target">
                              <Show
                                when={item().action === "newSession"}
                                fallback={
                                  <Show
                                    when={item().action === "insertText"}
                                    fallback={
                                      <Show
                                        when={item().action === "selectProject"}
                                        fallback={
                                          <ModelPicker
                                            agentKind={modelTarget()?.agentKind ?? (enabledAgentKinds()[0] ?? "alkaid")}
                                            agentKinds={enabledAgentKinds()}
                                            model={modelTarget()?.model ?? ""}
                                            sharedModels={shortcutSharedModels()}
                                            quotaPeerToken={modelTarget()?.peerToken}
                                            onPickModel={(agentKind, model, quotaPeer) =>
                                              updateSessionShortcut(index, {
                                                target: quotaPeer
                                                  ? encodeQuotaModelTarget(quotaPeer.token, agentKind, model)
                                                  : encodeModelTarget(agentKind, model),
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
                                <div class="session-shortcut-target-none">任意页 · 继承当前目录与模型</div>
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
              <h3 class="settings-group-title">轻量级模型</h3>
              <p class="settings-group-desc">
                用于标题生成、快速总结、摘要和上下文压缩等辅助任务；调用失败时自动回退到任务原模型。
              </p>
              <div class="field">
                <span class="field-label">Stage 模型</span>
                <span class="field-hint">
                  按顺序配置 /stage、/stage2、/stage3… 使用的模型；/stage 等同于 /stage1。命令会引用当前会话并直接开启对应模型的新会话。
                </span>
                <div class="share-list">
                  <Index each={stageModels()}>
                    {(target, index) => (
                      <div class="share-row stage-model-row" title={`/stage${index + 1}`}>
                        <span style={{ width: "64px", "flex-shrink": 0 }}>
                          {index === 0 ? "/stage" : `/stage${index + 1}`}
                        </span>
                        <ModelPicker
                          agentKind={target().agentKind}
                          agentKinds={stageRowKinds(target().agentKind)}
                          model={target().model}
                          onPickModel={(agentKind, model) =>
                            replaceStageModel(index, agentKind, model)
                          }
                          title={`Stage ${index + 1} 模型`}
                          portal
                        />
                        <button
                          type="button"
                          class="icon-btn share-row-remove"
                          title="移除该模型"
                          aria-label="移除该 Stage 模型"
                          onClick={() => removeStageModelAt(index)}
                        >
                          <IconX size={14} />
                        </button>
                      </div>
                    )}
                  </Index>
                  <Show when={stageModels().length === 0}>
                    <div class="share-empty">尚未配置 Stage 模型，/stage 将提示先完成配置。</div>
                  </Show>
                  <Show when={enabledAgentKinds().length > 0}>
                    <div class="share-row share-row-add stage-model-row">
                      <span style={{ width: "64px", "flex-shrink": 0 }}>
                        /stage{stageModels().length + 1}
                      </span>
                      <ModelPicker
                        agentKind={draftStageKind() ?? (enabledAgentKinds()[0] ?? "alkaid")}
                        agentKinds={enabledAgentKinds()}
                        model={draftStageModel()}
                        allowDefault
                        defaultLabel="选择并添加模型"
                        onPickModel={(agentKind, model) => addStageModel(agentKind, model)}
                        title="添加 Stage 模型"
                        portal
                      />
                      <span class="share-row-plus" title="选择模型后自动加入列表">
                        <IconPlus size={13} />
                      </span>
                    </div>
                  </Show>
                </div>
              </div>
              <div class="field">
                <span class="field-label">轻量级模型</span>
                <ModelPicker
                  agentKind={lightweightAgent()}
                  agentKinds={titleAgentKinds()}
                  model={lightweightModel()}
                  onPickModel={(a, m) => {
                    setLightweightAgent(a);
                    setLightweightModel(m);
                  }}
                  prefix="轻量模型"
                  title="轻量级模型"
                  portal
                />
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
              <h3 class="settings-group-title">上下文机制</h3>
              <label class="field">
                <span class="field-label">Cursor 上下文</span>
                <select
                  class="field-input"
                  value={cursorContextMode()}
                  onChange={(e) =>
                    setCursorContextMode(e.currentTarget.value === "super" ? "super" : "default")
                  }
                >
                  <option value="default">默认</option>
                  <option value="super">超级（旧版超级上下文）</option>
                </select>
                <span class="field-hint">
                  默认复用 live Cursor session；超级切换到改造前备份的 fresh Agent、compact memory 与自动摘要实现。
                </span>
              </label>
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
                  FastContext 使用原生常驻服务和增量索引；旧配置中的 SuperContext 会自动回退到 FastContext。保存后重启相关 Agent 生效。
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
                  默认开启。关闭后 Lyra 和 Vega 不再提供切换工作目录/项目的工具，也不会注入对应提示词。
                </span>
              </div>
            </section>

            <section class="settings-group">
              <h3 class="settings-group-title">聊天视图</h3>
              <label class="field">
                <span class="field-label">渲染方式</span>
                <select
                  class="field-input"
                  value={chatViewRender()}
                  onChange={(e) => {
                    const v = e.currentTarget.value;
                    setChatViewRender(
                      v === "dom" ? "dom" : v === "canvas_qwen" ? "canvas_qwen" : "canvas",
                    );
                  }}
                >
                  <option value="canvas">Canvas（默认）</option>
                  <option value="canvas_qwen">canvas(qwen)</option>
                  <option value="dom">DOM</option>
                </select>
                <span class="field-hint">
                  Canvas 在超长会话时更省 DOM 节点、滚动更轻；DOM 选区/复制更可靠；canvas(qwen) 为 GLM canvas 渲染实验实现。可随时切换，保存后立即生效。
                </span>
              </label>
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

          {/* ===== 模型后端 ===== */}
          <Show when={tab() === "backends"}>
            <p class="field-hint">
              每个后端可单独启用/关闭并配置启动方式。关闭的后端不会出现在新建/切换会话的后端列表里（历史会话仍可打开查看）。
            </p>

            <div class="backend-card">
              <div class="backend-card-head">
                <span class={`agent-badge alkaid`}>{agentLabel("alkaid")}</span>
                <span class="fixed-integration">PI</span>
                <label class="backend-switch">
                  <input
                    type="checkbox"
                    checked={vegaEnabled()}
                    disabled={vegaEnabled() && enabledCount() === 1}
                    onChange={(e) => setVegaEnabled(e.currentTarget.checked)}
                  />
                  <span>启用</span>
                </label>
              </div>
              <span class="field-hint">复用本机 Codex provider 凭据，支持并行文件工具、MCP 与 Skills。</span>
              <ProxyField value={vegaProxy()} onInput={setVegaProxy} />
              <div class="backend-quota-row">
                <span class="field-label">本地配置</span>
                <span class="field-hint">修改 ~/.nova/alkaid/config.jsonc 后点此按钮，立即重载模型列表、补全与预热配置。</span>
                <Show when={vegaRefreshMsg()}>
                  <span class="field-hint">{vegaRefreshMsg()}</span>
                </Show>
                <button
                  type="button"
                  class="link-btn backend-quota-refresh"
                  disabled={vegaRefreshing()}
                  onClick={() => void refreshVegaConfig()}
                >
                  {vegaRefreshing() ? "刷新中…" : "刷新配置"}
                </button>
              </div>
            </div>

            <div class="backend-card">
              <div class="backend-card-head">
                <span class={`agent-badge lyra`}>{agentLabel("lyra")}</span>
                <span class="fixed-integration">原生</span>
                <label class="backend-switch">
                  <input
                    type="checkbox"
                    checked={lyraEnabled()}
                    disabled={lyraEnabled() && enabledCount() === 1}
                    onChange={(e) => setLyraEnabled(e.currentTarget.checked)}
                  />
                  <span>启用</span>
                </label>
              </div>
              <span class="field-hint">Rust 原生 agent，不经 Node bridge；与 Vega 共用模型配置与 Skills。</span>
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
              <CliManager
                status={cliStatuses().devin}
                loading={cliLoading()}
                busy={upgradingCli() !== null}
                upgrading={upgradingCli() === "devin"}
                message={cliMessages().devin}
                progress={cliProgress().devin}
                output={cliOutputs().devin}
                onUpgrade={() => void upgradeCli("devin")}
              />
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
              <CliManager
                status={cliStatuses().codebuddy}
                loading={cliLoading()}
                busy={upgradingCli() !== null}
                upgrading={upgradingCli() === "codebuddy"}
                message={cliMessages().codebuddy}
                progress={cliProgress().codebuddy}
                output={cliOutputs().codebuddy}
                onUpgrade={() => void upgradeCli("codebuddy")}
              />
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
                <span class={`agent-badge claudecode`}>{agentLabel("claudecode")}</span>
                <span class="fixed-integration">SDK</span>
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
                progress={cliProgress().claudecode}
                output={cliOutputs().claudecode}
                onUpgrade={() => void upgradeCli("claudecode")}
              />
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
              <ProxyField value={claudecodeProxy()} onInput={setClaudecodeProxy} />
            </div>

            <div class="backend-card">
              <div class="backend-card-head">
                <span class={`agent-badge codex`}>{agentLabel("codex")}</span>
                <span class="fixed-integration">SDK</span>
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
                progress={cliProgress().codex}
                output={cliOutputs().codex}
                onUpgrade={() => void upgradeCli("codex")}
              />
              <div class="backend-fields">
                <label class="backend-field">
                  <span class="field-label">可执行文件</span>
                  <input class="field-input" value={codexPath()} onInput={(e) => setCodexPath(e.currentTarget.value)} placeholder="codex" />
                </label>
              </div>
              <ProxyField value={codexProxy()} onInput={setCodexProxy} />
            </div>

            <div class="backend-card">
              <div class="backend-card-head">
                <span class={`agent-badge cursor`}>{agentLabel("cursor")}</span>
                <span class="fixed-integration">SDK</span>
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
              <div class="backend-fields">
                <label class="backend-field backend-field-wide">
                  <span class="field-label">Cursor API Key</span>
                  <input class="field-input" value={cursorSdkApiKey()} onInput={(e) => setCursorSdkApiKey(e.currentTarget.value)} placeholder="留空使用 CURSOR_API_KEY" />
                </label>
              </div>
              <ProxyField value={cursorProxy()} onInput={setCursorProxy} />
              <label class="backend-switch" style={{ "align-self": "flex-start" }}>
                <input
                  type="checkbox"
                  checked={cursorDisableSubagents()}
                  onChange={(e) => setCursorDisableSubagents(e.currentTarget.checked)}
                />
                <span>阻止 Task / subagent</span>
              </label>
              <div class="field-hint">
                默认关闭。开启后写入 Cursor 全局 hook；关闭会清除 Nova 写入的 hook 和脚本，并保留其他 hook。
              </div>
              <div class="cursor-context-config">
                <div class="cursor-context-head">
                  <div>
                    <div class="field-label">模型上下文窗口</div>
                    <div class="field-hint">按模型 ID 包含匹配，最长匹配串优先；未匹配时使用 128K。</div>
                  </div>
                  <button type="button" class="btn secondary cursor-context-add" onClick={addCursorModelContext}>
                    添加
                  </button>
                </div>
                <Show when={cursorModelContexts().length > 0} fallback={
                  <div class="cursor-context-empty">暂无自定义规则。</div>
                }>
                  <div class="cursor-context-list">
                    <Index each={cursorModelContexts()}>
                      {(rule, index) => (
                        <div class="cursor-context-row">
                          <input
                            class="field-input"
                            value={rule().prefix}
                            onInput={(event) => updateCursorModelContext(index, { prefix: event.currentTarget.value })}
                            placeholder="模型匹配串，如 claude-4"
                            aria-label="Cursor 模型匹配串"
                          />
                          <select
                            class="field-input"
                            value={String(rule().contextWindow)}
                            onChange={(event) => updateCursorModelContext(index, {
                              contextWindow: Number(event.currentTarget.value),
                            })}
                            aria-label="Cursor 模型上下文窗口"
                          >
                            <For each={CURSOR_CONTEXT_WINDOW_OPTIONS}>
                              {(option) => <option value={option.value}>{option.label}</option>}
                            </For>
                          </select>
                          <button
                            type="button"
                            class="icon-btn cursor-context-remove"
                            onClick={() => removeCursorModelContext(index)}
                            aria-label="删除模型上下文规则"
                            title="删除"
                          >
                            <IconX size={14} />
                          </button>
                        </div>
                      )}
                    </Index>
                  </div>
                </Show>
              </div>
            </div>

            <div class="backend-card">
              <div class="backend-card-head">
                <span class={`agent-badge opencode`}>{agentLabel("opencode")}</span>
                <span class="fixed-integration">SDK</span>
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
                progress={cliProgress().opencode}
                output={cliOutputs().opencode}
                onUpgrade={() => void upgradeCli("opencode")}
              />
              <div class="backend-fields">
                <label class="backend-field">
                  <span class="field-label">可执行文件</span>
                  <input class="field-input" value={opencodePath()} onInput={(e) => setOpencodePath(e.currentTarget.value)} placeholder="opencode" />
                </label>
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
                  onClick={() => void syncGlobalInstructions()}
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

            <div class="field">
              <span class="field-label">共享模型额度</span>
              <span class="field-hint">
                每次添加一行；共享的模型会以“我的 Cursor”这类一级分类出现在队友的新会话模型选择器中。首次添加会安全同步并预热额度租约；移除后，旧缓存中的入口也无法再使用。
              </span>
              <Show
                when={quotaShareKinds().length > 0}
                fallback={<div class="sel-empty">暂无可共享的后端，请先在「后端」页启用。</div>}
              >
                <div class="share-list">
                  <Index each={quotaSharedModels()}>
                    {(key, index) => {
                      const parsed = () => parseQuotaShareKey(key());
                      return (
                        <Show when={parsed()}>
                          {(entry) => (
                            <div class="share-row" title={key()}>
                              <ModelPicker
                                agentKind={entry().kind}
                                agentKinds={quotaRowKinds(entry().kind)}
                                model={entry().model}
                                onPickModel={(kind, model) =>
                                  replaceQuotaSharedModel(key(), kind, model)
                                }
                                title="共享模型"
                                portal
                              />
                              <button
                                type="button"
                                class="icon-btn share-row-remove"
                                title="移除该模型"
                                aria-label="移除该模型"
                                onClick={() => removeQuotaSharedModelAt(index)}
                              >
                                <IconX size={14} />
                              </button>
                            </div>
                          )}
                        </Show>
                      );
                    }}
                  </Index>
                  <Show when={quotaSharedModels().length === 0}>
                    <div class="share-empty">尚未共享任何模型，用下方选择器添加一行。</div>
                  </Show>
                  <div class="share-row share-row-add">
                    <ModelPicker
                      agentKind={draftShareKind() ?? (quotaShareKinds()[0] ?? "cursor")}
                      agentKinds={quotaShareKinds()}
                      model={draftShareModel()}
                      allowDefault
                      defaultLabel="选择要共享的模型"
                      onPickModel={(kind, model) => addQuotaSharedModel(kind, model)}
                      title="添加共享模型"
                      portal
                    />
                    <span class="share-row-plus" title="选择模型后自动加入列表">
                      <IconPlus size={13} />
                    </span>
                  </div>
                </div>
              </Show>
            </div>
          </Show>

          {/* ===== 经验训练 ===== */}
          <Show when={tab() === "memory"}>
            <div class="field">
              <span class="field-label">经验训练</span>
              <span class="field-hint">
                经验是从会话结果中学习出的条件性结论；它与“客观做过什么”的记忆、以及“必须遵守”的守则严格分库存储。Lyra 只会按需加载经验，并用反馈调整经验，不会改写记忆或守则。
              </span>
            </div>
            <label class="field" style={{ display: "flex", "flex-direction": "row", "align-items": "center", gap: "8px" }}>
              <input type="checkbox" checked={experienceTrainingEnabled()} onChange={(e) => setExperienceTrainingEnabled(e.currentTarget.checked)} />
              <span>定期从新会话训练多个独立知识库</span>
            </label>
            <span class="field-hint">开启后 Lyra fast_context 会并行召回大熊座训练知识，Lyra 提示词和工具列表也会出现训练知识与反馈能力。</span>
            <div class="field">
              <span class="field-label">训练模型</span>
              <ModelPicker
                agentKind={experienceTrainingAgent()}
                agentKinds={trainingAgentKinds()}
                model={experienceTrainingModel()}
                onPickModel={(agentKind, model) => {
                  setExperienceTrainingAgent(agentKind);
                  setExperienceTrainingModel(model);
                }}
                portal
                favorites
              />
              <span class="field-hint">复用新会话同款后端 / 模型选择器；选择会保存到经验训练配置。</span>
            </div>
            <label class="field">
              <span class="field-label">训练间隔（分钟）</span>
              <input class="field-input" type="number" min="5" value={experienceTrainingIntervalMinutes()} onInput={(e) => setExperienceTrainingIntervalMinutes(Number(e.currentTarget.value))} />
            </label>
            <label class="field">
              <span class="field-label">世代演进间隔（分钟）</span>
              <input class="field-input" type="number" min="10" value={experienceEvolutionIntervalMinutes()} onInput={(e) => setExperienceEvolutionIntervalMinutes(Number(e.currentTarget.value))} />
              <span class="field-hint">到达间隔后自动执行一次群岛遗传世代演进（选择、交叉、变异、迁移、淘汰）。手动演进不受此限制。</span>
            </label>
            <div class="field">
              <span class="field-label" style={{ display: "flex", "align-items": "center", "justify-content": "space-between", gap: "8px" }}>
                <span>经验专家配置</span>
                <button type="button" class="btn secondary" onClick={resetExperienceExperts}>重置为基础权重</button>
              </span>
              <span class="field-hint">每个专家只提供独立经验库；学习率、遗忘率和迁移率不同，以维持学习多样性。重置只恢复专家名称和参数，不会删除已经训练出的内容。</span>
              <div class="expert-grid">
                <For each={experienceExperts()}>
                  {(expert, index) => (
                    <div class="expert-row">
                      <strong title={expert.id}>{expert.name || expert.id}</strong>
                      {([
                        ["writeRate", "写入率"],
                        ["valueLearningRate", "更新率"],
                        ["forgetRate", "遗忘率"],
                        ["migrationRate", "迁移率"],
                      ] as const).map(([key, label]) => (
                        <label>
                          <span>{label}</span>
                          <input class="field-input" type="number" min="0" max="1" step="0.01" value={expert[key]} onInput={(e) => updateExperienceExpert(index(), key, Number(e.currentTarget.value))} />
                        </label>
                      ))}
                    </div>
                  )}
                </For>
              </div>
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
              <span class="field-label">更新通道</span>
              <div class="seg-control" role="radiogroup" aria-label="更新通道">
                <button
                  type="button"
                  classList={{ active: updateChannel() === "release" }}
                  disabled={checking()}
                  onClick={() => void changeUpdateChannel("release")}
                >
                  正式版
                </button>
                <button
                  type="button"
                  classList={{ active: updateChannel() === "pre-release" }}
                  disabled={checking()}
                  onClick={() => void changeUpdateChannel("pre-release")}
                >
                  预发布版
                </button>
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
          <button class="btn secondary" onClick={props.onClose}>
            取消
          </button>
          <button class="btn primary" disabled={saving()} onClick={() => void save()}>
            {saving() ? "保存中…" : "保存"}
          </button>
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
