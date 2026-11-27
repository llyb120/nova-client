import type { PromptImage } from "./types";

export type PromptDraft = {
  text: string;
  images: PromptImage[];
};

type SessionDraft = PromptDraft & { autoSend?: boolean };
let lastPromptDraft: PromptDraft | null = null;
const sessionDrafts = new Map<string | null, SessionDraft>();

export const terminalDraftKey = (threadId: string) => `term:${threadId}`;
export const hasTerminalDraft = (threadId: string) => sessionDrafts.has(terminalDraftKey(threadId));

export function saveSessionDraft(threadId: string | null, text: string, images: PromptImage[], autoSend = false) {
  if (!text && images.length === 0) {
    sessionDrafts.delete(threadId);
    return;
  }
  sessionDrafts.set(threadId, { text, images: images.map((image) => ({ ...image })), ...(autoSend ? { autoSend } : {}) });
}

export function takeSessionDraft(threadId: string | null): SessionDraft | null {
  const draft = sessionDrafts.get(threadId) ?? null;
  sessionDrafts.delete(threadId);
  return draft;
}

export function rememberPromptDraft(text: string, images: PromptImage[]) {
  if (!text.trim() && images.length === 0) return;
  lastPromptDraft = {
    text,
    images: images.map((image) => ({ ...image })),
  };
}

export function takePromptDraft(): PromptDraft | null {
  const draft = lastPromptDraft;
  lastPromptDraft = null;
  return draft;
}
