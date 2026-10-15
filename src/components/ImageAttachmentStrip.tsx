import { convertFileSrc } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { api, fileUriPath } from "../ipc";
import type { PromptImage } from "../types";
import { isFileDropBlocked, workspaceFileDropTarget } from "../utils";
import { IconFile, IconX } from "./icons";

function fileToAttachment(f: File): Promise<PromptImage> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const url = reader.result as string;
      resolve({
        name: f.name || (f.type.startsWith("image/") ? "粘贴的图片" : "粘贴的文件"),
        mimeType: f.type || guessMimeType(f.name),
        data: url.slice(url.indexOf(",") + 1),
        size: f.size,
      });
    };
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(f);
  });
}

function fileNameExt(name: string) {
  const i = name.lastIndexOf(".");
  return i >= 0 ? name.slice(i + 1).toLowerCase() : "";
}

function fileName(path: string) {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

function guessMimeType(name: string) {
  switch (fileNameExt(name)) {
    case "png":
      return "image/png";
    case "jpg":
    case "jpeg":
      return "image/jpeg";
    case "gif":
      return "image/gif";
    case "webp":
      return "image/webp";
    case "bmp":
      return "image/bmp";
    case "svg":
      return "image/svg+xml";
    case "json":
      return "application/json";
    case "md":
      return "text/markdown";
    case "txt":
      return "text/plain";
    case "html":
      return "text/html";
    case "css":
      return "text/css";
    case "js":
    case "ts":
    case "tsx":
    case "jsx":
      return "text/plain";
    case "pdf":
      return "application/pdf";
    default:
      return "application/octet-stream";
  }
}

function pathToFileUri(path: string) {
  const normalized = path.replace(/\\/g, "/");
  const withSlash = normalized.startsWith("/") ? normalized : `/${normalized}`;
  return `file://${encodeURI(withSlash)}`;
}

export function attachmentPreviewSrc(image: PromptImage) {
  return image.data
    ? `data:${image.mimeType};base64,${image.data}`
    : convertFileSrc(fileUriPath(image.uri ?? ""));
}

// 附件进输入框时就预解码：发送后 Canvas 首帧即拿到尺寸和位图，不必先等解码再整帧提交
// （等待期间输入框收起、旧画面下移，消息随后才出现，表现为发送后画面闪一下）。
// ponytail: 只留最近 8 张；更大批量的附件回退到 Canvas 自己解码。
const warmImages = new Map<string, HTMLImageElement>();
const decodedImages = new WeakSet<HTMLImageElement>();

export function prewarmAttachmentImages(images: PromptImage[]) {
  for (const image of images) {
    if (!image.mimeType.startsWith("image/")) continue;
    const src = attachmentPreviewSrc(image);
    if (warmImages.has(src)) continue;
    const el = new Image();
    el.src = src;
    el.decode().then(() => decodedImages.add(el), () => { if (warmImages.get(src) === el) warmImages.delete(src); });
    warmImages.set(src, el);
    if (warmImages.size > 8) warmImages.delete(warmImages.keys().next().value!);
  }
}

/** 取走已解码完成的预热图片，所有权交给调用方。 */
export function takeWarmImage(src: string): HTMLImageElement | undefined {
  const el = warmImages.get(src);
  if (!el || !decodedImages.has(el)) return undefined;
  warmImages.delete(src);
  return el;
}

/** 附件状态：粘贴图片走 base64，拖入文件走 Tauri file path。 */
export function createImageAttachments(
  options: {
    enableFileDrop?: boolean;
    acceptAllPasteFiles?: boolean;
    /** 最前层（如弹窗）独占拖放：忽略全局 fileDropBlocked，由自己接管文件拖入。 */
    dropIgnoresBlock?: boolean;
  } = {},
) {
  const [images, setImages] = createSignal<PromptImage[]>([]);
  const add = (next: PromptImage[]) => {
    prewarmAttachmentImages(next);
    setImages((prev) => [...prev, ...next]);
  };
  const [dragging, setDragging] = createSignal(false);

  const onPaste = (e: ClipboardEvent) => {
    const files = [...(e.clipboardData?.items ?? [])]
      .filter(
        (it) =>
          it.kind === "file" &&
          (options.acceptAllPasteFiles || it.type.startsWith("image/")),
      )
      .map((it) => it.getAsFile())
      .filter((f): f is File => f != null);
    if (files.length === 0) return;
    e.preventDefault();
    void Promise.all(files.map(fileToAttachment)).then(add);
  };

  const addPaths = (paths: string[]) => {
    const next = paths.map((path) => {
      const name = fileName(path);
      return {
        name,
        mimeType: guessMimeType(name),
        uri: pathToFileUri(path),
      };
    });
    if (next.length > 0) add(next);
  };

  onMount(() => {
    if (!options.enableFileDrop) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    try {
      void getCurrentWebview()
        .onDragDropEvent((event) => {
          if (!options.dropIgnoresBlock && event.payload.type !== "leave" && workspaceFileDropTarget(event.payload.position)) {
            setDragging(false);
            return;
          }
          if (isFileDropBlocked() && !options.dropIgnoresBlock) {
            if (event.payload.type === "drop" || event.payload.type === "leave") {
              setDragging(false);
            }
            return;
          }
          if (event.payload.type === "enter" || event.payload.type === "over") {
            setDragging(true);
          } else if (event.payload.type === "drop") {
            setDragging(false);
            addPaths(event.payload.paths);
          } else {
            setDragging(false);
          }
        })
        .then((fn) => {
          if (cancelled) fn();
          else unlisten = fn;
        })
        .catch(() => setDragging(false));
    } catch {
      setDragging(false);
    }
    onCleanup(() => {
      cancelled = true;
      unlisten?.();
    });
  });

  const remove = (index: number) =>
    setImages((prev) => prev.filter((_, i) => i !== index));
  const clear = () => setImages([]);
  /** 用已有附件初始化（编辑历史消息时复制一份，避免引用 store 节点） */
  const set = (imgs: PromptImage[]) => {
    prewarmAttachmentImages(imgs);
    setImages(imgs.map((img) => ({ ...img })));
  };

  /** 通过系统文件对话框选择本地文件，读取为内嵌附件（不限图片类型）。 */
  const pickFiles = async () => {
    const picked = await openDialog({ multiple: true, title: "选择要附加的文件" });
    const paths = (Array.isArray(picked) ? picked : picked ? [picked] : []).filter(
      (item): item is string => typeof item === "string" && item.length > 0,
    );
    if (paths.length === 0) return;
    const added: PromptImage[] = [];
    for (const path of paths) {
      added.push(await api.readLocalAttachment(path));
    }
    if (added.length > 0) add(added);
  };

  return { images, dragging, onPaste, remove, clear, set, pickFiles };
}

export function ImageAttachmentStrip(props: {
  images: PromptImage[];
  onRemove: (index: number) => void;
}) {
  return (
    <Show when={props.images.length > 0}>
      <div class="image-strip">
        <For each={props.images}>
          {(image, index) => (
            <div classList={{ "image-chip": true, "file-chip": !image.mimeType.startsWith("image/") }} title={image.name}>
              <Show
                when={image.mimeType.startsWith("image/")}
                fallback={
                  <>
                    <IconFile size={22} />
                    <span>{image.name}</span>
                  </>
                }
              >
                <img
                  src={attachmentPreviewSrc(image)}
                  alt={image.name}
                  draggable={false}
                />
              </Show>
              <button
                class="image-remove"
                title="移除附件"
                onClick={() => props.onRemove(index())}
              >
                <IconX size={12} />
              </button>
            </div>
          )}
        </For>
      </div>
    </Show>
  );
}
