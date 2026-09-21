import { createEffect, createSignal, on, onCleanup, onMount } from "solid-js";
import captureUrl from "./voice-capture.worklet.js?url";
import { voiceRequest as request } from "./voiceService";

export const VOICE_HOLD_MS = 350;
let backendCleanup: Promise<unknown> = Promise.resolve();

export function createVoiceInput(options: {
  element: () => HTMLTextAreaElement | undefined;
  text: () => string;
  setText: (value: string) => void;
  context: () => unknown;
  enabled: () => boolean;
}) {
  const [phase, setPhase] = createSignal<"idle" | "starting" | "recording" | "finishing">("idle");
  const [status, setStatus] = createSignal("");
  const [level, setLevel] = createSignal(-100);
  let disposed = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let press: { x: number; y: number; id: number } | undefined;
  let active: {
    id: string; cancelled: boolean; released: boolean; started: boolean;
    stream?: MediaStream; audio?: AudioContext; node?: AudioWorkletNode;
    queue: Promise<void>; queued: number; last: string; original: string;
    prefix: string; suffix: string; flush?: () => void;
    startRequest?: Promise<string>;
    limitTimer?: ReturnType<typeof setTimeout>;
  } | undefined;

  const busy = () => phase() !== "idle";
  const clearPress = () => { clearTimeout(timer); timer = undefined; press = undefined; };
  const stopTracks = () => active?.stream?.getTracks().forEach(track => track.stop());
  const cancel = (restore = false) => {
    clearPress();
    const current = active;
    if (!current) return;
    current.cancelled = true;
    clearTimeout(current.limitTimer);
    current.released = true;
    stopTracks();
    current.flush?.();
    void current.audio?.close().catch(() => {});
    if (restore && options.text() === current.last) options.setText(current.original);
    if (current.startRequest) {
      backendCleanup = current.startRequest.then(() => request("cancel", { id: current.id })).catch(() => {});
    }
    active = undefined;
    if (!disposed) { setPhase("idle"); setStatus(restore ? "已取消语音输入" : ""); }
  };

  const apply = (current: NonNullable<typeof active>, value: string) => {
    if (disposed || active !== current || current.cancelled) return;
    // External edits (history, draft restore, etc.) must never be overwritten by a late result.
    if (options.text() !== current.last) { cancel(); return; }
    const next = value ? current.prefix + value + current.suffix : current.original;
    current.last = next;
    options.setText(next);
    const caret = current.prefix.length + value.length;
    options.element()?.setSelectionRange(caret, caret);
  };

  const fail = (current: NonNullable<typeof active>, error: unknown) => {
    if (active !== current || disposed) return;
    cancel();
    const message = error instanceof Error ? error.message : String(error);
    setStatus(`语音输入失败：${message}`);
  };

  const finish = async () => {
    clearPress();
    const current = active;
    if (!current || current.released) return;
    current.released = true;
    stopTracks(); // Release the hardware immediately, before waiting for inference.
    clearTimeout(current.limitTimer);
    if (!current.node) { cancel(); return; } // Permission / worklet setup has not completed.
    setPhase("finishing");
    setStatus("正在等待腾讯云最终识别结果…");
    try {
      await new Promise<void>((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error("停止音频采集超时")), 2000);
        current.flush = () => { clearTimeout(timeout); resolve(); };
        current.node!.port.postMessage("stop");
      });
      await current.audio?.close();
      await current.queue;
      if (active !== current || current.cancelled) return;
      apply(current, await request("finish", { id: current.id }));
      if (active !== current) return;
      active = undefined;
      setPhase("idle");
      setStatus(current.last === current.original
        ? "未识别到语音，请靠近麦克风重试"
        : "语音已填入，检查后按 Enter 发送");
    } catch (error) { fail(current, error); }
  };

  const start = async () => {
    if (busy() || disposed || !options.enabled()) return;
    const element = options.element();
    if (!element) return;
    const original = options.text();
    const current: NonNullable<typeof active> = {
      id: crypto.randomUUID(), cancelled: false, released: false, started: false,
      queue: Promise.resolve(), queued: 0, last: original, original,
      prefix: original.slice(0, element.selectionStart), suffix: original.slice(element.selectionEnd),
    };
    active = current;
    setPhase("starting");
    setStatus("正在启动麦克风，请继续按住…");
    const abandoned = () => disposed || active !== current || current.cancelled || current.released;
    const discard = () => {
      current.stream?.getTracks().forEach(track => track.stop());
      void current.audio?.close().catch(() => {});
      if (current.started && !current.cancelled) void request("cancel", { id: current.id }).catch(() => {});
      if (active === current) { active = undefined; setPhase("idle"); setStatus("已停止，长按输入框重新说话"); }
    };
    try {
      if (!navigator.mediaDevices?.getUserMedia) throw new Error("当前环境不支持麦克风，请在 Nova 桌面客户端使用");
      current.audio = new AudioContext({ sampleRate: 16000 });
      const captureReady = Promise.all([current.audio.resume(), current.audio.audioWorklet.addModule(captureUrl)]);
      void captureReady.catch(() => {});
      current.stream = await navigator.mediaDevices.getUserMedia({ audio: {
        channelCount: 1, echoCancellation: false, noiseSuppression: false, autoGainControl: false,
      }, video: false });
      if (abandoned()) { discard(); return; }
      current.startRequest = (async () => {
        await backendCleanup;
        if (current.cancelled || active !== current) return "";
        return request<string>("start", { id: current.id, sampleRate: current.audio!.sampleRate });
      })();
      // Capture immediately while the cloud connection opens; enqueue audio behind start so initial words aren't lost.
      current.queue = current.startRequest.then(() => { current.started = true; }).catch(error => fail(current, error));
      await captureReady;
      if (abandoned()) { discard(); return; }
      current.node = new AudioWorkletNode(current.audio, "nova-voice-capture");
      current.node.port.onmessage = ({ data }: MessageEvent<Float32Array | string | { levelDb: number }>) => {
        if (data === "stopped") { current.flush?.(); return; }
        if (typeof data === "object" && "levelDb" in data) {
          if (active === current && !current.cancelled) setLevel(data.levelDb);
          return;
        }
        if (!(data instanceof Float32Array) || current.cancelled || active !== current) return;
        current.queued += data.length;
        // ponytail: buffer at most 15 seconds including connection setup; use binary IPC if transport becomes the bottleneck.
        if (current.queued > current.audio!.sampleRate * 15) { fail(current, "网络连接或识别过慢，请稍后分短句重试"); return; }
        current.queue = current.queue.then(async () => {
          if (current.cancelled || active !== current) return;
          const value = await request("audio", { id: current.id, samples: Array.from(data) });
          current.queued -= data.length;
          apply(current, value);
        }).catch(error => fail(current, error));
      };
      current.audio.createMediaStreamSource(current.stream).connect(current.node);
      current.node.connect(current.audio.destination);
      current.stream.getAudioTracks()[0]?.addEventListener("ended", () => { void finish(); });
      current.limitTimer = setTimeout(() => void finish(), 120000);
      setStatus("已开始录音，可以说话；连接就绪后会自动提交…");
      await current.startRequest;
      if (active !== current || current.cancelled) { discard(); return; }
      if (current.released) return; // finish() is already draining captured audio.
      setPhase("recording");
      setStatus("腾讯云实时识别 · 松开结束，Esc 取消");
    } catch (error) { fail(current, error); }
  };

  const onPointerDown = (event: PointerEvent) => {
    if (event.button !== 0 || event.pointerType !== "mouse" || active || !options.enabled()) return;
    clearPress();
    press = { x: event.clientX, y: event.clientY, id: event.pointerId };
    timer = setTimeout(() => {
      timer = undefined;
      const element = options.element();
      if (element && press) element.setPointerCapture(press.id);
      void start();
    }, VOICE_HOLD_MS);
  };
  const move = (event: PointerEvent) => {
    if (timer && press && Math.hypot(event.clientX - press.x, event.clientY - press.y) > 6) clearPress();
  };
  const release = () => { void finish(); };
  const hidden = () => { if (document.hidden) release(); };
  const escape = (event: KeyboardEvent) => {
    if (event.key === "Escape" && active) { event.preventDefault(); event.stopPropagation(); cancel(true); }
  };
  onMount(() => {
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", release);
    window.addEventListener("pointercancel", release);
    window.addEventListener("blur", release);
    window.addEventListener("keydown", escape, true);
    document.addEventListener("visibilitychange", hidden);
  });
  createEffect(on(options.context, () => cancel(), { defer: true }));
  createEffect(on(options.enabled, enabled => { if (!enabled) cancel(); }));
  onCleanup(() => {
    disposed = true; cancel();
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", release);
    window.removeEventListener("pointercancel", release);
    window.removeEventListener("blur", release);
    window.removeEventListener("keydown", escape, true);
    document.removeEventListener("visibilitychange", hidden);
  });
  return { phase, status, level, busy, start, finish, cancel, onPointerDown };
}
