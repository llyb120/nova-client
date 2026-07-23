const FUZZY_THRESHOLD = 0.9;
const AMBIGUITY_MARGIN = 0.08;
const MAX_FALLBACK_CANDIDATES = 20_000;

const unicodeMap = new Map([
  ...["‐", "‑", "‒", "–", "—", "―", "−"].map((char) => [char, "-"]),
  ...["‘", "’", "‚", "‛"].map((char) => [char, "'"]),
  ...["“", "”", "„", "‟"].map((char) => [char, '"']),
  ...["\u00a0", " ", " ", " ", " ", " ", " ", " ", " ", " ", " ", " ", "　"].map((char) => [char, " "]),
]);

function normalizeUnicode(value) {
  return Array.from(value, (char) => unicodeMap.get(char) ?? char).join("");
}

function lineIndent(line) {
  return line.match(/^[\t ]*/)?.[0] ?? "";
}

function relativeIndentLines(lines) {
  let previous = 0;
  return lines.map((line, index) => {
    const indent = lineIndent(line).length;
    const delta = index === 0 ? 0 : indent - previous;
    previous = indent;
    return `${delta}:${normalizeUnicode(line).trim()}`;
  });
}

const lineModes = [
  { name: "rstrip", map: (lines) => lines.map((line) => line.trimEnd()) },
  { name: "unicode", map: (lines) => lines.map((line) => normalizeUnicode(line).trimEnd()) },
];
const relativeAnchorMode = { name: "relative-anchor", map: (lines) => lines.map((line) => normalizeUnicode(line).trim()) };

function findOccurrences(content, needle) {
  const positions = [];
  let from = 0;
  while (from <= content.length - needle.length) {
    const index = content.indexOf(needle, from);
    if (index < 0) break;
    positions.push(index);
    from = index + Math.max(1, needle.length);
  }
  return positions;
}

function buildLineTable(content) {
  const lines = content.split("\n");
  const starts = new Array(lines.length);
  let offset = 0;
  for (let index = 0; index < lines.length; index += 1) {
    starts[index] = offset;
    offset += lines[index].length + (index < lines.length - 1 ? 1 : 0);
  }
  return { lines, starts };
}

function buildIndex(lines) {
  const index = new Map();
  for (let position = 0; position < lines.length; position += 1) {
    const line = lines[position];
    if (!line.trim()) continue;
    const positions = index.get(line) ?? [];
    positions.push(position);
    index.set(line, positions);
  }
  return index;
}

function candidateStarts(targetLines, patternLines, mode) {
  const mappedTarget = mode.map(targetLines);
  const mappedPattern = mode.map(patternLines);
  const index = buildIndex(mappedTarget);
  const anchors = mappedPattern
    .map((line, offset) => ({ line, offset, positions: index.get(line) ?? [] }))
    .filter((anchor) => anchor.line.trim() && anchor.positions.length > 0 && anchor.positions.length <= MAX_FALLBACK_CANDIDATES)
    .sort((left, right) => left.positions.length - right.positions.length || right.line.length - left.line.length)
    .slice(0, 4);
  const votes = new Map();
  for (const anchor of anchors) {
    for (const position of anchor.positions) {
      const start = position - anchor.offset;
      if (start < 0 || start + patternLines.length > targetLines.length) continue;
      votes.set(start, (votes.get(start) ?? 0) + 1);
    }
  }
  return {
    mappedTarget,
    mappedPattern,
    starts: [...votes]
      .sort((left, right) => right[1] - left[1] || left[0] - right[0])
      .slice(0, MAX_FALLBACK_CANDIDATES)
      .map(([start]) => start),
  };
}

function sameSequence(lines, pattern, start) {
  for (let offset = 0; offset < pattern.length; offset += 1) {
    if (lines[start + offset] !== pattern[offset]) return false;
  }
  return true;
}

function spanForLines(table, start, pattern) {
  const last = start + pattern.length - 1;
  const end = pattern.at(-1) === "" && last < table.starts.length
    ? table.starts[last]
    : table.starts[last] + table.lines[last].length;
  return { index: table.starts[start], length: end - table.starts[start], line: start };
}

