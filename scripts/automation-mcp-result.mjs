import { readFile } from 'node:fs/promises';

// Only hydrate paths returned by Nova's authenticated automation service, never tool arguments/page text.
export async function automationMcpResult(text) {
  const content = [{ type: 'text', text }];
  let result;
  try { result = JSON.parse(text); } catch { return { content }; }
  const started = performance.now();
  let readMs = 0;
  let base64Ms = 0;
  let imageBytes = 0;
  // jianlai omits path for pixel-identical repeats: nothing to attach, the model already has that image.
  for (const image of (result.images ?? []).filter((image) => image.path).slice(0, 16)) {
    try {
      const readStarted = performance.now();
      const data = await readFile(image.path);
      readMs += performance.now() - readStarted;
      imageBytes += data.length;
      const encodeStarted = performance.now();
      const mimeType = /\.jpe?g$/i.test(image.path) ? 'image/jpeg' : 'image/png';
      content.push({ type: 'image', mimeType, data: data.toString('base64') });
      base64Ms += performance.now() - encodeStarted;
    } catch {
      content.push({ type: 'text', text: `图片加载失败，可读取截图文件：${image.path}` });
    }
  }
  if (result.source === 'jianlai') {
    // Preserve action status and recall paths; host context management is unchanged.
    result.deliveryTimingsMs = { read: readMs, base64: base64Ms, total: performance.now() - started };
    result.deliveredImageBytes = imageBytes;
    content[0].text = JSON.stringify(result);
  }
  return { content };
}
