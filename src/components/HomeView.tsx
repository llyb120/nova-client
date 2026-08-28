import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";

import { api } from "../ipc";
import { rememberPromptDraft, takePromptDraft } from "../promptDraft";
import {
  promptHistory,
  rememberPromptHistory,
  type PromptHistoryItem,
} from "../promptHistory";
import {
  createRoamingThread,
  createQuotaThread,
  createThread,
  createThreadOptimistic,
  clearPendingClueCard,
  clearQuotaRoamingProgress,
  enabledAgentKinds,
  ensureModelOptions,
  ensurePeerBranches,
  ensurePeerModels,
  lastUsed,
  peerBranchKey,
  modelChoices,
  openThread,
  preloadPeerModels,
  refreshSlashCommands,
  resolveAvailableModel,
  resolveEnabledAgentKind,
  quotaPeers,
  roamingPeers,
  sendPrompt,
  setTrainingProject,
  setView,
  state,
  stashWorktreePrompt,
  takePendingNewSessionSeed,
  assertBuiltinPrompt,
} from "../store";
import { mountSessionShortcuts } from "../sessionShortcuts";
import { isPasteFilePathsShortcut, resolveClipboardFilePaths } from "../pasteFilePaths";
import type { AgentKind, Peer } from "../types";
import { agentLabel, isScratch } from "../utils";
import { enabledWorkflows } from "../workflow/storage";
import type { WorkflowDef } from "../workflow/types";
import { ConfigSelects, type QuotaModelPeer, type SharedModelSource } from "./ConfigSelects";
import { ExclusiveChatMark } from "./ExclusiveChatMark";
import { IconClue, IconFile, IconFolder, IconLogo, IconMerge, IconSend, IconX } from "./icons";
import { createImageAttachments, ImageAttachmentStrip } from "./ImageAttachmentStrip";
import { createNoteFlow } from "./NoteFlow";
import { ProjectPicker } from "./ProjectPicker";
import { fitSlashMenuHeight } from "./slashMenuLayout";
import { getSlashSuggestions, type SlashSuggestion } from "./slashSuggestions";
import { TypewriterText } from "./TypewriterText";

const LAST_NEW_THREAD_PROJECT_KEY = "fd:lastNewThreadProject";

