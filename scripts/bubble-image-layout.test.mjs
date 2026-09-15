import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { transformSync } from "esbuild";

const source = readFileSync(new URL("../src/components/CanvasTranscript.tsx", import.meta.url), "utf8");
const section = (start, end) => source.slice(source.indexOf(start), source.indexOf(end, source.indexOf(start) + start.length));
const js = (code) => transformSync(code, { loader: "ts", target: "esnext" }).code;

test("长提示词流式重排复用换行，绘制数量仅随视口高度增长", () => {
  let wraps = 0;
  const draws = [];
  const measure = (text) => text.length * 7;
  const wrap = new Function("measure", `
    ${js(section("function pushTrimmedLine(", "function wrapStyledTextIndexed("))}
    return wrapTextFull;
  `)(measure);
  const wrapTextFull = (...args) => { wraps++; return wrap(...args); };
  const layout = new Function("wrapTextFull", "measure", "layoutBubbleImages", `
    const userTextLayouts = new WeakMap();
    return (item, contentW = 800, font = 'sans') => {
      const p = { sans: font }, result = [], side = 0, y = 20, gi = 0, loadImage = () => null;
      ${js(section("          const maxBubble = contentW * 0.85;", "          // .user-edit-btn:"))}
      return result[0];
    };
  `)(wrapTextFull, measure, () => ({ layouts: [], usedW: 0, stackH: 0, imgMaxW: 240 }));
  const item = { id: 1, text: "启动提示词 😀 keep all text\n".repeat(20000) };
  let block = layout(item);
  assert.equal(wraps, 1);
  for (let frame = 0; frame < 60; frame++) block = layout(item);
  assert.equal(wraps, 1, "模型每次输出不应重新测量完整提示词");
  assert.equal(block._lines.length, 20000);
  assert.ok(block._lineSeps.every(sep => sep === "\n"));
  layout(item, 600);
  layout(item, 600, "other font");
  item.text = item.text.replace("启动", "修改");
  block = layout(item, 600, "other font");
  assert.equal(wraps, 4, "宽度、字体和编辑变化必须失效");

  const paint = new Function("measure", "wrapTextFull", "fillTextCrisp", `
    const viewH = 600, BUBBLE_IMG_MAX_W = 240;
    ${js(section("  function paintUserBubble(", "  function "))}
    return paintUserBubble;
  `)(measure, wrapTextFull, (...args) => draws.push(args));
  const ctx = {};
  for (const by of [20, -200000, -block.h + 600]) {
    draws.length = 0;
    paint(ctx, block, 0, by, {}, false);
    assert.ok(draws.length > 0 && draws.length < 32, `${draws.length} draws at ${by}`);
    assert.equal(block.textLines.length, 20000, "选区/复制仍保留完整消息");
  }
  assert.equal(wraps, 4, "绘制不得再次换行");
});

test("冷加载图片更新闭合分组签名，重排后暖切换复用稳定布局", () => {
  const imgCache = new Map();
  const functions = new Function("imgCache", "promptImageSrc", "state", `
    const closedGroupSigCache = new WeakMap();
    ${js(section("  function userImagesSig(", "  async function computeLayout("))}
    ${js(section("const BUBBLE_IMG_MAX_W", "function promptImageSrc("))}
    ${js(section("function bubbleImageSize(", "// ─── Markdown parser"))}
    return { sig: cachedClosedGroupSig, layout: layoutBubbleImages };
  `)(imgCache, (img) => img.uri, { expanded: {} });
  const images = ["wide", "tall"].map((uri) => ({ uri, mimeType: "image/png", name: uri }));
  const group = { user: { id: 1, text: "图片说明", images }, turn: { id: 2 }, body: [] };
  const load = (img) => imgCache.get(img.uri) ?? null;
  const pendingSig = functions.sig(group);
  const pending = functions.layout(images, 340, load);
  assert.equal(pending.layouts[1].dy, 10); // 两个 160px 占位在同一行。

  imgCache.set("wide", { _loaded: true, naturalWidth: 1200, naturalHeight: 400 });
  const partialSig = functions.sig(group);
  assert.notEqual(partialSig, pendingSig);
  imgCache.set("tall", { _loaded: true, naturalWidth: 1000, naturalHeight: 1200 });
  const loadedSig = functions.sig(group);
  assert.notEqual(loadedSig, partialSig);
  const loaded = functions.layout(images, 340, load);
  assert.ok(loaded.layouts[1].dy > 10); // 真实尺寸必须换行，不能继续复用占位位置。
  for (const width of [100, 340, 700]) {
    const layout = functions.layout(images, width, load);
    for (const slot of layout.layouts) {
      assert.ok(slot.dx + slot.w <= 16 + width);
      assert.ok(slot.dy + slot.h <= 10 + layout.stackH);
    }
  }
  functions.sig({ user: { id: 3, text: "另一会话" }, body: [] });
  assert.equal(functions.sig(group), loadedSig);
  assert.deepEqual(functions.layout(images, 340, load), loaded);
});

test("图片完成加载但气泡尚未重排时不使用旧位置绘制新尺寸", () => {
  const draws = [];
  let rebuilds = 0;
  const paint = new Function("loadImage", "scheduleRebuild", "roundRect", `
    ${js(section("const BUBBLE_IMG_MAX_W", "function promptImageSrc("))}
    ${js(section("function bubbleImageSize(", "interface BubbleImageLayout"))}
    ${js(section("  function paintUserBubble(", "  function ").replace(/\s+$/, ""))}
    return paintUserBubble;
  `)(() => ({ naturalWidth: 1200, naturalHeight: 400 }), () => rebuilds++, () => {});
  const ctx = { save() {}, clip() {}, restore() {}, drawImage: (...args) => draws.push(args) };
  const slot = { img: {}, dx: 16, dy: 10, w: 160, h: 120 };
  const block = { data: { imageLayouts: [slot], imgMaxW: 240 } };
  paint(ctx, block, 0, 0, {}, false);
  assert.equal(rebuilds, 1);
  assert.equal(draws.length, 0);
  Object.assign(slot, { w: 240, h: 80 });
  paint(ctx, block, 0, 0, {}, false);
  assert.equal(rebuilds, 1);
  assert.deepEqual(draws[0].slice(1), [16, 10, 240, 80]);
});
