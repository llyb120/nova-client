import { readFile } from 'node:fs/promises';

// Only hydrate paths returned by Nova's authenticated webview service, never tool arguments/page text.
export async function webviewMcpResult(text) {
  const content = [{ type: 'text', text }];
  let result;
  try { result = JSON.parse(text); } catch { return { content }; }
  const started = performance.now();
  let readMs = 0;
  let base64Ms = 0;
  let imageBytes = 0;
  for (const image of (result.images ?? []).slice(0, 16)) {
    try {
      const readStarted = performance.now();
      const data = await readFile(image.path);
      readMs += performance.now() - readStarted;
      imageBytes += data.length;
      const encodeStarted = performance.now();
      content.push({ type: 'image', mimeType: 'image/png', data: data.toString('base64') });
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
