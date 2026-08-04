import { Show } from "solid-js";
import { dismissProposedPlan, implementProposedPlan, state } from "../store";

/** Plan（/plan）结束后的继续选项（对齐 Codex TUI 的 Implement this plan?） */
export function PlanActionCard() {
  return (
    <Show when={state.proposedPlan && !state.running[state.currentId ?? ""]}>
      <div class="plan-action-card">
        <div class="plan-action-title">实施此计划？</div>
        <div class="plan-action-desc">按计划开始编码，或继续用 /plan 补充规划。</div>
        <div class="plan-action-btns">
          <button class="perm-btn allow" onClick={() => void implementProposedPlan()}>
            是，实施此计划
          </button>
          <button class="perm-btn" onClick={() => dismissProposedPlan()}>
            否
          </button>
        </div>
      </div>
    </Show>
  );
}
