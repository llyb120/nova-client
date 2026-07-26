// markdown.js - minimal markdown -> Node tree parser.
// Supports: headings, paragraphs, bold, italic, inline code, code blocks,
// blockquotes, unordered/ordered lists, horizontal rules, links, line breaks.

import { Node, h } from './Node.js';

const CODE_BG = '#f4f4f4';
const CODE_COLOR = '#c7254e';
const QUOTE_BG = '#f7f7f7';
const QUOTE_BORDER = '#dfe2e5';
const LINK_COLOR = '#4a90d9';
const HR_COLOR = '#e0e0e0';

// Base text style inherited by paragraphs.
const BASE = {
  fontSize: 14,
  fontFamily: 'sans-serif',
  lineHeight: 1.6,
  color: '#222'
};

// Parse markdown text into a Node tree (a 'div' container with block children).
export function parseMarkdown(md, baseStyle = {}) {
  const root = new Node('div', Object.assign({ width: '100%' }, baseStyle));
  const lines = String(md).replace(/\r\n/g, '\n').split('\n');
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];

    // fenced code block
    const fence = line.match(/^```(\w*)\s*$/);
    if (fence) {
      const buf = [];
      i++;
      while (i < lines.length && !/^```\s*$/.test(lines[i])) { buf.push(lines[i]); i++; }
      i++; // skip closing fence
      root.appendChild(h('pre', {
        background: CODE_BG, borderRadius: 6, padding: 12, margin: [8, 0],
        fontFamily: 'monospace', fontSize: 13, color: '#333', whiteSpace: 'pre',
        border: 1, borderColor: '#e0e0e0'
      }, buf.join('\n'), []));
      continue;
    }

    // horizontal rule
    if (/^\s*([-*_])\1{2,}\s*$/.test(line)) {
      root.appendChild(h('hr', { height: 1, background: HR_COLOR, margin: [12, 0] }, '', []));
      i++;
      continue;
    }

    // heading
    const hm = line.match(/^(#{1,6})\s+(.*)$/);
    if (hm) {
      const level = hm[1].length;
      const sizes = [24, 20, 18, 16, 15, 14];
      const para = h('h' + level, {
        fontSize: sizes[level - 1], fontWeight: 'bold',
        margin: [level <= 2 ? 16 : 12, 0, 6, 0], color: '#111',
        lineHeight: 1.3
      }, '', []);
      parseInlineInto(hm[2], para, BASE);
      root.appendChild(para);
      i++;
      continue;
    }

    // blockquote
    if (/^>\s?/.test(line)) {
      const buf = [];
      while (i < lines.length && /^>\s?/.test(lines[i])) {
        buf.push(lines[i].replace(/^>\s?/, ''));
        i++;
      }
      const quote = h('blockquote', {
        background: QUOTE_BG, border: [0, 0, 0, 3], borderColor: QUOTE_BORDER,
        padding: [8, 12], margin: [8, 0], color: '#555'
      }, '', []);
      const inner = parseMarkdown(buf.join('\n'), {});
      for (const c of [...inner.children]) quote.appendChild(c);
      root.appendChild(quote);
      continue;
    }

    // unordered list
    if (/^\s*[-*+]\s+/.test(line)) {
      const ul = h('ul', { margin: [8, 0], padding: [0, 0, 0, 20] }, '', []);
      while (i < lines.length && /^\s*[-*+]\s+/.test(lines[i])) {
        const itemText = lines[i].replace(/^\s*[-*+]\s+/, '');
        const li = h('li', { margin: [2, 0], display: 'block' }, '', []);
        li.appendChild(h('span', { display: 'inline' }, '• ', []));
        parseInlineInto(itemText, li, BASE);
        ul.appendChild(li);
        i++;
      }
      root.appendChild(ul);
      continue;
    }

    // ordered list
    if (/^\s*\d+\.\s+/.test(line)) {
      const ol = h('ol', { margin: [8, 0], padding: [0, 0, 0, 24] }, '', []);
      let n = 1;
      while (i < lines.length && /^\s*\d+\.\s+/.test(lines[i])) {
        const itemText = lines[i].replace(/^\s*\d+\.\s+/, '');
        const li = h('li', { margin: [2, 0], display: 'block' }, '', []);
        // prefix number as inline text
        li.appendChild(h('span', { display: 'inline', color: '#888', width: 18 }, `${n}. `, []));
        parseInlineInto(itemText, li, BASE, true);
        ol.appendChild(li);
        n++;
        i++;
      }
      root.appendChild(ol);
      continue;
    }

    // blank line -> paragraph separator
    if (/^\s*$/.test(line)) { i++; continue; }

    // paragraph: gather consecutive non-empty, non-special lines
    const buf = [line];
    i++;
    while (i < lines.length) {
      const l = lines[i];
      if (/^\s*$/.test(l)) break;
      if (/^(#{1,6})\s+/.test(l)) break;
      if (/^```/.test(l)) break;
      if (/^>\s?/.test(l)) break;
      if (/^\s*[-*+]\s+/.test(l)) break;
      if (/^\s*\d+\.\s+/.test(l)) break;
      if (/^\s*([-*_])\1{2,}\s*$/.test(l)) break;
      buf.push(l);
      i++;
    }
    const para = h('p', { margin: [6, 0], display: 'block' }, '', []);
    parseInlineInto(buf.join('\n'), para, BASE);
    root.appendChild(para);
  }
  return root;
}

// Parse inline markdown (bold/italic/code/link) into inline children of `into`.
// If `appendMode` is true, the node already contains a leading number span;
// we just append more inline children.
function parseInlineInto(text, into, base, appendMode = false) {
  const tokens = tokenizeInline(text);
  for (const t of tokens) {
    if (t.type === 'text') {
      // split on newlines -> line break nodes between text spans
      const parts = t.value.split('\n');
      for (let k = 0; k < parts.length; k++) {
        if (k > 0) {
          into.appendChild(h('br', { display: 'inline' }, '', []));
        }
        if (parts[k]) {
          into.appendChild(h('span', Object.assign({ display: 'inline' }, base), parts[k], []));
        }
      }
    } else if (t.type === 'bold') {
      into.appendChild(h('strong', Object.assign({ display: 'inline', fontWeight: 'bold' }, base), t.value, []));
    } else if (t.type === 'italic') {
      into.appendChild(h('em', Object.assign({ display: 'inline', fontStyle: 'italic' }, base), t.value, []));
    } else if (t.type === 'code') {
      into.appendChild(h('code', {
        display: 'inline', fontFamily: 'monospace', fontSize: 13,
        background: CODE_BG, color: CODE_COLOR, padding: [1, 4], borderRadius: 3
      }, t.value, []));
    } else if (t.type === 'link') {
      into.appendChild(h('a', Object.assign({}, base, { display: 'inline', color: LINK_COLOR, textDecoration: 'underline', href: t.href }), t.value, []));
    }
  }
  if (into.children.length === 0) {
    // ensure node has measurable text content fallback (empty span)
    into.appendChild(h('span', Object.assign({ display: 'inline' }, base), '', []));
  }
}

// Tokenize inline markdown. Handles **bold**, *italic*, `code`, [text](url).
function tokenizeInline(text) {
  const tokens = [];
  let i = 0;
  let buf = '';
  const flush = () => { if (buf) { tokens.push({ type: 'text', value: buf }); buf = ''; } };
  while (i < text.length) {
    const rest = text.slice(i);
    // code
    let m = rest.match(/^`([^`]+)`/);
    if (m) { flush(); tokens.push({ type: 'code', value: m[1] }); i += m[0].length; continue; }
    // bold
    m = rest.match(/^\*\*([^*]+)\*\*/);
    if (m) { flush(); tokens.push({ type: 'bold', value: m[1] }); i += m[0].length; continue; }
    // italic
    m = rest.match(/^\*([^*]+)\*/);
    if (m) { flush(); tokens.push({ type: 'italic', value: m[1] }); i += m[0].length; continue; }
    // link [text](url)
    m = rest.match(/^\[([^\]]+)\]\(([^)\s]+)\)/);
    if (m) { flush(); tokens.push({ type: 'link', value: m[1], href: m[2] }); i += m[0].length; continue; }
    buf += text[i];
    i++;
  }
  flush();
  return tokens;
}
