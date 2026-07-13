import { message } from "@tauri-apps/plugin-dialog";
import { createSignal, Show } from "solid-js";
import { respondQuotaRequest, state } from "../store";
import { agentLabel } from "../utils";
import { IconBroadcast } from "./icons";

/** 额度提供方：确认后只发送对应后端的加密登录态，不开放本机目录。 */
export function QuotaRequestModal() {
  const [busy, setBusy] = createSignal(false);
  const current = () =>
    state.incomingRoams.length === 0 ? state.incomingQuotas[0] ?? null : null;

  const respond = async (accept: boolean) => {
    const request = current();
    if (!request || busy()) return;
    setBusy(true);
    try {
      await respondQuotaRequest(request.reqId, accept);
    } catch (error) {
      await message(String(error), { kind: "error" });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Show when={current()}>
      {(request) => (
        <div class="modal-backdrop">
          <div class="modal roam-req-modal quota-req-modal">
            <div class="modal-head">
              <span>额度租借请求</span>
              <Show when={state.incomingQuotas.length > 1}>
                <span class="roam-req-count">还有 {state.incomingQuotas.length - 1} 个</span>
              </Show>
            </div>
            <div class="modal-body">
              <p class="roam-req-text">
                <b>{request().fromName}</b> 想临时使用你的 {agentLabel(request().agentKind)} 额度。
              </p>
              <div class="roam-req-folder">
                <IconBroadcast size={15} />
                <span class="roam-req-folder-name">对方本地执行</span>
                <span class="roam-req-folder-path">{request().projectName || "未命名项目"}</span>
              </div>
              <Show when={request().prompt}>
                <div class="roam-req-prompt">
                  <span class="roam-req-prompt-label">对方想执行</span>
                  <p class="roam-req-prompt-text">{request().prompt}</p>
                </div>
              </Show>
              <p class="field-hint">
                同意后，Nova 会读取该后端登录态并使用一次性 X25519 + ChaCha20-Poly1305
                端到端加密发送。中转站只看到密文；对方的隔离 CLI 进程在会话期间能够使用解密后的凭证。
              </p>
            </div>
            <div class="modal-foot">
              <button class="btn danger" disabled={busy()} onClick={() => void respond(false)}>
                拒绝
              </button>
              <button class="btn primary" disabled={busy()} onClick={() => void respond(true)}>
                {busy() ? "加密处理中…" : "允许本次租借"}
              </button>
            </div>
          </div>
        </div>
      )}
    </Show>
  );
}
