import { confirm, message } from "@tauri-apps/plugin-dialog";
import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import { api } from "../ipc";
import { openThread, refreshMarks, setView, state } from "../store";
import type { Decision, Employee, Mark } from "../types";
import { IconCheck, IconEye, IconSend, IconTrash, IconX } from "./icons";
import { createImageAttachments, ImageAttachmentStrip } from "./ImageAttachmentStrip";

const CATS: Record<string, { label: string; cls: string }> = {
  approve: { label: "是否推进", cls: "approve" },
  choose: { label: "方案选型", cls: "choose" },
  input: { label: "补充信息", cls: "input" },
  priority: { label: "排优先级", cls: "priority" },
  other: { label: "请旨", cls: "other" },
};
const catOf = (c?: string) => CATS[(c || "other").toLowerCase()] ?? CATS.other;
const PAGE_SIZE = 8;
const LAST_EDICT_EMP_KEY = "fd:lastEdictEmployee";

const MARK_STATUS: Record<string, { label: string; cls: string }> = {
  open: { label: "待处理", cls: "open" },
  claimed: { label: "处理中", cls: "claimed" },
  done: { label: "已完成", cls: "done" },
  failed: { label: "失败", cls: "failed" },
};

function markView(m: Mark): { label: string; cls: string } {
  if (m.status === "claimed" && m.leaseUntil && m.leaseUntil < Date.now()) {
    return { label: "处理中·可接管", cls: "claimed stale" };
  }
  return MARK_STATUS[m.status] ?? { label: m.status, cls: "" };
}

function empScope(e: Employee) {
  return e.markScope.trim() ? e.markScope.trim() : `emp:${e.id}`;
}

function pageCount(total: number, pageSize = PAGE_SIZE): number {
  return Math.max(1, Math.ceil(total / pageSize));
}

function pageSlice<T>(list: T[], page: number, pageSize = PAGE_SIZE): T[] {
  const start = Math.max(0, page - 1) * pageSize;
  return list.slice(start, start + pageSize);
}

function PageNav(props: { page: number; total: number; onPage: (page: number) => void; pageSize?: number }) {
  const size = () => props.pageSize ?? PAGE_SIZE;
  const pages = () => pageCount(props.total, size());
  return (
    <Show when={props.total > size()}>
      <div class="list-pager">
        <span class="list-pager-info">
          第 {props.page} / {pages()} 页 · 共 {props.total} 条
        </span>
        <button
          class="btn tiny secondary"
          disabled={props.page <= 1}
          onClick={() => props.onPage(Math.max(1, props.page - 1))}
        >
          上一页
        </button>
        <button
          class="btn tiny secondary"
          disabled={props.page >= pages()}
          onClick={() => props.onPage(Math.min(pages(), props.page + 1))}
        >
          下一页
        </button>
      </div>
    </Show>
  );
}

/** 已批阅奏折的去向标注 */
const OUTCOMES: Record<string, { label: string; cls: string; hint: string }> = {
  resolved: { label: "已准奏", cls: "resolved", hint: "批示已下，员工将按发送时声明的后续动作继续" },
  consumed: { label: "已处理", cls: "consumed", hint: "批复 ActionPlan 已执行，员工已带着批示推进" },
  shelved: { label: "留中不发", cls: "shelved", hint: "未作批示，员工将按自己的专业判断推进" },
  rejected: { label: "已驳回", cls: "rejected", hint: "主管驳回该请求，单子已按 ActionPlan 停止" },
  withdrawn: { label: "已撤回", cls: "withdrawn", hint: "单子已完结，此折随之失效撤回" },
  read: { label: "已阅", cls: "read", hint: "完工汇报已读归档" },
  reviewed: { label: "已批阅", cls: "reviewed", hint: "完工汇报已批阅，批示将供 Dream 反思学习" },
};
const outcomeOf = (s: string) => OUTCOMES[s] ?? OUTCOMES.consumed;
const isReportArchive = (d: Decision) => d.status === "read" || d.status === "reviewed";

function uniqOptions(options: string[]): string[] {
  const seen = new Set<string>();
  return options
    .map((x) => x.trim())
    .filter((x) => x && !seen.has(x) && seen.add(x));
}