function tokenSet(value) {
  return new Set(normalizeUnicode(value).match(/[\p{L}\p{N}_$]+|[^\s\p{L}\p{N}_$]/gu) ?? []);
}

function jaccard(left, right) {
  if (left.size === 0 && right.size === 0) return 1;
  let common = 0;
  for (const value of left) if (right.has(value)) common += 1;
  return common / (left.size + right.size - common);
}

function lineSimilarity(left, right) {
  const a = normalizeUnicode(left).trim();
  const b = normalizeUnicode(right).trim();
  if (a === b) return 1;
  const length = Math.max(a.length, b.length);
  if (length === 0) return 1;
  // A linear positional score is intentionally conservative; fuzzy matching is only a final fallback.
  let prefix = 0;
  while (prefix < Math.min(a.length, b.length) && a[prefix] === b[prefix]) prefix += 1;
  let suffix = 0;
  while (suffix < Math.min(a.length, b.length) - prefix && a[a.length - 1 - suffix] === b[b.length - 1 - suffix]) suffix += 1;
  return 0.55 * ((prefix + suffix) / length) + 0.45 * jaccard(tokenSet(a), tokenSet(b));
}

function scoreCandidate(targetLines, patternLines, start) {
  let lineScore = 0;
  for (let offset = 0; offset < patternLines.length; offset += 1) {
    lineScore += lineSimilarity(targetLines[start + offset], patternLines[offset]);
  }
  const count = patternLines.length;
  const boundary = (lineSimilarity(targetLines[start], patternLines[0]) +
    lineSimilarity(targetLines[start + count - 1], patternLines[count - 1])) / 2;
  // lineSimilarity already rewards exact lines; avoid a second exact-only weight
  // that would make a one-token stale context unable to reach the safe threshold.
  return 0.9 * (lineScore / count) + 0.1 * boundary;
}

function rebaseIndent(newText, oldText, matchedText) {
  const oldFirst = oldText.split("\n").find((line) => line.trim());
  const matchedFirst = matchedText.split("\n").find((line) => line.trim());
  if (!oldFirst || !matchedFirst) return newText;
  const oldIndent = lineIndent(oldFirst);
  const matchedIndent = lineIndent(matchedFirst);
  if (oldIndent === matchedIndent) return newText;
  return newText.split("\n").map((line) => {
    if (!line.trim()) return line;
    if (line.startsWith(oldIndent)) return matchedIndent + line.slice(oldIndent.length);
    return line;
  }).join("\n");
}

function fuzzyCandidates(targetLines, patternLines) {
  const starts = new Set();
  for (const mode of lineModes) {
    for (const start of candidateStarts(targetLines, patternLines, mode).starts) starts.add(start);
  }
  const maxStart = targetLines.length - patternLines.length;
  if (starts.size === 0 && maxStart + 1 <= MAX_FALLBACK_CANDIDATES) {
    for (let start = 0; start <= maxStart; start += 1) starts.add(start);
  }
  return [...starts].slice(0, MAX_FALLBACK_CANDIDATES);
}

