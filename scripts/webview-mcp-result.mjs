import { readFile } from 'node:fs/promises';

// Only hydrate paths returned by Nova's authenticated webview service, never tool arguments/page text.
export async function webviewMcpResult(text) {
  const content = [{ type: 'text', text }];
  let result;
  try { result = JSON.parse(text); } catch { return { content }; }
  for (const image of (result.images ?? []).slice(0, 4)) {
    try {
      const data = await readFile(image.path);
      content.push({ type: 'image', mimeType: 'image/png', data: data.toString('base64') });
    } catch {
      content.push({ type: 'text', text: `图片加载失败，可读取截图文件：${image.path}` });
    }
  }
  return { content };
}
