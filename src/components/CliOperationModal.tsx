import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { api } from "../ipc";
import { setState, state } from "../store";
import { agentLabel } from "../utils";

export function CliOperationModal() {
  const [cancelling, setCancelling] = createSignal(false);
  const progress = () => state.cliOperationProgress;
  const active = () => {
    const stage = progress()?.stage;
    return stage === "waiting" || stage === "running" || stage === "verifying";
  };

  createEffect(() => {
    const current = progress();
    if (
      !current ||
      (current.stage !== "completed" && current.stage !== "cancelled")
    )
      return;
    const timer = window.setTimeout(
      () => setState("cliOperationProgress", null),
      1000,
    );
    onCleanup(() => window.clearTimeout(timer));
  });

  const cancel = async () => {
    const current = progress();
    if (!current || !active() || cancelling()) return;
    setCancelling(true);
    try {
      await api.cancelCliOperation(current.operationId);
    } finally {
      setCancelling(false);
    }
  };

  const percent = () => Math.max(0, Math.min(100, progress()?.percent ?? 0));

  return (
    <Show when={progress()}>
      {(current) => (
        <div class="modal-backdrop cli-operation-backdrop">
          <div class="modal cli-operation-modal">
            <div class="modal-head">
              <span>
                正在{current().action} {agentLabel(current().agentKind)} CLI
              </span>
            </div>
            <div class="modal-body">
              <div class="cli-operation-progress">
                <div class="cli-operation-bar">
                  <div
                    class="cli-operation-bar-fill"
                    style={{ width: `${percent()}%` }}
                  />
                </div>
                <span>{percent()}%</span>
              </div>
              <div class="cli-operation-message">{current().message}</div>
              <div class="field-hint">
                进度按执行阶段估算；取消会终止当前进程及其子进程。
              </div>
            </div>
            <div class="modal-foot">
              <Show
                when={active()}
                fallback={
                  <button
                    class="btn primary"
                    onClick={() => setState("cliOperationProgress", null)}
                  >
                    关闭
                  </button>
                }
              >
                <button
                  class="btn secondary"
                  disabled={cancelling()}
                  onClick={() => void cancel()}
                >
                  {cancelling() ? "正在取消…" : `取消${current().action}`}
                </button>
              </Show>
            </div>
          </div>
        </div>
      )}
    </Show>
  );
}
