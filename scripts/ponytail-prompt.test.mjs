import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  PONYTAIL_LEVELS,
  PONYTAIL_RULES,
  ponytailEnabled,
  ponytailPrompt,
} from "./ponytail-prompt.mjs";

const rust = readFileSync(new URL("../src-tauri/src/lyra/prompt.rs", import.meta.url), "utf8");

function rustConstant(name) {
  const match = rust.match(new RegExp(`pub const ${name}: &str = r#"([\\s\\S]*?)"#;`));
  assert.ok(match, `missing Rust constant ${name}`);
  return match[1].replace(/\r\n/g, "\n");
}

test("Node prompt mirrors the Rust rules and levels", () => {
  assert.equal(PONYTAIL_RULES, rustConstant("PONYTAIL_RULES"));
  assert.equal(PONYTAIL_LEVELS, rustConstant("PONYTAIL_LEVELS"));
});

test("Ponytail modes stay compatible with legacy environment values", () => {
  const expected = (level) => `${PONYTAIL_RULES}\n\n${PONYTAIL_LEVELS}\n\nActive level: ${level}.`;
  assert.equal(ponytailPrompt({}), expected("full"));
  assert.equal(ponytailPrompt({ NOVA_PONYTAIL: "1" }), expected("full"));
  assert.equal(ponytailPrompt({ NOVA_PONYTAIL: "unexpected" }), expected("full"));
  assert.equal(ponytailPrompt({ NOVA_PONYTAIL: "lite" }), expected("lite"));
  assert.equal(ponytailPrompt({ NOVA_PONYTAIL: "ultra" }), expected("ultra"));
  for (const mode of ["0", "off"]) {
    assert.equal(ponytailPrompt({ NOVA_PONYTAIL: mode }), "");
    assert.equal(ponytailEnabled({ NOVA_PONYTAIL: mode }), false);
  }
});
