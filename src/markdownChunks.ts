import type { Marked, Token, Tokens, TokensList } from "marked";

export const LONG_MARKDOWN_THRESHOLD = 32_768;
const CHUNK_CHARS = 2048;
const CHUNK_BLOCKS = 64;

export interface MarkdownChunk {
  source: string;
  tokens: Token[];
  start: number;
  end: number;
}
export interface MarkdownChunks {
  source: string;
  chunks: MarkdownChunk[];
  links: TokensList["links"];
  html: boolean;
}

/** Lex once, then re-lex only the unfinished last chunk on append. References are
 * resolved by the lexer for the whole document, never by parsing isolated strings. */
export function chunkMarkdown(marked: Marked, source: string, previous?: MarkdownChunks): MarkdownChunks {
  source = source.replace(/\r\n?/g, "\n");
  if (source === previous?.source) return previous;
  const append = previous && !previous.html && source.startsWith(previous.source);
  const prefix = append ? previous.chunks.slice(0, -1) : [];
  const start = prefix.at(-1)?.end ?? 0;
  const lexer = new marked.Lexer(marked.defaults);
  if (append) {
    Object.assign(lexer.tokens.links, previous.links);
    // Definitions in the unfinished chunk must be lexed again: marked uses
    // first-definition-wins, so seeding a partial URL would freeze it forever.
    const unfinished = new Set<string>();
    marked.walkTokens(previous.chunks.at(-1)?.tokens ?? [], token => {
      if (token.type === "def") { unfinished.add(token.tag); delete lexer.tokens.links[token.tag]; }
    });
    if (unfinished.size) for (const chunk of prefix) marked.walkTokens(chunk.tokens, token => {
      if (token.type === "def" && unfinished.has(token.tag) && !lexer.tokens.links[token.tag]) {
        lexer.tokens.links[token.tag] = { href: token.href, title: token.title };
      }
    });
  }
  const tokens = lexer.lex(source.slice(start));
  // A later definition can resolve an earlier literal [reference]. Re-lex once
  // when the definition map changes, not on every streaming delta.
  if (start && JSON.stringify(tokens.links) !== JSON.stringify(previous!.links)) return chunkMarkdown(marked, source);
  const html = tokens.some(token => token.type === "html");
  // ponytail: raw block HTML may open a container across Markdown tokens. Keep
  // that document indivisible; a balanced-HTML splitter can lift this ceiling.
  if (html && start) return chunkMarkdown(marked, source);
  const chunks = prefix;
  let offset = start, current: MarkdownChunk | undefined, blocks = 0;
  for (const token of tokens) {
    if (!current || (!html && token.type !== "space" && token.type !== "def" &&
      (current.source.length >= CHUNK_CHARS || blocks >= CHUNK_BLOCKS))) {
      current = { source: "", tokens: [], start: offset, end: offset };
      chunks.push(current);
      blocks = 0;
    }
    current.tokens.push(token);
    current.source += token.raw;
    offset += token.raw.length;
    current.end = offset;
    if (token.type !== "space" && token.type !== "def") blocks++;
  }
  return { source, chunks, links: tokens.links, html };
}

/** Plain inline text for source search (including formatting-spanning matches). */
export function markdownTokenText(tokens: Token[]): string {
  return tokens.map(token => {
    if (token.type === "space" || token.type === "def" || token.type === "br") return "";
    // Code entities are literal text; prose entities are decoded by the caller.
    if (token.type === "code" || token.type === "codespan") return token.text.replace(/&/g, "&amp;");
    if (token.type === "html") return token.text.replace(/<[^>]*>/g, "");
    if (token.type === "list") return (token as Tokens.List).items.map(item => markdownTokenText(item.tokens)).join("\n");
    if (token.type === "table") { const table = token as Tokens.Table; return [table.header, ...table.rows].map(row => row.map(cell => markdownTokenText(cell.tokens)).join("\t")).join("\n"); }
    if ("tokens" in token && token.tokens) return markdownTokenText(token.tokens);
    return "text" in token ? token.text : "";
  }).join("");
}
