// @ts-nocheck
// markdown.js - minimal markdown -> Node tree parser.
// Supports: headings, paragraphs, bold, italic, inline code, code blocks,
// blockquotes, unordered/ordered lists, horizontal rules, links, line breaks.

import { Node, h } from './Node';

const DEFAULT_THEME = {
  codeBg: '#f4f4f4',
  codeColor: '#c7254e',
  quoteBg: '#f7f7f7',
  quoteBorder: '#dfe2e5',
  linkColor: '#4a90d9',
  hrColor: '#e0e0e0',
  textColor: '#222',
};

function resolveTheme(baseStyle = {}, theme = {}) {
  const t = Object.assign({}, DEFAULT_THEME, theme);
  const base = Object.assign(
    {
      fontSize: 14,
      fontFamily: 'sans-serif',
      lineHeight: 1.6,
      color: t.textColor,
    },
    baseStyle,
  );
  if (baseStyle.color == null && baseStyle.textColor != null) {
    base.color = baseStyle.textColor;
  }
  return { theme: t, base };
}

// Parse markdown text into a Node tree (a 'div' container with block children).
export function parseMarkdown(md, baseStyle = {}, theme = {}) {
  const { theme: t, base: BASE } = resolveTheme(baseStyle, theme);
  const root = new Node('div', Object.assign({ width: '100%' }, BASE));
  const lines = String(md).replace(/\r\n/g, '\n').split('\n');
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];

    const fence = line.match(/^```(\w*)\s*$/);
    if (fence) {
      const buf = [];
      i++;
      while (i < lines.length && !/^```\s*$/.test(lines[i])) { buf.push(lines[i]); i++; }
      i++;
      root.appendChild(h('pre', {
        background: t.codeBg, borderRadius: 6, padding: 12, margin: [8, 0],
        fontFamily: 'monospace', fontSize: 13, color: t.textColor, whiteSpace: 'pre',
        border: 1, borderColor: t.hrColor
      }, buf.join('\n'), []));
      continue;
    }

    if (/^\s*([-*_])\1{2,}\s*$/.test(line)) {
      root.appendChild(h('hr', { height: 1, background: t.hrColor, margin: [12, 0] }, '', []));
      i++;
      continue;
    }

    const hm = line.match(/^(#{1,6})\s+(.*)$/);
    if (hm) {
      const level = hm[1].length;
      const sizes = [24, 20, 18, 16, 15, 14];
      const para = h('h' + level, {
        fontSize: sizes[level - 1], fontWeight: 'bold',
        margin: [level <= 2 ? 16 : 12, 0, 6, 0], color: BASE.color,
        lineHeight: 1.3
      }, '', []);
      parseInlineInto(hm[2], para, BASE, false, t);
      root.appendChild(para);
      i++;
      continue;
    }

    if (/^>\s?/.test(line)) {
      const buf = [];
      while (i < lines.length && /^>\s?/.test(lines[i])) {
        buf.push(lines[i].replace(/^>\s?/, ''));
        i++;
      }
      const quote = h('blockquote', {
        background: t.quoteBg, border: [0, 0, 0, 3], borderColor: t.quoteBorder,
        padding: [8, 12], margin: [8, 0], color: BASE.color
      }, '', []);
      const inner = parseMarkdown(buf.join('\n'), BASE, t);
      for (const c of inner.children) quote.appendChild(c);
      root.appendChild(quote);
      continue;
    }

    if (/^\s*[-*+]\s+/.test(line)) {
      const ul = h('ul', { margin: [8, 0], padding: [0, 0, 0, 20] }, '', []);
      while (i < lines.length && /^\s*[-*+]\s+/.test(lines[i])) {
        const itemText = lines[i].replace(/^\s*[-*+]\s+/, '');
        const li = h('li', { margin: [2, 0], display: 'block' }, '', []);
        parseInlineInto(itemText, li, BASE, false, t);
        ul.appendChild(li);
        i++;
      }
      root.appendChild(ul);
      continue;
    }

    if (/^\s*\d+\.\s+/.test(line)) {
      const ol = h('ol', { margin: [8, 0], padding: [0, 0, 0, 24] }, '', []);
      let n = 1;
      while (i < lines.length && /^\s*\d+\.\s+/.test(lines[i])) {
        const itemText = lines[i].replace(/^\s*\d+\.\s+/, '');
        const li = h('li', { margin: [2, 0], display: 'block' }, '', []);
        li.appendChild(h('span', { display: 'inline', color: '#888', width: 18 }, `${n}. `, []));
        parseInlineInto(itemText, li, BASE, true, t);
        ol.appendChild(li);
        n++;
        i++;
      }
      root.appendChild(ol);
      continue;
    }

    if (/^\s*$/.test(line)) { i++; continue; }

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
    parseInlineInto(buf.join('\n'), para, BASE, false, t);
    root.appendChild(para);
  }
  return root;
}

function parseInlineInto(text, into, base, appendMode = false, theme = DEFAULT_THEME) {
  const tokens = tokenizeInline(text);
  for (const t of tokens) {
    if (t.type === 'text') {
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
        background: theme.codeBg, color: theme.codeColor, padding: [1, 4], borderRadius: 3
      }, t.value, []));
    } else if (t.type === 'link') {
      into.appendChild(h('a', Object.assign({}, base, {
        display: 'inline', color: theme.linkColor, textDecoration: 'underline', href: t.href
      }), t.value, []));
    }
  }
  if (into.children.length === 0) {
    into.appendChild(h('span', Object.assign({ display: 'inline' }, base), '', []));
  }
}

function tokenizeInline(text) {
  const tokens = [];
  let i = 0;
  let buf = '';
  const flush = () => { if (buf) { tokens.push({ type: 'text', value: buf }); buf = ''; } };
  while (i < text.length) {
    const rest = text.slice(i);
    let m = rest.match(/^`([^`]+)`/);
    if (m) { flush(); tokens.push({ type: 'code', value: m[1] }); i += m[0].length; continue; }
    m = rest.match(/^\*\*([^*]+)\*\*/);
    if (m) { flush(); tokens.push({ type: 'bold', value: m[1] }); i += m[0].length; continue; }
    m = rest.match(/^\*([^*]+)\*/);
    if (m) { flush(); tokens.push({ type: 'italic', value: m[1] }); i += m[0].length; continue; }
    m = rest.match(/^\[([^\]]+)\]\(([^)\s]+)\)/);
    if (m) { flush(); tokens.push({ type: 'link', value: m[1], href: m[2] }); i += m[0].length; continue; }
    buf += text[i];
    i++;
  }
  flush();
  return tokens;
}

/** Find byte index where the stable markdown prefix ends (blank line or closed fence). */
export function findStableMarkdownPrefixEnd(md) {
  const text = String(md);
  const lines = text.split('\n');
  let inFence = false;
  let lastStableEnd = 0;
  let pos = 0;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const trimmed = line.trim();
    if (/^```/.test(trimmed)) {
      inFence = !inFence;
      if (!inFence) {
        lastStableEnd = pos + line.length + (i < lines.length - 1 ? 1 : 0);
      }
    } else if (!inFence && /^\s*$/.test(line) && i > 0) {
      lastStableEnd = pos + line.length + (i < lines.length - 1 ? 1 : 0);
    }
    pos += line.length + (i < lines.length - 1 ? 1 : 0);
  }
  return lastStableEnd;
}
