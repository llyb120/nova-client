import { createMemo, createSignal, Show } from "solid-js";
import { applyStagedUpdate, state } from "../store";

function fmtMB(n?: number): string {
  if (!n) return "";
  return (n / 1048576).toFixed(1) + " MB";
}

const PHASE_LABEL: Record<string, string> = {
  downloading: "下载中",
  extracting: "解压中",
  staged: "已就绪",
  applying: "替换文件",
  restarting: "即将重启",
};

/** 更新弹窗（由左上角角标点击打开）：已静默下载好 → 立即重启更新 */
export function UpdateModal(props: { show: boolean; onClose: () => void }) {
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");

  const info = () => state.update;
  const progress = () => state.updateProgress;

  const percent = createMemo(() => {
    const p = progress();
    if (!p || !p.total) return 0;
    return Math.min(100, Math.round((p.downloaded / p.total) * 100));
  });

  const start = async () => {
    setBusy(true);
    setError("");
    try {
      await applyStagedUpdate();
      // 成功路径不会走到这里（后端直接重启进程）
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  const dismiss = () => {
    if (busy()) return;
    props.onClose();
  };

  return (
    <Show when={props.show}>
      <div class="modal-backdrop" onClick={(e) => e.target === e.currentTarget && dismiss()}>
        <div class="modal update-modal">
          <div class="modal-head">
            <span>{state.updateStaging ? "正在下载新版本" : "新版本已就绪"}</span>
          </div>
          <div class="modal-body">
            <Show when={info()}>
              <p class="update-line">
                v{info()!.current} → <b>v{info()!.latest}</b>
                <Show when={info()!.size}>
                  <span class="update-size">（{fmtMB(info()!.size)}）</span>
                </Show>
              </p>
            </Show>
            <Show when={state.updateStaging}>
              <div class="update-progress">
                <div class="update-bar">
                  <div class="update-bar-fill" style={{ width: `${percent()}%` }} />
                </div>
                <div class="update-progress-text">
                  {PHASE_LABEL[progress()?.phase ?? "downloading"]} {percent()}%
                  <Show when={progress()?.phase === "downloading"}>
                    <span class="update-size">
                      {fmtMB(progress()?.downloaded)} / {fmtMB(progress()?.total)}
                    </span>
                  </Show>
                </div>
              </div>
            </Show>
            <Show when={!state.updateStaging && info()?.staged && !busy() && !error()}>
              <p class="field-hint">已在后台下载完成。点「立即更新」将替换并重启应用，进行中的任务会被打断。</p>
            </Show>
            <Show when={busy()}>
              <div class="update-progress">
                <div class="update-progress-text">{PHASE_LABEL[progress()?.phase ?? "applying"]}…</div>
              </div>
            </Show>
            <Show when={error()}>
              <p class="update-error">{error()}</p>
            </Show>
          </div>
          <div class="modal-foot">
            <button class="btn secondary" disabled={busy()} onClick={dismiss}>
              稍后
            </button>
            <button
              class="btn primary"
              disabled={busy() || state.updateStaging || !info()?.staged}
              onClick={() => void start()}
            >
              {busy() ? "更新中…" : error() ? "重试" : "立即更新"}
            </button>
          </div>
        </div>
      </div>
    </Show>
  );
}
