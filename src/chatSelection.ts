let canvasSelection = "";

export function setCanvasChatSelection(text: string) {
  canvasSelection = text.trim();
}

export function clearCanvasChatSelection() {
  canvasSelection = "";
}

/** 读取 Canvas 聊天记录当前选区。 */
export function selectedChatText(): string {
  return canvasSelection;
}
