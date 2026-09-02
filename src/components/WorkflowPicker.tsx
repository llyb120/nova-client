import { createMemo, createSignal } from "solid-js";
import { enabledWorkflows } from "../workflow/storage";
import { SearchSelect, type SelectOption } from "./SearchSelect";

/** 工作流选择器：只列启用的工作流，供快捷键目标等场景选择。 */
export function WorkflowPicker(props: {
  workflowId: string;
  onChange: (workflowId: string) => void;
  title?: string;
  /** 浮层用 Portal 渲染到 body（在设置弹窗等受限容器里避免被裁剪） */
  portal?: boolean;
}) {
  // 每次打开都重新读取 localStorage，保证工作流编辑器改动后列表最新。
  const [version, setVersion] = createSignal(0);
  const options = createMemo<SelectOption[]>(() => {
    version();
    return enabledWorkflows().map((wf) => ({
      value: wf.id,
      label: wf.name,
      title: wf.sharedBy ? `来自 ${wf.sharedBy} 的团队分享` : undefined,
      detail: `${wf.stages.length} 节点`,
    }));
  });

  return (
    <SearchSelect
      prefix="工作流"
      title={props.title ?? "选择工作流"}
      value={props.workflowId}
      options={options()}
      onChange={props.onChange}
      onOpen={() => setVersion((value) => value + 1)}
      searchable
      wide
      allowDefault
      defaultLabel="未选择"
      portal={props.portal}
    />
  );
}
