import { convertFileSrc } from "@tauri-apps/api/core";
import { state } from "./store";

/** 把各种形式的本地图片链接统一解析为绝对路径，供 convertFileSrc 使用。 */
export function localImagePath(href: string): string | null {
  // Strip file:// protocol; ignore other URL schemes entirely.
  if (/^(?:https?|data|javascript|mailto|tel):/i.test(href)) return null;
  let path = decodeURIComponent(href)
    .replace(/^file:\/\/(?:localhost\/)?/i, "")
    .replace(/^\\\\\?\\/, "")
    .replace(/\\/g, "/")
    // file:///X:/path leaves a leading slash before the drive letter.
    .replace(/^\/([A-Za-z]:)/, "$1");
  if (/^(?:[A-Za-z]:\/|\/(?!\/))/.test(path)) return path;
  // Relative paths resolve against the current working directory.
  const base = (state.cwd || "").replace(/\\/g, "/").replace(/\/+$/, "");
  if (!base) return null;
  const segments = path.split("/").filter(Boolean);
  const stack = base.split("/");
  for (const seg of segments) {
    if (seg === "." || seg === "") continue;
    if (seg === "..") { if (stack.length > 1) stack.pop(); continue; }
    stack.push(seg);
  }
  return stack.join("/");
}

export function transcriptImageSrc(href: string): string {
  const path = localImagePath(href);
  if (path) return convertFileSrc(path);
  return /^(?:https?:\/\/|data:image\/(?:png|jpeg|webp|gif);base64,)/i.test(href) ? href : "";
}
