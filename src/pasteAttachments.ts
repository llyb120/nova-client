/** 单个附件上限，与后端 read_local_attachment 相同：超限不读进内存，免得卡住界面和会话文件。 */
export const MAX_ATTACHMENT_BYTES = 20 * 1024 * 1024;

/** 剪贴板里可以作为附件的文件：图片和其他文件都算；超上限的只计数，由调用方提示。 */
export function clipboardFiles(items: ArrayLike<DataTransferItem>): { files: File[]; oversized: number } {
  const files: File[] = [];
  let oversized = 0;
  for (let index = 0; index < items.length; index++) {
    const item = items[index];
    if (item.kind !== "file") continue;
    const file = item.getAsFile();
    if (!file) continue;
    if (file.size > MAX_ATTACHMENT_BYTES) oversized += 1;
    else files.push(file);
  }
  return { files, oversized };
}
