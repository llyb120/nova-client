import { invoke } from "@tauri-apps/api/core";

export const voiceRequest = <T = string>(action: string, fields: Record<string, unknown> = {}) =>
  invoke<T>("voice_request", { request: { action, ...fields } });
