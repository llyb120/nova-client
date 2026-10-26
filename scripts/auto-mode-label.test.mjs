import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

test("thread list preserves the persisted auto-mode marker", () => {
  const meta = source("src-tauri/src/threads.rs").match(/pub struct ThreadMeta \{[\s\S]*?\n\}/)?.[0];
  assert.ok(meta, "ThreadMeta must exist");
  assert.match(meta, /#\[serde\(default, skip_serializing_if = "Option::is_none"\)\]\s*pub auto_scene: Option<String>/);
  const projection = source("src-tauri/src/lib.rs").match(/fn thread_metas\([\s\S]*?\n\}/)?.[0];
  assert.match(projection, /auto_scene:\s*t\.auto_scene\.clone\(\)/);
});

test("chat, sidebar and recent threads label only automatic non-employee sessions", () => {
  for (const file of ["ChatView", "Sidebar", "HomeView"]) {
    const match = source(`src/components/${file}.tsx`).match(
      /<Show when=\{([^\n]+)\}>\s*<span class="agent-badge auto-mode-badge"[^>]*>\s*(自动(?:模式)?)/,
    );
    assert.ok(match, `${file} must visibly label automatic sessions`);
    // Exercise the actual rendering guard rather than a duplicate predicate.
    const visible = new Function("currentMeta", "activeThread", "t", `return !!(${match[1]});`);
    for (const [thread, expected] of [
      [{}, false],
      [{ autoScene: null }, false],
      [{ autoScene: "" }, false],
      ...["lightweight", "coding", "computer"].map((autoScene) => [{ autoScene }, true]),
      [{ autoScene: "computer", employeeThread: true }, false],
    ]) {
      assert.equal(visible(() => thread, () => thread, thread), expected, `${file}: ${JSON.stringify(thread)}`);
    }
  }
});
