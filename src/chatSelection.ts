let canvasSelection = "";

export function setCanvasChatSelection(text: string) {
  canvasSelection = text.trim();
}

export function clearCanvasChatSelection() {
  canvasSelection = "";
}

/** 读取聊天记录当前选区；DOM 与 Canvas 两种渲染模式统一处理。 */
export function selectedChatText(): string {
  const selection = window.getSelection();
  if (selection && !selection.isCollapsed && selection.rangeCount > 0) {
    const node = selection.getRangeAt(0).commonAncestorContainer;
    const element = node instanceof Element ? node : node.parentElement;
    if (element?.closest(".chat-body .transcript")) return selection.toString().trim();
  }
  return canvasSelection;
}
