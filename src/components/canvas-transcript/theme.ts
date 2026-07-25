import type { MarkdownTheme } from "../../lib/canvas-dom";

export interface TranscriptTheme {
  bg: string;
  panel: string;
  hover: string;
  text: string;
  textDim: string;
  textMuted: string;
  textFaint: string;
  accent: string;
  accentDim: string;
  border: string;
  borderLight: string;
  red: string;
  yellow: string;
  green: string;
  mono: string;
  sans: string;
  radiusLg: number;
  radiusSm: number;
  markdown: MarkdownTheme;
}

function readCssVar(style: CSSStyleDeclaration, name: string, fallback: string): string {
  const v = style.getPropertyValue(name).trim();
  return v || fallback;
}

function parsePx(value: string, fallback: number): number {
  const n = parseFloat(value);
  return Number.isFinite(n) ? n : fallback;
}

/** Read live theme tokens from document (supports ink-dark / ink-light). */
export function readTranscriptTheme(el: Element = document.documentElement): TranscriptTheme {
  const s = getComputedStyle(el);
  const text = readCssVar(s, "--text", "#e4e7ec");
  const accent = readCssVar(s, "--accent", "#6e93f8");
  const panel = readCssVar(s, "--bg-panel", "#171b23");
  const border = readCssVar(s, "--border", "#252a33");
  const borderLight = readCssVar(s, "--border-light", "#313844");
  return {
    bg: readCssVar(s, "--bg", "#0e1014"),
    panel,
    hover: readCssVar(s, "--bg-hover", "#1e232d"),
    text,
    textDim: readCssVar(s, "--text-dim", "#a9b0bd"),
    textMuted: readCssVar(s, "--text-muted", "#7d8593"),
    textFaint: readCssVar(s, "--text-faint", "#5d6470"),
    accent,
    accentDim: readCssVar(s, "--accent-dim", "rgba(110, 147, 248, 0.14)"),
    border,
    borderLight,
    red: readCssVar(s, "--red", "#e07d76"),
    yellow: readCssVar(s, "--yellow", "#d4b26e"),
    green: readCssVar(s, "--green", "#8ec489"),
    mono: readCssVar(s, "--mono", "monospace"),
    sans: readCssVar(s, "--sans", "sans-serif"),
    radiusLg: parsePx(readCssVar(s, "--r-lg", "14px"), 14),
    radiusSm: parsePx(readCssVar(s, "--r-sm", "6px"), 6),
    markdown: {
      textColor: text,
      linkColor: accent,
      codeBg: panel,
      codeColor: readCssVar(s, "--seal", "#d19a66"),
      quoteBg: panel,
      quoteBorder: borderLight,
      hrColor: border,
    },
  };
}
