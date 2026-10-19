import { createEffect, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { jianpuFocus, setFocusRouteId, setJianpuFocus } from "../knowledgeGraph";
import { openThread, setView, state } from "../store";
import "./JianpuView.css";

type Summary = { id: string; startedAt: number; endedAt: number; steps: number; apps: string[]; state: "recorded" | "learning" | "learned"; learnThread: string | null; routes: string[] };
type Step = { at: number; kind: string; app: string; title: string; image?: string; x?: number; y?: number; toX?: number; toY?: number; text?: string; value?: string; key?: string; amount?: number };
type Detail = Summary & { stepList: Step[]; dir: string };

const STATE = { recorded: "未学习", learning: "学习中", learned: "已学习" } as const;
const KIND: Record<string, string> = { click: "点击", double_click: "双击", right_click: "右键", middle_click: "中键", drag: "拖拽", scroll: "滚动", type: "输入", key: "按键" };
const SCROLL: Record<string, string> = { up: "向上", down: "向下", left: "向左", right: "向右" };
const seconds = (ms: number) => (ms / 1000).toFixed(1) + "s";

function describe(step: Step) {
  if (step.kind === "type") return step.value && step.value !== step.text ? `「${step.text ?? ""}」 → 输入框内容「${step.value}」` : `「${step.value ?? step.text ?? ""}」`;
  if (step.kind === "key") return step.key ?? "";
  if (step.kind === "scroll") return `${SCROLL[step.key ?? ""] ?? step.key} ${step.amount ?? 0} 格`;
  if (step.kind === "drag") return `(${step.x}, ${step.y}) → (${step.toX}, ${step.toY})`;
  return step.x == null ? "" : `(${step.x}, ${step.y})`;
}

/** Click screenshot loaded on demand; images stay in the recording folder, not in the bundle.
 *  Plain signals, not createResource: a resource read here suspends the App-level <Suspense> and blanks the whole page. */
function Shot(props: { id: string; name: string }) {
  const [src, setSrc] = createSignal("");
  createEffect(on(() => [props.id, props.name] as const, ([id, name]) => {
    void invoke<string>("jianpu_image", { id, name }).then(setSrc, () => setSrc(""));
  }));
  return <Show when={src()}><img class="jianpu-shot" src={src()} alt={`点击截图 ${props.name}`} loading="lazy" /></Show>;
}

export default function JianpuView() {
  // Signals keep the previous recording on screen until the next one has loaded (no Suspense flash).
  const [list, setList] = createSignal<Summary[]>([]);
  const [selected, setSelected] = createSignal("");
  const [detail, setDetail] = createSignal<Detail | null>(null);
  const refetch = () => invoke<Summary[]>("jianpu_list").then(setList, () => {});
  const reloadDetail = () => {
    const id = selected();
    if (!id) return setDetail(null);
    void invoke<Detail>("jianpu_get", { id }).then(d => { if (selected() === id) setDetail(d); }, () => setDetail(null));
  };
  void refetch();
  createEffect(on(selected, reloadDetail));
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [recording, setRecording] = createSignal(false);

  createEffect(() => {
    const focus = jianpuFocus();
    if (focus) { setSelected(focus); setJianpuFocus(""); }
    else if (!selected() && list().length) setSelected(list()[0].id);
  });
  onMount(() => {
    void invoke<{ recording: boolean }>("jianpu_status").then(s => setRecording(s.recording)).catch(() => {});
    const off = listen<{ recording: boolean }>("jianpu:changed", event => {
      setRecording(event.payload.recording);
      void refetch();
      reloadDetail();
    });
    onCleanup(() => void off.then(fn => fn()));
  });

  const act = async (work: () => Promise<unknown>) => {
    setBusy(true); setError("");
    try { await work(); } catch (e) { setError(String(e)); }
    setBusy(false);
    void refetch(); reloadDetail();
  };
  const learn = (id: string) => act(async () => { await openThread(await invoke<string>("jianpu_learn", { id })); });
  const remove = (id: string) => {
    if (!confirm("删除这段剑谱及其截图？已存进知识图谱的路径不受影响。")) return;
    void act(async () => { await invoke("jianpu_delete", { id }); setSelected(""); });
  };
  const showRoute = (id: string) => { setFocusRouteId(id); setView("knowledge"); };

  return <main class="jianpu-page">
    <header class="jianpu-header">
      <div><h1>剑谱</h1><p>你演示的桌面操作，交给 AI 整理成知识图谱路径；打字内容按原文保存在本机。</p></div>
      <div class="jianpu-header-actions">
        <Show when={state.settings?.knowledgeGraphEnabled}>
          <button class="btn" onClick={() => setView("knowledge")}>← 知识图谱</button>
        </Show>
        <button class="btn" classList={{ "jianpu-rec": recording() }} disabled={busy()} onClick={() => void act(() => invoke("jianpu_toggle"))}>
          {recording() ? "■ 停止录制" : "● 开始录制"}
        </button>
      </div>
    </header>
    <Show when={error()}><p class="jianpu-error" role="alert">{error()}</p></Show>
    <div class="jianpu-body">
      <aside class="jianpu-list">
        <For each={list()} fallback={<p class="jianpu-empty">还没有剑谱。在任意应用里按 {state.settings?.jianpuShortcut || "Alt+R"}，或在 Butterfly 面板「快捷」里点「剑谱录制」开始，演示完再按一次停止。</p>}>
          {item => <button class="jianpu-item" classList={{ active: selected() === item.id }} onClick={() => setSelected(item.id)}>
            <strong>{item.apps.join("、") || "未录到应用"}</strong>
            <small>{new Date(item.startedAt).toLocaleString()} · {item.steps} 步 · <span class={`jianpu-state ${item.state}`}>{STATE[item.state]}</span></small>
          </button>}
        </For>
      </aside>
      <section class="jianpu-detail">
        <Show when={detail()} fallback={<p class="jianpu-empty">选择左侧的一段剑谱查看。</p>}>{d => <>
          <div class="jianpu-actions">
            <button class="btn primary" disabled={busy() || !d().steps} onClick={() => void learn(d().id)}>{d().state === "recorded" ? "交给 AI 学习" : "重新学习"}</button>
            <Show when={d().learnThread}>{thread => <button class="btn" onClick={() => void openThread(thread())}>打开整理会话</button>}</Show>
            <button class="btn" onClick={() => void invoke("open_in_explorer", { path: d().dir })}>打开文件夹</button>
            <button class="btn danger" disabled={busy()} onClick={() => remove(d().id)}>删除</button>
          </div>
          <Show when={d().routes.length}>
            <div class="jianpu-routes"><strong>已存进图谱的路径</strong>
              <Show when={state.settings?.knowledgeGraphEnabled} fallback={<span>{d().routes.length} 条（在「设置 → 高级」打开知识图谱后可查看）</span>}>
                <For each={d().routes}>{(id, i) => <button class="btn" onClick={() => showRoute(id)}>路径 {i() + 1}</button>}</For>
              </Show>
            </div>
          </Show>
          <ol class="jianpu-steps">
            <For each={d().stepList}>{step => <li>
              <div class="jianpu-step-head"><span class="jianpu-kind">{KIND[step.kind] ?? step.kind}</span><span>{describe(step)}</span>
                <small>{seconds(step.at)} · {step.app}{step.title ? " · " + step.title : ""}</small></div>
              <Show when={step.image}>{name => <Shot id={d().id} name={name()} />}</Show>
            </li>}</For>
          </ol>
        </>}</Show>
      </section>
    </div>
  </main>;
}
