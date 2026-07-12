import { createSignal, For, Show } from "solid-js";
import type { PlanEntry } from "../types";
import { IconChevron } from "./icons";

export function PlanCard(props: { plan: PlanEntry[] }) {
  const [open, setOpen] = createSignal(true);
  const done = () => props.plan.filter((e) => e.status === "completed").length;

  return (
    <div class="plan-card">
      <button class="plan-head" onClick={() => setOpen(!open())}>
        <IconChevron size={14} open={open()} />
        <span class="plan-title">计划</span>
        <span class="plan-progress">
          {done()}/{props.plan.length}
        </span>
      </button>
      <Show when={open()}>
        <div class="plan-body">
          <For each={props.plan}>
            {(entry) => (
              <div class={`plan-entry plan-${entry.status}`}>
                <span class="plan-dot" />
                <span class="plan-text">{entry.content}</span>
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
