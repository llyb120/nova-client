import { message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, Show } from "solid-js";
import { api } from "../ipc";
import { respondRoamRequest, state } from "../store";
import type { BranchList } from "../types";
import { agentLabel } from "../utils";
import { ConfigSelects } from "./ConfigSelects";
import { IconFolder } from "./icons";
import { SearchSelect } from "./SearchSelect";

/** host 侧：有人请求漫游本机项目时弹出，确认后才真正建立会话 */
export function RoamRequestModal() {
  const [busy, setBusy] = createSignal(false);
  const current = () => state.incomingRoams[0] ?? null;
  const [prompt, setPrompt] = createSignal("");
  const [folder, setFolder] = createSignal("");
  const [model, setModel] = createSignal("");
  const [mode, setMode] = createSignal("");
  const [worktree, setWorktree] = createSignal(false);
  const [worktreeBranch, setWorktreeBranch] = createSignal("");
  const [worktreeBase, setWorktreeBase] = createSignal("");
  const [branches, setBranches] = createSignal<BranchList | null>(null);

  createEffect(() => {
    const req = current();
    if (!req) return;
    setPrompt(req.prompt ?? "");
    setFolder(req.folder);
    setModel(req.model ?? "");
    setMode(req.mode ?? "");
    setWorktree(req.worktree ?? false);
    setWorktreeBranch(req.worktreeBranch ?? "");
    setWorktreeBase(req.worktreeBase ?? "");
    setBranches(null);
    if (!req.continuation && req.folderExists !== false) {
      void api.listBranches(req.folder).then((list) => {
        if (current()?.reqId !== req.reqId) return;
        setBranches(list);
        if (!req.worktreeBase?.trim()) setWorktreeBase(list.current);
      }).catch(() => {});
    }
  });

  const branchOptions = createMemo(() =>
    (branches()?.branches ?? []).map((branch) => ({
      value: branch,
      label: branch === branches()?.current ? `${branch}（当前）` : branch,
    })),
  );

  const respond = async (accept: boolean) => {
    const req = current();
    if (!req || busy()) return;
    setBusy(true);
    try {
      await respondRoamRequest(req.reqId, accept, {
        prompt: prompt(),
        folder: folder(),
        model: model(),
        mode: mode(),
        worktree: worktree(),
        worktreeBranch: worktreeBranch(),
        worktreeBase: worktreeBase(),
      });
    } catch (e) {
      await message(String(e), { kind: "error" });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Show when={current()}>
      {(req) => (
        <div class="modal-backdrop">
          <div class="modal roam-req-modal">
            <div class="modal-head">
              <span>漫游请求</span>
              <Show when={state.incomingRoams.length > 1}>
                <span class="roam-req-count">还有 {state.incomingRoams.length - 1} 个</span>
              </Show>
            </div>
            <div class="modal-body">
              <p class="roam-req-text">
                <b>{req().fromName}</b> 想在你的机器上漫游执行（
                {agentLabel(req().agentKind)}）。
              </p>
              <Show when={!req().continuation} fallback={
                <div class="roam-req-folder" title={req().folder}>
                  <IconFolder size={15} />
                  <span class="roam-req-folder-name">{req().folderName}</span>
                  <span class="roam-req-folder-path">{req().folder}</span>
                </div>
              }>
                <label class="field">
                  <span class="field-label">执行目录</span>
                  <input class="field-input" value={folder()} onInput={(e) => setFolder(e.currentTarget.value)} />
                </label>
              </Show>
              <label class="field">
                <span class="field-label">提示词</span>
                <textarea
                  class="field-input roam-req-prompt-input"
                  value={prompt()}
                  onInput={(e) => setPrompt(e.currentTarget.value)}
                />
              </label>
              <Show when={req().folderExists === false}>
                <p class="roam-req-warn">
                  该目录在你机器上不存在，允许后将自动创建。
                </p>
              </Show>
              <Show when={!req().continuation}>
                <label class="setting-check">
                  <input type="checkbox" checked={worktree()} onChange={(e) => setWorktree(e.currentTarget.checked)} />
                  <span>
                    在 git worktree 中执行
                    <span class="field-hint">新建独立工作目录和分支，不影响当前工作区。</span>
                  </span>
                </label>
                <Show when={worktree()}>
                  <div class="roam-req-grid">
                    <label class="field">
                      <span class="field-label">worktree 分支</span>
                      <input class="field-input" value={worktreeBranch()} onInput={(e) => setWorktreeBranch(e.currentTarget.value)} />
                    </label>
                    <label class="field">
                      <span class="field-label">基于分支/提交</span>
                      <SearchSelect
                        prefix="源分支"
                        value={worktreeBase()}
                        options={branchOptions()}
                        fallbackLabel={worktreeBase() || "请选择"}
                        searchable
                        wide
                        portal
                        onChange={setWorktreeBase}
                      />
                    </label>
                  </div>
                </Show>
                <div class="roam-req-config">
                  <ConfigSelects
                    agentKind={req().agentKind}
                    model={model()}
                    mode={mode()}
                    portal
                    onPickModel={(_, value) => setModel(value)}
                    onMode={setMode}
                  />
                </div>
              </Show>
              <p class="field-hint">
                {req().continuation
                  ? "上次授权已超过 30 分钟。同意后将续期 30 分钟并执行上方提示词。"
                  : "同意后对方可在该目录驱动会话、读写文件并执行命令，授权有效期为 30 分钟。"}
              </p>
            </div>
            <div class="modal-foot">
              <button class="btn danger" disabled={busy()} onClick={() => void respond(false)}>
                拒绝
              </button>
              <button
                class="btn primary"
                disabled={busy() || (req().continuation && !prompt().trim())}
                onClick={() => void respond(true)}
              >
                {busy() ? "处理中…" : req().continuation ? "续期并执行" : "允许漫游"}
              </button>
            </div>
          </div>
        </div>
      )}
    </Show>
  );
}
