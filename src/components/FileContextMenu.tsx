import { message } from "@tauri-apps/plugin-dialog";
import { createSignal, onCleanup, Show } from "solid-js";
import { api } from "../ipc";
import { state } from "../store";
import { IconCopy, IconFolder } from "./icons";

type FileMenu = { x: number; y: number; path: string };

function isAbsolutePath(path: string) {
  return /^[a-zA-Z]:[\\/]/.test(path) || /^\\\\/.test(path) || path.startsWith("/");
}

export function absolutePath(path: string) {
  if (!state.cwd || isAbsolutePath(path)) return path;
  return `${state.cwd.replace(/[\\/]+$/, "")}\\${path.replace(/^[\\/]+/, "")}`;
}

export function createFileContextMenu() {
  const [menu, setMenu] = createSignal<FileMenu | null>(null);
  const closeMenu = () => setMenu(null);
  const onDocDown = (e: MouseEvent) => {
    if (!(e.target as HTMLElement).closest(".ctx-menu")) closeMenu();
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") closeMenu();
  };
  document.addEventListener("mousedown", onDocDown);
  document.addEventListener("keydown", onKey);
  onCleanup(() => {
    document.removeEventListener("mousedown", onDocDown);
    document.removeEventListener("keydown", onKey);
  });

  const open = (e: MouseEvent, path: string) => {
    e.preventDefault();
    e.stopPropagation();
    setMenu({
      x: Math.min(e.clientX, window.innerWidth - 190),
      y: Math.max(0, Math.min(e.clientY, window.innerHeight - 84)),
      path: absolutePath(path),
    });
  };

  const Menu = () => (
    <Show when={menu()}>
      <div class="ctx-menu" style={{ left: `${menu()!.x}px`, top: `${menu()!.y}px` }}>
        <button
          class="ctx-item"
          onClick={() => {
            const path = menu()!.path;
            closeMenu();
            void api.openInExplorer(path).catch((e) => void message(String(e), { kind: "error" }));
          }}
        >
          <IconFolder size={13} />
          打开所在目录
        </button>
        <button
          class="ctx-item"
          onClick={() => {
            const path = menu()!.path;
            closeMenu();
            void navigator.clipboard.writeText(path).catch((e) => void message(String(e), { kind: "error" }));
          }}
        >
          <IconCopy size={13} />
          复制文件地址
        </button>
      </div>
    </Show>
  );

  return { open, Menu };
}