function presetOptions(d: Decision): string[] {
  const text = `${d.question}\n${d.brief ?? ""}`.toLowerCase();
  if (text.includes("分支已存在") || text.includes("branch")) {
    return ["换一个新分支名继续", "我已处理仓库状态，重试", "缩小范围后继续"];
  }
  const cat = (d.category || "other").toLowerCase();
  if (cat === "choose" || cat === "workflow-route") {
    return ["按风险最低的方案推进", "按员工推荐方案推进", "先补充证据再定"];
  }
  if (cat === "input") {
    return ["按已有信息自行判断", "先补充上下文再继续"];
  }
  if (cat === "priority") {
    return ["优先处理这张单", "排到后面，先做更急的"];
  }
  return ["同意推进，按当前方案处理", "换一个更稳妥的方案继续", "缩小范围后继续"];
}

function optionsOf(d: Decision): string[] {
  return uniqOptions([...d.options, ...presetOptions(d)]).slice(0, 6);
}

function fmtTime(ts: number): string {
  if (!ts) return "";
  const d = new Date(ts);
  const now = new Date();
  const sameDay = d.toDateString() === now.toDateString();
  return sameDay
    ? d.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })
    : d.toLocaleString("zh-CN", {
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
      });
}

type ActiveEvent = {
  id: string;
  kind: "mark" | "task";
  statusLabel: string;
  statusCls: string;
  title: string;
  employeeName: string;
  threadId?: string | null;
  updatedAt: number;
  scope?: string;
  markKey?: string;
  shared?: boolean;
  taskId?: string;
};

