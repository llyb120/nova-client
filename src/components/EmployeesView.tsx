import { confirm, message } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { api } from "../ipc";
import {
  enabledAgentKinds,
  ensureModelOptions,
  modeChoices,
  refreshEmployees,
  resolveAvailableModel,
  setView,
  state,
} from "../store";
import type {
  AgentKind,
  Employee,
  EmployeeJournalEntry,
  MindSnapshot,
  Partner,
  WorkHours,
} from "../types";
import { agentLabel } from "../utils";
import { ConfigSelects, ModelPicker } from "./ConfigSelects";
import {
  IconCheck,
  IconGear,
  IconPlus,
  IconThumbDown,
  IconThumbUp,
  IconTrash,
  IconX,
} from "./icons";

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

function fmtHeartbeat(secs: number): string {
  if (secs >= 3600 && secs % 3600 === 0) return `每 ${secs / 3600} 小时`;
  if (secs >= 60 && secs % 60 === 0) return `每 ${secs / 60} 分钟`;
  return `每 ${secs} 秒`;
}

const MIND_STATUS: Record<string, string> = {
  idle: "空闲",
  running: "做梦中",
  preempting: "让出工作",
  cooldown: "冷却",
  paused: "已暂停",
};

const HEARTBEAT_PRESETS = [
  { label: "1 分钟", v: 60 },
  { label: "5 分钟", v: 300 },
  { label: "15 分钟", v: 900 },
  { label: "1 小时", v: 3600 },
];

const PAGE_SIZE = 8;

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

/** 星期：1=周一 … 7=周日（与后端一致） */
const WEEKDAYS = [
  { v: 1, label: "一" },
  { v: 2, label: "二" },
  { v: 3, label: "三" },
  { v: 4, label: "四" },
  { v: 5, label: "五" },
  { v: 6, label: "六" },
  { v: 7, label: "日" },
];

function hmToMin(s: string): number | null {
  const m = /^(\d{1,2}):(\d{2})$/.exec(s.trim());
  if (!m) return null;
  const h = Number(m[1]);
  const mm = Number(m[2]);
  if (h > 23 || mm > 59) return null;
  return h * 60 + mm;
}

/** 当前是否在员工上班时段内（与后端 within_work_hours 逻辑一致，用于「休眠」提示） */
function withinWorkHours(wh: WorkHours | null | undefined): boolean {
  if (!wh) return true;
  const now = new Date();
  if (wh.days && wh.days.length > 0) {
    const dow = now.getDay() === 0 ? 7 : now.getDay(); // JS: 0=周日 → 7
    if (!wh.days.includes(dow)) return false;
  }
  const start = hmToMin(wh.start);
  const end = hmToMin(wh.end);
  if (start === null || end === null) return true;
  const cur = now.getHours() * 60 + now.getMinutes();
  if (start === end) return true;
  return start < end ? cur >= start && cur < end : cur >= start || cur < end;
}

/** 上班时间摘要文案，如「09:00–18:00 · 周一至周五」 */
function workHoursSummary(wh: WorkHours | null | undefined): string {
  if (!wh) return "";
  const days =
    !wh.days || wh.days.length === 0 || wh.days.length === 7
      ? "每天"
      : wh.days
          .slice()
          .sort((a, b) => a - b)
          .map((d) => `周${WEEKDAYS.find((w) => w.v === d)?.label ?? d}`)
          .join("、");
  return `${wh.start}–${wh.end} · ${days}`;
}

/** 清洗讨论伙伴：去空、去重、trim；remote 需有员工名，local 丢弃 peer */
function normalizePartners(list: Partner[]): Partner[] {
  const out: Partner[] = [];
  const seen = new Set<string>();
  for (const p of list) {
    const name = p.name.trim();
    if (!name) continue;
    if (p.kind === "remote") {
      const peer = (p.peer ?? "").trim();
      const key = `remote:${peer}:${name}`;
      if (seen.has(key)) continue;
      seen.add(key);
      out.push({ kind: "remote", name, peer });
    } else {
      const key = `local:${name}`;
      if (seen.has(key)) continue;
      seen.add(key);
      out.push({ kind: "local", name });
    }
  }
  return out;
}

/** 记忆排序：长期知识（pinned）在前，其余按时间倒序 */
function sortMemory(list: EmployeeJournalEntry[]): EmployeeJournalEntry[] {
  return [...list].sort((a, b) => {
    if (a.pinned !== b.pinned) return a.pinned ? -1 : 1;
    return b.ts - a.ts;
  });
}

type MemoryTab = "memory" | "knowledge" | "lessons" | "user";

const MEMORY_TABS: { id: MemoryTab; label: string }[] = [
  { id: "memory", label: "记忆" },
  { id: "knowledge", label: "知识" },
  { id: "lessons", label: "经验守则" },
  { id: "user", label: "人工知识" },
];

function memoryTabOf(m: EmployeeJournalEntry): MemoryTab | null {
  if (m.source === "user" || m.kind === "knowledge:user" || m.kind === "memory:user") return "user";
  if (m.kind?.startsWith("lesson")) return "lessons";
  if (m.kind === "knowledge" || (m.pinned && !m.kind?.startsWith("lesson"))) return "knowledge";
  if (!m.kind || m.kind === "memory") return "memory";
  return null;
}

/** 记忆条目徽章：区分知识 / 守则（试行·已内化）/ 自动留痕的经历 */
function memBadge(m: EmployeeJournalEntry): { label: string; cls: string } | null {
  if (memoryTabOf(m) === "user") return { label: "人工知识", cls: "user" };
  if (m.kind === "lesson:retired") return { label: "守则·已退役", cls: "lesson retired" };
  if (m.kind === "lesson:challenged") return { label: "守则·受挑战", cls: "lesson challenged" };
  if (m.kind === "lesson") {
    return m.pinned
      ? { label: "守则·已内化", cls: "lesson" }
      : { label: `守则·试行${m.evidence ? `·验证${m.evidence}次` : ""}`, cls: "lesson trial" };
  }
  if (m.pinned || m.kind === "knowledge") return { label: "员工知识", cls: "" };
  if (!m.kind || m.kind === "memory") return { label: "员工记忆", cls: "memory" };
  return null;
}

