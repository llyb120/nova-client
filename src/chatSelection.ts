let canvasSelection = "";

export function setCanvasChatSelection(text: string) {
  canvasSelection = text.trim();
}

export function clearCanvasChatSelection() {
  canvasSelection = "";
}

/** Native transcript selection; the canvas fallback is retained for the comparison fixture. */
export function selectedChatText(): string {
  const selection = window.getSelection();
  const node = selection?.anchorNode;
  const element = node instanceof Element ? node : node?.parentElement;
  if (element?.closest(".html-transcript")) return selection?.toString().trim() ?? "";
  return canvasSelection;
}
