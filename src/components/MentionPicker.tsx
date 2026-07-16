import { createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { Portal } from "solid-js/web";
import type { Peer } from "../types";

export function MentionPicker(props: {
  peers: Peer[];
  selectedTokens: string[];
  disabled?: boolean;
  onChange: (tokens: string[]) => void;
  placeholder?: string;
}) {
  const [query, setQuery] = createSignal("");
  const [opened, setOpened] = createSignal(false);
  const [menuRect, setMenuRect] = createSignal({ left: 0, top: 0, width: 0 });
  let rootRef: HTMLDivElement | undefined;
  let inputRef: HTMLInputElement | undefined;

  const selected = createMemo(() => {
    const peersByToken = new Map(props.peers.map((peer) => [peer.token, peer]));
    return [...new Set(props.selectedTokens)].map((token) => ({
      token,
      name: peersByToken.get(token)?.name || "未知成员",
    }));
  });

  const selectedSet = createMemo(() => new Set(props.selectedTokens));

  const candidates = createMemo(() => {
    const peersByToken = new Map<string, Peer>();
    for (const peer of props.peers) {
      if (peer.token && !peersByToken.has(peer.token)) peersByToken.set(peer.token, peer);
    }
    const needle = query().trim().replace(/^@+/, "").toLocaleLowerCase();
    return [...peersByToken.values()]
      .filter(
        (peer) =>
          !selectedSet().has(peer.token) &&
          (!needle || peer.name.toLocaleLowerCase().includes(needle)),
      )
      .sort(
        (left, right) =>
          Number(right.online) - Number(left.online) ||
          left.name.localeCompare(right.name, "zh-CN") ||
          left.token.localeCompare(right.token),
      );
  });

  const add = (token: string) => {
    if (props.disabled) return;
    const tokens = [...new Set(props.selectedTokens)];
    if (!tokens.includes(token)) props.onChange([...tokens, token]);
    setQuery("");
    setOpened(true);
    queueMicrotask(() => inputRef?.focus());
  };

  const remove = (token: string) => {
    if (props.disabled) return;
    props.onChange([...new Set(props.selectedTokens)].filter((item) => item !== token));
  };

  const positionMenu = () => {
    if (!rootRef) return;
    const rect = rootRef.getBoundingClientRect();
    const menuHeight = Math.min(190, window.innerHeight - 16);
    const top = window.innerHeight - rect.bottom >= menuHeight + 5
      ? rect.bottom + 5
      : Math.max(8, rect.top - menuHeight - 5);
    setMenuRect({ left: rect.left, top, width: rect.width });
  };

  const onReflow = () => opened() && positionMenu();
  window.addEventListener("resize", onReflow);
  window.addEventListener("scroll", onReflow, true);
  onCleanup(() => {
    window.removeEventListener("resize", onReflow);
    window.removeEventListener("scroll", onReflow, true);
  });

  return (
    <div
      class="mention-picker"
      ref={rootRef}
      classList={{ "mention-disabled": !!props.disabled }}
      onFocusOut={(event) => {
        const next = event.relatedTarget;
        if (!(next instanceof Node) || !event.currentTarget.contains(next)) setOpened(false);
      }}
    >
      <Show when={selected().length > 0}>
        <div class="mention-chips">
          <For each={selected()}>
            {(item) => (
              <span class="mention-chip">
                <span class="mention-chip-name">@{item.name}</span>
                <button
                  type="button"
                  class="mention-chip-remove"
                  disabled={props.disabled}
                  aria-label={`移除 @${item.name}`}
                  onClick={() => remove(item.token)}
                >
                  ×
                </button>
              </span>
            )}
          </For>
        </div>
      </Show>

      <input
        ref={inputRef}
        class="mention-input"
        value={query()}
        disabled={props.disabled}
        placeholder={props.placeholder ?? "@ 提醒团队成员"}
        aria-expanded={opened()}
        aria-haspopup="listbox"
        onFocus={() => {
          positionMenu();
          setOpened(true);
        }}
        onInput={(event) => {
          const value = event.currentTarget.value;
          setQuery(value && !value.startsWith("@") ? `@${value.replace(/^@+/, "")}` : value);
          positionMenu();
          setOpened(true);
        }}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            setOpened(false);
          }
        }}
      />

      <Show when={opened() && !props.disabled}>
        <Portal mount={document.body}>
        <div
          class="mention-menu portal"
          role="listbox"
          style={{ left: `${menuRect().left}px`, top: `${menuRect().top}px`, width: `${menuRect().width}px` }}
        >
          <Show when={candidates().length > 0} fallback={<div class="mention-empty">暂无匹配成员</div>}>
            <For each={candidates()}>
              {(peer) => (
                <button
                  type="button"
                  class="mention-option"
                  role="option"
                  onMouseDown={(event) => event.preventDefault()}
                  onClick={() => add(peer.token)}
                >
                  <span class={`peer-dot ${peer.online ? "on" : "off"}`} />
                  <span class="mention-option-name">@{peer.name}</span>
                  <span class="mention-option-status">{peer.online ? "在线" : "离线"}</span>
                </button>
              )}
            </For>
          </Show>
        </div>
        </Portal>
      </Show>
    </div>
  );
}
