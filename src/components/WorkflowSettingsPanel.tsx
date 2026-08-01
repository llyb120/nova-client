import { createMemo, createSignal, For, Show } from "solid-js";
import { WorkflowCanvas } from "./WorkflowCanvas";
import { IconChevron, IconMerge, IconPencil, IconPlus, IconX } from "./icons";
import { BUILTIN_WORKFLOWS } from "../workflow/builtin";
import {
  deleteUserWorkflow,
  listWorkflows,
  saveWorkflow,
} from "../workflow/storage";
import {
  newWorkflowId,
  validateWorkflow,
  WF_DONE,
  WF_FAIL,
  type WorkflowDef,
  type WorkflowStageDef,
  type WorkflowTransition,
  type WorkflowTrigger,
} from "../workflow/types";

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function blankWorkflow(): WorkflowDef {
  const stageId = newWorkflowId("s");
  return {
    id: newWorkflowId("wf"),
    name: "新工作流",
    version: 1,
    entry: stageId,
    maxTotalStages: 30,
    stages: [
      {
        id: stageId,
        name: "执行",
        promptTemplate: "请在当前项目中完成下面的目标：\n\n{{goal}}\n\n完成后简短汇报实际改动与验证结果。",
        mode: "build",
        reviewOnly: false,
        x: 80,
        y: 140,
        transitions: [{ id: newWorkflowId("t"), when: { kind: "always" }, to: WF_DONE, label: "完成" }],
      },
    ],
  };
}

