import { createMemo, createSignal, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { WorkflowCanvas } from "./WorkflowCanvas";
import { IconX } from "./icons";
import type { WorkflowDef, WorkflowStageDef, WorkflowTransition } from "../workflow/types";

/**
 * Hard 设计后的独立只读 Stage：复用工作流配置页 Canvas。
 * 画布允许平移、缩放、选择和双击查看详情，但不会修改临时工作流。
 */
export function WorkflowPreview(props: { workflow: WorkflowDef }) {
  const [selectedStageId, setSelectedStageId] = createSignal<string | null>(null);
  const [selectedTransitionId, setSelectedTransitionId] = createSignal<string | null>(null);
  const [stageModalOpen, setStageModalOpen] = createSignal(false);
  const [edgeModalOpen, setEdgeModalOpen] = createSignal(false);

  const selectedStage = createMemo(() =>
    props.workflow.stages.find((stage) => stage.id === selectedStageId()) ?? null,
  );
  const selectedTransitionView = createMemo(() => {
    const transitionId = selectedTransitionId();
    if (!transitionId) return null;
    const stage = props.workflow.stages.find((candidate) =>
      candidate.transitions.some((transition) => transition.id === transitionId),
    );
    const transition = stage?.transitions.find((candidate) => candidate.id === transitionId);
    return stage && transition ? { stage, transition } : null;
  });

  function openStageModal(id: string) {
    setSelectedStageId(id);
    setSelectedTransitionId(null);
    setStageModalOpen(true);
    setEdgeModalOpen(false);
  }

  function openEdgeModal(stageId: string, transitionId: string) {
    setSelectedStageId(stageId);
    setSelectedTransitionId(transitionId);
    setEdgeModalOpen(true);
    setStageModalOpen(false);
  }

  return (
    <div class="wf-preview workflow-transcript-wide">
      <div class="wf-preview-title">
        <div>
          工作流 · {props.workflow.name}
          <span class="wf-preview-meta">{props.workflow.stages.length} 阶段</span>
        </div>
        <span class="wf-preview-readonly">只读 · 双击节点或连线查看详情</span>
      </div>
      <div class="wf-preview-canvas">
        <WorkflowCanvas
          def={props.workflow}
          readonly
          onChange={() => {}}
          selectedStageId={selectedStageId()}
          selectedTransitionId={selectedTransitionId()}
          onSelectStage={setSelectedStageId}
          onSelectTransition={setSelectedTransitionId}
          onDeleteStage={() => {}}
          onDeleteTransition={() => {}}
          onEditStage={openStageModal}
          onEditTransition={openEdgeModal}
        />
      </div>

      <Show when={stageModalOpen() && selectedStage()}>
        {(stage) => (
          <Portal>
            <ReadonlyStageModal stage={stage()} entry={props.workflow.entry} onClose={() => setStageModalOpen(false)} />
          </Portal>
        )}
      </Show>

      <Show when={edgeModalOpen() && selectedTransitionView()}>
        {(view) => (
          <Portal>
            <ReadonlyTransitionModal
              stage={view().stage}
              transition={view().transition}
              workflow={props.workflow}
              onClose={() => setEdgeModalOpen(false)}
            />
          </Portal>
        )}
      </Show>
    </div>
  );
}

function ReadonlyStageModal(props: {
  stage: WorkflowStageDef;
  entry: string;
  onClose: () => void;
}) {
  const stageModel = () => props.stage.model?.trim()
    || (props.stage.agentKind ? `${props.stage.agentKind} 默认模型` : "跟随主会话");

  return (
    <div class="modal-backdrop wf-modal-backdrop" onClick={(event) => {
      if (event.target === event.currentTarget) props.onClose();
    }}>
      <div class="modal wf-modal wf-inspector-modal" onClick={(event) => event.stopPropagation()}>
        <div class="modal-head">
          <span>阶段详情 · 只读</span>
          <button class="icon-btn" onClick={props.onClose}><IconX size={16} /></button>
        </div>
        <div class="modal-body">
          <div class="wf-stage-editor">
            <div class="wf-modal-section">
              <div class="wf-modal-section-title">阶段</div>
              <label class="field">
                <span class="field-label">名称</span>
                <input class="field-input" value={props.stage.name} readOnly />
              </label>
              <div class="wf-preview-detail-row">
                <span>{props.entry === props.stage.id ? "首节点" : "普通节点"}</span>
                <span>{props.stage.manualReview ? "人工审核" : "自动流转"}</span>
                <span>{props.stage.transitions.length} 条连线</span>
              </div>
              <label class="field">
                <span class="field-label">模型</span>
                <input
                  class="field-input"
                  value={stageModel()}
                  readOnly
                />
              </label>
              <label class="field">
                <span class="field-label">提示词模板</span>
                <textarea class="field-input wf-textarea" rows={9} value={props.stage.promptTemplate} readOnly />
              </label>
            </div>
            <div class="wf-modal-foot">
              <span class="wf-zoom-spacer" />
              <button class="btn primary small" onClick={props.onClose}>关闭</button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

function ReadonlyTransitionModal(props: {
  workflow: WorkflowDef;
  stage: WorkflowStageDef;
  transition: WorkflowTransition;
  onClose: () => void;
}) {
  const targetName = () => {
    if (props.transition.to === "$done") return "结束";
    if (props.transition.to === "$fail") return "失败并结束";
    return props.workflow.stages.find((stage) => stage.id === props.transition.to)?.name ?? props.transition.to;
  };

  return (
    <div class="modal-backdrop wf-modal-backdrop" onClick={(event) => {
      if (event.target === event.currentTarget) props.onClose();
    }}>
      <div class="modal wf-modal wf-inspector-modal" onClick={(event) => event.stopPropagation()}>
        <div class="modal-head">
          <span>连线详情 · 只读</span>
          <button class="icon-btn" onClick={props.onClose}><IconX size={16} /></button>
        </div>
        <div class="modal-body">
          <div class="wf-stage-editor">
            <div class="wf-modal-section">
              <div class="wf-preview-detail-row">
                <span>{props.stage.name}</span>
                <span>→</span>
                <span>{targetName()}</span>
              </div>
              <label class="field">
                <span class="field-label">连线名称</span>
                <input class="field-input" value={props.transition.label ?? ""} readOnly />
              </label>
              <label class="field">
                <span class="field-label">跳转依据</span>
                <textarea class="field-input" rows={4} value={props.transition.prompt ?? ""} readOnly />
              </label>
            </div>
            <div class="wf-modal-foot">
              <span class="wf-zoom-spacer" />
              <button class="btn primary small" onClick={props.onClose}>关闭</button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
