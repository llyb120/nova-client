import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show, untrack } from "solid-js";

import { api } from "../ipc";
import { latestFireStage } from "../threadDisplay";
import { rememberPromptDraft, takePromptDraft, saveSessionDraft, takeSessionDraft } from "../promptDraft";
import {
  promptHistory,
  rememberPromptHistory,
  type PromptHistoryItem,
} from "../promptHistory";
import {
  createRoamingThread,
  createThread,
  createThreadOptimistic,
  sceneModel,
  clearPendingClueCard,
  enabledAgentKinds,
  ensureModelOptions,
  ensurePeerBranches,
  ensurePeerModels,
  lastUsed,
  peerBranchKey,
  modelChoices,
  openThread,
  refreshSlashCommands,
  resolveAvailableModel,
  resolveEnabledAgentKind,
  roamingPeers,
  sendPrompt,
  setView,
  state,
  stashWorktreePrompt,
  takePendingNewSessionSeed,
  assertBuiltinPrompt,
} from "../store";
import { mountSessionShortcuts } from "../sessionShortcuts";
import { setHomeTerminalCwd } from "../workspaceLayout";
import { isPasteFilePathsShortcut, resolveClipboardFilePaths } from "../pasteFilePaths";
import { isSupportedAgentKind, type AgentKind, type Peer } from "../types";
import { agentLabel, isScratch } from "../utils";
import { ConfigSelects } from "./ConfigSelects";
import { IconClue, IconFile, IconFolder, IconLogo, IconSend, IconX } from "./icons";
import { createImageAttachments, ImageAttachmentStrip } from "./ImageAttachmentStrip";
import { HaibaraBubble } from "./HaibaraBubble";
import { createNoteFlow } from "./NoteFlow";
import { ProjectPicker } from "./ProjectPicker";
import { fitSlashMenuHeight } from "./slashMenuLayout";
import { getSlashSuggestions, type SlashSuggestion } from "./slashSuggestions";
import { TypewriterText } from "./TypewriterText";

const LAST_NEW_THREAD_PROJECT_KEY = "fd:lastNewThreadProject";
const AUTO_MODE_KEY = "fd:newThreadAutoMode";

