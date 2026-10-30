import { createEffect, createSignal, onCleanup, Show, untrack } from "solid-js";

export function TypewriterText(props: { text: string; class?: string; title?: string; animate?: boolean }) {
  let previous = untrack(() => props.text);
  const [displayed, setDisplayed] = createSignal(previous);
  const [typing, setTyping] = createSignal(false);
  let timer: number | undefined;

  const stop = () => {
    if (timer !== undefined) {
      window.clearInterval(timer);
      timer = undefined;
    }
    setTyping(false);
  };

  createEffect(() => [props.text, props.animate] as const, ([next, animate]) => {
    if (next === previous) return;
    previous = next;
    stop();
    if (!animate) {
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

  onCleanup(() => window.clearInterval(timer));

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
