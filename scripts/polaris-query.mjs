/** Shared JS normalization. The native boundary independently applies the same contract. */
export function normalizePolarisArgs(params = {}) {
  if (!params || typeof params !== 'object' || Array.isArray(params)) throw new TypeError('polaris parameters must be an object');
  const list = (value, max) => {
    const seen = new Set();
    return (Array.isArray(value) ? value : typeof value === 'string' ? [value] : [])
      .filter(x => typeof x === 'string').map(x => x.trim())
      .filter(x => x && Buffer.byteLength(x) <= 4096 && !seen.has(x.toLowerCase()) && seen.add(x.toLowerCase())).slice(0, max);
  };
  const identifier = s => s.length <= 200 && /^[A-Za-z_$][A-Za-z0-9_$:.-]*$/.test(s);
  const query = [...String(params.query ?? '').trim()].slice(0, 1024).join('');
  let task = [...String(params.task ?? '').trim()].slice(0, 1024).join('');
  let keywords = list(params.keywords, 5);
  if (query) {
    if (identifier(query)) { if (!keywords.some(x => x.toLowerCase() === query.toLowerCase())) keywords.push(query); }
    else if (!task) task = query;
  }
  if (!task) task = keywords.filter(x => !identifier(x)).join(' ');
  keywords = keywords.filter(identifier).slice(0, 5);
  return { ...params, keywords, task, files: list(params.files, 6) };
}
