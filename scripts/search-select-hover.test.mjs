import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import ts from "typescript";

// Execute the component's actual hover logic with deterministic timers and rectangles.
const source = readFileSync(new URL("../src/components/SearchSelect.tsx", import.meta.url), "utf8");
const start = source.indexOf("  const HOVER_INTENT_MS =");
const end = source.indexOf("  createEffect(() => {", start);
assert.ok(start >= 0 && end > start, "hover logic extraction boundary changed");
const { outputText } = ts.transpileModule(`${source.slice(start, end)}
modelsRef = { getBoundingClientRect: () => ({ left: 200, top: 0, bottom: 320, width: 180 }) };
effortRef = { getBoundingClientRect: () => ({ left: 400, top: 0, bottom: 320, width: 120 }) };
return { scheduleActiveGroup, scheduleActiveModel, cancelGroupIntent, cancelIntent,
  setPoint: (x, y) => { lastPt = { x, y }; } };`, {
  compilerOptions: { target: ts.ScriptTarget.ES2022 },
});
const run = new Function("setTimeout", "clearTimeout", "onCleanup", "activeModel", "effortModel", "setActiveModel", "setActiveGroup", outputText);
function menu(hasEfforts = true) {
  let now = 0, nextId = 0, group = "group-a", model = "model-a";
  const timers = new Map(), changes = [], cleanups = [];
  const api = run((callback, ms) => {
    timers.set(++nextId, { callback, at: now + ms });
    return nextId;
  }, (id) => timers.delete(id), (fn) => cleanups.push(fn), () => model, () => hasEfforts ? {} : undefined,
  (value) => { model = value; changes.push(`model:${value}`); },
  (value) => { group = value; changes.push(`group:${value}`); });
  return {
    ...api, changes,
    group(name, x, y) {
      api.scheduleActiveGroup(name, group, { clientX: x, clientY: y });
      api.setPoint(x, y);
    },
    model(name, x, y) {
      api.scheduleActiveModel(name, { clientX: x, clientY: y });
      api.setPoint(x, y);
    },
    dispose() { cleanups.forEach((fn) => fn()); },
    advance(ms) {
      now += ms;
      for (const [id, timer] of timers) {
        if (timer.at > now) continue;
        timers.delete(id);
        timer.callback();
      }
    },
  };
}

// Vendor -> model: diagonal movement preserves the current vendor until the pointer stops.
for (const [fromY, toY] of [[250, 230], [70, 90]]) {
  const m = menu();
  m.setPoint(50, fromY);
  m.group("group-b", 70, toY);
  m.advance(299);
  assert.deepEqual(m.changes, []);
  m.advance(1);
  assert.deepEqual(m.changes, ["group:group-b"]);
}

// Entering the model column, leaving, clicking, searching/closing, or unmounting cancels it.
for (const cancel of [(m) => m.cancelGroupIntent(), (m) => m.dispose(), (m) => m.group("group-a", 50, 250)]) {
  const m = menu();
  m.setPoint(50, 250);
  m.group("group-b", 70, 230);
  cancel(m);
  m.advance(500);
  assert.deepEqual(m.changes, []);
}

// Normal browsing in the vendor column remains immediate, with no extra dwell delay.
{
  const m = menu();
  m.setPoint(50, 250);
  m.group("group-b", 50, 210);
  assert.deepEqual(m.changes, ["group:group-b"]);
}

// Model -> effort retains its existing 300ms protection and immediate browsing/first hover.
{
  const m = menu();
  m.setPoint(250, 250);
  m.model("model-b", 270, 230);
  m.advance(299);
  assert.deepEqual(m.changes, []);
  m.advance(1);
  assert.deepEqual(m.changes, ["model:model-b"]);
}
{
  const m = menu();
  m.setPoint(250, 250);
  m.model("model-b", 250, 210);
  assert.deepEqual(m.changes, ["model:model-b"]);
}
{
  const m = menu(false);
  m.setPoint(250, 250);
  m.model("model-b", 270, 230);
  assert.deepEqual(m.changes, ["model:model-b"]);
}

// The new vendor timer must not cancel or alter the existing model/effort timer.
{
  const m = menu();
  m.setPoint(50, 250);
  m.group("group-b", 70, 230);
  m.setPoint(250, 250);
  m.model("model-b", 270, 230);
  m.cancelGroupIntent();
  m.advance(300);
  assert.deepEqual(m.changes, ["model:model-b"]);
}
{
  const m = menu();
  m.setPoint(250, 250);
  m.model("model-b", 270, 230);
  m.cancelIntent();
  m.advance(300);
  assert.deepEqual(m.changes, []);
}

// Both vendor layouts use the new protection; backend and effort entry behavior stay intact.
for (const shown of ["shownProvider", "shownGroup"]) {
  for (const event of ["onMouseEnter", "onMouseMove"]) {
    assert.ok(source.includes(`${event}={(e) => scheduleActiveGroup(g.name, ${shown}(), e)}`));
  }
}
assert.equal((source.match(/ref=\{modelsRef\} class="sel-list sel-models" onMouseEnter=\{cancelGroupIntent\}/g) ?? []).length, 2);
assert.match(source, /onMouseEnter=\{\(\) => chooseBackend\(b.id\)\}/);
assert.match(source, /ref=\{effortRef\} class="sel-list sel-efforts" onMouseEnter=\{cancelIntent\}/);
assert.match(source, /onMouseDown=\{cancelGroupIntent\}/);
assert.match(source, /createEffect\(\(\) => \{\s*opened\(\);\s*query\(\);\s*cancelGroupIntent\(\);/);
console.log("SearchSelect vendor hover and existing model protection checks passed");
