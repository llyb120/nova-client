import type { JSX } from "solid-js";

type P = { size?: number; class?: string };

function svg(d: JSX.Element, p: P, viewBox = "0 0 24 24") {
  return (
    <svg
      width={p.size ?? 16}
      height={p.size ?? 16}
      viewBox={viewBox}
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      class={p.class}
    >
      {d}
    </svg>
  );
}

export const IconPlus = (p: P) => svg(<path d="M12 5v14M5 12h14" />, p);
export const IconFolder = (p: P) =>
  svg(<path d="M4 20h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13c0 1.1.9 2 2 2Z" />, p);
export const IconGear = (p: P) =>
  svg(
    <>
      <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" />
      <circle cx="12" cy="12" r="3" />
    </>,
    p,
  );
export const IconStop = (p: P) => svg(<rect x="6" y="6" width="12" height="12" rx="2" fill="currentColor" stroke="none" />, p);
export const IconSend = (p: P) => svg(<path d="m5 12 7-9 7 9M12 3v18" />, p);
export const IconTrash = (p: P) =>
  svg(<><path d="M3 6h18" /><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" /><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" /></>, p);
export const IconChevron = (p: P & { open?: boolean }) =>
  svg(<path d={p.open ? "m6 9 6 6 6-6" : "m9 6 6 6-6 6"} />, p);
