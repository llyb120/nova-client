import { createMemo, createRenderEffect, createSignal, untrack, type Accessor } from "solid-js";
import { attachmentPreviewSrc, takeWarmImage } from "./components/ImageAttachmentStrip";
import type { PromptImage } from "./types";

interface DisplayImage {
  src: string;
  width?: number;
  height?: number;
}

/** One logical attachment's stable <img>: bind src/width/height from this accessor.
 * Pass a reactive image accessor (e.g. Index's item), not a For-captured object.
 * Keep the element mounted across data -> URI updates; its owner disposes on session replacement.
 * CSS still owns max dimensions; file actions must read the latest attachment, not shown.src.
 * Cold sources without warm dimensions use native first-paint sizing; initial viewport decode
 * gating belongs to the transcript owner, not this slot (which may be offscreen).
 */
export function createHtmlPromptImage(image: Accessor<PromptImage>): Accessor<DisplayImage> {
  const source = createMemo(() => attachmentPreviewSrc(image()));
  // ponytail: attachments have no stable ID; slot-local metadata identifies a projection.
  // If same-name/type/size attachments can replace each other during persistence, add an attachment ID.
  const identity = createMemo(() => JSON.stringify([image().name, image().mimeType, image().size]));
  let shownIdentity = untrack(identity);
  const [shown, setShown] = createSignal<DisplayImage>({ src: untrack(source) });
  createRenderEffect(() => [source(), identity()] as const, ([src, key]) => {
    let current = untrack(shown);
    if (key !== shownIdentity || (src !== current.src && !(current.src.startsWith("data:") && !src.startsWith("data:")))) {
      setShown(current = { src });
    }
    shownIdentity = key;
    if (typeof Image === "undefined" || (current.src === src && current.width)) return;
    let active = true;
    const commit = (decoded: HTMLImageElement) => {
      if (active && decoded.naturalWidth > 0 && decoded.naturalHeight > 0) {
        setShown({ src, width: decoded.naturalWidth, height: decoded.naturalHeight });
      }
    };
    const warm = takeWarmImage(src);
    if (warm) {
      commit(warm);
    } else {
      const pending = new Image();
      pending.src = src;
      // Only a same-identity data -> URI projection retains old pixels/size on failure.
      void pending.decode().then(() => commit(pending), () => {});
    }
    return () => { active = false; };
  });
  return shown;
}
