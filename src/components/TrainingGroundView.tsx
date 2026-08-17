import { message } from "@tauri-apps/plugin-dialog";
import { createMemo, createResource, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { api } from "../ipc";
import { state } from "../store";
import type { ExperienceEntry } from "../types";

function fmtTime(ts: number) {
  return ts ? new Date(ts).toLocaleString("zh-CN") : "尚未训练";
}

function fmtDate(ts: number) {
  return ts
    ? new Date(ts).toLocaleDateString("zh-CN", { month: "short", day: "numeric" })
    : "—";
}

const dipperPositions: Record<string, { x: number; y: number }> = {
  slow: { x: 13, y: 23 },
  novel: { x: 24, y: 41 },
  negative: { x: 38, y: 57 },
  abstract: { x: 50, y: 58 },
  balanced: { x: 56, y: 35 },
  concrete: { x: 69, y: 49 },
  fast: { x: 84, y: 25 },
};

const dipperCatalog: Record<string, string> = {
  fast: "DUBHE · α UMA",
  concrete: "MERAK · β UMA",
  balanced: "PHECDA · γ UMA",
  abstract: "MEGREZ · δ UMA",
  negative: "ALIOTH · ε UMA",
  novel: "MIZAR · ζ UMA",
  slow: "ALKAID · η UMA",
};

/** 按真实恒星参数渲染：scale 对应视星等，tint/glow 对应光谱色（天枢为橙巨星，天权最暗）。 */
const starLooks: Record<string, { scale: number; tint: string; glow: string }> = {
  fast: { scale: 1.32, tint: "#ffdfae", glow: "255, 198, 128" },
  concrete: { scale: 1.02, tint: "#dce8ff", glow: "168, 198, 255" },
  balanced: { scale: 1.0, tint: "#e6edff", glow: "172, 196, 255" },
  abstract: { scale: 0.72, tint: "#ccd7ee", glow: "150, 176, 224" },
  negative: { scale: 1.36, tint: "#e8f1ff", glow: "188, 212, 255" },
  novel: { scale: 1.06, tint: "#eef3ff", glow: "176, 200, 255" },
  slow: { scale: 1.2, tint: "#d2e2ff", glow: "148, 186, 255" },
};

export function TrainingGroundView() {
  const projectCwd = () => state.trainingCwd;
  const [overview, { refetch }] = createResource(projectCwd, api.listExperiences);
  const [busy, setBusy] = createSignal(false);
  const [evolutionStatus, setEvolutionStatus] = createSignal("");
  const experts = createMemo(() => {
    const nameMap = new Map<string, string>();
    for (const expert of overview()?.experts ?? []) nameMap.set(expert.id, expert.name);
    const groups = new Map<string, { name: string; entries: ExperienceEntry[] }>();
    for (const expertId of nameMap.keys()) groups.set(expertId, { name: nameMap.get(expertId) ?? expertId, entries: [] });
    for (const entry of overview()?.experiences ?? []) {
      const group = groups.get(entry.expertId) ?? { name: nameMap.get(entry.expertId) ?? entry.expertId, entries: [] };
      group.entries.push(entry);
      groups.set(entry.expertId, group);
    }
    for (const group of groups.values()) group.entries.sort((a, b) => b.updatedAt - a.updatedAt);
    return [...groups.entries()].map(([id, group]) => [id, group.name, group.entries] as const);
  });
  const [activeExpert, setActiveExpert] = createSignal("");
  const selectedExpert = createMemo(() => experts().find(([id]) => id === activeExpert()) ?? experts()[0]);
  const unscored = (entries: ExperienceEntry[]) =>
    entries.filter((entry) => entry.userFeedback === 0).length;

  // Canvas 星野：三层景深的恒星按各自频率闪烁，色温按真实恒星分布，
  // 约三分之一星点聚集在一条斜贯的银河带中，亮星带辉光与衍射星芒。
  let starfield: HTMLCanvasElement | undefined;
  onMount(() => {
    const ctx = starfield?.getContext("2d");
    if (!starfield || !ctx) return;
    const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    interface BgStar { x: number; y: number; r: number; color: string; base: number; amp: number; speed: number; phase: number; glow: boolean; spike: boolean }
    let stars: BgStar[] = [];
    let raf = 0;
    const dpr = () => Math.min(window.devicePixelRatio || 1, 2);
    // 银带方程：从左上方到右下方的一条斜带（相对坐标）。
    const bandY = (x: number) => 0.3 + 0.34 * x;
    const gauss = () => (Math.random() + Math.random() + Math.random() - 1.5) / 1.5;
    const pickColor = () => {
      const roll = Math.random();
      if (roll < 0.42) return "222, 233, 255"; // 蓝白
      if (roll < 0.62) return "255, 255, 255"; // 白
      if (roll < 0.78) return "196, 214, 255"; // 蓝
      if (roll < 0.92) return "255, 231, 196"; // 暖黄
      return "255, 198, 148"; // 橙
    };
    const resize = () => {
      const rect = starfield!.getBoundingClientRect();
      const ratio = dpr();
      starfield!.width = Math.max(1, Math.round(rect.width * ratio));
      starfield!.height = Math.max(1, Math.round(rect.height * ratio));
      const count = Math.round((rect.width * rect.height) / 550);
      stars = Array.from({ length: count }, () => {
        const depth = Math.random();
        const near = depth < 0.05;
        const mid = !near && depth < 0.3;
        const inBand = Math.random() < 0.36;
        const x = Math.random();
        const y = inBand ? Math.min(1, Math.max(0, bandY(x) + gauss() * 0.14)) : Math.random();
        return {
          x,
          y,
          r: near ? 1.1 + Math.random() * 0.9 : mid ? 0.5 + Math.random() * 0.55 : 0.2 + Math.random() * 0.45,
          color: pickColor(),
          base: near ? 0.72 + Math.random() * 0.22 : mid ? 0.38 + Math.random() * 0.3 : 0.18 + Math.random() * 0.3,
          amp: 0.06 + Math.random() * 0.22,
          speed: 0.25 + Math.random() * 1.2,
          phase: Math.random() * Math.PI * 2,
          glow: near || (mid && Math.random() < 0.3),
          spike: near && Math.random() < 0.55,
        };
      });
    };
    const drawBand = (w: number, h: number) => {
      const angle = Math.atan2(0.34 * h, w);
      const thickness = Math.min(w, h) * 0.62;
      const len = Math.hypot(w, h);
      ctx.save();
      ctx.translate(0, 0.3 * h);
      ctx.rotate(angle);
      const grad = ctx.createLinearGradient(0, -thickness / 2, 0, thickness / 2);
      grad.addColorStop(0, "rgba(150, 170, 228, 0)");
      grad.addColorStop(0.5, "rgba(156, 178, 232, 0.13)");
      grad.addColorStop(1, "rgba(150, 170, 228, 0)");
      ctx.fillStyle = grad;
      ctx.fillRect(-40, -thickness / 2, len + 80, thickness);
      // 银带中的暗尘埃道
      const dust = ctx.createLinearGradient(0, -thickness * 0.08, 0, thickness * 0.22);
      dust.addColorStop(0, "rgba(4, 6, 12, 0)");
      dust.addColorStop(0.5, "rgba(4, 6, 12, 0.16)");
      dust.addColorStop(1, "rgba(4, 6, 12, 0)");
      ctx.fillStyle = dust;
      ctx.fillRect(-40, -thickness * 0.08, len + 80, thickness * 0.3);
      ctx.restore();
    };
    const draw = (time: number) => {
      const ratio = dpr();
      const w = starfield!.width / ratio;
      const h = starfield!.height / ratio;
      ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
      ctx.clearRect(0, 0, w, h);
      drawBand(w, h);
      for (const star of stars) {
        const alpha = Math.max(0.03, star.base + Math.sin((time / 1000) * star.speed + star.phase) * star.amp);
        const x = star.x * w;
        const y = star.y * h;
        if (star.glow) {
          const grad = ctx.createRadialGradient(x, y, 0, x, y, star.r * 4.6);
          grad.addColorStop(0, `rgba(${star.color}, ${(alpha * 0.55).toFixed(3)})`);
          grad.addColorStop(1, `rgba(${star.color}, 0)`);
          ctx.fillStyle = grad;
          ctx.beginPath();
          ctx.arc(x, y, star.r * 4.6, 0, Math.PI * 2);
          ctx.fill();
        }
        ctx.fillStyle = `rgba(${star.color}, ${alpha.toFixed(3)})`;
        ctx.beginPath();
        ctx.arc(x, y, star.r, 0, Math.PI * 2);
        ctx.fill();
        if (star.spike) {
          const len = star.r * 4.5;
          ctx.strokeStyle = `rgba(${star.color}, ${(alpha * 0.45).toFixed(3)})`;
          ctx.lineWidth = 0.6;
          ctx.beginPath();
          ctx.moveTo(x - len, y);
          ctx.lineTo(x + len, y);
          ctx.moveTo(x, y - len);
          ctx.lineTo(x, y + len);
          ctx.stroke();
        }
      }
    };
    resize();
    const observer = new ResizeObserver(resize);
    observer.observe(starfield);
    if (reduceMotion) {
      draw(0);
    } else {
      const loop = (time: number) => { draw(time); raf = requestAnimationFrame(loop); };
      raf = requestAnimationFrame(loop);
    }
    onCleanup(() => { cancelAnimationFrame(raf); observer.disconnect(); });
  });

  const train = async () => {
    if (busy()) return;
    setBusy(true);
    try {
      await api.trainExperience(projectCwd());
      await refetch();
    } catch (error) {
      await message(String(error), { kind: "error" });
    } finally {
      setBusy(false);
    }
  };

  const evolve = async () => {
    if (busy()) return;
    setBusy(true);
    setEvolutionStatus("正在生成并审核本代经验…");
    try {
      const result = await api.evolveExperiences(projectCwd());
      await refetch();
      setEvolutionStatus(`第 ${result.generation} 代：审核 ${result.reviewed} 条，形成 ${result.created} 条新经验，去重 ${result.rejected} 条`);
    } catch (error) {
      setEvolutionStatus(`演进失败：${String(error)}`);
    } finally {
      setBusy(false);
    }
  };

  const vote = async (id: string, reward: number) => {
    try {
      await api.feedbackExperience(projectCwd(), id, reward);
      await refetch();
    } catch (error) {
      await message(String(error), { kind: "error" });
    }
  };

  const remove = async (id: string) => {
    try {
      await api.deleteExperience(projectCwd(), id);
      await refetch();
    } catch (error) {
      await message(String(error), { kind: "error" });
    }
  };
  const kindLabel = (kind: ExperienceEntry["kind"]) => kind === "memory" ? "记忆" : kind === "rule" ? "守则" : "经验";

  return (
    <main class="training-ground">
      <div class="training-shell">
        <header class="training-head">
          <div class="training-intro">
            <span class="training-eyebrow">EXPERIENCE CONSTELLATION</span>
            <h1>大熊座</h1>
            <p>知识、记忆和经验会自动标记来源项目；项目按 Git 仓库识别，同一仓库下的目录与 worktree 属于同一个项目。</p>
          </div>
          <div class="training-actions">
            <Show when={evolutionStatus()}><span class="evolution-status" role="status">{evolutionStatus()}</span></Show>
            <button class="btn secondary evolution-action" disabled={busy() || overview()?.training} onClick={() => void evolve()}>世代演进</button>
            <button class="btn primary training-action" disabled={busy() || overview()?.training} onClick={() => void train()}>
              <span class="training-action-icon" aria-hidden="true">✦</span>
              {busy() || overview()?.training ? "处理中…" : "立即训练"}
              <kbd>/train</kbd>
            </button>
          </div>
        </header>

        <Show when={overview()} fallback={<div class="training-loading">正在读取经验库…</div>}>
          {(data) => (
            <>
              <section class="training-stats" aria-label="训练概览">
                <div class="training-stat"><span>训练轮次</span><strong>{data().trainingCycles}</strong></div>
                <div class="training-stat"><span>演进世代</span><strong>{data().evolutionGeneration}</strong></div>
                <div class="training-stat"><span>知识总数</span><strong>{data().experiences.length}</strong></div>
                <div class="training-stat training-stat-wide"><span>上次训练</span><strong>{fmtTime(data().lastTrainAt)}</strong></div>
              </section>

              <section class="knowledge-types" aria-label="知识类型说明">
                <div class="knowledge-type active"><b>经验</b><span>从结果归纳的条件性结论，可反馈和淘汰</span></div>
                <div class="knowledge-type"><b>记忆</b><span>从会话提炼的长期事实与稳定背景</span></div>
                <div class="knowledge-type"><b>守则</b><span>由明确要求或充分证据形成的强约束</span></div>
              </section>

              <Show when={experts().length > 0} fallback={<div class="training-empty"><span>◇</span><strong>没有配置经验专家</strong><p>请先在设置中恢复或添加专家配置。</p></div>}>
                <div class="dipper-map" role="tablist" aria-label="北斗七星经验专家">
                  <canvas class="dipper-starfield" ref={starfield} aria-hidden="true" />
                  <div class="dipper-nebula" aria-hidden="true" />
                  <div class="dipper-nebula dipper-nebula-warm" aria-hidden="true" />
                  <svg class="dipper-lines" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
                    <g class="dipper-path">
                      <path d="M13 23 L24 41 L38 57" />
                      <path d="M38 57 L50 58" />
                      <path d="M50 58 L56 35" />
                      <path d="M56 35 L69 49" />
                      <path d="M69 49 L84 25" />
                    </g>
                    <g class="dipper-flow">
                      <path d="M13 23 L24 41 L38 57" />
                      <path d="M38 57 L50 58" />
                      <path d="M50 58 L56 35" />
                      <path d="M56 35 L69 49" />
                      <path d="M69 49 L84 25" />
                    </g>
                  </svg>
                  <For each={experts()}>
                    {([expertId, expertName, entries], index) => {
                      const position = () => dipperPositions[expertId] ?? { x: 8 + index() * 11, y: 48 };
                      const look = () => starLooks[expertId] ?? { scale: 1, tint: "#e6edff", glow: "172, 196, 255" };
                      return (
                        <button
                          type="button"
                          role="tab"
                          aria-selected={selectedExpert()?.[0] === expertId}
                          aria-label={`${expertName}，${entries.length} 条经验`}
                          classList={{ "dipper-star": true, active: selectedExpert()?.[0] === expertId }}
                          style={{
                            left: `${position().x}%`,
                            top: `${position().y}%`,
                            "--star-scale": look().scale,
                            "--star-tint": look().tint,
                            "--star-glow": look().glow,
                            "--twinkle-delay": `${(index() * 0.53).toFixed(2)}s`,
                          }}
                          onClick={() => setActiveExpert(expertId)}
                        >
                          <span class="star-halo" aria-hidden="true" />
                          <span class="star-orbit" aria-hidden="true" />
                          <span class="star-core" aria-hidden="true"><i /></span>
                          <Show when={expertId === "novel"}><span class="star-companion" aria-hidden="true" /></Show>
                          <span class="star-label"><span class="star-name">{expertName}</span><small>{dipperCatalog[expertId] ?? expertId}</small></span>
                          <Show when={unscored(entries) > 0}><b title={`${unscored(entries)} 条经验尚未评分`}>{unscored(entries)}</b></Show>
                        </button>
                      );
                    }}
                  </For>
                  <span class="dipper-caption">北斗七星 · 七种认知视角</span>
                </div>
                <Show when={selectedExpert()}>
                  {(selected) => (
                    <section class="expert-panel" role="tabpanel">
                      <header class="expert-head">
                        <div><h2>{selected()[1]}</h2><p>经验专家 · {selected()[2].length} 条经验</p></div>
                        <Show when={unscored(selected()[2]) > 0}><span class="expert-pending">{unscored(selected()[2])} 条待评分</span></Show>
                      </header>
                      <div class="experience-grid">
                        <For each={selected()[2]} fallback={<div class="expert-empty">该专家尚未产出经验</div>}>
                          {(entry) => (
                            <article classList={{ "experience-card": true, quarantined: entry.status === "quarantined", [`kind-${entry.kind}`]: true }}>
                              <div class="experience-topline"><span class={`experience-condition kind-${entry.kind}`}>{kindLabel(entry.kind)}</span><span class="experience-condition">{entry.knowledgeScope === "universal" ? "泛用" : "项目独有"}</span><span class="experience-project" title={entry.projectRoot || "历史知识未记录来源仓库"}>{entry.projectRoot ? `仓库 · ${entry.projectRoot.replace(/[\\/]+$/, "").split(/[\\/]/).pop()}` : "来源仓库未知"}</span><span class="experience-date">更新于 {fmtDate(entry.updatedAt)}</span></div>
                              <Show when={entry.trigger}><div class="experience-trigger">{entry.trigger}</div></Show>
                              <div class="experience-action">{entry.action}</div>
                              <Show when={entry.avoid}><div class="experience-avoid"><span>避免</span>{entry.avoid}</div></Show>
                              <footer class="experience-footer">
                                <div class="experience-scopes"><For each={entry.scope.length ? entry.scope : ["通用"]}>{(scope) => <span>{scope}</span>}</For></div>
                                <span class="experience-confidence">置信度 <b>{Math.round(entry.confidence * 100)}%</b></span>
                              </footer>
                              <div class="experience-votes">
                                <span>{entry.userFeedback === 0 ? "待评分" : "已评分"}</span>
                                <button classList={{ active: entry.userFeedback === 1 }} title="再次点击取消" onClick={() => void vote(entry.id, 1)}>↑ 有用 <b>{entry.positiveCount}</b></button>
                                <button classList={{ active: entry.userFeedback === -1 }} title="再次点击取消" onClick={() => void vote(entry.id, -1)}>↓ 有问题 <b>{entry.negativeCount}</b></button>
                                <button class="experience-delete" title={`删除这条${kindLabel(entry.kind)}`} onClick={() => void remove(entry.id)}>删除</button>
                              </div>
                            </article>
                          )}
                        </For>
                      </div>
                    </section>
                  )}
                </Show>
              </Show>
            </>
          )}
        </Show>
      </div>
    </main>
  );
}
