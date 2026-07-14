import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { api } from "../ipc";
import { rememberPromptDraft, takePromptDraft } from "../promptDraft";
import {
  createRoamingThread,
  createQuotaThread,
  createThread,
  clearQuotaRoamingProgress,
  enabledAgentKinds,
  ensureModelOptions,
  ensurePeerBranches,
  ensurePeerModels,
  lastUsed,
  peerBranchKey,
  modeChoices,
  modelChoices,
  normalizeUnifiedMode,
  openThread,
  preloadPeerModels,
  refreshSlashCommands,
  resolveAvailableModel,
  resolveEnabledAgentKind,
  roamingPeers,
  sendPrompt,
  state,
  stashWorktreePrompt,
} from "../store";
import type { AgentKind, Peer } from "../types";
import { agentLabel } from "../utils";
import { ConfigSelects, type QuotaModelPeer, type SharedModelSource } from "./ConfigSelects";
import { IconFolder, IconLogo, IconSend, IconX } from "./icons";
import { createImageAttachments, ImageAttachmentStrip } from "./ImageAttachmentStrip";
import { ProjectPicker } from "./ProjectPicker";
import { getSlashSuggestions, type SlashSuggestion } from "./slashSuggestions";
import { TypewriterText } from "./TypewriterText";

/** codex 风格草稿首页：输入任务 + 选择项目/模型/模式，回车即开干 */
export function HomeView() {
  const [text, setText] = createSignal("");
  const [cursor, setCursor] = createSignal(0);
  const [slashStart, setSlashStart] = createSignal<number | null>(null);
  const [activeSlashIndex, setActiveSlashIndex] = createSignal(0);
  const attach = createImageAttachments({ enableFileDrop: true });
  const [cwd, setCwd] = createSignal("");
  const [agentKind, setAgentKind] = createSignal<AgentKind>(
    resolveEnabledAgentKind(lastUsed.agentKind()),
  );
  // 默认沿用上一次使用的模型/模式
  const [model, setModel] = createSignal(lastUsed.model(agentKind()));
  const [mode, setMode] = createSignal(lastUsed.mode(agentKind()));
  const [busy, setBusy] = createSignal(false);
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
  let modeTouched = false;
  let lastPrewarmKey = "";
  let scratchLoading = false;
  let submittingPrompt = false;
  let textareaRef: HTMLTextAreaElement | undefined;
  let slashMenuRef: HTMLDivElement | undefined;
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
    lastUsed.setModel(agentKind(), v, cwd());
    prewarmCurrent({ model: v });
  };
  const pickMode = (v: string) => {
    modeTouched = true;
    setMode(v);
    lastUsed.setMode(agentKind(), v);
    prewarmCurrent({ mode: v });
  };
  const pickModelAgent = (next: AgentKind) => {
    if (next === agentKind()) return;
    const nextModel = lastUsed.model(next, cwd());
    const nextMode = lastUsed.mode(next);
    setAgentKind(next);
    lastUsed.setAgentKind(next);
    setModel(nextModel);
    setMode(nextMode);
    modeTouched = false;
    if (!usesPeerModels()) {
      void ensureModelOptions(next);
      void refreshSlashCommands(next);
    }
    prewarmCurrent({ agentKind: next, model: nextModel, mode: nextMode });
  };
  // 三级菜单一次性提交「后端 + 模型」：跨后端时连同模式一起切换，同后端时仅换模型
  const pickModelCombined = (next: AgentKind, m: string, borrowed?: QuotaModelPeer | null) => {
    if (borrowed) {
      const peer = roamingPeers().find((item) => item.token === borrowed.token);
      if (!peer) return;
      const source = state.peerModels[peer.token]?.sharedOptions[next] ?? null;
      const nextModes = modeChoices(next, source);
      const nextMode = nextModes.some((item) => item.id === mode())
        ? mode()
        : (nextModes[0]?.id ?? "");
      setRoam(null);
      setQuotaPeer(peer);
      setAgentKind(next);
      setModel(m);
      setMode(nextMode);
      modeTouched = false;
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
    const nextMode = lastUsed.mode(next);
    setAgentKind(next);
    lastUsed.setAgentKind(next);
    setMode(nextMode);
    modeTouched = false;
    if (!usesPeerModels()) {
      void ensureModelOptions(next);
      void refreshSlashCommands(next);
    }
    setModel(m);
    lastUsed.setModel(next, m, cwd());
    prewarmCurrent({ agentKind: next, model: m, mode: nextMode });
  };

  // ===== 漫游：用对端（host）的模型/模式列表，而不是本机的（本机模型对方可能没有）=====
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
  // 模型/模式选项来源：漫游取对端列表（缺失返回 null → 空列表），否则用本机全局
  const peerModelSource = (k: AgentKind) => peerModels()?.options[k] ?? null;
  const quotaSharedModelSources = createMemo<SharedModelSource[]>(() =>
    roamingPeers()
      .map((peer) => ({
        peer: { token: peer.token, name: peer.name },
        options: state.peerModels[peer.token]?.sharedOptions ?? {},
      }))
      .filter((source) => Object.keys(source.options).length > 0),
  );

  // 没有历史/无效选择时的模式默认：优先设置里的默认（旧值如 bypass 归一到 build），
  // 否则用第一项（Build）。漫游单独处理。
  createEffect(() => {
    if (usesPeerModels() || quotaBorrowing() || modeTouched) return;
    const opts = modeChoices(agentKind());
    if (opts.length === 0) return;
    if (!mode() || !opts.some((m) => m.id === mode())) {
      const def =
        normalizeUnifiedMode(state.settings?.defaultMode) ?? state.settings?.defaultMode;
      if (def && opts.some((m) => m.id === def)) setMode(def);
      else setMode(opts[0].id);
    }
  });

  // 空值才落到可用项；已有选择即使暂不在列表也保留（Cursor 目录未就绪等中间态不应重置成 Auto）。
  createEffect(() => {
    if (usesPeerModels() || quotaBorrowing()) return;
    const choices = modelChoices(agentKind());
    if (choices.length === 0) return;
    const current = model();
    const resolved = resolveAvailableModel(agentKind(), current);
    if (resolved !== current) setModel(resolved);
  });

  // 漫游：拉取对端模型；到达后把后端/模型/模式收敛到对方可用项（跳过 value="" 的 Auto）
  createEffect(() => {
    const t = roamPeerToken();
    if (!t) return;
    ensurePeerModels(t);
    const pm = state.peerModels[t];
    if (!pm) return;
    const backend = pm.backends.includes(agentKind()) ? agentKind() : pm.backends[0];
    if (!backend) return;
    if (backend !== agentKind()) setAgentKind(backend);
    const models = modelChoices(backend, pm.options[backend] ?? null);
    if (!models.some((m) => m.value === model())) {
      setModel(models.find((m) => m.value)?.value ?? models[0]?.value ?? "");
    }
    const modes = modeChoices(backend, pm.options[backend] ?? null);
    if (!modes.some((m) => m.id === mode())) setMode(modes[0]?.id ?? "");
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

  const selectProject = (p: string, warm = true) => {
    setRoam(null); // 选了本地项目就退出漫游
    setCwd(p);
    const nextModel = resolveAvailableModel(agentKind(), lastUsed.model(agentKind(), p));
    setModel(nextModel);
    if (warm) prewarmCurrent({ cwd: p, model: nextModel });
  };

  const ensureScratchProject = () => {
    if (cwd() || scratchLoading) return;
    scratchLoading = true;
    void api.scratchDir().then((dir) => {
      if (!cwd()) selectProject(dir);
    }).finally(() => {
      scratchLoading = false;
    });
  };

  onMount(ensureScratchProject);
  // 每次进入新会话页都强制校准在线队友的共享模型，避免沿用旧 peerModels 缓存。
  onMount(() => preloadPeerModels(true));
  onCleanup(() => {
    if (!submittingPrompt) rememberPromptDraft(text(), attach.images());
  });

  createEffect(() => {
    if (state.currentId === null) ensureScratchProject();
  });

  const onInput = (e: InputEvent) => {
    const el = e.currentTarget as HTMLTextAreaElement;
    const typedSlash = e.inputType === "insertText" && e.data === "/";
    const trackingSlash = slashStart() !== null;
    setText(el.value);
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
    const images = attach.images();
    const target = roam();
    const quota = quotaPeer();
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
          t,
          wtOn,
          branch,
          base,
        );
        await sendPrompt(t, images);
      } else if (quota) {
        await createQuotaThread(quota, cwd(), agentKind(), model(), mode());
        await sendPrompt(t, images);
      } else if (wtOn) {
        // 本地 worktree：后台创建、界面立即进入会话，就绪后再自动补发首条提示词
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
        );
        stashWorktreePrompt(id, t, images);
      } else {
        await createThread(cwd(), agentKind(), model(), mode(), "", opts.ephemeral ?? false, false, "", "");
        await sendPrompt(t, images);
      }
      setText("");
      setSlashStart(null);
      setRoam(null);
      setQuotaPeer(null);
      attach.clear();
      if (textareaRef) textareaRef.style.height = "auto";
      submittingPrompt = false;
    } catch (error) {
      submittingPrompt = false;
      const text = String(error);
      if (!text.includes("额度漫游已取消") && !text.includes("CLI 操作已取消")) {
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

  const onKeyDown = (e: KeyboardEvent) => {
    const suggestions = slashSuggestions();
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

  const recent = () => state.threads.filter((t) => !t.employeeId).slice(0, 6);

  return (
    <main class="home">
      <div class="home-center">
        <IconLogo size={44} class="home-logo" />
        <h1 class="home-title">我们该做什么？</h1>

        <div
          class="home-composer"
          classList={{ "is-dragging": attach.dragging() }}
        >
          <ImageAttachmentStrip images={attach.images()} onRemove={attach.remove} />
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
            onPaste={attach.onPaste}
          />
          <div class="composer-bar">
            <ProjectPicker
              value={cwd()}
              onChange={selectProject}
              roam={roam()}
              onPickRoaming={(peer, folder) => {
                setQuotaPeer(null);
                setRoam({ peer, folder });
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
                mode={mode()}
                modelSource={usesPeerModels() ? peerModelSource : undefined}
                sharedModels={usesPeerModels() ? undefined : quotaSharedModelSources()}
                quotaPeerToken={quotaPeer()?.token}
                projectCwd={cwd()}
                onPickModel={pickModelCombined}
                onMode={pickMode}
                anchorTo=".home-composer"
              />
            </Show>
            <span class="bar-spacer" />
            <button
              class="composer-btn send"
              disabled={
                (!text().trim() && attach.images().length === 0) ||
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
          state.quotaRoamingProgress && state.quotaRoamingProgress.stage !== "installing"
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
