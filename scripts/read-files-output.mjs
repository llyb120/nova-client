export const READ_FILES_OUTPUT_MAX_BYTES = 64 * 1024;
export const READ_FILES_CONTENT_POOL_BYTES = 48 * 1024;

export function readFilesPerFileBudget(fileCount) {
  const count = Math.max(1, Number(fileCount) || 1);
  return Math.floor(READ_FILES_CONTENT_POOL_BYTES / count);
}

function escapeXmlAttribute(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll('"', "&quot;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
}

function cdata(value) {
  return `<![CDATA[${String(value ?? "").replaceAll("]]>", "]]]]><![CDATA[>")}]]>`;
}

function attribute(name, value) {
  return value === undefined || value === null ? "" : ` ${name}="${escapeXmlAttribute(value)}"`;
}

function render(results) {
  const files = results.map((result) => {
    const open = `<file${attribute("path", result.path)}`
      + attribute("start-line", result.startLine)
      + attribute("end-line", result.endLine)
      + attribute("lines-read", result.linesRead)
      + attribute("bytes-read", result.bytesRead)
      + attribute("max-content-bytes", result.maxContentBytes)
      + attribute("has-more", result.hasMore)
      + attribute("next-offset", result.nextOffset)
      + attribute("range-complete", result.rangeComplete)
      + attribute("stop-reason", result.stopReason)
      + attribute("line-bytes", result.lineBytes)
      + ">";
    if (result.error !== undefined) return `${open}<error>${cdata(result.error)}</error></file>`;
    return `${open}<content>${cdata(result.content)}</content></file>`;
  });
  return `<read_files version="1">\n${files.join("\n")}\n</read_files>`;
}

function trimToCompleteLines(content, maxBytes) {
  if (maxBytes <= 0 || !content) return "";
  const rows = content.split("\n");
  const kept = [];
  let bytes = 0;
  for (const row of rows) {
    const next = Buffer.byteLength(row, "utf8") + (kept.length ? 1 : 0);
    if (bytes + next > maxBytes) break;
    kept.push(row);
    bytes += next;
  }
  return kept.join("\n");
}

function normalizeLegacyResult(item) {
  if (item.error !== undefined || item.stopReason !== undefined) return { ...item };
  let content = String(item.content ?? "");
  let linesRead = content ? content.split("\n").length : 0;
  const legacyBytes = Buffer.byteLength(content, "utf8");
  const startLine = item.nextOffset ? Math.max(1, item.nextOffset - Math.max(linesRead, 1)) : 1;
  let stopReason = "eof";
  let hasMore = false;
  let rangeComplete = true;
  let lineBytes;
  if (item.truncated) {
    hasMore = true;
    if (!content.includes("\n") && legacyBytes >= 32 * 1024) {
      stopReason = "longLine";
      rangeComplete = false;
      lineBytes = legacyBytes;
      content = "";
      linesRead = 0;
    } else if (legacyBytes >= 32 * 1024) {
      stopReason = "byteBudget";
      rangeComplete = false;
    } else {
      stopReason = "lineLimit";
    }
  }
  return {
    path: item.path, content, startLine,
    ...(linesRead ? { endLine: startLine + linesRead - 1 } : {}),
    linesRead, bytesRead: Buffer.byteLength(content, "utf8"),
    maxContentBytes: item.maxContentBytes ?? READ_FILES_CONTENT_POOL_BYTES,
    hasMore, rangeComplete, stopReason,
    ...(hasMore ? { nextOffset: startLine + linesRead } : {}),
    ...(lineBytes === undefined ? {} : { lineBytes }),
  };
}

/** Format model-facing read_files output as bounded XML with unescaped source in safe CDATA. */
export function formatReadFilesXml(input, maxBytes = READ_FILES_OUTPUT_MAX_BYTES) {
  const results = (Array.isArray(input) ? input : []).map(normalizeLegacyResult);
  let xml = render(results);
  while (Buffer.byteLength(xml, "utf8") > maxBytes) {
    const candidates = results
      .map((result, index) => ({ index, bytes: Buffer.byteLength(result.content ?? "", "utf8") }))
      .filter(({ bytes }) => bytes > 0)
      .sort((a, b) => b.bytes - a.bytes);
    if (!candidates.length) {
      return `<read_files version="1" truncated="true"><error>${cdata("read_files metadata exceeded output budget")}</error></read_files>`;
    }
    const candidate = candidates[0];
    const result = results[candidate.index];
    const excess = Buffer.byteLength(xml, "utf8") - maxBytes;
    result.content = trimToCompleteLines(result.content, Math.max(0, candidate.bytes - excess - 256));
    result.linesRead = result.content ? result.content.split("\n").length : 0;
    result.bytesRead = Buffer.byteLength(result.content, "utf8");
    result.endLine = result.linesRead ? result.startLine + result.linesRead - 1 : undefined;
    result.hasMore = true;
    result.nextOffset = result.startLine + result.linesRead;
    result.rangeComplete = false;
    result.stopReason = "byteBudget";
    xml = render(results);
  }
  return xml;
}