export function WorkflowSettingsPanel() {
  const [list, setList] = createSignal<WorkflowDef[]>(listWorkflows());
  const [selectedId, setSelectedId] = createSignal<string | null>(null);
  const [draft, setDraft] = createSignal<WorkflowDef | null>(null);
  const [selectedStageId, setSelectedStageId] = createSignal<string | null>(null);
  const [selectedTransitionId, setSelectedTransitionId] = createSignal<string | null>(null);
  const [msg, setMsg] = createSignal("");

  // 布局状态：抽屉 / 工具条折叠 / 编辑弹窗 / 触发器弹窗。
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const [barCollapsed, setBarCollapsed] = createSignal(false);
  const [inspectorOpen, setInspectorOpen] = createSignal(false);
  const [triggerOpen, setTriggerOpen] = createSignal(false);

  const reload = () => setList(listWorkflows());

  const select = (id: string | null) => {
    setSelectedId(id);
    setSelectedStageId(null);
    setSelectedTransitionId(null);
    setMsg("");
    setInspectorOpen(false);
    const found = list().find((w) => w.id === id);
    setDraft(found ? clone(found) : null);
  };

  const errors = createMemo(() => (draft() ? validateWorkflow(draft()!) : []));

  const selectedStage = createMemo(() =>
    draft()?.stages.find((s) => s.id === selectedStageId()) ?? null,
  );

  // 由转移 id 反查所属阶段（转移 id 全局唯一）。
  const selectedTransitionView = createMemo(() => {
    const tid = selectedTransitionId();
    const d = draft();
    if (!tid || !d) return null;
    const stage = d.stages.find((s) => s.transitions.some((t) => t.id === tid));
    const transition = stage?.transitions.find((t) => t.id === tid);
    return stage && transition ? { stage, transition } : null;
  });

  function patchDraft(patch: Partial<WorkflowDef>) {
    setDraft((d) => (d ? { ...d, ...patch } : d));
  }
  function patchStage(stageId: string, patch: Partial<WorkflowStageDef>) {
    setDraft((d) =>
      d ? { ...d, stages: d.stages.map((s) => (s.id === stageId ? { ...s, ...patch } : s)) } : d,
    );
  }

  function addStage() {
    const d = draft();
    if (!d) return;
    const stage: WorkflowStageDef = {
      id: newWorkflowId("s"),
      name: `阶段 ${d.stages.length + 1}`,
      promptTemplate: "{{goal}}\n\n上一阶段结论：\n{{prev}}",
      mode: "build",
      reviewOnly: false,
      x: 120 + d.stages.length * 40,
      y: 160 + d.stages.length * 40,
      transitions: [],
    };
    setDraft({ ...d, stages: [...d.stages, stage] });
    setSelectedStageId(stage.id);
    setInspectorOpen(true);
  }

  function deleteStage(stageId: string) {
    const d = draft();
    if (!d) return;
    if (d.stages.length <= 1) {
      setMsg("至少需要保留一个阶段");
      return;
    }
    const stages = d.stages
      .filter((s) => s.id !== stageId)
      .map((s) => ({ ...s, transitions: s.transitions.filter((t) => t.to !== stageId) }));
    const entry = d.entry === stageId ? stages[0].id : d.entry;
    setDraft({ ...d, stages, entry });
    if (selectedStageId() === stageId) {
      setSelectedStageId(null);
      setInspectorOpen(false);
    }
  }

  function patchTransition(stageId: string, transitionId: string, patch: Partial<WorkflowTransition>) {
    setDraft((d) =>
      d
        ? {
            ...d,
            stages: d.stages.map((s) =>
              s.id === stageId
                ? { ...s, transitions: s.transitions.map((t) => (t.id === transitionId ? { ...t, ...patch } : t)) }
                : s,
            ),
          }
        : d,
    );
  }
  function addTransition(stageId: string) {
    const d = draft();
    if (!d) return;
    const transition: WorkflowTransition = { id: newWorkflowId("t"), when: { kind: "always" }, to: WF_DONE, label: "" };
    setDraft({ ...d, stages: d.stages.map((s) => (s.id === stageId ? { ...s, transitions: [...s.transitions, transition] } : s)) });
    setSelectedTransitionId(transition.id);
  }
  function deleteTransition(stageId: string, transitionId: string) {
    setDraft((d) =>
      d
        ? { ...d, stages: d.stages.map((s) => (s.id === stageId ? { ...s, transitions: s.transitions.filter((t) => t.id !== transitionId) } : s)) }
        : d,
    );
    if (selectedTransitionId() === transitionId) setSelectedTransitionId(null);
  }

  function addTrigger() {
    setDraft((d) => (d ? { ...d, triggers: [...(d.triggers ?? []), { kind: "slash", command: "" }] } : d));
  }
  function patchTrigger(index: number, trigger: WorkflowTrigger) {
    setDraft((d) => (d ? { ...d, triggers: (d.triggers ?? []).map((t, i) => (i === index ? trigger : t)) } : d));
  }
  function deleteTrigger(index: number) {
    setDraft((d) => (d ? { ...d, triggers: (d.triggers ?? []).filter((_, i) => i !== index) } : d));
  }

  function save() {
    const d = draft();
    if (!d) return;
    const errs = validateWorkflow(d);
    if (errs.length > 0) {
      setMsg(errs.join("；"));
      return;
    }
    try {
      saveWorkflow(d);
      reload();
      setSelectedId(d.id);
      setMsg("已保存");
    } catch (e) {
      setMsg(e instanceof Error ? e.message : String(e));
    }
  }

  function createNew() {
    const wf = blankWorkflow();
    setList([...list(), wf]);
    select(wf.id);
    setDrawerOpen(false);
    setMsg("新建后请记得保存");
  }
  function cloneBuiltin(source: WorkflowDef) {
    const copy = clone(source);
    copy.id = newWorkflowId("wf");
    copy.builtin = false;
    copy.name = `${source.name} 副本`;
    setList([...list(), copy]);
    select(copy.id);
    setDrawerOpen(false);
    setMsg("已从内置复制，修改后保存即可");
  }
  function removeSelected() {
    const d = draft();
    if (!d || d.builtin) return;
    if (!confirm(`删除工作流「${d.name}」？`)) return;
    deleteUserWorkflow(d.id);
    reload();
    select(null);
  }

  // 双击画布节点/连线 → 打开编辑弹窗。
  function openStageModal(id: string) {
    setSelectedStageId(id);
    setSelectedTransitionId(null);
    setInspectorOpen(true);
  }
  function openEdgeModal(stageId: string, id: string) {
    setSelectedStageId(stageId);
    setSelectedTransitionId(id);
    setInspectorOpen(true);
  }

  const targetOptions = createMemo(() => {
    const d = draft();
    if (!d) return [];
    return [
      ...d.stages.map((s) => ({ value: s.id, label: `→ ${s.name || s.id}` })),
      { value: WF_DONE, label: "→ 成功结束" },
      { value: WF_FAIL, label: "→ 失败结束" },
    ];
  });

  const currentName = createMemo(() => draft()?.name ?? "未选择");

  return (
    <div class="wf-panel">
      <div class="wf-ambient" aria-hidden="true" />

      {/* 抽屉触发按钮：始终悬浮左上，显示当前工作流名 */}
      <button
        class="wf-fab wf-fab-drawer"
        classList={{ open: drawerOpen() }}
        onClick={() => setDrawerOpen((o) => !o)}
        title="工作流列表"
      >
        <IconMerge size={15} />
        <span class="wf-fab-name">{currentName()}</span>
        <IconChevron size={13} open={drawerOpen()} />
      </button>

      {/* 左侧抽屉 */}
      <Show when={drawerOpen()}>
        <div class="wf-drawer-scrim" onClick={() => setDrawerOpen(false)} />
        <aside class="wf-drawer">
          <div class="wf-drawer-head">
            <span class="wf-drawer-title">工作流</span>
            <button class="icon-btn" onClick={() => setDrawerOpen(false)} title="收起">
              <IconX size={15} />
            </button>
          </div>
          <div class="wf-sidebar-head">
            <button class="btn primary small" onClick={createNew}>
              <IconPlus size={14} /> 新建
            </button>
            <button class="btn secondary small" disabled={!draft() || draft()!.builtin} onClick={removeSelected}>
              删除
            </button>
          </div>
          <div class="wf-list">
            <For each={list()}>
              {(wf) => (
                <div
                  classList={{ "wf-list-item": true, active: selectedId() === wf.id }}
                  onClick={() => { select(wf.id); setDrawerOpen(false); }}
                >
                  <span class="wf-list-name">{wf.name}</span>
                  <Show when={wf.builtin}><span class="wf-badge">内置</span></Show>
                </div>
              )}
            </For>
          </div>
          <div class="wf-builtin-clone">
            <span class="field-hint">从内置复制：</span>
            <For each={BUILTIN_WORKFLOWS}>
              {(wf) => (
                <button class="link-btn" onClick={() => cloneBuiltin(wf)}>{wf.name}</button>
              )}
            </For>
          </div>
          <div class="field-hint wf-run-hint">
            在会话中输入 <code>/run 工作流名 目标</code> 即可运行。
          </div>
        </aside>
      </Show>

      <Show
        when={draft()}
        fallback={
          <div class="wf-empty-stage">
            <div class="wf-empty-glow" aria-hidden="true" />
            <div class="wf-empty-card">
              <div class="wf-empty-mark"><IconMerge size={26} /></div>
              <h2 class="wf-empty-title">还没有打开工作流</h2>
              <p class="wf-empty-sub">从左侧选择一个工作流，或新建一个开始编排阶段接力。</p>
              <div class="wf-empty-actions">
                <button class="btn primary" onClick={createNew}><IconPlus size={15} /> 新建工作流</button>
                <button class="btn secondary" onClick={() => setDrawerOpen(true)}>选择已有</button>
              </div>
            </div>
          </div>
        }
      >
        {(d) => (
          <>
            {/* 画布铺满整个主区域 */}
            <WorkflowCanvas
              def={d()}
              onChange={(next) => setDraft(next)}
              selectedStageId={selectedStageId()}
              selectedTransitionId={selectedTransitionId()}
              onSelectStage={setSelectedStageId}
              onSelectTransition={setSelectedTransitionId}
              onDeleteStage={deleteStage}
              onDeleteTransition={deleteTransition}
              onEditStage={openStageModal}
              onEditTransition={openEdgeModal}
            />

            {/* 浮动工具条 */}
            <div class="wf-topbar" classList={{ collapsed: barCollapsed(), builtin: !!d().builtin }}>
              <Show
                when={!barCollapsed()}
                fallback={
                  <button class="wf-bar-toggle" onClick={() => setBarCollapsed(false)} title="展开工具条">
                    <IconChevron size={14} open />
                  </button>
                }
              >
                <input
                  class="field-input wf-name-input"
                  value={d().name}
                  disabled={d().builtin}
                  onInput={(e) => patchDraft({ name: e.currentTarget.value })}
                  placeholder="工作流名称"
                  title="工作流名称"
                />
                <span class="wf-bar-sep" />
                <label class="wf-inline-label" title="起始阶段">
                  起点
                  <select
                    class="field-input wf-select"
                    value={d().entry}
                    disabled={d().builtin}
                    onChange={(e) => patchDraft({ entry: e.currentTarget.value })}
                  >
                    <For each={d().stages}>{(s) => <option value={s.id}>{s.name || s.id}</option>}</For>
                  </select>
                </label>
                <label class="wf-inline-label" title="整条链最多创建的阶段会话数">
                  上限
                  <input
                    class="field-input wf-number"
                    type="number"
                    min={1}
                    value={d().maxTotalStages}
                    disabled={d().builtin}
                    onInput={(e) => patchDraft({ maxTotalStages: Math.max(1, Number(e.currentTarget.value) || 1) })}
                  />
                </label>
                <span class="wf-bar-sep" />
                <button
                  class="wf-chip"
                  classList={{ active: triggerOpen() }}
                  disabled={d().builtin}
                  onClick={() => setTriggerOpen(true)}
                  title="触发条件：满足即自动启动"
                >
                  触发器
                  <span class="wf-chip-count">{(d().triggers ?? []).length}</span>
                </button>
                <button
                  class="wf-chip"
                  classList={{ active: inspectorOpen() }}
                  onClick={() => setInspectorOpen(true)}
                  title="编辑选中的阶段或连线（也可双击画布节点/连线）"
                >
                  <IconPencil size={13} /> 编辑
                </button>
                <button class="wf-chip" disabled={d().builtin} onClick={addStage} title="添加阶段">
                  <IconPlus size={13} /> 阶段
                </button>
                <span class="wf-bar-sep" />
                <Show when={d().builtin}><span class="wf-badge">内置只读</span></Show>
                <button class="btn secondary small" onClick={() => select(d().id)} title="放弃未保存改动">撤销</button>
                <button class="btn primary small" disabled={d().builtin} onClick={save}>保存</button>
                <button class="wf-bar-toggle" onClick={() => setBarCollapsed(true)} title="收起工具条">
                  <IconChevron size={14} />
                </button>
              </Show>
            </div>

            {/* 浮动操作提示 */}
            <div class="wf-hint-float">
              双击节点 / 连线编辑 · 拖右侧手柄连线 · 右键删除 · 滚轮缩放
            </div>

            {/* 校验 / 消息 toast */}
            <Show when={errors().length > 0 || msg()}>
              <div class="wf-toast" classList={{ error: errors().length > 0 }}>
                <Show when={errors().length > 0} fallback={<span>{msg()}</span>}>
                  <For each={errors()}>{(err) => <div>{err}</div>}</For>
                </Show>
              </div>
            </Show>
          </>
        )}
      </Show>

      {/* 阶段 / 连线 编辑弹窗 */}
      <Show when={inspectorOpen() && draft()}>
        <div class="modal-backdrop wf-modal-backdrop" onClick={() => setInspectorOpen(false)}>
          <div class="modal wf-modal wf-inspector-modal" onClick={(e) => e.stopPropagation()}>
            <div class="modal-head">
              <span>
                <Show when={selectedTransitionView()} fallback="编辑阶段">
                  编辑连线
                </Show>
              </span>
              <button class="icon-btn" onClick={() => setInspectorOpen(false)}><IconX size={16} /></button>
            </div>
            <div class="modal-body">
              <Show
                when={selectedStage()}
                fallback={
                  <div class="wf-inspector-empty">
                    在画布中双击一个阶段或连线来编辑；或先单击选中再打开本窗口。拖动手柄圆圈可连线。
                    <div style={{ "margin-top": "12px" }}>
                      <button class="btn secondary small" disabled={draft()!.builtin} onClick={addStage}>+ 添加阶段</button>
                    </div>
                  </div>
                }
              >
                {(stage) => (
                  <div class="wf-stage-editor">
                    {/* 若当前聚焦某条连线，先突出该连线编辑 */}
                    <Show when={selectedTransitionView()}>
                      {(tv) => (
                        <div class="wf-modal-section">
                          <div class="wf-modal-section-title">连线 · 来自「{tv().stage.name}」</div>
                          <div class="wf-transition focused">
                            <select
                              class="field-input wf-select"
                              value={tv().transition.when.kind}
                              disabled={draft()!.builtin}
                              onChange={(e) => {
                                const kind = e.currentTarget.value as "always" | "marker" | "regex";
                                patchTransition(tv().stage.id, tv().transition.id, { when: kind === "always" ? { kind } : { kind, value: "" } });
                              }}
                            >
                              <option value="always">总是</option>
                              <option value="marker">结尾标记</option>
                              <option value="regex">正则</option>
                            </select>
                            <Show when={tv().transition.when.kind !== "always"}>
                              <input
                                class="field-input wf-cond-value"
                                value={(tv().transition.when as { value?: string }).value ?? ""}
                                disabled={draft()!.builtin}
                                placeholder={tv().transition.when.kind === "marker" ? "如 DONE" : "正则"}
                                onInput={(e) => patchTransition(tv().stage.id, tv().transition.id, { when: { kind: tv().transition.when.kind, value: e.currentTarget.value } as WorkflowTransition["when"] })}
                              />
                            </Show>
                            <select
                              class="field-input wf-select"
                              value={tv().transition.to}
                              disabled={draft()!.builtin}
                              onChange={(e) => patchTransition(tv().stage.id, tv().transition.id, { to: e.currentTarget.value })}
                            >
                              <For each={targetOptions()}>{(opt) => <option value={opt.value}>{opt.label}</option>}</For>
                            </select>
                            <input
                              class="field-input wf-edge-label-input"
                              value={tv().transition.label ?? ""}
                              disabled={draft()!.builtin}
                              placeholder="标签"
                              onInput={(e) => patchTransition(tv().stage.id, tv().transition.id, { label: e.currentTarget.value })}
                            />
                            <button class="btn danger small" disabled={draft()!.builtin} onClick={() => deleteTransition(tv().stage.id, tv().transition.id)}>×</button>
                          </div>
                        </div>
                      )}
                    </Show>

                    <div class="wf-modal-section">
                      <div class="wf-modal-section-title">阶段</div>
                      <div class="wf-inspector-row">
                        <input
                          class="field-input"
                          value={stage().name}
                          disabled={draft()!.builtin}
                          onInput={(e) => patchStage(stage().id, { name: e.currentTarget.value })}
                          placeholder="阶段名称"
                        />
                        <button class="btn small secondary" disabled={draft()!.builtin || draft()!.entry === stage().id} onClick={() => patchDraft({ entry: stage().id })}>
                          设为起点
                        </button>
                      </div>

                      <label class="wf-field">
                        <span>提示词模板</span>
                        <textarea
                          class="field-input wf-textarea"
                          rows={8}
                          disabled={draft()!.builtin}
                          value={stage().promptTemplate}
                          onInput={(e) => patchStage(stage().id, { promptTemplate: e.currentTarget.value })}
                        />
                        <span class="field-hint">变量：{"{{goal}} {{criteria}} {{prev}} {{attempt}}"}</span>
                      </label>

                      <div class="wf-inspector-row">
                        <label class="wf-field">
                          <span>模式</span>
                          <select
                            class="field-input wf-select"
                            value={stage().mode ?? "build"}
                            disabled={draft()!.builtin}
                            onChange={(e) => patchStage(stage().id, { mode: e.currentTarget.value })}
                          >
                            <For each={["build", "plan", "ask"]}>{(m) => <option value={m}>{m}</option>}</For>
                          </select>
                        </label>
                        <label class="wf-check">
                          <input
                            type="checkbox"
                            checked={!!stage().reviewOnly}
                            disabled={draft()!.builtin}
                            onChange={(e) => patchStage(stage().id, { reviewOnly: e.currentTarget.checked })}
                          />
                          只读核验阶段
                        </label>
                      </div>
                    </div>

                    <div class="wf-modal-section">
                      <div class="wf-transitions-head">
                        <span class="wf-modal-section-title">转移（按顺序求值）</span>
                        <button class="link-btn" disabled={draft()!.builtin} onClick={() => addTransition(stage().id)}>+ 添加</button>
                      </div>
                      <For each={stage().transitions}>
                        {(t) => (
                          <div
                            classList={{ "wf-transition": true, selected: selectedTransitionId() === t.id }}
                            onClick={() => setSelectedTransitionId(t.id)}
                          >
                            <select
                              class="field-input wf-select"
                              value={t.when.kind}
                              disabled={draft()!.builtin}
                              onChange={(e) => {
                                const kind = e.currentTarget.value as "always" | "marker" | "regex";
                                patchTransition(stage().id, t.id, { when: kind === "always" ? { kind } : { kind, value: "" } });
                              }}
                            >
                              <option value="always">总是</option>
                              <option value="marker">结尾标记</option>
                              <option value="regex">正则</option>
                            </select>
                            <Show when={t.when.kind !== "always"}>
                              <input
                                class="field-input wf-cond-value"
                                value={(t.when as { value?: string }).value ?? ""}
                                disabled={draft()!.builtin}
                                placeholder={t.when.kind === "marker" ? "如 DONE" : "正则"}
                                onInput={(e) => patchTransition(stage().id, t.id, { when: { kind: t.when.kind, value: e.currentTarget.value } as WorkflowTransition["when"] })}
                              />
                            </Show>
                            <select
                              class="field-input wf-select"
                              value={t.to}
                              disabled={draft()!.builtin}
                              onChange={(e) => patchTransition(stage().id, t.id, { to: e.currentTarget.value })}
                            >
                              <For each={targetOptions()}>{(opt) => <option value={opt.value}>{opt.label}</option>}</For>
                            </select>
                            <input
                              class="field-input wf-edge-label-input"
                              value={t.label ?? ""}
                              disabled={draft()!.builtin}
                              placeholder="标签"
                              onInput={(e) => patchTransition(stage().id, t.id, { label: e.currentTarget.value })}
                            />
                            <button class="btn danger small" disabled={draft()!.builtin} onClick={(e) => { e.stopPropagation(); deleteTransition(stage().id, t.id); }}>×</button>
                          </div>
                        )}
                      </For>
                    </div>

                    <div class="wf-modal-foot">
                      <button class="btn danger small" disabled={draft()!.builtin} onClick={() => deleteStage(stage().id)}>删除该阶段</button>
                      <span class="wf-zoom-spacer" />
                      <button class="btn secondary small" disabled={draft()!.builtin} onClick={addStage}>+ 添加阶段</button>
                      <button class="btn primary small" onClick={() => setInspectorOpen(false)}>完成</button>
                    </div>
                  </div>
                )}
              </Show>
            </div>
          </div>
        </div>
      </Show>

      {/* 触发条件弹窗 */}
      <Show when={triggerOpen() && draft()}>
        <div class="modal-backdrop wf-modal-backdrop" onClick={() => setTriggerOpen(false)}>
          <div class="modal wf-modal wf-trigger-modal" onClick={(e) => e.stopPropagation()}>
            <div class="modal-head">
              <span>触发条件</span>
              <button class="icon-btn" onClick={() => setTriggerOpen(false)}><IconX size={16} /></button>
            </div>
            <div class="modal-body">
              <div class="wf-triggers-sub wf-modal-lead">满足任一即自动启动该工作流，无需 /run。</div>
              <Show
                when={(draft()!.triggers ?? []).length > 0}
                fallback={<div class="wf-triggers-empty">暂无触发器，仅能用 /run 启动。</div>}
              >
                <For each={draft()!.triggers ?? []}>
                  {(t, idx) => (
                    <div class="wf-trigger-row">
                      <select
                        class="field-input wf-select wf-trigger-kind"
                        value={t.kind}
                        disabled={draft()!.builtin}
                        onChange={(e) => {
                          const kind = e.currentTarget.value as WorkflowTrigger["kind"];
                          patchTrigger(idx(), kind === "slash" ? { kind, command: "" } : kind === "contains" ? { kind, text: "" } : { kind, pattern: "" });
                        }}
                      >
                        <option value="slash">指令 /cmd</option>
                        <option value="contains">包含文本</option>
                        <option value="regex">正则</option>
                      </select>
                      <Show when={t.kind === "slash"}>
                        <span class="wf-trigger-slash">/</span>
                        <input class="field-input wf-trigger-value" value={(t as { command: string }).command} disabled={draft()!.builtin} placeholder="fix" onInput={(e) => patchTrigger(idx(), { kind: "slash", command: e.currentTarget.value })} />
                      </Show>
                      <Show when={t.kind === "contains"}>
                        <input class="field-input wf-trigger-value" value={(t as { text: string }).text} disabled={draft()!.builtin} placeholder="提示词中包含的文本" onInput={(e) => patchTrigger(idx(), { kind: "contains", text: e.currentTarget.value })} />
                      </Show>
                      <Show when={t.kind === "regex"}>
                        <input class="field-input wf-trigger-value" value={(t as { pattern: string }).pattern} disabled={draft()!.builtin} placeholder="正则表达式" onInput={(e) => patchTrigger(idx(), { kind: "regex", pattern: e.currentTarget.value })} />
                      </Show>
                      <button class="btn danger small" disabled={draft()!.builtin} onClick={() => deleteTrigger(idx())}>×</button>
                    </div>
                  )}
                </For>
              </Show>
              <div class="field-hint">
                指令触发形如 <code>/fix 修登录</code>；包含/正则会匹配整条提示词。过于宽泛的包含或正则可能误触发普通对话，请谨慎。
              </div>
              <div class="wf-modal-foot">
                <span class="wf-zoom-spacer" />
                <button class="btn secondary small" disabled={draft()!.builtin} onClick={addTrigger}>+ 添加触发器</button>
                <button class="btn primary small" onClick={() => setTriggerOpen(false)}>完成</button>
              </div>
            </div>
          </div>
        </div>
      </Show>
    </div>
  );
}
