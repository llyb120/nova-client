import { createEffect, createSignal, onCleanup, Show } from "solid-js";

export function TypewriterText(props: { text: string; class?: string; title?: string; animate?: boolean }) {
  const [displayed, setDisplayed] = createSignal(props.text);
  const [typing, setTyping] = createSignal(false);
  let previous = props.text;
  let timer: number | undefined;

  const stop = () => {
    if (timer !== undefined) {
      window.clearInterval(timer);
      timer = undefined;
    }
    setTyping(false);
  };

  createEffect(() => {
    const next = props.text;
    if (next === previous) return;
    previous = next;
    stop();
    if (!props.animate) {
      setDisplayed(next);
      return;
    }
    if (!next) {
      setDisplayed("");
      return;
    }
    setDisplayed("");
    setTyping(true);
    let i = 0;
    const chars = Array.from(next);
    timer = window.setInterval(() => {
      i += 1;
      setDisplayed(chars.slice(0, i).join(""));
      if (i >= chars.length) stop();
    }, 34);
  });

  onCleanup(stop);

  return (
    <span
      class={`${props.class ?? ""}${typing() ? " title-typing" : ""}`}
      title={props.title ?? props.text}
    >
      {displayed()}
      <Show when={typing()}>
        <span class="title-caret" />
      </Show>
    </span>
  );
}