/** codex 风格草稿首页：输入任务 + 选择项目/模型/模式，回车即开干 */
export function HomeView() {
  const sessionSeed = takePendingNewSessionSeed();
  const initialDraft = takeSessionDraft(null);
  const [text, setText] = createSignal(initialDraft?.text ?? "");
  const [quote, setQuote] = createSignal(sessionSeed?.quote ?? "");
  const [cursor, setCursor] = createSignal(0);
  const [slashStart, setSlashStart] = createSignal<number | null>(null);
  const [activeSlashIndex, setActiveSlashIndex] = createSignal(0);
  const attach = createImageAttachments({ enableFileDrop: true });
  attach.set(initialDraft?.images ?? []);
  const noteFlow = createNoteFlow();
  const [cwd, setCwd] = createSignal(
    sessionSeed && !sessionSeed.roam ? sessionSeed.cwd : "",
  );
  createEffect(() => setHomeTerminalCwd(cwd()));
  const [agentKind, setAgentKind] = createSignal<AgentKind>(
    resolveEnabledAgentKind(sessionSeed?.agentKind ?? lastUsed.agentKind()),
  );
  // 默认沿用上一次使用的模型；模式固定 Build（Plan 仅由 /plan 启动）。
  const [model, setModel] = createSignal(sessionSeed?.model ?? lastUsed.model(agentKind()));
  const [mode, setMode] = createSignal("build");
  const [busy, setBusy] = createSignal(false);
  // 自动模式：按提示词自动选择项目目录与场景模型；场景模型配置只在此模式下用于新会话。
  const [auto, setAuto] = createSignal(localStorage.getItem(AUTO_MODE_KEY) === "1");
  const toggleAuto = (on: boolean) => {
    setAuto(on);
    localStorage.setItem(AUTO_MODE_KEY, on ? "1" : "0");
  };
  const [roam, setRoam] = createSignal<{ peer: Peer; folder: string } | null>(null);
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
  let lastPrewarmAt = 0;
  let lastRoamModelSyncKey = "";
  let scratchLoading = false;
  let submittingPrompt = false;
  let textareaRef: HTMLTextAreaElement | undefined;
  let slashMenuRef: HTMLDivElement | undefined;
  let historyMenuRef: HTMLDivElement | undefined;
  type PrewarmTarget = {
    cwd?: string;
    agentKind?: AgentKind;
    mode?: string;
  };

  const resizeInput = () => {
    if (!textareaRef) return;
    textareaRef.style.height = "auto";
    textareaRef.style.height = Math.min(textareaRef.scrollHeight, 220) + "px";
  };

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
    if (!state.settings || roam()) return;
    const p = target.cwd ?? cwd();
    if (!p) return;
    const nextAgentKind = target.agentKind ?? agentKind();
    if (!isSupportedAgentKind(nextAgentKind)) return;
    const nextMode = target.mode ?? mode();
    const key = `${nextAgentKind}\n${p}\n${nextMode}`;
    if (key === lastPrewarmKey && Date.now() - lastPrewarmAt < 15_000) return;
    lastPrewarmKey = key;
    lastPrewarmAt = Date.now();
    const shouldRestoreFocus = document.activeElement === textareaRef;
    const selectionStart = textareaRef?.selectionStart ?? null;
    const selectionEnd = textareaRef?.selectionEnd ?? null;
    void api.prewarm(p, nextAgentKind, nextMode || null).catch((error) => {
      if (lastPrewarmKey === key) lastPrewarmKey = "";
      console.warn("prewarm failed", error);
    }).finally(() => {
      if (!shouldRestoreFocus || !textareaRef) return;
      textareaRef.focus();
      if (selectionStart !== null && selectionEnd !== null) {
        textareaRef.setSelectionRange(selectionStart, selectionEnd);
      }
    });
  };

  // 恢复项目/草稿和切换模式也要预热，把用户阅读、输入的时间用于启动 ACP。
  createEffect(() => {
    if (!state.settings || roam()) return;
    const target = { cwd: cwd(), agentKind: agentKind(), mode: mode() };
    if (!target.cwd) return;
    const timer = setTimeout(() => prewarmCurrent(target), 150);
    onCleanup(() => clearTimeout(timer));
  });

  const pickModel = (v: string) => {
    setModel(v);
    lastUsed.setModel(agentKind(), v);
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
  };
  // 三级菜单一次性提交「后端 + 模型」：跨后端时切后端，同后端时仅换模型
  const pickModelCombined = (next: AgentKind, m: string) => {
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
  };

  // ===== 漫游：用对端（host）的模型列表，而不是本机的（本机模型对方可能没有）=====
  const roaming = () => !!roam();
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
    usesPeerModels() ? (peerModels()?.backends ?? []).filter(isSupportedAgentKind) : enabledAgentKinds();
  // 模型选项来源：漫游取对端列表（缺失返回 null → 空列表），否则用本机全局
  const peerModelSource = (k: AgentKind) => peerModels()?.options[k] ?? null;
  // 空值才落到可用项；已有选择即使模型列表尚未就绪也保留。
  createEffect(() => {
    if (usesPeerModels()) return;
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
    if (!pm || !isSupportedAgentKind(agentKind())) return;
    const backends = pm.backends.filter(isSupportedAgentKind);
    const backend = backends.find((kind) => kind === agentKind()) ?? backends[0];
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
    if (!usesPeerModels()) void ensureModelOptions(agentKind());
  });

  // 当前选中的后端若被设置里关闭，回退到第一个启用的后端（漫游时后端由对端决定，跳过）
  createEffect(() => {
    if (usesPeerModels()) return;
    const next = resolveEnabledAgentKind(agentKind());
    if (next !== agentKind()) pickModelAgent(next);
  });

  // worktree 是否可用：漫游（对方仓库交给 host 校验）或本地当前目录是 git 仓库
  const worktreeAvailable = () => roaming() || cwdIsRepo();

  // 本地会话：判断当前目录是否 git 仓库（决定 worktree 开关可用性）。
  // 漫游目录在对方机器上，无法本地判断，统一按可用处理、交由 host 校验。
  createEffect(() => {
    const dir = cwd();
    if (usesPeerModels() || !dir) {
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

  const selectProject = (p: string) => {
    setRoam(null); // 选了本地项目就退出漫游
    setCwd(p);
    if (isScratch(p)) localStorage.removeItem(LAST_NEW_THREAD_PROJECT_KEY);
    else localStorage.setItem(LAST_NEW_THREAD_PROJECT_KEY, p);
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
        selectProject(path);
      })();
    },
    onSelectModel: pickModelCombined,
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
        setRoam({ peer, folder: sessionSeed.roam.folder });
        ensurePeerModels(peer.token, true);
        return;
      }
      void restoreLastProject();
      return;
    }
    if (sessionSeed.cwd) selectProject(sessionSeed.cwd);
    else void restoreLastProject();
  });
  // 快捷键/侧栏「新会话」后聚焦首页输入框（含已在首页、组件未重挂载的情况）。
  createEffect(() => {
    if (state.homeComposerFocusAt <= 0) return;
    // 星屑「带入新会话」落在已挂载的首页时，把草稿并入当前输入。
    const draft = takeSessionDraft(null);
    if (draft) untrack(() => {
      const merged = [text(), draft.text].filter(Boolean).join("\n\n");
      setText(merged);
      setCursor(merged.length);
      attach.set([...attach.images(), ...draft.images]);
    });
    queueMicrotask(() => textareaRef?.focus());
  });
  onCleanup(() => {
    if (!submittingPrompt) {
      saveSessionDraft(null, text(), attach.images());
      rememberPromptDraft(text(), attach.images());
    }
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
    if (auto()) return "描述任务，自动选择项目与模型（Enter 发送 · Ctrl+Enter 临时会话）";
    const wt = worktreeAvailable() ? " · Alt+Enter 在 worktree 执行" : "";
    const target = roam();
    if (target) return `描述任务，将在 ${target.peer.name} 的机器上执行（Enter 发送${wt}）`;
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
    const autoMode = auto();
    const target = autoMode ? null : roam();
    const clue = state.pendingClueCard;
    if (
      (!t && images.length === 0) ||
      busy() ||
      (!cwd() && !target && !autoMode) ||
      !peerReady() ||
      (usesPeerModels() && configAgentKinds().length === 0)
    ) return;
    const wtOn = !autoMode && opts.worktree === true && worktreeAvailable();
    const branch = opts.branch?.trim() ?? "";
    const base = wtOn ? opts.base?.trim() ?? "" : "";
    if (wtOn && !branch && !base) return; // 新分支名与基于分支至少填一个（留空分支名 = 直接用所选分支）
    submittingPrompt = true;
    rememberPromptHistory(t, images);
    setBusy(true);
    try {
      if (!autoMode && !isSupportedAgentKind(agentKind())) {
        throw new Error("此后端已移除，请先选择其他后端后再发送");
      }
      if (autoMode) {
        // 直接以轻量化场景模型开局：由它判断是否把整块工作按场景转交，并选择项目目录。
        const { agentKind: autoKind, model: autoModel } = sceneModel("lightweight");
        const clueId = clue?.id ?? "";
        if (clue) clearPendingClueCard();
        createThreadOptimistic(await api.scratchDir(), autoKind, autoModel, mode(), "", prompt, images,
          opts.ephemeral ?? false, clueId, "lightweight");
      } else if (target) {
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
        stashWorktreePrompt(id, prompt, images);
      } else {
        const ephemeral = opts.ephemeral ?? false;
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
      setText("");
      setQuote("");
      setSlashStart(null);
      setRoam(null);
      attach.clear();
      clearPendingClueCard();
      if (textareaRef) textareaRef.style.height = "auto";
      submittingPrompt = false;
    } catch (error) {
      submittingPrompt = false;
      await message(String(error), { kind: "error" });
    } finally {
      setBusy(false);
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

  // 旧版本训练记录继续隔离，不进入首页最近会话。
  const recent = createMemo(() =>
    state.threads.filter((t) => !t.experienceThread && !t.employeeThread).slice(0, 6),
  );

  return (
    <main class="home">
      <div class="home-center">
        <header class="home-intro">
          <div class="home-eyebrow"><IconLogo size={28} class="home-logo" /><span>NOVA WORKSPACE</span></div>
          <h1 class="home-title">让想法，从这里开始。</h1>
          <p class="home-description">选择项目，描述任务，和 Nova 一起把想法变成现实。</p>
        </header>

        <div
          class="home-composer"
          classList={{ "is-dragging": attach.dragging() }}
        >
          <HaibaraBubble />
          <noteFlow.Notes />
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
            <label class="pill composer-auto" title="以轻量化模型开局，按需把编码或剑来工作交给对应场景模型（设置 → 场景模型）">
              <input type="checkbox" checked={auto()} onChange={(e) => toggleAuto(e.currentTarget.checked)} />
              自动模式
            </label>
            <Show when={!auto()}>
            <ProjectPicker
              value={cwd()}
              onChange={selectProject}
              roam={roam()}
              onPickRoaming={(peer, folder) => {
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
                onPickModel={pickModelCombined}
                anchorTo=".home-composer"
                favorites
              />
            </Show>
            </Show>
            <div class="composer-actions">
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
                  (!cwd() && !roam() && !auto()) ||
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
        </div>

        <Show when={recent().length > 0}>
          <div class="recent">
            <div class="recent-label">最近会话</div>
            <For each={recent()}>
              {(t) => (
                <button
                  class="recent-item"
                  onClick={() =>
                    void openThread(
                      // 与侧栏同口径：任务链运行中时直达当前进行到的阶段，而不是回到根会话。
                      latestFireStage(
                        state.threads,
                        t,
                        (id) => !!state.running[id],
                        (id) => state.unreadTurns[id] ?? 0,
                        "running",
                      )?.id ?? t.id,
                    )
                  }
                >
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
    </main>
  );
}
