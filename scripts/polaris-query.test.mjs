import {test} from 'node:test';
import assert from 'node:assert/strict';
import {normalizePolarisArgs as normalize} from './polaris-query.mjs';
test('CJK sentences and two-character queries are tasks, not literal anchors',()=>{
  for(const query of ['停止按钮为什么不能立即中断生成','取消','登录','怎么处理 OOPIF 的坐标？']) {
    const p=normalize({query});assert.equal(p.task,query);assert.deepEqual(p.keywords,[]);
  }
});
test('exact names stay anchors and a supplied task is preserved',()=>{
  assert.deepEqual(normalize({query:'cancel_turn'}).keywords,['cancel_turn']);
  assert.equal(normalize({query:'cancel_turn',task:'停止任务'}).task,'停止任务');
});
test('native-shaped task and natural-language keyword forms normalize consistently',()=>{
  for(const p of [{query:'取消任务'},{task:'取消任务'},{keywords:'取消任务'}]){
    assert.equal(normalize(p).task,'取消任务');assert.deepEqual(normalize(p).keywords,[]);
  }
});
test('de-duplicates, bounds and rejects non-object requests',()=>{
  assert.deepEqual(normalize({keywords:[' Name ','name',null,{}]}).keywords,['Name']);
  assert.throws(()=>normalize(null));assert.throws(()=>normalize([]));
  assert.equal([...normalize({query:'词'.repeat(9000)}).task].length,1024);
});
test('file names retain case and natural-language keyword tasks are bounded',()=>{
  assert.deepEqual(normalize({files:['Foo.ts','foo.ts']}).files,['Foo.ts','foo.ts']);
  assert.deepEqual(normalize({files:['src\\job.rs']}).files,['src/job.rs']);
  assert.equal([...normalize({keywords:['词'.repeat(1000),'字'.repeat(1000)]}).task].length,1024);
});
