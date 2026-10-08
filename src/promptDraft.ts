import type { PromptImage } from "./types";

type PromptDraft = {
  text: string;
  images: PromptImage[];
};

let lastPromptDraft: PromptDraft | null = null;
const sessionDrafts = new Map<string | null, PromptDraft>();

export function saveSessionDraft(threadId: string | null, text: string, images: PromptImage[]) {
  if (!text && images.length === 0) {
    sessionDrafts.delete(threadId);
    return;
  }
  sessionDrafts.set(threadId, { text, images: images.map((image) => ({ ...image })) });
}

export function takeSessionDraft(threadId: string | null): PromptDraft | null {
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
