import assert from 'node:assert/strict';
import { workspaceDiffRows } from '../src/workspaceDiff.ts';
const rows = workspaceDiffRows('--- a/file\n+++ b/file\n@@ -4,2 +4,2 @@\n same\n-old\n+new\n\\ No newline at end of file\n').flat();
assert.deepEqual(rows.filter(row => row.kind !== 'meta'), [
  {kind:'context',text:'same',old:4,next:4},
  {kind:'del',text:'old',old:5},
  {kind:'add',text:'new',next:5},
]);
assert.equal(workspaceDiffRows('Binary files a/x and b/x differ')[0][0].kind, 'meta');
console.log('workspace diff checks passed');
