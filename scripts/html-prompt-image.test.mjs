// Headless: real Solid reactivity; controllable image decoding, no browser or IPC.
import assert from "node:assert/strict";
import { build } from "esbuild";

const result = await build({
  stdin: {
    contents: `export { createHtmlPromptImage } from './src/createHtmlPromptImage';
      export { createRoot, createSignal, flush } from 'solid-js';
      export { warmImages } from './src/components/ImageAttachmentStrip';`,
    resolveDir: process.cwd(), loader: "ts",
  },
  bundle: true, write: false, format: "esm", platform: "browser",
  plugins: [{
    name: "attachment-cache-stub",
    setup(build) {
      build.onResolve({ filter: /\/ImageAttachmentStrip$/ }, () => ({ path: "attachments", namespace: "mock" }));
      build.onLoad({ filter: /.*/, namespace: "mock" }, () => ({ contents: `
        export const warmImages = new Map();
        export const attachmentPreviewSrc = image => image.data
          ? 'data:' + image.mimeType + ';base64,' + image.data : image.uri;
        export function takeWarmImage(src) {
          const image = warmImages.get(src);
          warmImages.delete(src);
          return image;
        }`, loader: "js" }));
    },
  }],
});
const { createHtmlPromptImage, createRoot, createSignal, flush, warmImages } = await import(
  `data:text/javascript;base64,${Buffer.from(result.outputFiles[0].text).toString("base64")}`,
);

const requests = [];
const originalImage = globalThis.Image;
class PendingImage {
  naturalWidth = 0;
  naturalHeight = 0;
  constructor() { requests.push(this); }
  decode() {
    return new Promise((resolve, reject) => {
      this.ready = (width = 640, height = 480) => {
        this.naturalWidth = width;
        this.naturalHeight = height;
        resolve();
      };
      this.fail = reject;
    });
  }
}
const inline = { name: "image.png", mimeType: "image/png", data: "original" };
const uri = value => ({ name: inline.name, mimeType: inline.mimeType, uri: `asset://${value}` });
let dispose;
try {
  globalThis.Image = PendingImage;
  warmImages.set("data:image/png;base64,original", { naturalWidth: 800, naturalHeight: 600 });
  const { shown, update } = createRoot(cleanup => {
    dispose = cleanup;
    const [image, update] = createSignal(inline);
    return { shown: createHtmlPromptImage(image), update: value => flush(() => update(value)) };
  });
  flush();
  assert.deepEqual(shown(), { src: "data:image/png;base64,original", width: 800, height: 600 });
  assert.equal(requests.length, 0, "warm image supplies first-render dimensions without another decode");

  update(uri("persisted"));
  const persisted = requests.at(-1);
  const previous = shown();
  assert.equal(shown().src, "data:image/png;base64,original", "keep old pixels and size during projection");
  update({ ...uri("persisted") });
  assert.equal(requests.at(-1), persisted, "equivalent object replacement must not restart decoding");
  persisted.ready(800, 600);
  await Promise.resolve(); flush();
  assert.deepEqual(shown(), { src: "asset://persisted", width: 800, height: 600 });
  assert.equal(shown().height, previous.height, "same image projection preserves intrinsic height");

  update(inline);
  assert.deepEqual(shown(), { src: "data:image/png;base64,original" }, "a new inline source must not retain unrelated URI pixels");
  requests.at(-1).ready(800, 600);
  await Promise.resolve(); flush();
  update(uri("slow"));
  const slow = requests.at(-1);
  update(uri("latest"));
  requests.at(-1).ready(320, 200);
  await Promise.resolve(); flush();
  slow.ready(999, 999);
  await Promise.resolve(); flush();
  assert.deepEqual(shown(), { src: "asset://latest", width: 320, height: 200 }, "out-of-order decode cannot overwrite latest source");

  update(inline);
  requests.at(-1).ready(800, 600);
  await Promise.resolve(); flush();
  update(uri("broken"));
  requests.at(-1).fail(new Error("decode failed"));
  await Promise.resolve(); flush();
  assert.deepEqual(shown(), { src: "data:image/png;base64,original", width: 800, height: 600 }, "failed projection retains the last usable image");
  update(uri("zero-size"));
  requests.at(-1).ready(0, 0);
  await Promise.resolve(); flush();
  assert.equal(shown().width, 800, "invalid decoded dimensions must not collapse the image");

  update(uri("pending"));
  const pending = requests.at(-1);
  const count = requests.length;
  update(inline);
  assert.equal(requests.length, count, "returning to the displayed source needs no decode");
  pending.ready();
  await Promise.resolve(); flush();
  assert.equal(shown().src, "data:image/png;base64,original", "return to displayed source cancels stale completion");

  for (const change of [{ name: "other.png" }, { mimeType: "image/jpeg" }, { size: 42 }]) {
    update(inline);
    requests.at(-1).ready(800, 600);
    await Promise.resolve(); flush();
    update(uri("old-slot"));
    const stale = requests.at(-1);
    update({ ...uri("new-slot"), ...change });
    assert.deepEqual(shown(), { src: "asset://new-slot" }, "wrong identity clears old pixels and dimensions immediately");
    requests.at(-1).fail(new Error("new attachment failed"));
    stale.ready(999, 999);
    await Promise.resolve(); flush();
    assert.deepEqual(shown(), { src: "asset://new-slot" }, "failure and stale completion cannot restore a different attachment");
  }

  update(uri("unmounted"));
  const beforeDisposal = shown();
  dispose();
  requests.at(-1).ready();
  await Promise.resolve(); flush();
  assert.equal(shown(), beforeDisposal, "disposed slot cannot publish decode results");

  const cold = createRoot(cleanup => {
    dispose = cleanup;
    return createHtmlPromptImage(() => uri("cold"));
  });
  flush();
  assert.deepEqual(cold(), { src: "asset://cold" }, "cold initial source remains a native image request");
  requests.at(-1).ready(1200, 900);
  await Promise.resolve(); flush();
  assert.deepEqual(cold(), { src: "asset://cold", width: 1200, height: 900 });
  dispose();
  delete globalThis.Image;
  createRoot(cleanup => {
    assert.deepEqual(createHtmlPromptImage(() => inline)(), { src: "data:image/png;base64,original" });
    cleanup();
  });
  console.log("Html prompt image checks passed (warm dimensions, projection, races, failure, identity isolation, disposal, cold load, SSR).");
} finally {
  dispose?.();
  if (originalImage === undefined) delete globalThis.Image;
  else globalThis.Image = originalImage;
}
