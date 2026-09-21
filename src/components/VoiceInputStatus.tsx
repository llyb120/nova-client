import type { createVoiceInput } from "../voiceInput";
import "./VoiceInputStatus.css";

export function VoiceInputStatus(props: { voice: ReturnType<typeof createVoiceInput> }) {
  const voice = props.voice;
  return <div class="voice-input-status" classList={{ recording: voice.phase() === "recording", error: voice.status().startsWith("语音输入失败") }}>
    <button type="button" class="composer-btn"
      aria-busy={voice.busy()}
      disabled={voice.phase() === "finishing"}
      aria-label="按住说话，也可按住空格键，松开结束"
      title={`${voice.status() || "按住说话，松开结束；也可长按输入框"}（腾讯云语音识别）`}
      onPointerDown={event => {
        if (event.button !== 0) return;
        event.preventDefault(); event.currentTarget.setPointerCapture(event.pointerId);
        void voice.start();
      }}
      onKeyDown={event => {
        if (event.code === "Space") { event.preventDefault(); if (!event.repeat) void voice.start(); }
      }}
      onKeyUp={event => { if (event.code === "Space") { event.preventDefault(); void voice.finish(); } }}
      onLostPointerCapture={() => void voice.finish()}
      onBlur={() => void voice.finish()}
    >
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" aria-hidden="true">
        <rect x="9" y="2" width="6" height="12" rx="3" />
        <path d="M5 10v2a7 7 0 0 0 14 0v-2M12 19v3m-4 0h8" />
      </svg>
    </button>
    <span role="status" aria-live="polite" class="voice-status-hidden">
      {voice.busy() ? (voice.phase() === "finishing" ? "识别中…" : voice.phase() === "starting" ? "准备中…" : "录音中…") : voice.status()}
    </span>
  </div>;
}
