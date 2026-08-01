import { WorkflowSettingsPanel } from "./WorkflowSettingsPanel";

/** 主区域「工作流」视图：画布优先，编辑器全屏沉浸呈现。 */
export function WorkflowsView() {
  return (
    <main class="wf-view">
      <WorkflowSettingsPanel />
    </main>
  );
}
