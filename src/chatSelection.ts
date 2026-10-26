/** Native transcript selection. */
export function selectedChatText(): string {
  const selection = window.getSelection();
  const node = selection?.anchorNode;
  const element = node instanceof Element ? node : node?.parentElement;
  return element?.closest(".html-transcript") ? selection?.toString().trim() ?? "" : "";
}
