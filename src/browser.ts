import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { createSignal } from "solid-js";
import type { AgentKind } from "./types";

export interface BrowserInfo {
  url: string;
  recording: boolean;
  paused: boolean;
  running: boolean;
  eventCount: number;
}

/** 双子座步骤只保留三种：跳转、记录、操作（操作内容即提示词） */
export type BrowserAction = "navigate" | "record" | "operate";

export interface RecordEvent {
  id: number;
  ts: number;
  url: string;
  kind: BrowserAction;
  target?: {
    selector?: string;
    tag?: string;
    text?: string;
    href?: string;
    rect?: { x: number; y: number; width: number; height: number };
    imagePaths?: string[];
  } | null;
  data?: {
    value?: string;
    checked?: boolean;
    key?: string;
    navigationSource?: "operation" | "address_bar" | "record_start";
    inputType?: "paste";
    trigger?: { kind?: string; selector?: string } | null;
    recordContent?: string;
    outputName?: string;
    imagePaths?: string[];
    storageKey?: string;
    storageValue?: string;
    navigateStorageEnabled?: boolean;
  } | null;
}

export interface RecordedClip {
  id: string;
  name: string;
  createdAt: number;
  startUrl: string;
  events: RecordEvent[];
  marks: ClipMark[];
  analysisPrompt?: string;
  analysisRecordRefs?: string[];
  runAgentKind?: AgentKind;
  runModel?: string;
  headless?: boolean;
}

export interface ClipMark {
  id: string;
  note: string;
  /** agent 分析后回填的稳定 selector */
  selector?: string;
  confidence?: number;
  /** 截图本地路径（保存后） */
  imagePath?: string;
}

export interface PlayStep {
  action: "goto" | "record" | "operate";
  selector?: string;
  targetImagePaths?: string[];
  url?: string;
  sessionStorage?: { key: string; value: string };
  recordContent?: string;
  outputName?: string;
  /** operate 步骤的提示词 */
  prompt?: string;
}

export interface PlayPlan {
  version?: number;
  steps: PlayStep[];
  analysisPrompt?: string;
  analysisRecordRefs?: string[];
  headless?: boolean;
}

export interface PlanRunResult {
  planId: string;
  ok: boolean;
  steps: unknown[];
  final: { ok: boolean; error?: string; url?: string; summary?: string };
}

// ---------- 状态 ----------
export const [browserInfo, setBrowserInfo] = createSignal<BrowserInfo | null>(null);
export const [liveEvents, setLiveEvents] = createSignal<RecordEvent[]>([]);

export async function openBrowser() {
  await invoke("browser_open", { url: "" });
}

export async function navigate(url: string) {
  await invoke("browser_navigate", { url });
}

export async function closeBrowser() {
  await invoke("browser_close");
}

export async function refreshInfo() {
  setBrowserInfo(await invoke<BrowserInfo>("browser_info"));
}

export async function startRecording() {
  setLiveEvents([]);
  await invoke("browser_record_start");
}

export async function stopRecording(): Promise<RecordEvent[]> {
  const events = await invoke<RecordEvent[]>("browser_record_stop");
  setLiveEvents(events);
  return events;
}

export const pauseRecording = () => invoke("browser_record_pause");
export const resumeRecording = () => invoke("browser_record_resume");

export async function refreshEvents() {
  setLiveEvents(await invoke<RecordEvent[]>("browser_events"));
}

/** 在页面内框选一个区域并截图 */
export async function captureRegion(): Promise<{ image: string }> {
  return invoke("browser_capture_region");
}

export async function saveShot(dataUrl: string, name?: string): Promise<string> {
  return invoke<string>("browser_save_shot", { dataUrl, name });
}

/** 把片段事件编排为运行 Plan：仅保留 跳转(goto)/记录(record)/操作(operate) 三类步骤 */
export function compilePlan(events: RecordEvent[]): PlayPlan {
  const steps: PlayStep[] = [];
  for (const ev of events) {
    if (ev.kind === "navigate") {
      const url = (ev.url || "").trim();
      if (!url) continue;
      const storageEnabled = ev.data?.navigateStorageEnabled ?? false;
      const storageKey = ev.data?.storageKey || "";
      if (storageEnabled && storageKey) {
        steps.push({
          action: "goto",
          url,
          sessionStorage: { key: storageKey, value: ev.data?.storageValue || "" },
        });
      } else {
        steps.push({ action: "goto", url });
      }
    } else if (ev.kind === "record") {
      steps.push({
        action: "record",
        selector: ev.target?.selector || "",
        outputName: ev.data?.outputName || "",
        recordContent: ev.data?.recordContent || "",
        targetImagePaths: ev.data?.imagePaths || [],
      });
    } else {
      // 旧录制事件（点击/输入/按键等）统一转成“操作”提示词步骤
      steps.push({ action: "operate", prompt: ev.data?.value || "", targetImagePaths: ev.data?.imagePaths || [] });
    }
  }
  return { version: 1, steps };
}

export async function analyzeScreenshot(cwd: string, imageDataUrl: string, hint: string): Promise<string> {
  return invoke<string>("analyze_screenshot", { cwd, imageDataUrl, hint });
}

/** 运行计划：交给后台临时会话，由 agent/skill 自己调用 Playwright 执行并返回 JSON 结论 */
export async function runPlanWithAgent(cwd: string, plan: PlayPlan, agentKind: AgentKind, model?: string): Promise<string> {
  return invoke<string>("run_plan_with_agent", { cwd, plan, agentKind, model });
}

// ---------- 事件订阅 ----------
export function subscribeBrowser(handlers: {
  onInfo?: (info: BrowserInfo) => void;
  onNavigate?: (url: string) => void;
  onEvent?: () => void;
  onClosed?: () => void;
  onError?: (error: string) => void;
}): UnlistenFn[] {
  const unlistens: UnlistenFn[] = [];
  void listen<BrowserInfo>("browser://info", (e) => {
    setBrowserInfo(e.payload);
    handlers.onInfo?.(e.payload);
  }).then((u) => unlistens.push(u));
  void listen<{ url: string }>("browser://navigate", (e) => handlers.onNavigate?.(e.payload.url)).then((u) =>
    unlistens.push(u),
  );
  void listen("browser://event", () => {
    void refreshEvents();
    handlers.onEvent?.();
  }).then((u) => unlistens.push(u));
  void listen("browser://closed", () => handlers.onClosed?.()).then((u) => unlistens.push(u));
  void listen<{ error: string }>("browser://error", (e) => handlers.onError?.(e.payload.error)).then((u) =>
    unlistens.push(u),
  );
  return unlistens;
}