export const IconTerminal = (p: P) => svg(<><path d="m4 17 6-6-6-6" /><path d="M12 19h8" /></>, p);
export const IconFile = (p: P) =>
  svg(<><path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" /><path d="M14 2v4a2 2 0 0 0 2 2h4" /></>, p);
export const IconPencil = (p: P) =>
  svg(<><path d="M21.17 6.83a2.83 2.83 0 0 0-4-4L3 17v4h4Z" /><path d="m15 5 4 4" /></>, p);
export const IconSearch = (p: P) => svg(<><circle cx="11" cy="11" r="8" /><path d="m21 21-4.3-4.3" /></>, p);
export const IconGlobe = (p: P) =>
  svg(<><circle cx="12" cy="12" r="10" /><path d="M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20M2 12h20" /></>, p);
export const IconBrain = (p: P) =>
  svg(<><path d="M12 5a3 3 0 1 0-5.997.125 4 4 0 0 0-2.526 5.77 4 4 0 0 0 .556 6.588A4 4 0 1 0 12 18Z" /><path d="M12 5a3 3 0 1 1 5.997.125 4 4 0 0 1 2.526 5.77 4 4 0 0 1-.556 6.588A4 4 0 1 1 12 18Z" /></>, p);
export const IconWrench = (p: P) =>
  svg(<path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z" />, p);
export const IconMove = (p: P) =>
  svg(<><path d="M5 9l-3 3 3 3M9 5l3-3 3 3M15 19l-3 3-3-3M19 9l3 3-3 3M2 12h20M12 2v20" /></>, p);
export const IconCheck = (p: P) => svg(<path d="M20 6 9 17l-5-5" />, p);
export const IconThumbUp = (p: P) =>
  svg(<><path d="M7 10v12H3V10h4Z" /><path d="M7 20h10.2a2 2 0 0 0 1.94-1.52l1.5-6A2 2 0 0 0 18.7 10H14l.7-4.2A3.25 3.25 0 0 0 11.5 2L7 10Z" /></>, p);
export const IconThumbDown = (p: P) =>
  svg(<><path d="M7 14V2H3v12h4Z" /><path d="M7 4h10.2a2 2 0 0 1 1.94 1.52l1.5 6A2 2 0 0 1 18.7 14H14l.7 4.2A3.25 3.25 0 0 1 11.5 22L7 14Z" /></>, p);
export const IconMerge = (p: P) =>
  svg(
    <>
      <circle cx="6" cy="6" r="2.5" />
      <circle cx="6" cy="18" r="2.5" />
      <circle cx="18" cy="12" r="2.5" />
      <path d="M6 8.5v7M8 7c3 1 5 3 7.5 4.4M8 17c3-1 5-3 7.5-4.4" />
    </>,
    p,
  );
export const IconClue = (p: P) =>
  svg(
    <>
      <circle cx="6" cy="6" r="2.5" />
      <circle cx="18" cy="12" r="2.5" />
      <circle cx="6" cy="18" r="2.5" />
      <path d="M8.5 6c4.2 0 4.5 4.4 7 5.5M8.5 18c4.2 0 4.5-4.4 7-5.5" />
    </>,
    p,
  );
export const IconEye = (p: P) =>
  svg(<><path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7-10-7-10-7Z" /><circle cx="12" cy="12" r="3" /></>, p);
export const IconCopy = (p: P) =>
  svg(<><rect x="9" y="9" width="13" height="13" rx="2" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" /></>, p);
export const IconUndo = (p: P) =>
  svg(<><path d="M3 7v6h6" /><path d="M21 17a9 9 0 0 0-9-9 9 9 0 0 0-6.7 3L3 13" /></>, p);
export const IconX = (p: P) => svg(<path d="M18 6 6 18M6 6l12 12" />, p);
export const IconShare = (p: P) =>
  svg(<><circle cx="18" cy="5" r="3" /><circle cx="6" cy="12" r="3" /><circle cx="18" cy="19" r="3" /><path d="m8.6 13.5 6.8 4M15.4 6.5l-6.8 4" /></>, p);
export const IconBell = (p: P) =>
  svg(<><path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" /><path d="M10.3 21a1.94 1.94 0 0 0 3.4 0" /></>, p);
export const IconDownload = (p: P) =>
  svg(<><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" /><path d="M7 10l5 5 5-5" /><path d="M12 15V3" /></>, p);
export const IconUsers = (p: P) =>
  svg(<><path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2" /><circle cx="9" cy="7" r="4" /><path d="M22 21v-2a4 4 0 0 0-3-3.87M16 3.13A4 4 0 0 1 16 11" /></>, p);
export const IconBroadcast = (p: P) =>
  svg(<><circle cx="12" cy="12" r="2" /><path d="M16.24 7.76a6 6 0 0 1 0 8.49M7.76 16.24a6 6 0 0 1 0-8.49M19.07 4.93a10 10 0 0 1 0 14.14M4.93 19.07a10 10 0 0 1 0-14.14" /></>, p);
// Nova：一枚四芒星（新星/闪耀），实心朱砂阴文，配一点小火花，寓意「新星」。
export const IconLogo = (p: P) =>
  svg(
    <>
      <path
        d="M12 2c.9 5.2 1.2 6.1 2.6 7.4 1.3 1.4 2.2 1.7 7.4 2.6-5.2.9-6.1 1.2-7.4 2.6-1.4 1.3-1.7 2.2-2.6 7.4-.9-5.2-1.2-6.1-2.6-7.4C8.1 12.8 7.2 12.5 2 12c5.2-.9 6.1-1.2 7.4-2.6C10.8 8.1 11.1 7.2 12 2Z"
        fill="currentColor"
        stroke="none"
      />
      <circle cx="18.7" cy="5.3" r="1.15" fill="currentColor" stroke="none" opacity="0.85" />
    </>,
    p,
  );
export const IconCompress = (p: P) =>
  svg(<path d="M8 3v3a2 2 0 0 1-2 2H3M21 8h-3a2 2 0 0 1-2-2V3M3 16h3a2 2 0 0 1 2 2v3M16 21v-3a2 2 0 0 1 2-2h3" />, p);

export function toolIcon(kind: string, size = 14) {
  switch (kind) {
    case "read":
      return <IconFile size={size} />;
    case "edit":
      return <IconPencil size={size} />;
    case "delete":
      return <IconTrash size={size} />;
    case "move":
      return <IconMove size={size} />;
    case "search":
      return <IconSearch size={size} />;
    case "execute":
      return <IconTerminal size={size} />;
    case "think":
      return <IconBrain size={size} />;
    case "fetch":
      return <IconGlobe size={size} />;
    default:
      return <IconWrench size={size} />;
  }
}
