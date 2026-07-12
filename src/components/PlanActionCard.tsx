import { Show } from "solid-js";
import { dismissProposedPlan, implementProposedPlan, state } from "../store";

/** Plan 模式结束后的继续选项（对齐 Codex TUI 的 Implement this plan?） */
export function PlanActionCard() {
  return (
    <Show when={state.proposedPlan && !state.running[state.currentId ?? ""]}>
      <div class="plan-action-card">
        <div class="plan-action-title">实施此计划？</div>
        <div class="plan-action-desc">切换到 Build 模式并开始按计划编码，或继续留在 Plan 模式。</div>
        <div class="plan-action-btns">
          <button class="perm-btn allow" onClick={() => void implementProposedPlan()}>
            是，实施此计划
          </button>
          <button class="perm-btn" onClick={() => dismissProposedPlan()}>
            否，继续规划
          </button>
        </div>
      </div>
    </Show>
  );
}
