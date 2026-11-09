import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

// The picker is click-driven with two screens in one column:
// 1) model names → click one; 2) that model's full models, each effort on its own row.
const source = readFileSync(new URL("../src/components/SearchSelect.tsx", import.meta.url), "utf8");

// No hover-intent machinery is left behind.
for (const gone of ["HOVER_INTENT_MS", "scheduleActiveModel", "scheduleActiveGroup", "headingToPane", "cancelIntent", "activeModel"]) {
  assert.equal(source.includes(gone), false, `${gone} 应随悬停切换一起移除`);
}

// Screen 1: model names only, click drills in.
assert.match(source, /onClick=\{\(\) => setDrill\(g\.name\)\}/);
// Screen 2: back row, then the models themselves.
assert.match(source, /onClick=\{\(\) => setDrill\(null\)\}/, "进入后可以返回模型名列表");
assert.match(source, /if \(drill\(\)\) setDrill\(null\);/, "Esc 先退回模型名列表");

// Effort is not a separate button bar: each level is its own row (模型 · High).
assert.equal(source.includes("effortBar"), false, "不再有独立强度按钮条");
assert.equal(source.includes("effortPick"), false, "不再有待提交的强度选择");
assert.equal(source.includes("sel-effort"), false);
assert.match(source, /const drilledRows = createMemo/);
assert.match(source, /o\.efforts\?\.length\s*\n\s*\? o\.efforts\.map\(\(effort\) => \(\{ option: effort, parent: o \}\)\)/);
assert.match(source, /<For each=\{drilledRows\(\)\}>\{\(r\) => itemRow\(r\.option, r\.parent\)\}<\/For>/);
assert.match(source, /onClick=\{\(\) => pick\(o\.value\)\}/, "点行即提交该项");

// Second screen is a single column: no side-by-side panes, no separate effort column.
assert.equal(source.includes("sel-panes"), false, "第二屏不分两栏");
assert.equal(source.includes("sel-efforts"), false, "不再有独立强度列");
assert.equal(source.includes("effortPane"), false);

// Search filters only the first screen (model names); drilling in stays the same.
assert.match(source, /const shownGroups = createMemo\(\(\) => \{[\s\S]{0,200}groups\(\)\.filter\(\(g\) => g\.name\.toLowerCase\(\)\.includes\(q\)\)/);
assert.match(source, /<For each=\{shownGroups\(\)\}>/);
assert.match(source, /if \(q\.trim\(\) && drill\(\)\) setDrill\(null\);/, "开始搜索就退回模型名列表");
assert.equal(source.includes('<Show when={!isGrouped() || query().trim()}>'), false, "搜索不再切成扁平模型列表");
assert.match(source, /else if \(!drill\(\) && shownGroups\(\)\.length > 0\) setDrill\(shownGroups\(\)\[0\]\.name\);/, "回车进入第一个匹配的模型名");

// Panel width is constant: it must not change when drilling in (that jump was the bug).
assert.match(source, /const popWidth = \(\) => \(isGrouped\(\) \? 480 : 280\);/);
assert.equal((source.match(/const popWidth/g) ?? []).length, 1);
console.log("SearchSelect drill-in checks passed");
