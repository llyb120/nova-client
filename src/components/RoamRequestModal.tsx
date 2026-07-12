import { message } from "@tauri-apps/plugin-dialog";
import { createSignal, Show } from "solid-js";
import { respondRoamRequest, state } from "../store";
import { agentLabel } from "../utils";
import { IconFolder } from "./icons";

/** host 侧：有人请求漫游本机项目时弹出，确认后才真正建立会话 */
export function RoamRequestModal() {
  const [busy, setBusy] = createSignal(false);
  const current = () => state.incomingRoams[0] ?? null;

  const respond = async (accept: boolean) => {
    const req = current();
    if (!req || busy()) return;
    setBusy(true);
    try {
      await respondRoamRequest(req.reqId, accept);
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
              <div class="roam-req-folder" title={req().folder}>
                <IconFolder size={15} />
                <span class="roam-req-folder-name">{req().folderName}</span>
                <span class="roam-req-folder-path">{req().folder}</span>
              </div>
              <Show when={req().prompt}>
                <div class="roam-req-prompt">
                  <span class="roam-req-prompt-label">对方想执行</span>
                  <p class="roam-req-prompt-text">{req().prompt}</p>
                </div>
              </Show>
              <Show when={req().folderExists === false}>
                <p class="roam-req-warn">
                  该目录在你机器上不存在，允许后将自动创建。
                </p>
              </Show>
              <Show when={req().worktree}>
                <p class="roam-req-worktree">
                  ⎇ 对方要求在 <b>git worktree</b> 中执行：将在此仓库新建分支{" "}
                  <b>{req().worktreeBranch || "（未命名）"}</b> 的独立工作目录，不影响你当前工作区。
                  该目录须为 git 仓库，否则会失败。
                </p>
              </Show>
              <p class="field-hint">
                同意后对方将在该目录直接驱动会话、读写文件并执行命令，仅在你信任对方时允许。
              </p>
            </div>
            <div class="modal-foot">
              <button class="btn danger" disabled={busy()} onClick={() => void respond(false)}>
                拒绝
              </button>
              <button class="btn primary" disabled={busy()} onClick={() => void respond(true)}>
                {busy() ? "处理中…" : "允许漫游"}
              </button>
            </div>
          </div>
        </div>
      )}
    </Show>
  );
}
