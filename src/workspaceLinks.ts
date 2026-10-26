export function linkedFile(href: string): { path: string; line?: number } | null {
  if (!href || href.startsWith("#")) return null;
  // Strip the source location before decoding: %23L12 / %3A12 can be part of a real filename.
  const location = href.match(/(?::(\d+)(?::\d+)?|#L(\d+)(?:C\d+)?(?:-L?\d+(?:C\d+)?)?)$/i);
  let path = location ? href.slice(0, -location[0].length) : href;
  // `a.ts:12` looks like a URI scheme; real schemes (https:, mailto:, javascript:) are not bare file names.
  if (/^[a-z][a-z\d+.-]*:/i.test(href) && !/^(?:file:|[a-z]:[\\/])/i.test(href)
    && !(location && /^[^:/\\]+\.\w+$/.test(path))) return null;
  try { path = decodeURIComponent(path); } catch { /* Literal percent signs are valid in filenames. */ }
  path = path.replace(/^file:\/\/localhost\//i, "/").replace(/^file:\/\/(?=\/|[a-z]:)/i, "").replace(/^file:\/\//i, "//").replace(/^\/([a-z]:[\\/])/i, "$1");
  return path ? { path, line: location ? Number(location[1] ?? location[2]) : undefined } : null;
}

export function openWorkspaceFile(path: string, line?: number) {
  if (path) window.dispatchEvent(new CustomEvent("nova:preview-file", { detail: { path, line } }));
}
