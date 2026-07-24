/** Matches `.slash-menu { bottom: calc(100% + 8px) }` */
const MENU_GAP = 8;
const VIEWPORT_PAD = 8;
const MIN_HEIGHT = 120;

/** Cap slash / prompt-history menus so upward popovers stay inside the viewport. */
export function fitSlashMenuHeight(
  menu: HTMLElement | undefined,
  options?: { maxHeight?: number },
) {
  if (!menu) return;
  const host = menu.closest(".composer, .home-composer");
  if (!(host instanceof HTMLElement)) return;
  const available = host.getBoundingClientRect().top - MENU_GAP - VIEWPORT_PAD;
  const compact = window.matchMedia("(max-width: 680px), (max-height: 620px)").matches;
  const fallbackCap = options?.maxHeight ?? (compact ? 280 : 360);
  const next = Math.max(MIN_HEIGHT, Math.min(fallbackCap, available));
  menu.style.maxHeight = `${Math.floor(next)}px`;
}
