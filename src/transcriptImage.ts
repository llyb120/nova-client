import { convertFileSrc } from "@tauri-apps/api/core";

export function localImagePath(href: string): string | null {
  // Generated output uses absolute paths; do not resolve relative links against the app origin.
  const path = href.replace(/^\\\\\?\\/, "").replace(/\\/g, "/");
  return /^(?:[A-Za-z]:\/|\/(?!\/))/.test(path) ? path : null;
}

export function transcriptImageSrc(href: string): string {
  if (href.startsWith("nova-history://")) return href;
  const path = localImagePath(href);
  if (path) return convertFileSrc(path);
  return /^(?:https?:\/\/|data:image\/(?:png|jpeg|webp|gif);base64,)/i.test(href) ? href : "";
}
