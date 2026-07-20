import { createEffect, createSignal, For, onCleanup, type Component } from "solid-js";

const NOTE_GLYPHS = ["♩", "♪", "♫", "♬", "♭", "♮", "♯", "𝄞", "𝄢"];
// 所有音符同速流动 + 恒定最小生成间隔（间距 ≈ 24px > 音符最大宽度）→ 永不追撞
const MIN_SPAWN_GAP = 290;

type FlowNote = {
  id: number;
  glyph: string;
  size: string;
};

export type NoteFlow = {
  bump: () => void;
  Notes: Component;
};

export function createNoteFlow(running?: () => boolean): NoteFlow {
  const [flowNotes, setFlowNotes] = createSignal<FlowNote[]>([]);
  const [energyLive, setEnergyLive] = createSignal(false);
  let notesRef: HTMLDivElement | undefined;
  let noteSeq = 0;
  let energyTarget = 0;
  let energyCurrent = 0;
  let lastTypeAt = 0;
  let lastSpawnAt = 0;
  let energyTimer: number | undefined;
  let runningNoteTimer: number | undefined;

  const spawnNote = () => {
    noteSeq += 1;
    const glyph = NOTE_GLYPHS[(Math.random() * NOTE_GLYPHS.length) | 0];
    setFlowNotes((list) => [
      ...list.slice(-11),
      { id: noteSeq, glyph, size: `${11 + ((Math.random() * 5) | 0)}px` },
    ]);
  };

  const trySpawnNote = () => {
    const now = performance.now();
    if (now - lastSpawnAt < MIN_SPAWN_GAP) return;
    lastSpawnAt = now;
    spawnNote();
  };

  const dropNote = (id: number) =>
    setFlowNotes((list) => list.filter((note) => note.id !== id));

  const ensureEnergyLoop = () => {
    if (energyTimer !== undefined) return;
    energyTimer = window.setInterval(() => {
      energyTarget *= 0.9;
      energyCurrent += (energyTarget - energyCurrent) * 0.25;
      if (energyTarget < 0.01 && energyCurrent < 0.01) {
        energyTarget = 0;
        energyCurrent = 0;
        window.clearInterval(energyTimer);
        energyTimer = undefined;
      }
      notesRef?.style.setProperty("--note-amp", energyCurrent.toFixed(2));
      setEnergyLive(energyCurrent > 0.02);
    }, 50);
  };

  const bump = () => {
    const now = performance.now();
    const dt = now - lastTypeAt;
    lastTypeAt = now;
    const speedBoost = dt > 0 && dt < 600 ? 1 - dt / 600 : 0.3;
    energyTarget = Math.min(1, energyTarget + 0.25 + speedBoost * 0.4);
    ensureEnergyLoop();
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
    if (energyTimer !== undefined) window.clearInterval(energyTimer);
    if (runningNoteTimer !== undefined) window.clearInterval(runningNoteTimer);
  });

  const active = () => !!(running?.() || energyLive() || flowNotes().length > 0);

  const Notes: Component = () => (
    <div
      ref={notesRef}
      class="composer-notes"
      classList={{ active: active() }}
      aria-hidden="true"
    >
      <For each={flowNotes()}>
        {(note) => (
          <span
            class="composer-note"
            style={{ "font-size": note.size }}
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
