import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const source = path => readFileSync(new URL(`../src-tauri/src/lyra/${path}.rs`, import.meta.url), "utf8");

// Keep the native provider wiring when removing standalone ACP backends.
// Protocol and encrypted-login behavior is covered by the API adapter tests.
test("Lyra retains native Devin, CodeBuddy and WorkBuddy providers", () => {
  const modules = source("mod"), presets = source("presets"), transport = source("provider");
  for (const name of ["devin", "codebuddy"]) {
    assert.match(modules, new RegExp(`mod ${name}_provider;`));
    for (const method of ["load", "models", "stream"]) {
      assert.match(source(`${name}_provider`), new RegExp(`fn ${method}\\(`));
      assert.match(method === "stream" ? transport : presets, new RegExp(`${name}_provider::${method}\\(`));
    }
  }
  for (const name of ["devin", "codebuddy", "workbuddy"]) {
    assert.ok(presets.includes(`"${name}" => Some("${name}")`), `${name} preset must remain available`);
  }
  assert.match(presets, /local_provider::load\(/);
  assert.match(source("local_provider"), /fn load\(/);
  assert.match(source("codebuddy_provider"), /include_str!\("\.\.\/\.\.\/\.\.\/scripts\/workbuddy-login\.cjs"\)/);
  assert.match(readFileSync(new URL("./workbuddy-login.cjs", import.meta.url), "utf8"), /exports\.decodeWorkBuddyToken/);
});
