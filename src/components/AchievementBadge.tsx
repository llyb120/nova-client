import { createSignal, Show } from "solid-js";
import type { Achievement } from "../types";

export function AchievementBadge(props: { achievement: Achievement }) {
  const [failed, setFailed] = createSignal(false);
  const src = () => {
    if (failed()) return undefined;
    const remote = props.achievement.imageUrl?.trim();
    return remote || undefined;
  };
  return (
    <article class={`achv-card achv-${props.achievement.icon || props.achievement.id}`}>
      <div class="achv-badge-wrap" aria-hidden="true">
        <Show when={src()} fallback={<div class="achv-badge-fallback">{props.achievement.title.slice(0, 1)}</div>}>
          <img
            class="achv-badge"
            src={src()}
            alt=""
            onError={() => setFailed(true)}
          />
        </Show>
        <Show when={props.achievement.number}>
          <span class="achv-number">{props.achievement.number}</span>
        </Show>
      </div>
      <div class="achv-body">
        <h4 class="achv-title">{props.achievement.title}</h4>
        <p class="achv-desc">{props.achievement.description}</p>
      </div>
    </article>
  );
}
