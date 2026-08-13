import { api } from "./ipc";

const ABSOLUTE_PATH = /^(?:[A-Za-z]:[\\/]|\\\\|\/)/;

/** Ctrl/Cmd+Shift+V：把资源管理器复制的文件粘贴为绝对路径，而不是附件。 */
export function isPasteFilePathsShortcut(event: KeyboardEvent): boolean {
  return (
    event.code === "KeyV" &&
    event.shiftKey &&
    (event.ctrlKey || event.metaKey) &&
    !event.altKey &&
    !event.isComposing &&
    !event.repeat
  );
}

function addAbsolutePath(paths: string[], value: string | undefined) {
  const path = value?.trim().replace(/^"|"$/g, "");
  if (path && ABSOLUTE_PATH.test(path) && !paths.includes(path)) paths.push(path);
}

function fileUriPath(uri: string) {
  const path = decodeURI(uri.replace(/^file:\/\//, ""));
  return /^\/[A-Za-z]:\//.test(path) ? path.slice(1) : path;
}

export function pastedAbsoluteFilePaths(data: DataTransfer): string[] {
  const paths: string[] = [];
  for (const file of Array.from(data.files)) {
    addAbsolutePath(paths, (file as File & { path?: string }).path);
  }
  for (const item of Array.from(data.items)) {
    if (item.kind === "file") {
      addAbsolutePath(paths, (item.getAsFile() as (File & { path?: string }) | null)?.path);
    }
  }
  let uriList = "";
  let plain = "";
  try {
    uriList = data.getData("text/uri-list");
  } catch {
    // Some WebView paste events throw on getData.
  }
  try {
    plain = data.getData("text/plain");
  } catch {
    // Ignore formats the paste event does not expose.
  }
  for (const uri of uriList.split(/\r?\n/)) {
    if (!uri || uri.startsWith("#") || !uri.startsWith("file://")) continue;
    try {
      addAbsolutePath(paths, fileUriPath(uri));
    } catch {
      // Ignore malformed clipboard URI entries.
    }
  }
  for (const line of plain.split(/\r?\n/)) addAbsolutePath(paths, line);
  return paths;
}

/** 优先用 paste 事件，其次读系统剪贴板 HDROP，最后才尝试纯文本路径。 */
export async function resolveClipboardFilePaths(data?: DataTransfer | null): Promise<string[]> {
  if (data) {
    const fromEvent = pastedAbsoluteFilePaths(data);
    if (fromEvent.length > 0) return fromEvent;
  }
  try {
    const native = await api.clipboardFilePaths();
    if (native.length > 0) return native;
  } catch {
    // Native clipboard may be unavailable in tests / headless.
  }
  try {
    const text = await navigator.clipboard.readText();
    const paths: string[] = [];
    for (const line of text.split(/\r?\n/)) addAbsolutePath(paths, line);
    return paths;
  } catch {
    return [];
  }
}
