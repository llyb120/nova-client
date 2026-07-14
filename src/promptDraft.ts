import type { PromptImage } from "./types";

type PromptDraft = {
  text: string;
  images: PromptImage[];
};

let lastPromptDraft: PromptDraft | null = null;

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
