import { readdir, readFile } from "node:fs/promises";
import { basename, join } from "node:path";

const DEFAULT_MAX_FILES = 240;
const DEFAULT_SNIPPET_CHARS = 1_600;

function tokenize(value) {
  const text = String(value ?? "").toLowerCase();
  const words = text.match(/[a-z0-9_./:-]+|[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Hangul}]/gu) ?? [];
  return [...new Set(words.filter((token) => token.length > 0))];
}

function memoryText(parsed) {
  const sections = [];
  for (const prompt of parsed?.preservedUserPrompts ?? []) sections.push(`User: ${prompt}`);
  for (const digest of parsed?.digests ?? []) sections.push(`Digest: ${digest}`);
  if (parsed?.summary) sections.push(`Digest: ${parsed.summary}`);
  for (const turn of parsed?.turns ?? []) {
    const prompts = Array.isArray(turn?.userPrompts) ? turn.userPrompts : [turn?.userPrompt];
    for (const prompt of prompts.filter(Boolean)) sections.push(`User: ${prompt}`);
    if (turn?.conclusion) sections.push(`Assistant: ${turn.conclusion}`);
  }
  return sections.join("\n\n");
}

function snippetAround(text, terms, maxChars) {
  const lower = text.toLowerCase();
  const positions = terms.map((term) => lower.indexOf(term)).filter((index) => index >= 0);
  const center = positions.length ? Math.min(...positions) : 0;
  const start = Math.max(0, center - Math.floor(maxChars * 0.25));
  const end = Math.min(text.length, start + maxChars);
  return `${start > 0 ? "…" : ""}${text.slice(start, end)}${end < text.length ? "…" : ""}`;
}

/** Bounded local BM25-style search over Vega and Cursor canonical slim-memory files. */
export async function searchSessionHistory(roots, query, options = {}) {
  const terms = tokenize(query);
  if (!terms.length) return [];
  const maxFiles = options.maxFiles ?? DEFAULT_MAX_FILES;
  const candidates = [];
  for (const root of roots ?? []) {
    const names = await readdir(root).catch(() => []);
    const bySession = new Map();
    for (const name of names.filter((value) => value.endsWith(".slim.json") || value.endsWith(".json"))) {
      const sessionId = name.replace(/\.slim\.json$|\.json$/g, "");
      if (sessionId === options.currentSessionId) continue;
      const existing = bySession.get(sessionId);
      if (!existing || name.endsWith(".slim.json")) bySession.set(sessionId, name);
    }
    for (const [sessionId, name] of bySession) {
      candidates.push({ root, name, sessionId });
      if (candidates.length >= maxFiles) break;
    }
    if (candidates.length >= maxFiles) break;
  }

  const documents = (await Promise.all(candidates.map(async (candidate) => {
    try {
      const parsed = JSON.parse(await readFile(join(candidate.root, candidate.name), "utf8"));
      const text = memoryText(parsed);
      return text ? { ...candidate, text, tokens: tokenize(text) } : null;
    } catch {
      return null;
    }
  }))).filter(Boolean);
  if (!documents.length) return [];

  const documentFrequency = new Map();
  for (const document of documents) {
    for (const token of document.tokens) documentFrequency.set(token, (documentFrequency.get(token) ?? 0) + 1);
  }
  const averageLength = documents.reduce((sum, document) => sum + document.tokens.length, 0) / documents.length || 1;
  const scored = documents.map((document) => {
    const tokenSet = new Set(document.tokens);
    let score = 0;
    for (const term of terms) {
      if (!tokenSet.has(term)) continue;
      const frequency = documentFrequency.get(term) ?? 0;
      const idf = Math.log(1 + (documents.length - frequency + 0.5) / (frequency + 0.5));
      const lengthPenalty = 1.2 * (0.25 + 0.75 * document.tokens.length / averageLength);
      score += idf * 2.2 / (1 + lengthPenalty);
    }
    return { ...document, score };
  }).filter((document) => document.score > 0);

  return scored
    .sort((left, right) => right.score - left.score || left.sessionId.localeCompare(right.sessionId))
    .slice(0, Math.max(1, Math.min(Number(options.limit) || 5, 10)))
    .map((document) => ({
      sessionId: document.sessionId,
      source: basename(document.root),
      score: Number(document.score.toFixed(4)),
      snippet: snippetAround(document.text, terms, options.snippetChars ?? DEFAULT_SNIPPET_CHARS),
    }));
}
