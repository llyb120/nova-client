export function linkedFile(href: string): { path: string; line?: number } | null {
  if (!href || href.startsWith("#") || (/^[a-z][a-z\d+.-]*:/i.test(href) && !/^(?:file:|[a-z]:[\\/])/i.test(href))) return null;
  let path = href;
  try { path = decodeURIComponent(path); } catch { /* Literal percent signs are valid in filenames. */ }
  path = path.replace(/^file:\/\/localhost\//i, "/").replace(/^file:\/\/(?=\/|[a-z]:)/i, "").replace(/^file:\/\//i, "//").replace(/^\/([a-z]:[\\/])/i, "$1");
  const location = path.match(/(?::(\d+)(?::\d+)?|#L(\d+)(?:C\d+)?(?:-L?\d+(?:C\d+)?)?)$/i);
  if (location) path = path.slice(0, -location[0].length);
  return path ? { path, line: location ? Number(location[1] ?? location[2]) : undefined } : null;
}

export function openWorkspaceFile(path: string, line?: number) {
  if (path) window.dispatchEvent(new CustomEvent("nova:preview-file", { detail: { path, line } }));
}
