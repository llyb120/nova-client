import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const source = path => readFileSync(new URL(`../src-tauri/src/lyra/${path}.rs`, import.meta.url), "utf8");

// Keep the native provider wiring when removing standalone ACP backends.
// Protocol and Nova-login isolation are covered by the API adapter tests.
test("Lyra retains native Devin and CodeBuddy providers", () => {
  const modules = source("mod"), presets = source("presets"), transport = source("provider");
  for (const name of ["devin", "codebuddy"]) {
    assert.match(modules, new RegExp(`mod ${name}_provider;`));
    for (const method of ["load", "models", "stream"]) {
      assert.match(source(`${name}_provider`), new RegExp(`fn ${method}\\(`));
      assert.match(method === "stream" ? transport : presets, new RegExp(`${name}_provider::${method}\\(`));
    }
  }
  for (const name of ["devin", "codebuddy"]) {
    assert.ok(presets.includes(`"${name}" => Some("${name}")`), `${name} preset must remain available`);
  }
  // 统一到 CodeBuddy 登录：唯一的登录会话文件与固定协议版本。
  assert.match(source("codebuddy_provider"), /"codebuddy-login\.json"/);
  assert.match(presets, /local_provider::load\(/);
  assert.match(source("local_provider"), /fn load\(/);
  // 统一到 CodeBuddy 登录：唯一的登录会话文件与固定协议版本；旧 workbuddy 会话按 CodeBuddy 读取。
  assert.match(source("codebuddy_provider"), /workbuddy-login\.json/);
  assert.doesNotMatch(source("codebuddy_provider"), /5\.8\.2|std::process::Command|crate::workbuddy::|include_str!/);
  assert.doesNotMatch(readFileSync(new URL("./codebuddy-protocol.mjs", import.meta.url), "utf8"), /execFile|node:child_process|product\.json|settings\.json/);
});