function locateEdit(content, oldText, path, editIndex, totalEdits) {
  if (!oldText) throw new Error(`${totalEdits === 1 ? "oldText" : `edits[${editIndex}].oldText`} must not be empty in ${path}.`);
  const exact = findOccurrences(content, oldText);
  if (exact.length === 1) return { index: exact[0], length: oldText.length, mode: "exact", newTextRebaser: (text) => text };
  if (exact.length > 1) throw new Error(`Found ${exact.length} occurrences of ${totalEdits === 1 ? "the text" : `edits[${editIndex}]`} in ${path}. Add context to make oldText unique.`);

  const table = buildLineTable(content);
  const patternLines = oldText.split("\n");
  if (patternLines.length > table.lines.length) throw new Error(`Could not find edits[${editIndex}] in ${path}.`);

  for (const mode of lineModes) {
    const candidates = candidateStarts(table.lines, patternLines, mode);
    const matches = candidates.starts.filter((start) => sameSequence(candidates.mappedTarget, candidates.mappedPattern, start));
    if (matches.length > 1) throw new Error(`Ambiguous ${mode.name} match for edits[${editIndex}] in ${path}; add more context.`);
    if (matches.length === 1) {
      const span = spanForLines(table, matches[0], patternLines);
      const matched = content.slice(span.index, span.index + span.length);
      return { ...span, mode: mode.name, newTextRebaser: (text) => rebaseIndent(text, oldText, matched) };
    }
  }

  // Compute indentation deltas independently for each candidate slice. The
  // first line is depth-neutral; following lines must preserve the full shape.
  const relativeCandidates = candidateStarts(table.lines, patternLines, relativeAnchorMode).starts;
  const relativePattern = relativeIndentLines(patternLines);
  const relativeMatches = relativeCandidates.filter((start) =>
    sameSequence(relativeIndentLines(table.lines.slice(start, start + patternLines.length)), relativePattern, 0));
  if (relativeMatches.length > 1) throw new Error(`Ambiguous relative-indent match for edits[${editIndex}] in ${path}; add more context.`);
  if (relativeMatches.length === 1) {
    const span = spanForLines(table, relativeMatches[0], patternLines);
    const matched = content.slice(span.index, span.index + span.length);
    return { ...span, mode: "relative-indent", newTextRebaser: (text) => rebaseIndent(text, oldText, matched) };
  }

  const ranked = fuzzyCandidates(table.lines, patternLines)
    .map((start) => ({ start, score: scoreCandidate(table.lines, patternLines, start) }))
    .sort((left, right) => right.score - left.score || left.start - right.start);
  const best = ranked[0];
  const second = ranked[1];
  // Report close high-quality contenders as ambiguity even if they sit just
  // below the apply threshold; this gives the agent the correct recovery action.
  if (best && second && best.score >= FUZZY_THRESHOLD - AMBIGUITY_MARGIN && best.score - second.score < AMBIGUITY_MARGIN) {
    throw new Error(`Ambiguous fuzzy match for edits[${editIndex}] in ${path} (${Math.round(best.score * 100)}% vs ${Math.round(second.score * 100)}%); add more context.`);
  }
  if (!best || best.score < FUZZY_THRESHOLD) {
    throw new Error(`Could not find a sufficiently similar match for edits[${editIndex}] in ${path}${best ? ` (best ${Math.round(best.score * 100)}%)` : ""}.`);
  }
  const span = spanForLines(table, best.start, patternLines);
  const matched = content.slice(span.index, span.index + span.length);
  return { ...span, mode: "fuzzy", newTextRebaser: (text) => rebaseIndent(text, oldText, matched) };
}

/** Locate every edit against one immutable snapshot, reject ambiguity/overlap, then apply bottom-up. */
export function applySmartEdits(content, edits, path) {
  const normalizedEdits = edits.map((edit) => ({
    oldText: edit.oldText.replace(/\r\n/g, "\n"),
    newText: edit.newText.replace(/\r\n/g, "\n"),
  }));
  const matches = normalizedEdits.map((edit, index) => ({
    ...locateEdit(content, edit.oldText, path, index, normalizedEdits.length),
    editIndex: index,
    newText: edit.newText,
  })).sort((left, right) => left.index - right.index);
  for (let index = 1; index < matches.length; index += 1) {
    const previous = matches[index - 1];
    const current = matches[index];
    if (previous.index + previous.length > current.index) {
      throw new Error(`edits[${previous.editIndex}] and edits[${current.editIndex}] overlap in ${path}.`);
    }
  }
  let output = content;
  for (const match of matches.toReversed()) {
    const replacement = match.newTextRebaser(match.newText);
    output = output.slice(0, match.index) + replacement + output.slice(match.index + match.length);
  }
  if (output === content) throw new Error(`No changes made to ${path}.`);
  return {
    content: output,
    matches: matches.map(({ editIndex, mode, line }) => ({
      editIndex,
      mode,
      line: line === undefined ? undefined : line + 1,
    })),
  };
}