const LAST_EMP_KEY = "fd:lastEmployee";

export function EmployeesView() {
  // 记住上次点击的员工：初始即选中他，下次进来直接续上
  const [selectedId, setSelectedId] = createSignal<string | null>(
    localStorage.getItem(LAST_EMP_KEY),
  );
  const selectEmp = (id: string) => {
    setSelectedId(id);
    localStorage.setItem(LAST_EMP_KEY, id);
  };
  const selected = createMemo(() => state.employees.find((e) => e.id === selectedId()) ?? null);
  const [mind, setMind] = createSignal<MindSnapshot | null>(null);

  const reloadMind = () => {
    const id = selectedId();
    if (!id) {
      setMind(null);
      return;
    }
    void api
      .getEmployeeMind(id)
      .then((value) => {
        setMind(value);
      })
      .catch(() => setMind(null));
  };

  createEffect(() => {
    selectedId();
    reloadMind();
  });

  let disposed = false;
  let unlistenMind: (() => void) | undefined;
  void listen<{ employeeId?: string }>("mind:changed", (event) => {
    const id = selectedId();
    if (!event.payload.employeeId || event.payload.employeeId === id) reloadMind();
  }).then((unlisten) => {
    if (disposed) unlisten();
    else unlistenMind = unlisten;
  });
  onCleanup(() => {
    disposed = true;
    unlistenMind?.();
  });

  // 自动选中：上次记住的员工不存在（或没记过）则回退到首个员工；选中的被删同理
  createEffect(() => {
    const list = state.employees;
    const cur = selectedId();
    if (list.length === 0) {
      if (cur !== null) setSelectedId(null);
      return;
    }
    if (!cur || !list.some((e) => e.id === cur)) setSelectedId(list[0].id);
  });

  // 每分钟 tick 一次，让「休眠中」标记随上班时间边界自动出现/消失
  const [nowTick, setNowTick] = createSignal(Date.now());
  const timer = setInterval(() => setNowTick(Date.now()), 60_000);
  onCleanup(() => clearInterval(timer));
  const isSleeping = (e: Employee) => {
    nowTick(); // 建立响应式依赖
    return e.enabled && !!e.workHours && !withinWorkHours(e.workHours);
  };

  // ===== 新建 / 编辑员工弹窗 =====
  const [showForm, setShowForm] = createSignal(false);
  const [editingId, setEditingId] = createSignal<string | null>(null);
  const [fName, setFName] = createSignal("");
  const [fAgent, setFAgent] = createSignal<AgentKind>("devin");
  const [fModel, setFModel] = createSignal("");
  const [fHeartbeatAgent, setFHeartbeatAgent] = createSignal<AgentKind>("devin");
  const [fHeartbeatModel, setFHeartbeatModel] = createSignal("");
  const [fMindAgent, setFMindAgent] = createSignal<AgentKind>("devin");
  const [fMindModel, setFMindModel] = createSignal("");
  const [fCharter, setFCharter] = createSignal("");
  const [fHeartbeatOn, setFHeartbeatOn] = createSignal(true);
  const [fHeartbeat, setFHeartbeat] = createSignal(300);
  // 默认设置上班时间（工作日 09:00-18:00），避免新员工一上来就 7×24 自主跑；需要 7×24 再取消勾选
  const [fWorkEnabled, setFWorkEnabled] = createSignal(true);
  const [fWorkStart, setFWorkStart] = createSignal("09:00");
  const [fWorkEnd, setFWorkEnd] = createSignal("18:00");
  const [fWorkDays, setFWorkDays] = createSignal<number[]>([1, 2, 3, 4, 5]);
  const [fEnabled, setFEnabled] = createSignal(true);
  const [fAllowWorktree, setFAllowWorktree] = createSignal(false);
  const [fMode, setFMode] = createSignal("");
  const [fDirective, setFDirective] = createSignal("");
  const [fMarkScope, setFMarkScope] = createSignal("");
  const [fSharedLedger, setFSharedLedger] = createSignal(false);
  const [fPartners, setFPartners] = createSignal<Partner[]>([]);
  const [fBusy, setFBusy] = createSignal(false);
  const [fErr, setFErr] = createSignal("");

  // 讨论伙伴编辑：本机同事按名字勾选；跨机队友录入 relay 展示名 + 员工名
  const toggleLocalPartner = (name: string) => {
    setFPartners((prev) => {
      const exists = prev.some((p) => p.kind === "local" && p.name === name);
      return exists
        ? prev.filter((p) => !(p.kind === "local" && p.name === name))
        : [...prev, { kind: "local", name }];
    });
  };
  const isLocalPartner = (name: string) =>
    fPartners().some((p) => p.kind === "local" && p.name === name);
  const remotePartners = () => fPartners().filter((p) => p.kind === "remote");
  const addRemotePartner = () =>
    setFPartners((prev) => [...prev, { kind: "remote", name: "", peer: "" }]);
  const updateRemotePartner = (idx: number, patch: Partial<Partner>) => {
    let seen = -1;
    setFPartners((prev) =>
      prev.map((p) => {
        if (p.kind !== "remote") return p;
        seen += 1;
        return seen === idx ? { ...p, ...patch } : p;
      }),
    );
  };
  const removeRemotePartner = (idx: number) => {
    let seen = -1;
    setFPartners((prev) =>
      prev.filter((p) => {
        if (p.kind !== "remote") return true;
        seen += 1;
        return seen !== idx;
      }),
    );
  };

  // 表单：切后端时拉取模型列表；空值才落到可用项，已有选择即使暂不在列表也保留
  createEffect(() => {
    if (!showForm()) return;
    const k = fAgent();
    void ensureModelOptions(k);
    const nextModel = resolveAvailableModel(k, fModel());
    if (nextModel !== fModel()) setFModel(nextModel);
    // 巡查/心跳模型与工作模型用同一个（新会话）选择器，可独立选后端
    void ensureModelOptions(fHeartbeatAgent());
    const nextHb = resolveAvailableModel(fHeartbeatAgent(), fHeartbeatModel());
    if (nextHb !== fHeartbeatModel()) setFHeartbeatModel(nextHb);
    void ensureModelOptions(fMindAgent());
    const nextMind = resolveAvailableModel(fMindAgent(), fMindModel());
    if (nextMind !== fMindModel()) setFMindModel(nextMind);
    // 运行权限必须是该后端真实支持的模式，否则会话创建/设置模式时后端会报错（巡查出错的根因）。
    // 与新会话一致：空值/非法值（如把 Devin 的 accept-edits 用到 Codex 上）回退到第一项，
    // 保证所见即所存、且始终把合法 mode 发给后端。
    const modes = modeChoices(k);
    if (modes.length > 0 && !modes.some((m) => m.id === fMode())) {
      setFMode(modes[0].id);
    }
  });

  const resetForm = () => {
    setFName("");
    setFAgent(enabledAgentKinds()[0] ?? "devin");
    setFModel("");
    setFHeartbeatAgent(enabledAgentKinds()[0] ?? "devin");
    setFHeartbeatModel("");
    setFMindAgent(enabledAgentKinds()[0] ?? "devin");
    setFMindModel("");
    setFCharter("");
    setFHeartbeatOn(true);
    setFHeartbeat(300);
    setFWorkEnabled(true);
    setFWorkStart("09:00");
    setFWorkEnd("18:00");
    setFWorkDays([1, 2, 3, 4, 5]);
    setFEnabled(true);
    setFAllowWorktree(false);
    setFMode("");
    setFDirective("");
    setFMarkScope("");
    setFSharedLedger(false);
    setFPartners([]);
    setFErr("");
  };

  const openCreate = () => {
    setEditingId(null);
    resetForm();
    setShowForm(true);
  };

  const openEdit = (e: Employee) => {
    setEditingId(e.id);
    setFName(e.name);
    setFAgent(e.agentKind);
    setFModel(e.model ?? "");
    setFHeartbeatAgent(e.heartbeatAgentKind ?? e.agentKind);
    setFHeartbeatModel(e.heartbeatModel ?? "");
    setFMindAgent(e.mindAgentKind ?? e.learningAgentKind ?? e.heartbeatAgentKind ?? e.agentKind);
    setFMindModel(e.mindModel ?? e.learningModel ?? e.heartbeatModel ?? "");
    setFCharter(e.charter);
    setFHeartbeatOn(e.heartbeatEnabled !== false);
    setFHeartbeat(e.heartbeatSecs);
    setFWorkEnabled(!!e.workHours);
    setFWorkStart(e.workHours?.start || "09:00");
    setFWorkEnd(e.workHours?.end || "18:00");
    setFWorkDays(e.workHours?.days?.length ? [...e.workHours.days] : [1, 2, 3, 4, 5]);
    setFEnabled(e.enabled);
    setFAllowWorktree(e.allowWorktree ?? false);
    setFMode(e.mode ?? "");
    setFDirective(e.directive);
    setFMarkScope(e.markScope);
    setFSharedLedger(e.sharedLedger);
    setFPartners((e.partners ?? []).map((p) => ({ ...p })));
    setFErr("");
    setShowForm(true);
  };

  const saveForm = async () => {
    if (fBusy()) return;
    const name = fName().trim();
    if (!name) {
      setFErr("请填写员工名字");
      return;
    }
    setFBusy(true);
    setFErr("");
    try {
      const editId = editingId();
      const mode = fMode().trim() || null;
      const model = fModel().trim() || null;
      const heartbeatAgentKind = fHeartbeatAgent();
      const heartbeatModel = fHeartbeatModel().trim() || null;
      const mindAgentKind = fMindAgent();
      const mindModel = fMindModel().trim() || null;
      const directive = fDirective().trim();
      const markScope = fMarkScope().trim();
      const sharedLedger = fSharedLedger();
      const partners = normalizePartners(fPartners());
      // 上班时间只对心跳自主行动有意义：不开心跳 = 不存上班时间（详情页也不再展示）。
      const workHours: WorkHours | null =
        fHeartbeatOn() && fWorkEnabled()
          ? {
              start: fWorkStart().trim() || "09:00",
              end: fWorkEnd().trim() || "18:00",
              days: fWorkDays().slice().sort((a, b) => a - b),
            }
          : null;
      if (editId) {
        const base = state.employees.find((e) => e.id === editId);
        if (!base) throw new Error("员工不存在");
        await api.updateEmployee({
          ...base,
          name,
          cwd: "",
          agentKind: fAgent(),
          model,
          heartbeatAgentKind,
          heartbeatModel,
          mindAgentKind,
          mindModel,
          mode,
          charter: fCharter().trim(),
          heartbeatEnabled: fHeartbeatOn(),
          heartbeatSecs: Math.max(10, Math.round(fHeartbeat())),
          workHours,
          enabled: fEnabled(),
          allowWorktree: fAllowWorktree(),
          directive,
          markScope,
          sharedLedger,
          partners,
        });
        await refreshEmployees();
        selectEmp(editId);
      } else {
        const emp = await api.createEmployee({
          name,
          agentKind: fAgent(),
          model,
          heartbeatAgentKind,
          heartbeatModel,
          mindAgentKind,
          mindModel,
          mode,
          charter: fCharter().trim(),
          cwd: "",
          heartbeatEnabled: fHeartbeatOn(),
          heartbeatSecs: Math.max(10, Math.round(fHeartbeat())),
          workHours,
          enabled: fEnabled(),
          allowWorktree: fAllowWorktree(),
          directive,
          markScope,
          sharedLedger,
          partners,
        });
        await refreshEmployees();
        selectEmp(emp.id);
      }
      setShowForm(false);
    } catch (e) {
      setFErr(String(e));
    } finally {
      setFBusy(false);
    }
  };

  // ===== 记忆 / 知识库（选中员工 / 任务变化时刷新）=====
  const [memory, setMemory] = createSignal<EmployeeJournalEntry[]>([]);
  const reloadMemory = () => {
    const id = selectedId();
    if (!id) {
      setMemory([]);
      return;
    }
    void api
      .getEmployeeMemory(id)
      .then((m) => setMemory(sortMemory(m)))
      .catch(() => setMemory([]));
  };
  createEffect(() => {
    selectedId();
    // 依赖任务列表：任务完成后记忆会追加
    void state.employeeTasks.length;
    reloadMemory();
  });
  const [memoryTab, setMemoryTab] = createSignal<MemoryTab>("memory");
  const [memoryPage, setMemoryPage] = createSignal(1);
  const filteredMemory = createMemo(() => {
    const tab = memoryTab();
    return memory().filter((m) => memoryTabOf(m) === tab);
  });
  const memoryTabCount = (tab: MemoryTab) =>
    memory().filter((m) => memoryTabOf(m) === tab).length;
  const pagedMemory = createMemo(() => pageSlice(filteredMemory(), memoryPage()));

  // 记忆条目编辑 / 新增知识
  const [editTs, setEditTs] = createSignal<number | null>(null);
  const [editText, setEditText] = createSignal("");
  const [showAddKnow, setShowAddKnow] = createSignal(false);
  const [knowText, setKnowText] = createSignal("");

  const startEditMem = (m: EmployeeJournalEntry) => {
    setEditTs(m.ts);
    setEditText(m.summary);
  };
  const cancelEditMem = () => {
    setEditTs(null);
    setEditText("");
  };
  const saveEditMem = async (m: EmployeeJournalEntry) => {
    const id = selectedId();
    if (!id) return;
    try {
      await api.updateEmployeeMemory(id, m.ts, editText().trim());
      cancelEditMem();
      reloadMemory();
    } catch (e) {
      void message(String(e), { kind: "error" });
    }
  };
  const deleteMem = async (m: EmployeeJournalEntry) => {
    const id = selectedId();
    if (!id) return;
    const ok = await confirm("删除这条记忆？", { title: "删除记忆", kind: "warning" });
    if (!ok) return;
    try {
      await api.deleteEmployeeMemory(id, m.ts);
      reloadMemory();
    } catch (e) {
      void message(String(e), { kind: "error" });
    }
  };
  const togglePin = async (m: EmployeeJournalEntry) => {
    const id = selectedId();
    if (!id) return;
    try {
      await api.setEmployeeMemoryPinned(id, m.ts, !m.pinned);
      reloadMemory();
    } catch (e) {
      void message(String(e), { kind: "error" });
    }
  };
  const setMemoryFeedback = async (m: EmployeeJournalEntry, feedback: -1 | 1) => {
    const id = selectedId();
    if (!id) return;
    const next = m.userFeedback === feedback ? 0 : feedback;
    try {
      await api.setEmployeeMemoryFeedback(id, m.ts, next);
      reloadMemory();
    } catch (e) {
      void message(String(e), { kind: "error" });
    }
  };
  const addKnowledge = async () => {
    const id = selectedId();
    if (!id) return;
    const txt = knowText().trim();
    if (!txt) return;
    try {
      // 无需标题：后端会用内容首行/「知识」作为默认标题
      await api.addEmployeeMemory(id, "", txt, true);
      setKnowText("");
      setShowAddKnow(false);
      setMemoryTab("user");
      reloadMemory();
    } catch (e) {
      void message(String(e), { kind: "error" });
    }
  };

  createEffect(() => {
    selectedId();
    setMemoryPage(1);
    setMemoryTab("memory");
  });
  createEffect(() => {
    memoryTab();
    const pages = pageCount(filteredMemory().length);
    if (memoryPage() > pages) setMemoryPage(pages);
  });

  const removeEmployee = async (e: Employee) => {
    const ok = await confirm(`解雇员工「${e.name}」？其任务与工作记忆将一并删除（不影响已产生的会话）。`, {
      title: "解雇员工",
      kind: "warning",
    });
    if (ok) {
      try {
        await api.deleteEmployee(e.id);
      } catch (err) {
        void message(String(err), { kind: "error" });
      }
    }
  };

  const toggleEnabled = async (e: Employee) => {
    try {
      await api.setEmployeeEnabled(e.id, !e.enabled);
    } catch (err) {
      void message(String(err), { kind: "error" });
    }
  };

  const empScope = (e: Employee) => (e.markScope.trim() ? e.markScope.trim() : `emp:${e.id}`);

  const toggleMind = async () => {
    const current = mind();
    if (!current) return;
    try {
      await api.setEmployeeMindEnabled(current.employeeId, !current.enabled);
      reloadMind();
    } catch (e) {
      void message(String(e), { kind: "error" });
    }
  };

  const resumeMind = async () => {
    const id = selectedId();
    if (!id) return;
    try {
      await api.resumeEmployeeMind(id);
      reloadMind();
    } catch (e) {
      void message(String(e), { kind: "error" });
    }
  };

  // 账本 scope 建议：汇总已有员工与账本里出现过的 scope，方便多员工填相同值
  const scopeSuggestions = createMemo(() => {
    const set = new Set<string>();
    for (const e of state.employees) if (e.markScope.trim()) set.add(e.markScope.trim());
    for (const m of state.marks) if (!m.scope.startsWith("emp:")) set.add(m.scope);
    return [...set].sort();
  });

  // 讨论伙伴候选：本机其他员工（按名字）；跨机队友取在线名单展示名
  const otherEmployeeNames = createMemo(() =>
    state.employees.filter((e) => e.id !== editingId()).map((e) => e.name),
  );
  const peerNameSuggestions = createMemo(() => {
    const me = state.settings?.relayToken ?? "";
    return state.peers.filter((p) => p.token !== me).map((p) => p.name);
  });

  return (
    <main class="emp">
      <div class="emp-head">
        <div class="emp-head-text">
          <h1 class="emp-title">数字员工</h1>
          <p class="emp-sub">配置岗位、心跳、模型与知识库。日常交办与批阅请到御书房。</p>
        </div>
        <div class="emp-head-actions">
          <button class="btn secondary" onClick={() => setView("workbench")} title="前往御书房下旨与批阅">
            去御书房
          </button>
          <button class="btn primary" onClick={openCreate}>
            <IconPlus size={15} />
            新建员工
          </button>
        </div>
      </div>

      <Show
        when={state.employees.length > 0}
        fallback={
          <div class="emp-empty">
            <p>还没有数字员工。</p>
            <p class="emp-empty-sub">新建一名员工，配置岗位说明书和常驻职责。工作目录会在每次 Wake 时动态确定。</p>
            <button class="btn primary big" onClick={openCreate}>
              <IconPlus size={16} />
              新建第一名员工
            </button>
          </div>
        }
      >
        <div class="emp-body">
          {/* 左：员工列表 */}
          <div class="emp-list">
            <For each={state.employees}>
              {(e) => (
                <button
                  class="emp-card"
                  classList={{ active: e.id === selectedId() }}
                  onClick={() => selectEmp(e.id)}
                >
                  <span class={`emp-dot ${e.enabled ? "on" : "off"}`} title={e.enabled ? "在岗" : "休息"} />
                  <span class="emp-card-main">
                    <span class="emp-card-name">{e.name}</span>
                    <span class="emp-card-meta">
                      <span class={`agent-badge ${e.agentKind}`}>{agentLabel(e.agentKind)}</span>
                      <Show
                        when={e.heartbeatEnabled !== false}
                        fallback={
                          <span class="emp-card-hb" title="自动心跳已关闭，只在御书房交办或朱批时行动">
                            手动
                          </span>
                        }
                      >
                        <Show when={isSleeping(e)} fallback={<span class="emp-card-hb">{fmtHeartbeat(e.heartbeatSecs)}</span>}>
                          <span class="emp-card-hb emp-card-sleep">休眠中</span>
                        </Show>
                      </Show>
                    </span>
                  </span>
                </button>
              )}
            </For>
          </div>

          {/* 右：员工详情 */}
          <Show
            when={selected()}
            fallback={<div class="emp-detail-empty">选择一名员工查看详情</div>}
          >
            {(emp) => (
              <div class="emp-detail">
                <div class="emp-detail-head">
                  <div class="emp-detail-title">
                    <span class={`emp-dot ${emp().enabled ? "on" : "off"}`} />
                    <span class="emp-detail-name">{emp().name}</span>
                    <span class={`agent-badge ${emp().agentKind}`}>{agentLabel(emp().agentKind)}</span>
                    <span class="emp-detail-cwd" title="普通模式沿用当前项目；其他模式由 Wake 查找">
                      动态工作目录
                    </span>
                    <Show
                      when={emp().heartbeatEnabled !== false}
                      fallback={
                        <span
                          class="emp-detail-hb"
                          title="自动心跳已关闭：不巡查、不自动领活，只在御书房交办或朱批时行动"
                        >
                          手动指派
                        </span>
                      }
                    >
                      <span class="emp-detail-hb">{fmtHeartbeat(emp().heartbeatSecs)}</span>
                    </Show>
                    <Show when={emp().workHours && emp().heartbeatEnabled !== false}>
                      <span class="emp-detail-hb" title="上班时间（时段外自动休眠）">
                        {workHoursSummary(emp().workHours)}
                      </span>
                    </Show>
                    <Show when={isSleeping(emp()) && emp().heartbeatEnabled !== false}>
                      <span
                        class="emp-sleep-badge"
                        title="当前不在上班时段，员工休眠中"
                      >
                        休眠中
                      </span>
                    </Show>
                  </div>
                  <div class="emp-detail-actions">
                    <button
                      class="btn small"
                      classList={{ primary: !emp().enabled, secondary: emp().enabled }}
                      onClick={() => void toggleEnabled(emp())}
                      title={emp().enabled ? "停用该员工（配置）" : "启用该员工（配置）"}
                    >
                      {emp().enabled ? "停用" : "启用"}
                    </button>
                    <button class="icon-btn" title="编辑员工" onClick={() => openEdit(emp())}>
                      <IconGear size={15} />
                    </button>
                    <button class="icon-btn" title="解雇员工" onClick={() => void removeEmployee(emp())}>
                      <IconTrash size={15} />
                    </button>
                  </div>
                </div>

                <Show when={emp().charter.trim()}>
                  <div class="emp-charter" title={emp().charter}>
                    {emp().charter}
                  </div>
                </Show>

                <div class="emp-directive">
                  <div class="emp-directive-head">
                    <span class="emp-directive-badge">常驻职责</span>
                    <span class="emp-directive-scope" title="协作账本命名空间：同 scope 的员工共享去重/互斥/接力">
                      账本 scope：{empScope(emp())}
                    </span>
                    <Show when={(emp().partners ?? []).length > 0}>
                      <span class="emp-directive-scope" title="开发中需要协议约定时自动联动的伙伴">
                        讨论伙伴：
                        {(emp().partners ?? [])
                          .map((p) =>
                            p.kind === "remote"
                              ? `${p.name}${p.peer ? `@${p.peer}` : ""}`
                              : p.name,
                          )
                          .join("、")}
                      </span>
                    </Show>
                  </div>
                  <div class="emp-directive-body">
                    {emp().directive.trim() || "（未填写常驻职责，点右上齿轮编辑补充）"}
                  </div>
                </div>

                <Show when={mind()}>
                  {(m) => (
                    <div class="emp-mind">
                      <div class="emp-mind-head">
                        <span class="emp-section-title">Dream</span>
                        <span class={`emp-mind-status ${m().status}`}>
                          {MIND_STATUS[m().status] ?? m().status}
                        </span>
                        <span class="emp-mind-meta">待处理 {m().pendingEvents}</span>
                        <span class="emp-mind-meta">
                          记忆 {m().memoryEntries} / {m().journalLimit}
                          <Show when={m().protectedMemoryEntries > 0}>
                            {" "}· 保护 {m().protectedMemoryEntries}
                          </Show>
                        </span>
                        <span class="emp-mind-actions">
                          <Show
                            when={m().status === "paused" || m().status === "cooldown"}
                            fallback={
                              <button class="btn tiny secondary" onClick={() => void toggleMind()}>
                                {m().enabled ? "暂停" : "启用"}
                              </button>
                            }
                          >
                            <button class="btn tiny secondary" onClick={() => void resumeMind()}>
                              恢复
                            </button>
                          </Show>
                        </span>
                      </div>
                      <Show when={m().lastSummary || m().lastError}>
                        <div class="emp-mind-body">
                          <Show when={m().lastSummary}>
                            <span>{m().lastSummary}</span>
                          </Show>
                          <Show when={m().lastError}>
                            <span class="emp-mind-error">{m().lastError}</span>
                          </Show>
                        </div>
                      </Show>
                    </div>
                  )}
                </Show>

                {/* 记忆 / 知识库 */}
                <div class="emp-section-title">
                  记忆 / 知识库
                  <span class="emp-section-hint">只保存可复用结论，过程留在会话里</span>
                  <button
                    class="btn tiny secondary emp-mem-add"
                    onClick={() => {
                      setShowAddKnow((v) => !v);
                      setKnowText("");
                      setMemoryTab("user");
                    }}
                  >
                    <IconPlus size={12} />
                    添加知识
                  </button>
                </div>

                <Show when={showAddKnow()}>
                  <div class="emp-know-form">
                    <textarea
                      class="field-input"
                      rows={3}
                      placeholder="写清适用条件和知识结论。人工知识会独立保存，并按任务相关性检索。"
                      value={knowText()}
                      onInput={(e) => setKnowText(e.currentTarget.value)}
                    />
                    <div class="emp-know-actions">
                      <button class="btn tiny secondary" onClick={() => setShowAddKnow(false)}>
                        取消
                      </button>
                      <button
                        class="btn tiny primary"
                        disabled={!knowText().trim()}
                        onClick={() => void addKnowledge()}
                      >
                        保存为知识
                      </button>
                    </div>
                  </div>
                </Show>

                <div class="emp-memory-tabs" role="tablist" aria-label="记忆分类">
                  <For each={MEMORY_TABS}>
                    {(tab) => (
                      <button
                        type="button"
                        role="tab"
                        aria-selected={memoryTab() === tab.id}
                        classList={{
                          "emp-memory-tab": true,
                          active: memoryTab() === tab.id,
                        }}
                        onClick={() => {
                          setMemoryTab(tab.id);
                          setMemoryPage(1);
                          cancelEditMem();
                        }}
                      >
                        {tab.label}
                        <span>{memoryTabCount(tab.id)}</span>
                      </button>
                    )}
                  </For>
                </div>

                <Show
                  when={filteredMemory().length > 0}
                  fallback={
                    <div class="emp-hint">
                      这个分类还没有内容。
                    </div>
                  }
                >
                  <div class="emp-memory">
                    <For each={pagedMemory()}>
                      {(m) => (
                        <div class="emp-mem" classList={{ pinned: m.pinned }}>
                          <div class="emp-mem-head">
                            <Show when={memBadge(m)}>
                              {(b) => <span class={`emp-mem-badge ${b().cls}`}>{b().label}</span>}
                            </Show>
                            <span class="emp-mem-task">{m.taskTitle}</span>
                            <span class="emp-mem-time">{fmtTime(m.ts)}</span>
                            <span class="emp-mem-actions">
                              <Show when={memoryTabOf(m) !== "user"}>
                                <button
                                  class="icon-btn emp-mem-feedback"
                                  classList={{ active: m.userFeedback === 1, positive: true }}
                                  title="点赞：Dream 会优先保留和强化"
                                  onClick={() => void setMemoryFeedback(m, 1)}
                                >
                                  <IconThumbUp size={13} />
                                </button>
                                <button
                                  class="icon-btn emp-mem-feedback"
                                  classList={{ active: m.userFeedback === -1, negative: true }}
                                  title="点踩：Dream 会优先复核、修正或降级"
                                  onClick={() => void setMemoryFeedback(m, -1)}
                                >
                                  <IconThumbDown size={13} />
                                </button>
                              </Show>
                              <button
                                class="icon-btn"
                                title={m.pinned ? "取消长期知识" : "设为长期知识"}
                                onClick={() => void togglePin(m)}
                              >
                                <IconCheck size={13} />
                              </button>
                              <button
                                class="icon-btn"
                                title="编辑"
                                onClick={() => (editTs() === m.ts ? cancelEditMem() : startEditMem(m))}
                              >
                                <IconGear size={13} />
                              </button>
                              <button class="icon-btn" title="删除" onClick={() => void deleteMem(m)}>
                                <IconTrash size={13} />
                              </button>
                            </span>
                          </div>
                          <Show
                            when={editTs() === m.ts}
                            fallback={<div class="emp-mem-summary">{m.summary}</div>}
                          >
                            <textarea
                              class="field-input emp-mem-edit"
                              rows={4}
                              value={editText()}
                              onInput={(e) => setEditText(e.currentTarget.value)}
                            />
                            <div class="emp-know-actions">
                              <button class="btn tiny secondary" onClick={cancelEditMem}>
                                取消
                              </button>
                              <button class="btn tiny primary" onClick={() => void saveEditMem(m)}>
                                保存
                              </button>
                            </div>
                          </Show>
                        </div>
                      )}
                    </For>
                  </div>
                  <PageNav page={memoryPage()} total={filteredMemory().length} onPage={setMemoryPage} />
                </Show>
              </div>
            )}
          </Show>
        </div>
      </Show>

      {/* 新建 / 编辑员工弹窗 */}
      <Show when={showForm()}>
        <div class="modal-backdrop" onClick={() => setShowForm(false)}>
          <div class="modal emp-form" onClick={(e) => e.stopPropagation()}>
            <div class="modal-head">
              <span>{editingId() ? "编辑员工" : "新建员工"}</span>
              <button class="icon-btn" onClick={() => setShowForm(false)}>
                <IconX size={16} />
              </button>
            </div>
            <div class="modal-body">
              <label class="field">
                <span class="field-label">名字</span>
                <input
                  class="field-input"
                  placeholder="如「后端小助手」"
                  value={fName()}
                  onInput={(e) => setFName(e.currentTarget.value)}
                />
              </label>

              <label class="field">
                <span class="field-label">工作模型与运行权限</span>
                <div class="emp-config-row">
                  <ConfigSelects
                    agentKind={fAgent()}
                    agentKinds={enabledAgentKinds()}
                    model={fModel()}
                    mode={fMode()}
                    onPickModel={(k, m) => {
                      setFAgent(k);
                      setFModel(m);
                    }}
                    onMode={setFMode}
                    portal
                  />
                </div>
                <span class="emp-field-hint">
                  和新会话完全一样：模型下拉里直接选后端与模型；无人值守建议权限选「全自动 ·
                  Bypass」，员工执行时不再弹确认。
                </span>
              </label>

              <label class="field emp-enable-row">
                <input
                  type="checkbox"
                  checked={fAllowWorktree()}
                  onChange={(e) => setFAllowWorktree(e.currentTarget.checked)}
                />
                <span>允许员工使用独立 git worktree</span>
                <span class="emp-field-hint">
                  只控制能力边界。员工会结合任务、知识与经验，自行决定在当前分支工作、切独立分支或使用
                  worktree。
                </span>
              </label>

              <label class="field">
                <span class="field-label">岗位说明书（启动提示词）</span>
                <textarea
                  class="field-input"
                  rows={5}
                  placeholder="这名员工是谁、负责什么、工作规范与偏好。会作为每次任务的开场设定。"
                  value={fCharter()}
                  onInput={(e) => setFCharter(e.currentTarget.value)}
                />
              </label>

              <label class="field emp-enable-row">
                <input
                  type="checkbox"
                  checked={fHeartbeatOn()}
                  onChange={(e) => setFHeartbeatOn(e.currentTarget.checked)}
                />
                <span>
                  自动心跳（定时巡查、自动领活；关闭 =
                  完全由你在御书房下旨交办或朱批后才行动）
                </span>
              </label>

              <label class="field">
                <span class="field-label">Dream 模型</span>
                <div class="emp-config-row">
                  <ModelPicker
                    agentKind={fMindAgent()}
                    agentKinds={enabledAgentKinds()}
                    model={fMindModel()}
                    onPickModel={(k, m) => {
                      setFMindAgent(k);
                      setFMindModel(m);
                    }}
                    title="Dream 模型"
                    portal
                  />
                </div>
                <span class="emp-field-hint">
                  Dream 独立于 Wake 开工预检，只在员工没有工作且有新事件时反思学习、合并经验、降级低价值记忆。开工检索和收尾事件走被动 Mind。
                </span>
              </label>

              {/* 心跳相关配置整组随开关显隐：不开心跳就没有巡查，自然也不需要
                  心跳周期 / 巡查模型 / 上班时间（上班时间只约束心跳自主行动）。 */}
              <Show when={fHeartbeatOn()}>
                <label class="field">
                  <span class="field-label">心跳周期</span>
                  <div class="emp-hb-row">
                    <input
                      class="field-input emp-hb-input"
                      type="number"
                      min={10}
                      value={fHeartbeat()}
                      onInput={(e) => setFHeartbeat(Number(e.currentTarget.value) || 0)}
                    />
                    <span class="emp-hb-unit">秒</span>
                    <div class="emp-hb-presets">
                      <For each={HEARTBEAT_PRESETS}>
                        {(p) => (
                          <button
                            type="button"
                            class="btn small secondary"
                            classList={{ primary: fHeartbeat() === p.v }}
                            onClick={() => setFHeartbeat(p.v)}
                          >
                            {p.label}
                          </button>
                        )}
                      </For>
                    </div>
                  </div>
                </label>

                <label class="field">
                  <span class="field-label">巡查/心跳模型</span>
                  <div class="emp-config-row">
                    <ModelPicker
                      agentKind={fHeartbeatAgent()}
                      agentKinds={enabledAgentKinds()}
                      model={fHeartbeatModel()}
                      onPickModel={(k, m) => {
                        setFHeartbeatAgent(k);
                        setFHeartbeatModel(m);
                      }}
                      title="巡查/心跳模型"
                      portal
                    />
                  </div>
                  <span class="emp-field-hint">
                    和工作模型用同一个选择器（可单独选后端与模型）。巡查只做「找活/选单」，建议选更便宜的模型省钱。
                  </span>
                </label>

                <label class="field emp-enable-row">
                  <input
                    type="checkbox"
                    checked={fWorkEnabled()}
                    onChange={(e) => setFWorkEnabled(e.currentTarget.checked)}
                  />
                  <span>设置上班时间（非上班时段自动休眠，不巡查、不开发；关闭 = 7×24 在岗）</span>
                </label>

                <Show when={fWorkEnabled()}>
                  <label class="field">
                    <div class="emp-workhours">
                      <div class="emp-workhours-time">
                        <input
                          class="field-input emp-time-input"
                          type="time"
                          value={fWorkStart()}
                          onInput={(e) => setFWorkStart(e.currentTarget.value)}
                        />
                        <span class="emp-workhours-sep">至</span>
                        <input
                          class="field-input emp-time-input"
                          type="time"
                          value={fWorkEnd()}
                          onInput={(e) => setFWorkEnd(e.currentTarget.value)}
                        />
                      </div>
                      <div class="emp-workdays">
                        <For each={WEEKDAYS}>
                          {(d) => (
                            <button
                              type="button"
                              class="emp-day-chip"
                              classList={{ on: fWorkDays().includes(d.v) }}
                              onClick={() =>
                                setFWorkDays((prev) =>
                                  prev.includes(d.v)
                                    ? prev.filter((x) => x !== d.v)
                                    : [...prev, d.v],
                                )
                              }
                            >
                              {d.label}
                            </button>
                          )}
                        </For>
                      </div>
                    </div>
                    <span class="emp-field-hint">
                      跨夜班可设为如 22:00 至 06:00；不选任何星期 = 每天都上班。时段外员工自动休眠。
                    </span>
                  </label>
                </Show>
              </Show>

              <label class="field emp-enable-row">
                <input
                  type="checkbox"
                  checked={fEnabled()}
                  onChange={(e) => setFEnabled(e.currentTarget.checked)}
                />
                <span>创建后立即上岗（到点自动干活）</span>
              </label>

              <label class="field">
                <span class="field-label">常驻职责</span>
                <textarea
                  class="field-input"
                  rows={4}
                  placeholder="去哪里找、找什么样的单子、怎么算完成。例：读 ./requirements 下状态为「待开发」的需求单，选一个实现并补测试。"
                  value={fDirective()}
                  onInput={(e) => setFDirective(e.currentTarget.value)}
                />
                <span class="emp-field-hint">
                  员工到点会按此说明自己找活、认领、推进；留空则只处理你登记到账本的单子。
                </span>
              </label>

              <label class="field">
                <span class="field-label">协作账本 scope</span>
                <input
                  class="field-input"
                  list="emp-scope-suggest"
                  placeholder="如 requirements（留空则用员工私有账本）"
                  value={fMarkScope()}
                  onInput={(e) => setFMarkScope(e.currentTarget.value)}
                />
                <datalist id="emp-scope-suggest">
                  <For each={scopeSuggestions()}>{(s) => <option value={s} />}</For>
                </datalist>
                <span class="emp-field-hint">
                  多个员工填相同 scope，即可对同一批单子去重、互斥认领、失败接力。
                </span>
              </label>

              <label class="field emp-enable-row">
                <input
                  type="checkbox"
                  checked={fSharedLedger()}
                  onChange={(e) => setFSharedLedger(e.currentTarget.checked)}
                />
                <span>
                  共享到中转站：同组队友（不同机器）填相同 scope 时共用一个账本，跨机器去重/互斥/接力（需已配置团队中转站）
                </span>
              </label>

              <div class="field">
                <span class="field-label">讨论伙伴</span>
                <span class="emp-field-hint">
                  开发中若需与协作方约定接口/协议，员工会自动联系这些伙伴，把答复带回后继续（大事才上奏御书房）。
                </span>
                <Show
                  when={otherEmployeeNames().length > 0}
                  fallback={<span class="emp-field-hint">（暂无本机其他员工可选）</span>}
                >
                  <div class="emp-partner-locals">
                    <For each={otherEmployeeNames()}>
                      {(nm) => (
                        <label class="emp-partner-chip" classList={{ on: isLocalPartner(nm) }}>
                          <input
                            type="checkbox"
                            checked={isLocalPartner(nm)}
                            onChange={() => toggleLocalPartner(nm)}
                          />
                          <span>{nm}</span>
                        </label>
                      )}
                    </For>
                  </div>
                </Show>
                <div class="emp-partner-remotes">
                  <For each={remotePartners()}>
                    {(p, i) => (
                      <div class="emp-partner-remote-row">
                        <input
                          class="field-input"
                          list="emp-peer-suggest"
                          placeholder="队友（团队展示名）"
                          value={p.peer ?? ""}
                          onInput={(e) => updateRemotePartner(i(), { peer: e.currentTarget.value })}
                        />
                        <input
                          class="field-input"
                          placeholder="对方的员工名"
                          value={p.name}
                          onInput={(e) => updateRemotePartner(i(), { name: e.currentTarget.value })}
                        />
                        <button
                          type="button"
                          class="icon-btn"
                          title="移除该跨机伙伴"
                          onClick={() => removeRemotePartner(i())}
                        >
                          <IconX size={14} />
                        </button>
                      </div>
                    )}
                  </For>
                  <datalist id="emp-peer-suggest">
                    <For each={peerNameSuggestions()}>{(s) => <option value={s} />}</For>
                  </datalist>
                  <button type="button" class="btn tiny secondary" onClick={addRemotePartner}>
                    <IconPlus size={12} />
                    添加跨机队友伙伴
                  </button>
                </div>
              </div>

              <Show when={fErr()}>
                <div class="emp-form-err">{fErr()}</div>
              </Show>
            </div>
            <div class="modal-foot">
              <button class="btn secondary" onClick={() => setShowForm(false)}>
                取消
              </button>
              <button class="btn primary" disabled={fBusy()} onClick={() => void saveForm()}>
                {editingId() ? "保存" : "创建"}
              </button>
            </div>
          </div>
        </div>
      </Show>
    </main>
  );
}
