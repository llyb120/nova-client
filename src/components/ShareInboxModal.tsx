import { message, open as openDialog } from "@tauri-apps/plugin-dialog";
import { createMemo, createSignal, For, Show } from "solid-js";
import { api } from "../ipc";
import {
  clueCardById,
  openThread,
  refreshClueGroups,
  refreshInbox,
  refreshThreads,
  startSessionFromClue,
  state,
} from "../store";
import { agentLabel, isScratch } from "../utils";
import { IconBell, IconFolder, IconPlus, IconX } from "./icons";
import { projectDisplayName } from "./ProjectPicker";

function fmtTime(ts: number): string {
  const d = new Date(ts);
  return d.toLocaleString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** 收件箱：别人分享过来的对话，选一个已有项目（或临时目录）接收成本地会话 */
export function ShareInboxModal(props: { onClose: () => void }) {
  const [busy, setBusy] = createSignal<string | null>(null);
  // 正在为哪条分享选项目（内联展开，避免下拉被弹窗裁剪）
  const [picking, setPicking] = createSignal<string | null>(null);
  const [query, setQuery] = createSignal("");

  // 项目列表由后端统一合并（含 worktree 标注、排除临时/漫游/已删目录）
  const filtered = createMemo(() => {
    const q = query().toLowerCase();
    if (!q) return state.projects;
    return state.projects.filter(
      (p) =>
        p.path.toLowerCase().includes(q) ||
        (p.worktree ? p.worktree.branch.toLowerCase().includes(q) : false),
    );
  });

  const accept = async (id: string, cwd: string) => {
    setBusy(id);
    try {
      const clueCardId = state.inbox.find((share) => share.id === id)?.activeClueCardId;
      const newId = await api.acceptShare(id, cwd, isScratch(cwd));
      await refreshThreads();
      await refreshInbox();
      if (clueCardId) {
        await refreshClueGroups();
        const clue = clueCardById(clueCardId);
        if (clue) {
          startSessionFromClue(clue);
          props.onClose();
          return;
        }
      }
      await openThread(newId);
      props.onClose();
    } catch (e) {
      await message(String(e), { kind: "error" });
    } finally {
      setBusy(null);
    }
  };

  const useScratch = async (id: string) => {
    const dir = await api.scratchDir();
    await accept(id, dir);
  };

  const browse = async (id: string) => {
    const dir = await openDialog({ directory: true, title: "选择接收目录" });
    if (typeof dir === "string" && dir) await accept(id, dir);
  };

  const startPick = (id: string) => {
    setQuery("");
    setPicking(picking() === id ? null : id);
  };

  const decline = async (id: string) => {
    await api.declineShare(id);
    await refreshInbox();
  };

  return (
    <div class="modal-backdrop" onClick={(e) => e.target === e.currentTarget && props.onClose()}>
      <div class="modal inbox-modal">
        <div class="modal-head">
          <span>收到的 Flow（{state.inbox.length}）</span>
          <button class="icon-btn" onClick={props.onClose}>
            <IconX size={16} />
          </button>
        </div>
        <div class="modal-body">
          <Show
            when={state.inbox.length > 0}
            fallback={
              <div class="inbox-empty">
                <IconBell size={28} />
                <p>暂时没有新的 Flow</p>
                <p class="field-hint">队友把对话通过 Flow 分享给你时，会出现在这里。</p>
              </div>
            }
          >
            <For each={state.inbox}>
              {(s) => (
                <div class="inbox-item">
                  <div class="inbox-row">
                    <div class="inbox-info">
                      <div class="inbox-title-row">
                        <span class={`thread-agent ${s.agentKind}`}>
                          {agentLabel(s.agentKind)}
                        </span>
                        <Show when={s.recall}>
                          <span class="inbox-recall" title="你召回的漫游会话，对方已自动回传完整快照">
                            召回
                          </span>
                        </Show>
                        <span class="inbox-title" title={s.title}>
                          {s.title}
                        </span>
                      </div>
                      <span class="inbox-meta">
                        来自 {s.fromName} · {fmtTime(s.ts)} · {s.items.length} 条消息
                      </span>
                    </div>
                    <div class="inbox-actions">
                      <Show when={busy() !== s.id} fallback={<span class="field-hint">接收中…</span>}>
                        <button class="btn small primary" onClick={() => startPick(s.id)}>
                          {picking() === s.id ? "收起" : "接收到…"}
                        </button>
                        <button class="btn danger small" onClick={() => void decline(s.id)}>
                          忽略
                        </button>
                      </Show>
                    </div>
                  </div>
                  <Show when={picking() === s.id && busy() !== s.id}>
                    <div class="inbox-pick">
                      <input
                        class="proj-search"
                        placeholder="搜索已有项目"
                        value={query()}
                        onInput={(e) => setQuery(e.currentTarget.value)}
                      />
                      <div class="inbox-pick-list">
                        <div class="proj-item scratch" onClick={() => void useScratch(s.id)}>
                          <IconPlus size={13} />
                          <span class="proj-name">临时目录（不使用项目）</span>
                        </div>
                        <For each={filtered()}>
                          {(p) => (
                            <div
                              class="proj-item"
                              onClick={() => void accept(s.id, p.path)}
                              title={p.path}
                            >
                              <IconFolder size={13} />
                              <span class="proj-name">{projectDisplayName(p)}</span>
                              <Show when={p.worktree}>
                                <span class="proj-wt">⎇ {p.worktree!.branch}</span>
                              </Show>
                              <span class="proj-path">{p.path}</span>
                            </div>
                          )}
                        </For>
                        <Show when={filtered().length === 0}>
                          <div class="proj-empty">没有匹配的项目</div>
                        </Show>
                      </div>
                      <button class="proj-browse" onClick={() => void browse(s.id)}>
                        <IconPlus size={13} />
                        选择其它文件夹…
                      </button>
                    </div>
                  </Show>
                </div>
              )}
            </For>
          </Show>
        </div>
      </div>
    </div>
  );
}