export function DecisionWorkbench() {
  const pending = createMemo(() =>
    state.decisions
      .filter((d) => d.status === "pending")
      .sort((a, b) => a.createdAt - b.createdAt),
  );
  // 完工汇报：可以直接已读，也可以留下批阅供 Dream 反思。
  const reports = createMemo(() =>
    state.decisions
      .filter((d) => d.status === "report")
      .sort((a, b) => b.createdAt - a.createdAt),
  );
  const reviewed = createMemo(() =>
    state.decisions
      .filter((d) => d.status !== "pending" && d.status !== "report")
      .sort((a, b) => (b.resolvedAt || b.createdAt) - (a.resolvedAt || a.createdAt)),
  );
  const [pendingPage, setPendingPage] = createSignal(1);
  const [reportPage, setReportPage] = createSignal(1);
  const [reviewedPage, setReviewedPage] = createSignal(1);
  const [eventPage, setEventPage] = createSignal(1);
  const pagedPending = createMemo(() => pageSlice(pending(), pendingPage()));
  const pagedReports = createMemo(() => pageSlice(reports(), reportPage()));
  const pagedReviewed = createMemo(() => pageSlice(reviewed(), reviewedPage()));

  createEffect(() => {
    const pages = pageCount(pending().length);
    if (pendingPage() > pages) setPendingPage(pages);
  });
  createEffect(() => {
    const pages = pageCount(reports().length);
    if (reportPage() > pages) setReportPage(pages);
  });
  createEffect(() => {
    const pages = pageCount(reviewed().length);
    if (reviewedPage() > pages) setReviewedPage(pages);
  });

  // 每道折子的朱批草稿（按 id）
  const [drafts, setDrafts] = createSignal<Record<string, string>>({});
  const draftOf = (id: string) => drafts()[id] ?? "";
  const setDraft = (id: string, v: string) => setDrafts((prev) => ({ ...prev, [id]: v }));
  const [busy, setBusy] = createSignal<string | null>(null);

  // ===== 下旨（交办弹窗）=====
  const allEmployees = createMemo(() => state.employees);
  const [showEdict, setShowEdict] = createSignal(false);
  const [edictEmpId, setEdictEmpId] = createSignal(
    localStorage.getItem(LAST_EDICT_EMP_KEY) || "",
  );
  const [edictBrief, setEdictBrief] = createSignal("");
  const [edictBusy, setEdictBusy] = createSignal(false);
  const edictAttach = createImageAttachments({ enableFileDrop: true });
  let edictBriefRef: HTMLTextAreaElement | undefined;

  createEffect(() => {
    const list = allEmployees();
    const cur = edictEmpId();
    if (list.length === 0) return;
    if (!cur || !list.some((e) => e.id === cur)) {
      setEdictEmpId(list[0].id);
    }
  });

  const resizeEdictBrief = () => {
    if (!edictBriefRef) return;
    edictBriefRef.style.height = "auto";
    edictBriefRef.style.height = Math.min(edictBriefRef.scrollHeight, 260) + "px";
  };
  createEffect(() => {
    if (!showEdict()) return;
    edictBrief();
    queueMicrotask(resizeEdictBrief);
  });

  const edictEmpty = () => !edictBrief().trim() && edictAttach.images().length === 0;

  const openEdict = () => {
    if (allEmployees().length === 0) {
      setView("employees");
      return;
    }
    setShowEdict(true);
    queueMicrotask(() => edictBriefRef?.focus());
  };

  const closeEdict = () => {
    if (edictBusy()) return;
    setShowEdict(false);
  };

  const issueEdict = async () => {
    const empId = edictEmpId();
    if (!empId || edictBusy()) return;
    const content = edictBrief().trim();
    const images = edictAttach.images();
    if (!content && images.length === 0) return;
    setEdictBusy(true);
    try {
      await api.registerLedgerItem(empId, content, images);
      localStorage.setItem(LAST_EDICT_EMP_KEY, empId);
      setEdictBrief("");
      edictAttach.clear();
      if (edictBriefRef) edictBriefRef.style.height = "auto";
      setShowEdict(false);
      await refreshMarks();
      await reloadSharedMarks();
    } catch (e) {
      void message(String(e), { kind: "error" });
    } finally {
      setEdictBusy(false);
    }
  };

  const onEdictKeyDown = (ev: KeyboardEvent) => {
    if (ev.key === "Escape") {
      ev.preventDefault();
      closeEdict();
      return;
    }
    if (ev.key === "Enter" && !ev.shiftKey && !ev.isComposing) {
      ev.preventDefault();
      void issueEdict();
    }
  };

  // ===== 进行中事件（门锁账本 + 活跃任务）=====
  const [sharedByScope, setSharedByScope] = createSignal<Record<string, Mark[]>>({});
  const reloadSharedMarks = async () => {
    const sharedEmps = state.employees.filter((e) => e.sharedLedger);
    if (sharedEmps.length === 0) {
      setSharedByScope({});
      return;
    }
    const next: Record<string, Mark[]> = {};
    await Promise.all(
      sharedEmps.map(async (e) => {
        const scope = empScope(e);
        if (next[scope]) return;
        try {
          next[scope] = await api.listSharedMarks(scope);
        } catch {
          next[scope] = [];
        }
      }),
    );
    setSharedByScope(next);
  };
  createEffect(() => {
    void state.employees.length;
    void state.marks.length;
    void reloadSharedMarks();
  });

  const activeEvents = createMemo(() => {
    const events: ActiveEvent[] = [];
    const seenMark = new Set<string>();
    const empName = (id: string) => state.employees.find((e) => e.id === id)?.name ?? id;

    const pushMark = (m: Mark, fallbackName: string, shared: boolean) => {
      if (m.status !== "open" && m.status !== "claimed") return;
      const id = `mark:${m.scope}:${m.key}`;
      if (seenMark.has(id)) return;
      seenMark.add(id);
      const v = markView(m);
      events.push({
        id,
        kind: "mark",
        statusLabel: v.label,
        statusCls: v.cls,
        title: m.title || m.key,
        employeeName: m.ownerName || fallbackName,
        threadId: m.threadId,
        updatedAt: m.updatedAt,
        scope: m.scope,
        markKey: m.key,
        shared,
      });
    };

    for (const e of state.employees) {
      if (e.sharedLedger) {
        for (const m of sharedByScope()[empScope(e)] ?? []) pushMark(m, e.name, true);
      } else {
        const scope = empScope(e);
        for (const m of state.marks) {
          if (m.scope === scope) pushMark(m, e.name, false);
        }
      }
    }

    for (const t of state.employeeTasks) {
      if (t.status !== "queued" && t.status !== "working") continue;
      events.push({
        id: `task:${t.id}`,
        kind: "task",
        statusLabel: t.status === "working" ? "执行中" : "排队中",
        statusCls: t.status === "working" ? "claimed" : "open",
        title: t.title || t.brief || t.id,
        employeeName: empName(t.employeeId),
        threadId: t.threadId,
        updatedAt: t.updatedAt,
        taskId: t.id,
      });
    }

    return events.sort((a, b) => b.updatedAt - a.updatedAt);
  });
  const pagedEvents = createMemo(() => pageSlice(activeEvents(), eventPage()));
  createEffect(() => {
    const pages = pageCount(activeEvents().length);
    if (eventPage() > pages) setEventPage(pages);
  });

  // 从员工档案取岗位说明，帮主管理解「是谁、以什么身份在问」
  const roleOf = (id: string) => {
    const e = state.employees.find((x) => x.id === id);
    const c = (e?.charter || "").trim().replace(/\s+/g, " ");
    if (!c) return "";
    return c.length > 64 ? `${c.slice(0, 64)}…` : c;
  };

  const resolve = async (d: Decision, answer: string) => {
    const ans = answer.trim();
    if (!ans || busy()) return;
    setBusy(d.id);
    try {
      await api.resolveDecision(d.id, ans);
      setDrafts((prev) => {
        const next = { ...prev };
        delete next[d.id];
        return next;
      });
    } catch (e) {
      void message(String(e), { kind: "error" });
    } finally {
      setBusy(null);
    }
  };

  const dismiss = async (d: Decision) => {
    const ok = await confirm(
      d.source === "mind"
        ? `将「${d.employeeName}」的 Mind 奏折留中不发？\n这批心智事件会停止处理，Mind 保持暂停。`
        : `将「${d.employeeName}」的这道折子留中不发？\n不作批示，该员工会立即被唤起，按自己的专业判断推进这张单子。`,
      {
        title: "留中不发",
        kind: "warning",
      },
    );
    if (!ok) return;
    try {
      await api.dismissDecision(d.id);
    } catch (e) {
      void message(String(e), { kind: "error" });
    }
  };

  const reject = async (d: Decision) => {
    if (busy()) return;
    const reason = draftOf(d.id).trim() || "驳回。这张单先不要继续推进。";
    setBusy(d.id);
    try {
      await api.rejectDecision(d.id, reason);
      setDrafts((prev) => {
        const next = { ...prev };
        delete next[d.id];
        return next;
      });
    } catch (e) {
      void message(String(e), { kind: "error" });
    } finally {
      setBusy(null);
    }
  };

  const markRead = async (d: Decision) => {
    if (busy()) return;
    setBusy(d.id);
    try {
      await api.readReport(d.id);
    } catch (e) {
      void message(String(e), { kind: "error" });
    } finally {
      setBusy(null);
    }
  };

  const reviewReport = async (d: Decision) => {
    const answer = draftOf(d.id).trim();
    if (!answer || busy()) return;
    setBusy(d.id);
    try {
      await api.reviewReport(d.id, answer);
      setDrafts((prev) => {
        const next = { ...prev };
        delete next[d.id];
        return next;
      });
    } catch (e) {
      void message(String(e), { kind: "error" });
    } finally {
      setBusy(null);
    }
  };

  const deleteEvent = async (ev: ActiveEvent) => {
    if (busy()) return;
    const ok = await confirm(`删除事件「${ev.title}」？`, {
      title: "删除事件",
      kind: "warning",
    });
    if (!ok) return;
    setBusy(ev.id);
    try {
      if (ev.kind === "mark" && ev.scope && ev.markKey) {
        if (ev.shared) {
          await api.resetSharedMark(ev.scope, ev.markKey, ev.threadId);
          await reloadSharedMarks();
        } else {
          await api.resetMark(ev.scope, ev.markKey);
        }
      } else if (ev.kind === "task" && ev.taskId) {
        await api.deleteTask(ev.taskId);
      }
    } catch (e) {
      void message(String(e), { kind: "error" });
    } finally {
      setBusy(null);
    }
  };

  const deleteReviewed = async (d: Decision) => {
    if (busy()) return;
    const ok = await confirm(`删除「${d.employeeName}」的这条已批阅记录？`, {
      title: "删除记录",
      kind: "warning",
    });
    if (!ok) return;
    setBusy(d.id);
    try {
      await api.deleteDecision(d.id);
    } catch (e) {
      void message(String(e), { kind: "error" });
    } finally {
      setBusy(null);
    }
  };

  return (
    <main class="emp wb">
      <div class="emp-head">
        <div class="emp-head-text">
          <h1 class="emp-title">御书房</h1>
          <p class="emp-sub">在此下旨交办、查看进行中事件、批阅奏折与汇报。日常操作都在这里完成。</p>
        </div>
        <div class="emp-head-actions">
          <div class="wb-stats">
            <span class="wb-stat" classList={{ hot: pending().length > 0 }}>
              候旨 <b>{pending().length}</b>
            </span>
            <span class="wb-stat" classList={{ warm: reports().length > 0 }}>
              待阅汇报 <b>{reports().length}</b>
            </span>
            <span class="wb-stat" classList={{ warm: activeEvents().length > 0 }}>
              进行中 <b>{activeEvents().length}</b>
            </span>
          </div>
          <button
            class="btn primary"
            onClick={openEdict}
            title={allEmployees().length === 0 ? "先配置数字员工" : "下旨交办给员工"}
          >
            <IconSend size={15} />
            下旨
          </button>
        </div>
      </div>

      {/* 进行中事件 */}
      <div class="wb-section-title">
        进行中事件
        <Show when={activeEvents().length > 0}>
          <span class="wb-count warm">{activeEvents().length}</span>
        </Show>
        <span class="wb-section-hint">账本占用与执行中的任务；可打开关联会话查看过程</span>
      </div>
      <Show
        when={activeEvents().length > 0}
        fallback={<div class="emp-hint">当前没有进行中的事件。点右上角「下旨」交办后，单子会出现在这里。</div>}
      >
        <div class="emp-marks wb-events">
          <For each={pagedEvents()}>
            {(ev) => (
              <div class="emp-mark">
                <div class="emp-mark-row">
                  <span class={`emp-mark-status ${ev.statusCls}`}>{ev.statusLabel}</span>
                  <span class="emp-mark-key" title={ev.title}>
                    {ev.title}
                  </span>
                  <span class="emp-mark-owner" title="承办员工">
                    {ev.employeeName}
                  </span>
                  <Show when={ev.scope}>
                    <span class="emp-mark-owner" title="账本 scope">
                      {ev.scope}
                    </span>
                  </Show>
                  <span class="emp-mark-time">{fmtTime(ev.updatedAt)}</span>
                  <span class="emp-mark-actions">
                    <Show when={ev.threadId}>
                      <button
                        class="icon-btn"
                        title="打开关联会话"
                        onClick={() => ev.threadId && void openThread(ev.threadId)}
                      >
                        <IconEye size={14} />
                      </button>
                    </Show>
                    <button
                      class="icon-btn"
                      title="删除此事件"
                      disabled={busy() === ev.id}
                      onClick={() => void deleteEvent(ev)}
                    >
                      <IconTrash size={14} />
                    </button>
                  </span>
                </div>
              </div>
            )}
          </For>
        </div>
        <PageNav page={eventPage()} total={activeEvents().length} onPage={setEventPage} />
      </Show>

      <div class="wb-section-title">
        候旨
        <Show when={pending().length > 0}>
          <span class="wb-count">{pending().length}</span>
        </Show>
      </div>
      <Show
        when={pending().length > 0}
        fallback={<div class="emp-hint">御案上暂无候旨的折子。员工遇到拿不准的大事时会上奏到这里。</div>}
      >
        <div class="wb-list">
          <For each={pagedPending()}>
            {(d) => (
              <div class="wb-card">
                <div class="wb-card-head">
                  <span class={`wb-cat wb-cat-${catOf(d.category).cls}`}>
                    {catOf(d.category).label}
                  </span>
                  <span class="wb-emp">{d.employeeName}{d.source === "mind" ? " · Mind" : d.source === "wake" ? " · Wake" : ""}</span>
                  <span class="wb-mark">{d.taskTitle || d.markKey}</span>
                  <span class="wb-time">{fmtTime(d.createdAt)}</span>
                  <button class="icon-btn" title="留中不发：不作批示，员工自行斟酌推进" onClick={() => void dismiss(d)}>
                    <IconX size={14} />
                  </button>
                </div>
                <div class="wb-meta">
                  <span class="wb-meta-item" title="账本单号（同一单子多轮复用）">单号 {d.markKey}</span>
                  <span class="wb-meta-item" title="协作账本 scope">scope {d.scope}</span>
                  <Show when={d.threadId}>
                    <button
                      class="wb-thread-link"
                      title="打开员工上奏时的会话，查看完整上下文与来龙去脉"
                      onClick={() => d.threadId && void openThread(d.threadId)}
                    >
                      <IconEye size={13} />
                      查看关联对话
                    </button>
                  </Show>
                </div>
                <Show when={roleOf(d.employeeId)}>
                  <div class="wb-role" title="该员工的岗位说明">
                    {roleOf(d.employeeId)}
                  </div>
                </Show>
                <Show when={d.brief?.trim()}>
                  <div class="wb-brief">
                    <span class="wb-label">背景</span>
                    <span class="wb-brief-text">{d.brief}</span>
                  </div>
                </Show>
                <Show when={d.proposedAction?.trim()}>
                  <div class="wb-brief">
                    <span class="wb-label">建议动作</span>
                    <span class="wb-brief-text">{d.proposedAction}</span>
                  </div>
                </Show>
                <div class="wb-q-block">
                  <span class="wb-label wb-label-q">所奏何事</span>
                  <div class="wb-question">{d.question}</div>
                </div>
                <Show when={optionsOf(d).length > 0}>
                  <div class="wb-opt-block">
                    <span class="wb-label">可选批示（点击填入下方，可改后再准奏）</span>
                    <div class="wb-options">
                      <For each={optionsOf(d)}>
                        {(opt) => (
                          <button
                            class="btn small secondary wb-option"
                            classList={{ active: draftOf(d.id).trim() === opt.trim() }}
                            disabled={busy() === d.id}
                            onClick={() => setDraft(d.id, opt)}
                          >
                            <Show when={draftOf(d.id).trim() === opt.trim()}>
                              <IconCheck size={12} />
                            </Show>
                            {opt}
                          </button>
                        )}
                      </For>
                    </div>
                  </div>
                </Show>
                <div class="wb-answer">
                  <textarea
                    class="field-input"
                    rows={2}
                    placeholder="点上方选项填入，或在此直接朱批你的决定… 点「准奏」或 Ctrl+Enter 下旨"
                    value={draftOf(d.id)}
                    onInput={(e) => setDraft(d.id, e.currentTarget.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
                        e.preventDefault();
                        void resolve(d, draftOf(d.id));
                      }
                    }}
                  />
                  <button
                    class="btn primary"
                    disabled={!draftOf(d.id).trim() || busy() === d.id}
                    onClick={() => void resolve(d, draftOf(d.id))}
                  >
                    <IconCheck size={14} />
                    准奏
                  </button>
                  <button
                    class="btn secondary"
                    disabled={busy() === d.id}
                    onClick={() => void dismiss(d)}
                  >
                    留中不发
                  </button>
                  <button
                    class="btn danger"
                    disabled={busy() === d.id}
                    onClick={() => void reject(d)}
                  >
                    <IconX size={14} />
                    驳回
                  </button>
                </div>
              </div>
            )}
          </For>
        </div>
        <PageNav page={pendingPage()} total={pending().length} onPage={setPendingPage} />
      </Show>

      <Show when={reports().length > 0}>
        <div class="wb-section-title">
          完工汇报
          <span class="wb-count warm">{reports().length}</span>
          <span class="wb-section-hint">可直接已读，也可批阅；批示会供 Dream 反思学习</span>
        </div>
        <div class="wb-list">
          <For each={pagedReports()}>
            {(d) => (
              <div class="wb-card report">
                <div class="wb-card-head">
                  <span class="wb-cat wb-cat-report">完工</span>
                  <span class="wb-emp">{d.employeeName}{d.source === "mind" ? " · Mind" : d.source === "wake" ? " · Wake" : ""}</span>
                  <span class="wb-mark">{d.taskTitle || d.markKey}</span>
                  <span class="wb-time">{fmtTime(d.createdAt)}</span>
                  <Show when={d.threadId}>
                    <button
                      class="wb-thread-link"
                      title="打开员工干这单活的会话，查看完整过程"
                      onClick={() => d.threadId && void openThread(d.threadId)}
                    >
                      <IconEye size={13} />
                      查看对话
                    </button>
                  </Show>
                  <button
                    class="btn small primary wb-read-btn"
                    disabled={busy() === d.id}
                    onClick={() => void markRead(d)}
                  >
                    <IconCheck size={13} />
                    已读
                  </button>
                </div>
                <div class="wb-question wb-report-body">{d.question}</div>
                <div class="wb-answer">
                  <textarea
                    class="field-input"
                    rows={2}
                    placeholder="写下对这次完工的评价、纠正或可复用经验"
                    value={draftOf(d.id)}
                    onInput={(e) => setDraft(d.id, e.currentTarget.value)}
                  />
                  <button
                    class="btn small primary"
                    disabled={!draftOf(d.id).trim() || busy() === d.id}
                    onClick={() => void reviewReport(d)}
                  >
                    <IconCheck size={13} />
                    批阅
                  </button>
                </div>
              </div>
            )}
          </For>
        </div>
        <PageNav page={reportPage()} total={reports().length} onPage={setReportPage} />
      </Show>

      <Show when={reviewed().length > 0}>
        <div class="wb-section-title dim">已批阅</div>
        <div class="wb-list">
          <For each={pagedReviewed()}>
            {(d) => (
              <div class="wb-card done">
                <div class="wb-card-head">
                  <span
                    class={`wb-cat wb-cat-${isReportArchive(d) ? "report" : catOf(d.category).cls}`}
                  >
                    {isReportArchive(d) ? "完工" : catOf(d.category).label}
                  </span>
                  <span class="wb-emp">{d.employeeName}{d.source === "mind" ? " · Mind" : d.source === "wake" ? " · Wake" : ""}</span>
                  <span class="wb-mark">{d.taskTitle || d.markKey}</span>
                  <span
                    class={`wb-outcome wb-outcome-${outcomeOf(d.status).cls}`}
                    title={outcomeOf(d.status).hint}
                  >
                    {outcomeOf(d.status).label}
                  </span>
                  <span class="wb-time">{fmtTime(d.resolvedAt || d.createdAt)}</span>
                  <Show when={d.threadId}>
                    <button
                      class="wb-thread-link"
                      title="打开关联会话查看上下文"
                      onClick={() => d.threadId && void openThread(d.threadId)}
                    >
                      <IconEye size={13} />
                      查看对话
                    </button>
                  </Show>
                  <button
                    class="icon-btn"
                    title="删除此记录"
                    disabled={busy() === d.id}
                    onClick={() => void deleteReviewed(d)}
                  >
                    <IconTrash size={14} />
                  </button>
                </div>
                <div class="wb-question">{d.question}</div>
                <Show when={d.answer?.trim()}>
                  <div class="wb-resolved-answer">
                    <span class="wb-answer-badge">朱批</span>
                    {d.answer}
                  </div>
                </Show>
              </div>
            )}
          </For>
        </div>
        <PageNav page={reviewedPage()} total={reviewed().length} onPage={setReviewedPage} />
      </Show>

      <Show when={showEdict()}>
        <div class="modal-backdrop" onClick={closeEdict}>
          <div class="modal wb-edict-modal" onClick={(e) => e.stopPropagation()}>
            <div class="modal-head">
              <span>下旨</span>
              <button class="icon-btn" title="关闭" disabled={edictBusy()} onClick={closeEdict}>
                <IconX size={16} />
              </button>
            </div>
            <div class="modal-body">
              <label class="field">
                <span class="field-label">交办给</span>
                <select
                  class="field-input"
                  value={edictEmpId()}
                  onChange={(e) => setEdictEmpId(e.currentTarget.value)}
                >
                  <For each={allEmployees()}>
                    {(e) => (
                      <option value={e.id}>
                        {e.name}
                        {!e.enabled ? "（已停用）" : ""}
                        {e.heartbeatEnabled === false ? " · 手动" : ""}
                      </option>
                    )}
                  </For>
                </select>
              </label>
              <div
                class="emp-assign wb-edict"
                classList={{ "is-dragging": edictAttach.dragging() }}
              >
                <ImageAttachmentStrip images={edictAttach.images()} onRemove={edictAttach.remove} />
                <textarea
                  ref={edictBriefRef}
                  class="composer-input emp-assign-brief"
                  rows={4}
                  placeholder="写下旨内容，Enter 下旨，Shift+Enter 换行，可粘贴或拖入图片"
                  value={edictBrief()}
                  onInput={(ev) => {
                    setEdictBrief(ev.currentTarget.value);
                    resizeEdictBrief();
                  }}
                  onKeyDown={onEdictKeyDown}
                  onPaste={edictAttach.onPaste}
                />
              </div>
            </div>
            <div class="modal-foot">
              <button class="btn secondary" disabled={edictBusy()} onClick={closeEdict}>
                取消
              </button>
              <button
                class="btn primary"
                disabled={edictEmpty() || edictBusy()}
                onClick={() => void issueEdict()}
              >
                <IconSend size={14} />
                下旨
              </button>
            </div>
          </div>
        </div>
      </Show>
    </main>
  );
}
