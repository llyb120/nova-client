import { listen } from "@tauri-apps/api/event";
import { createSignal } from "solid-js";
import { api } from "../ipc";
import type { PromptImage } from "../types";
import { enabledWorkflows } from "./storage";
import { startWorkflow } from "./runtime";
import { parseRoamingWorkflowPrompt } from "./roamingProtocol";

export interface PeerWorkflow {
  id: string;
  name: string;
  stageCount: number;
}
export const [peerWorkflows, setPeerWorkflows] = createSignal<Record<string, PeerWorkflow[]>>({});

export async function initRoamingWorkflows(): Promise<void> {
  await listen<{ peer: string }>("relay:workflows-request", ({ payload }) => {
    void api.replyRoamingWorkflows(payload.peer, enabledWorkflows().map((workflow) => ({
      id: workflow.id, name: workflow.name, stageCount: workflow.stages.length,
    }))).catch(console.error);
  });
  await listen<{ peer: string; workflows: unknown }>("relay:peer-workflows", ({ payload }) => {
    if (!payload.peer || !Array.isArray(payload.workflows)) return;
    const workflows = payload.workflows.filter((value): value is PeerWorkflow =>
      value && typeof value.id === "string" && typeof value.name === "string" &&
      Number.isSafeInteger(value.stageCount) && value.stageCount > 0);
    setPeerWorkflows((previous) => ({ ...previous, [payload.peer]: workflows }));
  });
  await listen<{ threadId: string; text: string; images: PromptImage[] }>("relay:workflow-start", ({ payload }) => {
    void (async () => {
      try {
        await api.checkRoamingWorkflow(payload.threadId);
        const request = parseRoamingWorkflowPrompt(payload.text);
        await startWorkflow(request.id, { goal: request.goal }, payload.threadId, payload.images);
      } catch (error) {
        await api.pushSystemItem(payload.threadId, `漫游工作流启动失败：${String(error)}`, "error");
        await api.failRoamingWorkflow(payload.threadId, String(error));
      }
    })().catch(console.error);
  });
}
