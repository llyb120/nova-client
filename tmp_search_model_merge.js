const fs = require('fs');
const path = require('path');

const dir = 'C:/Users/41406/AppData/Local/cursor-agent/versions/2026.07.09-a3815c0';
const files = ['index.js', '351.index.js', '5305.index.js', '3659.index.js'];
const needles = [
  'selectedModel',
  'modelParameters',
  'resolveModel',
  'flatten',
  'compose',
  '--model',
  'parseModel',
  'applyModel',
  '"fast"',
  "'fast'",
  'modelId',
  'hasChangedDefaultModel',
  'displayModelId',
  'modelParameters[',
  'selectedModel.parameters',
  'parameters.fast',
  'id:"fast"',
  "id:'fast'",
  'id: "fast"',
  "id: 'fast'",
];

const out = [];
const ctx = 800;

for (const file of files) {
  const full = path.join(dir, file);
  if (!fs.existsSync(full)) {
    out.push(`\n=== MISSING: ${file} ===\n`);
    continue;
  }
  const content = fs.readFileSync(full, 'utf8');
  out.push(`\n=== ${file} (${content.length} chars) ===\n`);
  for (const needle of needles) {
    let idx = 0;
    let count = 0;
    const positions = [];
    while ((idx = content.indexOf(needle, idx)) !== -1) {
      positions.push(idx);
      count++;
      idx += needle.length;
      if (count > 50) break;
    }
    out.push(`\n--- needle "${needle}": ${count}${count > 50 ? '+' : ''} hits ---\n`);
    // show up to 5 unique-ish contexts
    const shown = positions.slice(0, 8);
    for (const pos of shown) {
      const start = Math.max(0, pos - ctx);
      const end = Math.min(content.length, pos + needle.length + ctx);
      let snippet = content.slice(start, end);
      // collapse extreme whitespace a bit but keep readable
      snippet = snippet.replace(/\r/g, '');
      out.push(`\n@${pos} (±${ctx}):\n${snippet}\n`);
    }
  }
}

const outPath = 'D:/code/nova-client/tmp_cursor_model_merge_raw.txt';
fs.writeFileSync(outPath, out.join(''), 'utf8');
console.log('Wrote', outPath, 'bytes', fs.statSync(outPath).size);
