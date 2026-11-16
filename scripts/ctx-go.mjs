// Go package declarations, mirrored by nova_tools_native/context/go.rs.
function code(text) {
  let out = '', quote = '', block = false, comment = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i], next = text[i + 1];
    if (c === '\n') { out += c; comment = false; continue; }
    if (comment) { out += ' '; continue; }
    if (block) { if (c === '*' && next === '/') { i++; block = false; } out += ' '; continue; }
    if (quote) {
      if (c === quote) { quote = ''; out += ')'; }
      else if (c === '\\' && quote !== '`') { i++; out += ' '; }
      else out += ' ';
      continue;
    }
    if (c === '/' && next === '/') { i++; comment = true; out += ' '; }
    else if (c === '/' && next === '*') { i++; block = true; out += ' '; }
    else if (['"', "'", '`'].includes(c)) { quote = c; out += '('; }
    else out += c;
  }
  return out.replace(/\n$/, '').split('\n');
}
function end(lines, start) {
  let balance = 0;
  for (let i = start; i < lines.length; i++) {
    for (const ch of lines[i]) {
      if ('([{'.includes(ch)) balance++;
      else if (')]}'.includes(ch)) balance--;
    }
    if (balance <= 0 && !/[=,+\-*/|&]$/.test(lines[i].trimEnd())) return i;
  }
  return lines.length - 1;
}
export function scanGo(text) {
  const raw = text.replace(/\n$/, '').split('\n'), lines = code(text), syms = [], imports = [];
  let group = null;
  for (let i = 0; i < lines.length;) {
    const line = lines[i].trim();
    if (!line) { i++; continue; }
    if (group && line === ')') { group = null; i++; continue; }
    let value = line, kind = group?.kind;
    if (!kind) {
      const head = /^(var|const|type|import)\s+(.*)$/.exec(line);
      if (head) { kind = head[1]; value = head[2].trim(); }
      if (kind && value === '(') { group = { kind, start:i, last:end(lines, i) }; i++; continue; }
    }
    const finish = end(lines, i);
    if (kind === 'import') {
      const m = /^(?:(\w+|\.)\s+)?["`]([^"`]+)["`]/.exec(raw[i].trim().replace(/^import\s+/, ''));
      if (m) { const name = m[1] ?? m[2].split('/').at(-1); if (name !== '_') imports.push({ name, from:m[2] }); }
    } else {
      const fn = /^func\s+(?:\([^)]*\)\s*)?([\p{L}_][\p{L}\p{N}_]*)\s*(?:\[|\()/u.exec(line);
      const decl = kind && /^([\p{L}_][\p{L}\p{N}_]*(?:\s*,\s*[\p{L}_][\p{L}\p{N}_]*)*)\b/u.exec(value);
      const names = fn?.[1] ?? decl?.[1];
      if (names) for (const name of names.split(',').map(n => n.trim()).filter(n => n !== '_')) {
        syms.push({ name, ln:(group?.kind === 'const' ? group.start : i) + 1,
          end:(group?.kind === 'const' ? group.last : finish) + 1, depth:0,
          kind:fn ? (/^func\s*\(/.test(line) ? 'method' : 'fn') : kind === 'type' ? 'type' : 'const',
          sig:raw[i].trim().replace(/\s+/g, ' ').slice(0,120), exp:/^\p{Lu}/u.test(name) });
      }
    }
    i = finish + 1;
  }
  return { total:raw.length, syms, imports };
}
