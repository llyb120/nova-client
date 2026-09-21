import assert from "node:assert/strict";
import { collectWorkspaceArtifacts } from "../src/workspaceArtifacts.ts";

const items = [
  { type: "assistant", text: "[文件](<D:/项目/a b.md:12>) ![图](output.png) [网站](https://example.com)" },
  { type: "tool", content: [{ type: "diff", path: "output.png" }, { type: "diff", path: "src/main.ts" }] },
];
assert.deepEqual(collectWorkspaceArtifacts(items), ["output.png", "src/main.ts", "D:/项目/a b.md"]);
assert.deepEqual(collectWorkspaceArtifacts([{ type: "assistant", text: "[x](file:///D:/a%20b.md#L12)" }]), ["D:/a b.md"]);
assert.equal(collectWorkspaceArtifacts(Array.from({ length: 300 }, (_, i) => ({ type: "tool", content: [{ type: "diff", path: `${i}.txt` }] }))).length, 300);
assert.deepEqual(collectWorkspaceArtifacts([
  {type:'tool',kind:'read',status:'completed',locations:[{path:'read.txt'}],content:[]},
  {type:'tool',kind:'edit',status:'failed',locations:[{path:'failed.txt'}],content:[]},
  {type:'tool',kind:'edit',status:'completed',locations:[{path:'edited.txt'}],content:[]},
  {type:'tool',kind:'write',status:'completed',rawInput:{file_path:'new.txt'},content:[]},
]), ['new.txt','edited.txt']);
// 图片生成工具（kind=other、无 locations）：产物路径在结果 JSON 的 path+markdown 契约中。
const imageResult = JSON.stringify({ path: 'D:/w/nova-image-1.png', model: 'm', markdown: '![图片](<D:/w/nova-image-1.png>)' });
assert.deepEqual(collectWorkspaceArtifacts([
  {type:'tool',kind:'other',status:'completed',title:'Called generate_image from nova-tools',
    content:[{type:'content',content:{type:'text',text:imageResult}}],rawOutput:{durationMs:5}},
  {type:'tool',kind:'other',status:'failed',
    content:[{type:'content',content:{type:'text',text:JSON.stringify({path:'x.png',markdown:'![i](x.png)'})}}]},
  {type:'tool',kind:'read',status:'completed',
    content:[{type:'content',content:{type:'text',text:'{"path":"doc.png","note":"not a result"}'}}]},
]), ['D:/w/nova-image-1.png']);
// rawOutput 中的结构化结果同样识别（未做文本去重时）。
assert.deepEqual(collectWorkspaceArtifacts([
  {type:'tool',kind:'other',status:'completed',content:[],
    rawOutput:{content:[{type:'text',text:imageResult}]}},
]), ['D:/w/nova-image-1.png']);
// Markdown syntax examples are not artifacts; placeholders and non-file schemes are not paths.
assert.deepEqual(collectWorkspaceArtifacts([{type:'assistant', text:[
  '`![SC03](...)`',
  '```markdown\n[example](fake.md)\n```',
  '[placeholder](...) [encoded](%2E%2E%2E) [ellipsis](…) [empty](< >)',
  '[mail](mailto:test@example.com) [command](javascript:alert)',
  '[real](evidence/SC03.png "截图") [reference][report]',
  '[report]: report.md',
].join('\n\n')}]), ['evidence/SC03.png', 'report.md']);
assert.deepEqual(collectWorkspaceArtifacts([{type:'tool',kind:'edit',status:'completed',
  content:[],locations:[{path:'...'},{path:'README'},{path:'.gitignore'}]}]), ['README','.gitignore']);
console.log("workspace artifacts checks passed");