/** codex 风格草稿首页：输入任务 + 选择项目/模型/模式，回车即开干 */
export function HomeView() {
  const sessionSeed = takePendingNewSessionSeed();
  const [text, setText] = createSignal("");
  const [quote, setQuote] = createSignal(sessionSeed?.quote ?? "");
  const [cursor, setCursor] = createSignal(0);
  const [slashStart, setSlashStart] = createSignal<number | null>(null);
  const [activeSlashIndex, setActiveSlashIndex] = createSignal(0);
  const attach = createImageAttachments({ enableFileDrop: true });
  const noteFlow = createNoteFlow();
  const [cwd, setCwd] = createSignal(
    sessionSeed && !sessionSeed.roam ? sessionSeed.cwd : "",
  );
  const [agentKind, setAgentKind] = createSignal<AgentKind>(
    resolveEnabledAgentKind(sessionSeed?.agentKind ?? lastUsed.agentKind()),
  );
  // 默认沿用上一次使用的模型；模式固定 Build（Plan 仅由 /plan 启动）。
  const [model, setModel] = createSignal(sessionSeed?.model ?? lastUsed.model(agentKind()));
  const [mode, setMode] = createSignal("build");
  const [busy, setBusy] = createSignal(false);
  // 工作流：新会话页选定后，发送时以输入内容为 goal、本会话为根启动流程（等效 /run）。
  // 只对本地普通会话生效，与漫游/额度互斥。
  const [workflowMenuOpen, setWorkflowMenuOpen] = createSignal(false);
  const [selectedWorkflowId, setSelectedWorkflowId] = createSignal<string | null>(null);
  const selectedWorkflow = createMemo<WorkflowDef | null>(() => {
    const id = selectedWorkflowId();
    if (!id) return null;
    // 已停用的工作流不展示也不可选：一旦在别处被停用，这里的选中自动失效。
    return enabledWorkflows().find((workflow) => workflow.id === id) ?? null;
  });
  // 每次打开菜单都重新读取 localStorage，保证工作流编辑器改动后列表最新；只列启用的。
  const workflowChoices = createMemo<WorkflowDef[]>(() => {
    workflowMenuOpen();
    return enabledWorkflows();
  });
  const [quotaCancelling, setQuotaCancelling] = createSignal(false);
  // 漫游目标：选中队友目录后在对方机器上执行，本机只接收
  const [roam, setRoam] = createSignal<{ peer: Peer; folder: string } | null>(null);
  // 额度租借目标：代码仍在 A 的本地目录执行，只临时使用所选队友的后端凭证/额度。
  const [quotaPeer, setQuotaPeer] = createSignal<Peer | null>(null);
  // worktree：在独立 git worktree（分支 + 工作目录）中执行，不干扰主工作区正在进行的任务。
  // 通过 Alt+Enter 或工具条按钮弹窗填分支名后创建（不占用输入行空间）。
  const [cwdIsRepo, setCwdIsRepo] = createSignal(false);
  const [showWorktreeDialog, setShowWorktreeDialog] = createSignal(false);
  const [wtBranchDraft, setWtBranchDraft] = createSignal("");
  // 「基于分支」可搜索下拉：branchQuery 既是搜索词也是最终 base 值（空=对应仓库当前 HEAD）
  const [branchList, setBranchList] = createSignal<{ current: string; branches: string[] } | null>(
    null,
  );
  const [branchQuery, setBranchQuery] = createSignal("");
  const [branchOpen, setBranchOpen] = createSignal(false);
  // 仅当用户真正键入时才按关键字过滤；预填/选中的分支名不应把列表过滤到只剩自己
  const [branchFiltering, setBranchFiltering] = createSignal(false);
  let wtBranchRef: HTMLInputElement | undefined;
  // 漫游首次同步时优先保留从当前会话继承的模型。
  let preferSeedModelOnRoamSync = !!sessionSeed?.roam && !!sessionSeed.model;
  let lastPrewarmKey = "";
  let lastRoamModelSyncKey = "";
  let scratchLoading = false;
  let submittingPrompt = false;
  let textareaRef: HTMLTextAreaElement | undefined;
  let slashMenuRef: HTMLDivElement | undefined;
  let historyMenuRef: HTMLDivElement | undefined;
  let workflowPickerRef: HTMLDivElement | undefined;
  type PrewarmTarget = {
    cwd?: string;
    agentKind?: AgentKind;
    model?: string;
    mode?: string;
  };

  const resizeInput = () => {
    if (!textareaRef) return;
    textareaRef.style.height = "auto";
    textareaRef.style.height = Math.min(textareaRef.scrollHeight, 220) + "px";
  };

  const pickWorkflow = (workflowId: string | null) => {
    setSelectedWorkflowId(workflowId);
    setWorkflowMenuOpen(false);
  };

  createEffect(() => {
    if (roam() || quotaPeer()) setWorkflowMenuOpen(false);
  });

  onMount(() => {
    const closeWorkflowMenu = (event: PointerEvent) => {
      if (!workflowMenuOpen() || workflowPickerRef?.contains(event.target as Node)) return;
      setWorkflowMenuOpen(false);
    };
    document.addEventListener("pointerdown", closeWorkflowMenu);
    onCleanup(() => {
      document.removeEventListener("pointerdown", closeWorkflowMenu);
    });
  });

  createEffect(() => {
    text();
    queueMicrotask(resizeInput);
  });

  const updateSlashState = (el = textareaRef, allowOpen = false) => {
    if (!el) return;
    const value = el.value;
    const pos = el.selectionStart ?? value.length;
    setCursor(pos);
    if (slashStart() === null && !allowOpen) {
      setSlashStart(null);
      return;
    }
    const prefix = value.slice(0, pos);
    const start = Math.max(prefix.lastIndexOf(" "), prefix.lastIndexOf("\n"), prefix.lastIndexOf("\t")) + 1;
    const token = prefix.slice(start);
    setSlashStart(token.startsWith("/") ? start : null);
  };

  const slashQuery = createMemo(() => {
    const start = slashStart();
    if (start === null) return null;
    return text().slice(start + 1, cursor()).toLowerCase();
  });

  const slashSuggestions = createMemo(() => {
    const query = slashQuery();
    if (query === null) return [];
    return getSlashSuggestions(agentKind(), state.slashCommands[agentKind()], query);
  });
  const [historyOpen, setHistoryOpen] = createSignal(false);
  const [activeHistoryIndex, setActiveHistoryIndex] = createSignal(0);

  createEffect(() => {
    const count = promptHistory().length;
    if (activeHistoryIndex() >= count) setActiveHistoryIndex(Math.max(0, count - 1));
    if (count === 0 && historyOpen()) setHistoryOpen(false);
  });

  createEffect(() => {
    activeHistoryIndex();
    historyMenuRef
      ?.querySelector(".prompt-history-item.active")
      ?.scrollIntoView({ block: "nearest" });
  });

  // 首页历史菜单与 slash 菜单一样向上展开，并限制在输入框上方的空间内。
  createEffect(() => {
    if (!historyOpen()) return;
    void promptHistory().length;
    const sync = () => fitSlashMenuHeight(historyMenuRef, { maxHeight: 300 });
    const frame = requestAnimationFrame(sync);
    const host = textareaRef?.closest(".composer, .home-composer");
    const ro = host instanceof HTMLElement ? new ResizeObserver(sync) : undefined;
    if (host instanceof HTMLElement) ro?.observe(host);
    window.addEventListener("resize", sync);
    onCleanup(() => {
      cancelAnimationFrame(frame);
      ro?.disconnect();
      window.removeEventListener("resize", sync);
    });
  });

  createEffect(() => {
    const count = slashSuggestions().length;
    if (activeSlashIndex() >= count) setActiveSlashIndex(Math.max(0, count - 1));
  });

  createEffect(() => {
    activeSlashIndex();
    slashMenuRef
      ?.querySelector(".slash-item.active")
      ?.scrollIntoView({ block: "nearest" });
  });

  // Slash menu opens upward; clamp height to space above the composer.
  createEffect(() => {
    if (slashQuery() === null) return;
    void slashSuggestions().length;
    const sync = () => fitSlashMenuHeight(slashMenuRef);
    const frame = requestAnimationFrame(sync);
    const host = textareaRef?.closest(".composer, .home-composer");
    const ro = host instanceof HTMLElement ? new ResizeObserver(sync) : undefined;
    if (host instanceof HTMLElement) ro?.observe(host);
    window.addEventListener("resize", sync);
    onCleanup(() => {
      cancelAnimationFrame(frame);
      ro?.disconnect();
      window.removeEventListener("resize", sync);
    });
  });

  const prewarmCurrent = (target: PrewarmTarget = {}) => {
    if (roam() || quotaPeer()) return;
    const p = target.cwd ?? cwd();
    if (!p) return;
    const nextAgentKind = target.agentKind ?? agentKind();
    const nextModel = target.model ?? model();
    const nextMode = target.mode ?? mode();
    const key = `${nextAgentKind}\n${p}\n${nextModel}\n${nextMode}`;
    if (key === lastPrewarmKey) return;
    lastPrewarmKey = key;
    const shouldRestoreFocus = document.activeElement === textareaRef;
    const selectionStart = textareaRef?.selectionStart ?? null;
    const selectionEnd = textareaRef?.selectionEnd ?? null;
    void api.prewarm(p, nextAgentKind, nextModel || null, nextMode || null).finally(() => {
      if (!shouldRestoreFocus || !textareaRef) return;
      textareaRef.focus();
      if (selectionStart !== null && selectionEnd !== null) {
        textareaRef.setSelectionRange(selectionStart, selectionEnd);
      }
    });
  };

  const pickModel = (v: string) => {
    setModel(v);
    lastUsed.setModel(agentKind(), v);
    prewarmCurrent({ model: v });
  };
  const pickModelAgent = (next: AgentKind) => {
    if (next === agentKind()) return;
    const nextModel = resolveAvailableModel(next, lastUsed.model(next));
    setAgentKind(next);
    setModel(nextModel);
    setMode("build");
    if (!usesPeerModels()) {
      void ensureModelOptions(next);
      void refreshSlashCommands(next);
    }
    prewarmCurrent({ agentKind: next, model: nextModel, mode: "build" });
  };
  // 三级菜单一次性提交「后端 + 模型」：跨后端时切后端，同后端时仅换模型
  const pickModelCombined = (next: AgentKind, m: string, borrowed?: QuotaModelPeer | null) => {
    if (borrowed) {
      const peer = quotaPeers().find((item) => item.token === borrowed.token);
      if (!peer) return;
      setRoam(null);
      setQuotaPeer(peer);
      setAgentKind(next);
      setModel(m);
      setMode("build");
      void api
        .prepareQuotaLease(peer.token, next, m)
        .catch((error) => console.warn("额度租约预热失败", error));
      return;
    }
    setQuotaPeer(null);
    if (next === agentKind()) {
      pickModel(m);
      return;
    }
    setAgentKind(next);
    setMode("build");
    if (!usesPeerModels()) {
      void ensureModelOptions(next);
      void refreshSlashCommands(next);
    }
    setModel(m);
    prewarmCurrent({ agentKind: next, model: m, mode: "build" });
  };

  // ===== 漫游：用对端（host）的模型列表，而不是本机的（本机模型对方可能没有）=====
  const roaming = () => !!roam();
  const quotaBorrowing = () => !!quotaPeer();
  const usesPeerModels = () => roaming();
  const roamPeerToken = () => roam()?.peer.token ?? null;
  const peerModels = () => {
    const t = roamPeerToken();
    return t ? state.peerModels[t] : undefined;
  };
  // 对端模型列表就绪（非漫游恒真）；未就绪时选择器显示「加载对方模型…」
  const peerReady = () => !usesPeerModels() || !!peerModels();
  // ConfigSelects 的后端列表：漫游用对端已启用的后端，否则本机已启用的
  const configAgentKinds = () =>
    usesPeerModels() ? peerModels()?.backends ?? [] : enabledAgentKinds();
  // 模型选项来源：漫游取对端列表（缺失返回 null → 空列表），否则用本机全局
  const peerModelSource = (k: AgentKind) => peerModels()?.options[k] ?? null;
  const quotaSharedModelSources = createMemo<SharedModelSource[]>(() =>
    quotaPeers()
      .map((peer) => ({
        peer: { token: peer.token, name: peer.name },
        options: state.peerModels[peer.token]?.sharedOptions ?? {},
      }))
      .filter((source) => Object.keys(source.options).length > 0),
  );
  // 空值才落到可用项；已有选择即使暂不在列表也保留（Cursor 目录未就绪等中间态不应重置成 Auto）。
  createEffect(() => {
    if (usesPeerModels() || quotaBorrowing()) return;
    const choices = modelChoices(agentKind());
    if (choices.length === 0) return;
    const current = model();
    const resolved = resolveAvailableModel(agentKind(), current);
    if (resolved !== current) setModel(resolved);
  });

  // 漫游：拉取对端模型；到达后采用对端当前模型，并把后端/模式收敛到可用项。
  // 只在切换对端或对端配置确实变化时同步，避免覆盖用户随后在选择器里的手动选择。
  createEffect(() => {
    const t = roamPeerToken();
    if (!t) {
      lastRoamModelSyncKey = "";
      return;
    }
    ensurePeerModels(t);
    const pm = state.peerModels[t];
    if (!pm) return;
    const backend = pm.backends.includes(agentKind()) ? agentKind() : pm.backends[0];
    if (!backend) return;
    if (backend !== agentKind()) setAgentKind(backend);

    const source = pm.options[backend] ?? null;
    const models = modelChoices(backend, source);
    const syncKey = `${t}\n${JSON.stringify(pm)}`;
    if (syncKey !== lastRoamModelSyncKey) {
      lastRoamModelSyncKey = syncKey;
      const remoteModel = source?.configOptions?.find((option) => option.id === "model")
        ?.currentValue;
      let nextModel = models.find((item) => item.value)?.value ?? models[0]?.value ?? "";
      if (
        preferSeedModelOnRoamSync &&
        model() &&
        models.some((item) => item.value === model())
      ) {
        nextModel = model();
        preferSeedModelOnRoamSync = false;
      } else if (remoteModel && models.some((item) => item.value === remoteModel)) {
        nextModel = remoteModel;
      }
      setModel(nextModel);
    } else if (!models.some((item) => item.value === model())) {
      setModel(models.find((item) => item.value)?.value ?? models[0]?.value ?? "");
    }
    setMode("build");
  });

  // 只加载当前后端；其他后端在用户打开模型选择器时按需加载。
  createEffect(() => {
    if (!usesPeerModels() && !quotaBorrowing()) void ensureModelOptions(agentKind());
  });

  // 当前选中的后端若被设置里关闭，回退到第一个启用的后端（漫游时后端由对端决定，跳过）
  createEffect(() => {
    if (usesPeerModels() || quotaBorrowing()) return;
    const next = resolveEnabledAgentKind(agentKind());
    if (next !== agentKind()) pickModelAgent(next);
  });

  // worktree 是否可用：漫游（对方仓库交给 host 校验）或本地当前目录是 git 仓库
  const worktreeAvailable = () => roaming() || (!quotaBorrowing() && cwdIsRepo());

  // 本地会话：判断当前目录是否 git 仓库（决定 worktree 开关可用性）。
  // 漫游目录在对方机器上，无法本地判断，统一按可用处理、交由 host 校验。
  createEffect(() => {
    const dir = cwd();
    if (usesPeerModels() || quotaBorrowing() || !dir) {
      setCwdIsRepo(false);
      return;
    }
    let stale = false;
    onCleanup(() => {
      stale = true;
    });
    void api.isGitRepo(dir).then((ok) => {
      if (!stale) setCwdIsRepo(ok);
    });
  });

  const selectProject = (p: string, warm = false) => {
    setRoam(null); // 选了本地项目就退出漫游
    setCwd(p);
    if (isScratch(p)) localStorage.removeItem(LAST_NEW_THREAD_PROJECT_KEY);
    else localStorage.setItem(LAST_NEW_THREAD_PROJECT_KEY, p);
    if (warm) prewarmCurrent({ cwd: p });
  };

  const insertShortcutText = (snippet: string, mayFocus: boolean): boolean => {
    if (!textareaRef) return false;
    const focused = document.activeElement === textareaRef;
    // 焦点不在输入框：允许全局触发时先聚焦并追加到末尾；否则忽略（如其它输入框里的无修饰键输入）。
    if (!focused && !mayFocus) return false;
    const start = focused ? (textareaRef.selectionStart ?? text().length) : text().length;
    const end = focused ? (textareaRef.selectionEnd ?? start) : text().length;
    const next = `${text().slice(0, start)}${snippet}${text().slice(end)}`;
    const nextCursor = start + snippet.length;
    setText(next);
    setCursor(nextCursor);
    queueMicrotask(() => {
      if (!textareaRef) return;
      textareaRef.focus();
      textareaRef.setSelectionRange(nextCursor, nextCursor);
      updateSlashState(textareaRef, true);
      resizeInput();
    });
    return true;
  };

  mountSessionShortcuts({
    allowedActions: ["selectProject", "selectModel", "insertText"],
    onInsertText: insertShortcutText,
    onSelectProject: (path, roam) => {
      if (roam) {
        const peer = state.peers.find((item) => item.token === roam.peerToken);
        if (!peer?.online) {
          void message(`队友不在线，无法漫游：${peer?.name ?? "未知队友"}`, { kind: "error" });
          return;
        }
        if (!peer.folders.some((folder) => folder.path === roam.folder)) {
          void message(`对方未共享该目录：${roam.folder}`, { kind: "error" });
          return;
        }
        setQuotaPeer(null);
        setRoam({ peer, folder: roam.folder });
        ensurePeerModels(peer.token, true);
        return;
      }
      void (async () => {
        try {
          if (!(await api.directoryExists(path))) {
            void message(`项目目录不存在：${path}`, { kind: "error" });
            return;
          }
        } catch {
          // 目录校验不可用时仍尝试切换。
        }
        selectProject(path, true);
      })();
    },
    onSelectModel: (next, modelId, quotaPeer) => {
      if (quotaPeer && !quotaPeers().some((peer) => peer.token === quotaPeer.token)) {
        void message(`队友不在线，无法使用共享模型：${quotaPeer.name}`, { kind: "error" });
        return;
      }
      pickModelCombined(next, modelId, quotaPeer);
    },
  });

  const ensureScratchProject = () => {
    if (cwd() || scratchLoading) return;
    scratchLoading = true;
    void api.scratchDir().then((dir) => {
      if (!cwd()) selectProject(dir);
    }).finally(() => {
      scratchLoading = false;
    });
  };

  const restoreLastProject = async () => {
    const remembered = localStorage.getItem(LAST_NEW_THREAD_PROJECT_KEY)?.trim();
    if (remembered) {
      try {
        if (await api.directoryExists(remembered)) {
          selectProject(remembered);
          return;
        }
      } catch {
        // 目录校验不可用时按“不存在”处理，避免新会话卡在无效路径。
      }
      localStorage.removeItem(LAST_NEW_THREAD_PROJECT_KEY);
    }
    ensureScratchProject();
  };

  onMount(() => {
    if (!sessionSeed) {
      void restoreLastProject();
      return;
    }
    if (sessionSeed.roam) {
      const peer = state.peers.find((item) => item.token === sessionSeed.roam!.peerToken);
      if (
        peer?.online &&
        peer.folders.some((folder) => folder.path === sessionSeed.roam!.folder)
      ) {
        setQuotaPeer(null);
        setRoam({ peer, folder: sessionSeed.roam.folder });
        ensurePeerModels(peer.token, true);
        return;
      }
      void restoreLastProject();
      return;
    }
    if (sessionSeed.cwd) selectProject(sessionSeed.cwd);
    else void restoreLastProject();
    if (sessionSeed.quotaPeerToken) {
      const peer = quotaPeers().find((item) => item.token === sessionSeed.quotaPeerToken);
      if (peer) {
        pickModelCombined(sessionSeed.agentKind, sessionSeed.model, {
          token: peer.token,
          name: peer.name,
        });
      }
    }
  });
  // 每次进入新会话页都强制校准在线队友的共享模型，避免沿用旧 peerModels 缓存。
  onMount(() => preloadPeerModels(true));
  // 快捷键/侧栏「新会话」后聚焦首页输入框（含已在首页、组件未重挂载的情况）。
  createEffect(() => {
    if (state.homeComposerFocusAt <= 0) return;
    queueMicrotask(() => textareaRef?.focus());
  });
  onCleanup(() => {
    if (!submittingPrompt) rememberPromptDraft(text(), attach.images());
  });

  const onInput = (e: InputEvent) => {
    const el = e.currentTarget as HTMLTextAreaElement;
    const typedSlash = e.inputType === "insertText" && e.data === "/";
    const trackingSlash = slashStart() !== null;
    setText(el.value);
    if (historyOpen()) setHistoryOpen(false);
    noteFlow.bump();
    if (typedSlash) void refreshSlashCommands(agentKind());
    updateSlashState(el, typedSlash || trackingSlash);
    if (el.value.trim()) prewarmCurrent();
  };

  const composerPlaceholder = () => {
    const wt = worktreeAvailable() ? " · Alt+Enter 在 worktree 执行" : "";
    const target = roam();
    if (target) return `描述任务，将在 ${target.peer.name} 的机器上执行（Enter 发送${wt}）`;
    const quota = quotaPeer();
    if (quota) return `描述任务，将在本机执行并使用 ${quota.name} 的额度（Enter 发送）`;
    if (cwd()) return `描述任务，Enter 发送 · Ctrl+Enter 临时会话${wt}`;
    return "先选择一个项目目录…";
  };

  const submit = async (
    opts: { ephemeral?: boolean; worktree?: boolean; branch?: string; base?: string } = {},
  ) => {
    const t = text().trim();
    const quoted = quote().trim();
    const prompt = quoted ? `<nova_quote>\n${quoted}\n</nova_quote>\n\n${t}` : t;
    const images = attach.images();
    const target = roam();
    const quota = quotaPeer();
    const workflow = !target && !quota ? selectedWorkflow() : null;
    const clue = state.pendingClueCard;
    if (t === "/train" && images.length === 0) {
      if (busy()) return;
      setBusy(true);
      try {
        setTrainingProject(cwd());
        const result = await api.trainExperience(cwd());
        setText("");
        setSlashStart(null);
        // 大熊座训练静默进行：成功/无新会话均不弹窗，错误仍提示；
        // 训练完成会切换到训练视图，用户可在那里看到最新结果。
        if (!result.trained && result.reason === "noNewSessions") return;
        setView("training");
      } catch (error) {
        await message(String(error), { kind: "error" });
      } finally {
        setBusy(false);
      }
      return;
    }
    if (
      (!t && images.length === 0) ||
      busy() ||
      (!cwd() && !target) ||
      !peerReady() ||
      (usesPeerModels() && configAgentKinds().length === 0)
    ) return;
    const wtOn = opts.worktree === true && worktreeAvailable();
    const branch = opts.branch?.trim() ?? "";
    const base = wtOn ? opts.base?.trim() ?? "" : "";
    if (wtOn && !branch && !base) return; // 新分支名与基于分支至少填一个（留空分支名 = 直接用所选分支）
    submittingPrompt = true;
    rememberPromptHistory(t, images);
    setQuotaCancelling(false);
    setBusy(true);
    try {
      if (target) {
        // 漫游：worktree 由 host 后台创建，首条提示词走后端排队机制，正常发送即可
        await createRoamingThread(
          target.peer,
          target.folder,
          agentKind(),
          model(),
          mode(),
          prompt,
          clue?.id ?? "",
          wtOn,
          branch,
          base,
        );
        if (clue) clearPendingClueCard();
        await sendPrompt(prompt, images);
      } else if (quota) {
        await createQuotaThread(quota, cwd(), agentKind(), model(), mode(), clue?.id ?? "");
        if (clue) clearPendingClueCard();
        await sendPrompt(prompt, images);
      } else if (wtOn) {
        // 本地 worktree：后台创建、界面立即进入会话，就绪后再自动补发首条提示词。
        // 先校验 /fire，避免建完 worktree 才因语法错误失败。
        assertBuiltinPrompt(t, images);
        const id = await createThread(
          cwd(),
          agentKind(),
          model(),
          mode(),
          "",
          opts.ephemeral ?? false,
          true,
          branch,
          base,
          clue?.id ?? "",
        );
        if (clue) clearPendingClueCard();
        stashWorktreePrompt(id, prompt, images, workflow?.id ?? null);
      } else {
        const ephemeral = opts.ephemeral ?? false;
        const workflowId = workflow?.id ?? null;
        if (workflowId) {
          // 工作流：会话需先建好再启动工作流，沿用同步创建，不乐观跳转。
          await createThread(
            cwd(),
            agentKind(),
            model(),
            mode(),
            "",
            ephemeral,
            false,
            "",
            "",
            clue?.id ?? "",
          );
          if (!ephemeral) lastUsed.setModel(agentKind(), model());
          if (clue) clearPendingClueCard();
          await sendPrompt(prompt, images, workflowId);
        } else {
          // 乐观进入聊天页：消息立即上屏，后台建会话并补发，失败回退。
          if (!ephemeral) lastUsed.setModel(agentKind(), model());
          const clueId = clue?.id ?? "";
          if (clue) clearPendingClueCard();
          createThreadOptimistic(
            cwd(),
            agentKind(),
            model(),
            mode(),
            "",
            prompt,
            images,
            ephemeral,
            clueId,
          );
        }
      }
      setText("");
      setQuote("");
      setSlashStart(null);
      setRoam(null);
      setQuotaPeer(null);
      setSelectedWorkflowId(null);
      attach.clear();
      clearPendingClueCard();
      if (textareaRef) textareaRef.style.height = "auto";
      submittingPrompt = false;
    } catch (error) {
      submittingPrompt = false;
      const text = String(error);
      if (!text.includes("额度漫游已取消")) {
        await message(text, { kind: "error" });
      }
    } finally {
      clearQuotaRoamingProgress();
      setQuotaCancelling(false);
      setBusy(false);
    }
  };

  const cancelQuotaPreparation = async () => {
    const operationId = state.quotaRoamingProgress?.operationId;
    if (!operationId || quotaCancelling()) return;
    setQuotaCancelling(true);
    try {
      await api.cancelQuotaRoaming(operationId);
    } catch (error) {
      setQuotaCancelling(false);
      await message(String(error), { kind: "error" });
    }
  };

  // 「基于分支」下拉：按搜索词过滤 + 标出当前分支
  const branchCurrent = () => branchList()?.current ?? "";
  const filteredBranches = () => {
    const list = branchList()?.branches ?? [];
    if (!branchFiltering()) return list; // 未主动搜索：展示全部分支
    const q = branchQuery().trim().toLowerCase();
    if (!q) return list;
    return list.filter((b) => b.toLowerCase().includes(q));
  };

  // 漫游：对方分支列表异步回传后填充（未手动改过搜索框时预填对方当前分支）
  createEffect(() => {
    if (!showWorktreeDialog() || !roaming()) return;
    const target = roam();
    if (!target) return;
    const data = state.peerBranches[peerBranchKey(target.peer.token, target.folder)];
    if (data) {
      setBranchList(data);
      if (!branchQuery().trim()) setBranchQuery(data.current || "");
    }
  });

  // worktree 弹窗：Alt+Enter 触发，填「新分支名 + 基于分支」后创建 worktree 会话
  const openWorktreeDialog = () => {
    if (!worktreeAvailable()) {
      void message(
        roaming() ? "漫游目标不可用。" : "当前目录不是 git 仓库，无法在 worktree 中执行。",
        { kind: "info" },
      );
      return;
    }
    if (!text().trim() && attach.images().length === 0) {
      void message("请先在输入框描述任务，再用 worktree 执行。", { kind: "info" });
      return;
    }
    setWtBranchDraft("");
    setBranchQuery("");
    setBranchOpen(false);
    setBranchFiltering(false);
    setBranchList(null);
    setShowWorktreeDialog(true);
    queueMicrotask(() => wtBranchRef?.focus());
    // 加载「基于分支」候选：本地直接列，漫游向对方请求
    const target = roam();
    if (roaming() && target) {
      const cached = state.peerBranches[peerBranchKey(target.peer.token, target.folder)];
      if (cached) {
        setBranchList(cached);
        setBranchQuery(cached.current || "");
      }
      ensurePeerBranches(target.peer.token, target.folder);
    } else {
      const dir = cwd();
      void api
        .listBranches(dir)
        .then((data) => {
          if (!showWorktreeDialog()) return;
          setBranchList(data);
          if (!branchQuery().trim()) setBranchQuery(data.current || "");
        })
        .catch(() => {});
    }
  };
  const confirmWorktree = () => {
    const branch = wtBranchDraft().trim();
    const base = branchQuery().trim();
    // 新分支名可留空 = 不建新分支，直接把「基于分支」所选分支检出到 worktree
    if ((!branch && !base) || busy()) return;
    setShowWorktreeDialog(false);
    // 预检失败要让用户看到（典型：留空分支名但所选分支已被主工作区检出）
    void submit({ worktree: true, branch, base }).catch(
      (e) => void message(String(e), { kind: "error" }),
    );
  };

  const insertSlashSuggestion = (item: SlashSuggestion) => {
    const start = slashStart();
    if (start === null) return;
    const pos = cursor();
    const insert = item.insertText.endsWith(" ") ? item.insertText : `${item.insertText} `;
    const next = `${text().slice(0, start)}${insert}${text().slice(pos)}`;
    const nextCursor = start + insert.length;
    setText(next);
    setSlashStart(null);
    setCursor(nextCursor);
    queueMicrotask(() => {
      textareaRef?.focus();
      textareaRef?.setSelectionRange(nextCursor, nextCursor);
      resizeInput();
    });
  };

  const insertHistoryItem = (item: PromptHistoryItem) => {
    const nextCursor = item.text.length;
    setText(item.text);
    attach.set(item.images ?? []);
    setHistoryOpen(false);
    setSlashStart(null);
    setCursor(nextCursor);
    queueMicrotask(() => {
      textareaRef?.focus();
      textareaRef?.setSelectionRange(nextCursor, nextCursor);
      resizeInput();
    });
  };

  const restoreDraft = () => {
    const draft = takePromptDraft();
    if (!draft) return false;
    const nextCursor = draft.text.length;
    setText(draft.text);
    attach.set(draft.images);
    setSlashStart(null);
    setCursor(nextCursor);
    queueMicrotask(() => {
      textareaRef?.focus();
      textareaRef?.setSelectionRange(nextCursor, nextCursor);
      resizeInput();
    });
    return true;
  };

  let pasteAsPaths = false;
  let pastePathsSeq = 0;
  const insertClipboardFilePaths = (data?: DataTransfer | null) => {
    const seq = ++pastePathsSeq;
    void resolveClipboardFilePaths(data).then((paths) => {
      if (seq !== pastePathsSeq || paths.length === 0) return;
      insertShortcutText(paths.join("\n"), true);
    });
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (isPasteFilePathsShortcut(e)) {
      e.preventDefault();
      e.stopPropagation();
      pasteAsPaths = true;
      insertClipboardFilePaths();
      queueMicrotask(() => {
        pasteAsPaths = false;
      });
      return;
    }
    const suggestions = slashSuggestions();
    const history = promptHistory();
    if (historyOpen() && history.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setActiveHistoryIndex((i) => (i + 1) % history.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setActiveHistoryIndex((i) => (i - 1 + history.length) % history.length);
        return;
      }
      if (
        e.key === "Tab" ||
        (e.key === "Enter" && !e.shiftKey && !e.ctrlKey && !e.metaKey && !e.isComposing)
      ) {
        e.preventDefault();
        insertHistoryItem(history[activeHistoryIndex()] ?? history[0]);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setHistoryOpen(false);
        return;
      }
    }
    if (slashQuery() !== null && suggestions.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setActiveSlashIndex((i) => (i + 1) % suggestions.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setActiveSlashIndex((i) => (i - 1 + suggestions.length) % suggestions.length);
        return;
      }
      if (
        e.key === "Tab" ||
        (e.key === "Enter" && !e.shiftKey && !e.ctrlKey && !e.metaKey && !e.isComposing)
      ) {
        e.preventDefault();
        insertSlashSuggestion(suggestions[activeSlashIndex()] ?? suggestions[0]);
        return;
      }
    }
    if (e.key === "Escape" && slashQuery() !== null) {
      e.preventDefault();
      setSlashStart(null);
      return;
    }

    if (e.key === "ArrowUp" && !text().trim() && attach.images().length === 0 && history.length > 0) {
      e.preventDefault();
      setSlashStart(null);
      setActiveHistoryIndex(0);
      setHistoryOpen(true);
      return;
    }
    if (
      e.key === "ArrowDown" &&
      !text().trim() &&
      attach.images().length === 0 &&
      restoreDraft()
    ) {
      e.preventDefault();
      return;
    }
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      // Alt+Enter：在 git worktree 中执行（弹窗填分支名）
      if (e.altKey) {
        openWorktreeDialog();
        return;
      }
      // Ctrl/Cmd+Enter：创建临时会话，程序关闭时自动删除
      void submit({ ephemeral: e.ctrlKey || e.metaKey });
    }
  };

  // 训练与世代演进会话只在训练视图展示，不进入首页最近会话。
  const recent = () => state.threads.filter((t) => !t.experienceThread).slice(0, 6);

  return (
    <main class="home">
      <div class="home-center">
        <IconLogo size={44} class="home-logo" />
        <h1 class="home-title">我们该做什么？</h1>

        <div
          class="home-composer"
          classList={{ "is-dragging": attach.dragging() }}
        >
          <noteFlow.Notes />
          <ExclusiveChatMark
            token={roam()?.peer.token || state.settings?.relayToken || ""}
          />
          <ImageAttachmentStrip images={attach.images()} onRemove={attach.remove} />
          <Show when={quote()}>
            <div class="clue-context-chip" title={quote()}>
              <IconClue size={13} />
              <span class="clue-context-label">会话引用</span>
              <span class="clue-context-separator" aria-hidden="true" />
              <span class="clue-context-title">{quote()}</span>
              <button
                type="button"
                aria-label="移除会话引用"
                title="移除会话引用"
                onClick={() => setQuote("")}
              >
                <IconX size={12} />
              </button>
            </div>
          </Show>
          <Show when={state.pendingClueCard}>
            {(clue) => (
              <div class="clue-context-chip" title={`引用线索：${clue().title}`}>
                <IconClue size={13} />
                <span class="clue-context-label">证据链</span>
                <span class="clue-context-separator" aria-hidden="true" />
                <span class="clue-context-title">{clue().title}</span>
                <button
                  type="button"
                  aria-label="移除线索上下文"
                  title="移除线索上下文"
                  onClick={clearPendingClueCard}
                >
                  <IconX size={12} />
                </button>
              </div>
            )}
          </Show>
          <Show when={slashQuery() !== null}>
            <div ref={slashMenuRef} class="slash-menu">
              <div class="slash-menu-head">
                {agentKind() === "codex"
                  ? "Codex skills / commands"
                  : `${agentLabel(agentKind())} commands`}
              </div>
              <Show
                when={slashSuggestions().length > 0}
                fallback={<div class="slash-empty">暂无可用项</div>}
              >
                <For each={slashSuggestions()}>
                  {(item, index) => (
                    <button
                      type="button"
                      classList={{ "slash-item": true, active: index() === activeSlashIndex() }}
                      onMouseEnter={() => setActiveSlashIndex(index())}
                      onMouseDown={(e) => {
                        e.preventDefault();
                        insertSlashSuggestion(item);
                      }}
                    >
                      <span class="slash-title">{item.title}</span>
                      <span class="slash-detail">{item.detail}</span>
                      <span class="slash-kind">{item.kind}</span>
                    </button>
                  )}
                </For>
              </Show>
            </div>
          </Show>
          <Show when={historyOpen()}>
            <div ref={historyMenuRef} class="slash-menu prompt-history-menu">
              <div class="slash-menu-head">历史输入</div>
              <For each={promptHistory()}>
                {(item, index) => (
                  <button
                    type="button"
                    classList={{ "prompt-history-item": true, active: index() === activeHistoryIndex() }}
                    onMouseEnter={() => setActiveHistoryIndex(index())}
                    onMouseDown={(e) => {
                      e.preventDefault();
                      insertHistoryItem(item);
                    }}
                  >
                    <span class="prompt-history-text">{item.text}</span>
                    <Show when={item.images.length > 0}>
                      <span class="prompt-history-attach" title={`${item.images.length} 个附件`}>
                        <IconFile size={12} />
                        {item.images.length}
                      </span>
                    </Show>
                  </button>
                )}
              </For>
            </div>
          </Show>
          <div class="composer-input-wrap">
            <textarea
              ref={textareaRef}
              class="composer-input"
              placeholder={composerPlaceholder()}
              rows={3}
              value={text()}
              onInput={onInput}
              onKeyDown={onKeyDown}
              onClick={(e) => updateSlashState(e.currentTarget)}
              onKeyUp={(e) => updateSlashState(e.currentTarget)}
              onPaste={(event) => {
                const asPaths = pasteAsPaths;
                pasteAsPaths = false;
                if (asPaths) {
                  event.preventDefault();
                  insertClipboardFilePaths(event.clipboardData);
                  return;
                }
                attach.onPaste(event);
              }}
            />
          </div>
          <div class="composer-bar">
            <ProjectPicker
              value={cwd()}
              onChange={selectProject}
              roam={roam()}
              onPickRoaming={(peer, folder) => {
                setQuotaPeer(null);
                setRoam({ peer, folder });
                // 用户明确选择队友时强制刷新，不能继续依赖可能已过期的预加载缓存。
                ensurePeerModels(peer.token, true);
              }}
            />
            <Show
              when={peerReady()}
              fallback={<span class="pill roam-models-loading">模型：加载对方模型…</span>}
            >
              <ConfigSelects
                agentKind={agentKind()}
                agentKinds={configAgentKinds()}
                model={model()}
                modelSource={usesPeerModels() ? peerModelSource : undefined}
                sharedModels={usesPeerModels() ? undefined : quotaSharedModelSources()}
                quotaPeerToken={quotaPeer()?.token}
                onPickModel={pickModelCombined}
                anchorTo=".home-composer"
                favorites
              />
            </Show>
            <span class="bar-spacer" />
            <Show when={!roam() && !quotaPeer()}>
              <div ref={workflowPickerRef} class="composer-workflow-picker">
                <Show when={workflowMenuOpen()}>
                  <div class="composer-workflow-menu">
                    <div class="composer-workflow-head">本次会话运行的工作流</div>
                    <button
                      type="button"
                      classList={{
                        "composer-workflow-item": true,
                        active: !selectedWorkflowId(),
                      }}
                      onClick={() => pickWorkflow(null)}
                    >
                      <span>普通会话</span>
                      <small>不运行工作流，直接执行任务</small>
                    </button>
                    <For each={workflowChoices()}>
                      {(wf) => (
                        <button
                          type="button"
                          classList={{
                            "composer-workflow-item": true,
                            active: selectedWorkflowId() === wf.id,
                          }}
                          onClick={() => pickWorkflow(wf.id)}
                          title={wf.sharedBy ? `来自 ${wf.sharedBy} 的团队分享` : undefined}
                        >
                          <span>{wf.name}</span>
                          <small>
                            {wf.sharedBy
                              ? `团队 · ${wf.sharedBy}`
                              : wf.builtin
                                ? "内置"
                                : "自定义"} · {wf.stages.length} 个节点
                          </small>
                        </button>
                      )}
                    </For>
                  </div>
                </Show>
                <button
                  type="button"
                  class="composer-btn workflow"
                  classList={{ active: !!selectedWorkflow() }}
                  onClick={() => setWorkflowMenuOpen((open) => !open)}
                  title={
                    selectedWorkflow()
                      ? `工作流：${selectedWorkflow()!.name}`
                      : "选择用哪个工作流运行本次任务"
                  }
                >
                  <IconMerge size={16} />
                </button>
              </div>
            </Show>
            <button
              type="button"
              class="composer-btn clue"
              title={state.pendingClueCard ? "查看或更换证据链" : "从证据链发起会话"}
              onClick={() => setView("clues")}
            >
              <IconClue size={16} />
            </button>
            <button
              class="composer-btn send"
              disabled={
                (!text().trim() && attach.images().length === 0) ||
                (!!selectedWorkflow() && !text().trim() && attach.images().length === 0) ||
                (!cwd() && !roam()) ||
                busy() ||
                !peerReady() ||
                (usesPeerModels() && configAgentKinds().length === 0)
              }
              onClick={(e) => void submit({ ephemeral: e.ctrlKey || e.metaKey })}
              title="发送（Enter）· Ctrl+Enter 临时会话"
            >
              <IconSend size={16} />
            </button>
          </div>
        </div>

        <Show when={recent().length > 0}>
          <div class="recent">
            <div class="recent-label">最近会话</div>
            <For each={recent()}>
              {(t) => (
                <button class="recent-item" onClick={() => void openThread(t.id)}>
                  <IconFolder size={14} />
                  <span class={`agent-badge ${t.agentKind}`}>{agentLabel(t.agentKind)}</span>
                  <TypewriterText
                    class="recent-title"
                    text={t.title}
                    title={t.title}
                    animate={state.titleTyping[t.id]}
                  />
                  <span class="recent-cwd">
                    {t.worktree ? `${t.worktree.repo} ⎇ ${t.worktree.branch}` : t.cwd}
                  </span>
                </button>
              )}
            </For>
          </div>
        </Show>
      </div>

      <Show when={showWorktreeDialog()}>
        <div class="modal-backdrop" onClick={() => setShowWorktreeDialog(false)}>
          <div class="modal wt-dialog" onClick={(e) => e.stopPropagation()}>
            <div class="modal-head">
              <span>在 worktree 中执行</span>
              <button class="icon-btn" onClick={() => setShowWorktreeDialog(false)}>
                <IconX size={16} />
              </button>
            </div>
            <div class="modal-body">
              <p class="field-hint">
                在独立工作目录中执行，不影响
                {roaming() ? "对方的主工作区" : "当前主工作区"}
                正在进行的任务。填新分支名 = 基于所选分支切新分支；留空 =
                直接使用所选分支（不能是已检出的分支）。
              </p>
              <label class="field">
                <span class="field-label">新分支名（可留空）</span>
                <input
                  ref={wtBranchRef}
                  class="field-input"
                  placeholder="如 feature/login；留空 = 直接用下面所选分支"
                  value={wtBranchDraft()}
                  spellcheck={false}
                  onInput={(e) => setWtBranchDraft(e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      confirmWorktree();
                    } else if (e.key === "Escape") {
                      e.preventDefault();
                      setShowWorktreeDialog(false);
                    }
                  }}
                />
              </label>
              <label class="field">
                <span class="field-label">基于分支</span>
                <div class="wt-combo">
                  <input
                    class="field-input"
                    placeholder="默认当前分支 · 可搜索/手填"
                    value={branchQuery()}
                    spellcheck={false}
                    onInput={(e) => {
                      setBranchQuery(e.currentTarget.value);
                      setBranchFiltering(true);
                      setBranchOpen(true);
                    }}
                    onFocus={() => setBranchOpen(true)}
                    onBlur={() => window.setTimeout(() => setBranchOpen(false), 120)}
                    onKeyDown={(e) => {
                      if (e.key === "Escape") {
                        e.preventDefault();
                        setBranchOpen(false);
                      }
                    }}
                  />
                  <Show when={branchOpen() && filteredBranches().length > 0}>
                    <div class="wt-combo-list">
                      <For each={filteredBranches()}>
                        {(b) => (
                          <button
                            type="button"
                            class="wt-combo-item"
                            classList={{ active: b === branchQuery().trim() }}
                            onMouseDown={(e) => {
                              e.preventDefault();
                              setBranchQuery(b);
                              setBranchFiltering(false);
                              setBranchOpen(false);
                              wtBranchRef?.focus();
                            }}
                          >
                            <span class="wt-combo-name">{b}</span>
                            <Show when={b === branchCurrent()}>
                              <span class="wt-combo-cur">当前</span>
                            </Show>
                          </button>
                        )}
                      </For>
                    </div>
                  </Show>
                </div>
              </label>
            </div>
            <div class="modal-foot">
              <button class="btn secondary" onClick={() => setShowWorktreeDialog(false)}>
                取消
              </button>
              <button
                class="btn primary"
                disabled={(!wtBranchDraft().trim() && !branchQuery().trim()) || busy()}
                onClick={confirmWorktree}
              >
                {wtBranchDraft().trim() ? "创建并执行" : "在所选分支执行"}
              </button>
            </div>
          </div>
        </div>
      </Show>
      <Show
        when={
          state.quotaRoamingProgress
        }
      >
        <div class="modal-backdrop quota-loading-backdrop">
          <div class="modal quota-loading-modal">
            <div class="quota-loading-spinner" />
            <div class="quota-loading-title">正在准备额度会话</div>
            <div class="field-hint">
              {quotaCancelling()
                ? "正在取消本次额度漫游…"
                : state.quotaRoamingProgress?.message}
            </div>
            <Show when={state.quotaRoamingProgress?.stage !== "ready"}>
              <button
                class="btn secondary quota-loading-cancel"
                disabled={quotaCancelling()}
                onClick={() => void cancelQuotaPreparation()}
              >
                {quotaCancelling() ? "正在取消…" : "取消本次漫游"}
              </button>
            </Show>
          </div>
        </div>
      </Show>
    </main>
  );
}
