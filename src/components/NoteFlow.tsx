import { createEffect, createSignal, For, onCleanup, type Component } from "solid-js";

const NOTE_GLYPHS = ["♩", "♪", "♫", "♬", "♭", "♮", "♯", "𝄞", "𝄢"];
// 所有音符同速流动 + 恒定最小生成间隔（间距 ≈ 24px > 音符最大宽度）→ 永不追撞
const MIN_SPAWN_GAP = 290;

type FlowNote = {
  id: number;
  glyph: string;
  size: string;
  rise: string;
};

export type NoteFlow = {
  bump: () => void;
  Notes: Component;
};

export function createNoteFlow(running?: () => boolean): NoteFlow {
  const [flowNotes, setFlowNotes] = createSignal<FlowNote[]>([]);
  let noteSeq = 0;
  let energy = 0;
  let energyUpdatedAt = 0;
  let lastTypeAt = 0;
  let lastSpawnAt = 0;
  let runningNoteTimer: number | undefined;

  const currentEnergy = (now: number) => {
    if (energyUpdatedAt === 0) return 0;
    // 只在需要时计算衰减，避免打字期间用定时器反复改写所有音符的样式。
    return energy * Math.exp(-(now - energyUpdatedAt) / 650);
  };

  const spawnNote = (now: number) => {
    noteSeq += 1;
    const glyph = NOTE_GLYPHS[(Math.random() * NOTE_GLYPHS.length) | 0];
    const rise = -3 - currentEnergy(now) * 7;
    setFlowNotes((list) => [
      ...list.slice(-11),
      {
        id: noteSeq,
        glyph,
        size: `${11 + ((Math.random() * 5) | 0)}px`,
        rise: `${rise.toFixed(2)}px`,
      },
    ]);
  };

  const trySpawnNote = () => {
    const now = performance.now();
    if (now - lastSpawnAt < MIN_SPAWN_GAP) return;
    lastSpawnAt = now;
    spawnNote(now);
  };

  const dropNote = (id: number) =>
    setFlowNotes((list) => list.filter((note) => note.id !== id));

  const bump = () => {
    const now = performance.now();
    const dt = now - lastTypeAt;
    lastTypeAt = now;
    const speedBoost = dt > 0 && dt < 600 ? 1 - dt / 600 : 0.3;
    energy = Math.min(1, currentEnergy(now) + 0.25 + speedBoost * 0.4);
    energyUpdatedAt = now;
    trySpawnNote();
  };

  createEffect(() => {
    if (!running?.()) return;
    runningNoteTimer = window.setInterval(trySpawnNote, MIN_SPAWN_GAP);
    onCleanup(() => {
      window.clearInterval(runningNoteTimer);
      runningNoteTimer = undefined;
    });
  });

  onCleanup(() => {
    if (runningNoteTimer !== undefined) window.clearInterval(runningNoteTimer);
  });

  const active = () => !!(running?.() || flowNotes().length > 0);

  const Notes: Component = () => (
    <div
      class="composer-notes"
      classList={{ active: active() }}
      aria-hidden="true"
    >
      <For each={flowNotes()}>
        {(note) => (
          <span
            class="composer-note"
            style={{ "font-size": note.size, "--note-rise": note.rise }}
            onAnimationEnd={() => dropNote(note.id)}
          >
            {note.glyph}
          </span>
        )}
      </For>
    </div>
  );

  return { bump, Notes };
}
