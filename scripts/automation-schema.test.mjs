// Verify all adapter registrations retain the new fields and automatic visual feedback.
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
const tool = async name=>JSON.parse(await readFile(new URL(`./${name}-tool.json`,import.meta.url),'utf8'));
test('ordinary agents stay separate from internal scene switching and fallback', async()=>{
  const read = path=>readFile(new URL(`../${path}`,import.meta.url),'utf8');
  const [agentJson,session,lib,threads] = await Promise.all([
    'scripts/agent-tools.json','src-tauri/src/lyra/session.rs','src-tauri/src/lib.rs','src-tauri/src/threads.rs'
  ].map(read));
  const agents = JSON.parse(agentJson);
  const spawn = agents.find(tool=>tool.name==='spawn_agent');
  assert.deepEqual(Object.keys(spawn.inputSchema.properties).sort(),['fork_turns','message','task_name']);
  assert.deepEqual(spawn.inputSchema.required,['task_name','message']);
  assert.match(spawn.description,/普通子 Agent/);
  assert.match(spawn.description,/switch_scene/);
  assert.doesNotMatch(agentJson,/独立的 Stage 会话|转交的场景子任务|同一个场景会话/);
  assert.match(session,/pub fn agent_tools\(\) -> Vec<Tool>/);
  const nativeSpawn = session.match(/let spawn = Tool \{([\s\S]*?)\n    \};/)?.[1];
  assert.ok(nativeSpawn,'ordinary spawn schema must remain available');
  assert.doesNotMatch(nativeSpawn,/"scene"|"cwd"|handoff/);
  assert.match(nativeSpawn,/"fork_turns"/);
  assert.doesNotMatch(session,/fn scene_spawn\(/);
  assert.doesNotMatch(lib+threads,/request_escalation|register_auto_escalation|pending_escalation|fn escalates?\(/);
  const context = lib.match(/fn auto_mode_context\([\s\S]*?\n\}/)?.[0];
  assert.ok(context);
  assert.match(context,/switch_scene/);
  assert.match(context,/自动恢复之前的模型继续用户任务/);
  assert.doesNotMatch(context,/scope\.scene\.label\(\)|结论会交回上游|spawn_agent/);
  const fallback = lib.match(/fn auto_failover\([\s\S]*?\n\}/)?.[0];
  assert.ok(fallback);
  const guard = /if thread\.agent_kind == AgentKind::Lyra && !thread\.employee_thread \{\s*return Ok\(false\);\s*\}/;
  assert.match(fallback,guard,'ordinary Lyra fallback must stay inside the runtime, not replay via IPC');
  const guardAt = fallback.search(guard);
  for (const effect of ['thread.items','set_thread_agent(','dispatch_prompt(']) {
    assert.ok(fallback.indexOf(effect)>guardAt,`${effect} must stay after the non-employee guard`);
  }
});
test('Chrome retains image coordinates, fast scope and bounded batch schemas',async()=>{
  const chrome=await tool('chrome');
  for(const name of ['action','imageId','scope','region','visual','maxEdge']) assert(chrome.inputSchema.properties[name]);
  const withoutDescriptions=value=>JSON.parse(JSON.stringify(value,(key,value)=>key==='description'?undefined:value));
  assert.deepEqual(withoutDescriptions(chrome.inputSchema.properties.actions.items),withoutDescriptions(chrome.inputSchema.properties.action));
  assert.equal(chrome.inputSchema.properties.actions.maxItems,16);
  assert.equal(chrome.inputSchema.properties.incognito.type,"boolean");
  assert.equal(chrome.inputSchema.properties.actions.items.properties.duration_ms.maximum,1500);
});
test('short lookahead is available without experience and Chrome exposes a pre-action guard', async()=>{
  const [chrome,jianlai]=await Promise.all(['chrome','jianlai'].map(tool));
  for (const tool of [chrome,jianlai]) {
    const guidance = tool.inputSchema.properties.plan?.description ?? tool.inputSchema.properties.actions.description;
    assert.match(guidance,/无论有无经验/);
    assert.match(guidance,/没有经验或检索无命中也直接根据界面和任务推演/);
    assert.match(guidance,/2–4步/);
  }
  const when=chrome.inputSchema.properties.plan.properties.steps.items.properties.when;
  assert.equal(when.type,'string');
  assert.equal(when.minLength,1);
  assert.equal(when.maxLength,500);
  assert.match(when.description,/本步及后续不执行/);
  assert.match(jianlai.inputSchema.properties.actions.description,/不猜未来坐标/);
});

test('Chrome and Jianlai run only executes pre-confirmed steps', async()=>{
  const [chrome,jianlai]=await Promise.all(['chrome','jianlai'].map(tool));
  for (const browser of [chrome]) {
    assert.doesNotMatch(browser.description,/JEV|advise|runTemplate|DOM排障|Altair|vision/);
    assert.deepEqual(browser.inputSchema.properties.plan.required,['task','steps']);
    assert.equal(browser.inputSchema.properties.action.properties.target,undefined);
    assert.equal(browser.inputSchema.properties.actions.items.properties.target,undefined);
    assert(browser.inputSchema.properties.operation.enum.includes('experience_save'));
  }
  const typing=jianlai.inputSchema.properties.actions.items.properties.text.description;
  assert.match(typing,/Windows的type整段粘贴，目标读取后自动恢复原剪贴板，从不逐字模拟按键/);
  assert.doesNotMatch(jianlai.description,/Unicode/);
  assert.match(typing,/clipboard\.text.*相等就不重输/);
  assert.match(typing,/未核验前不发送/);
  assert.match(jianlai.inputSchema.properties.snapshotId.description,/act\/run必填/);
  assert.match(jianlai.inputSchema.properties.snapshotId.description,/漏snapshotId只返回补参提示/);
  assert.match(jianlai.inputSchema.properties.snapshotId.description,/纯wait不依赖快照/);
  assert(jianlai.description.length<650,'Keep operation-specific instructions beside their parameters');
  assert.match(jianlai.description,/已满足就立即汇报并结束/);
  assert.match(jianlai.description,/verification=unverified不要求额外检查/);
  assert.match(jianlai.description,/禁止再截图、查状态/);
  assert.match(jianlai.inputSchema.properties.experience.description,/返回accepted即可收尾/);
  assert.match(jianlai.inputSchema.properties.observe.description,/无匹配或有歧义/);
  assert.equal(jianlai.inputSchema.properties.plan,undefined);
  assert.deepEqual(jianlai.inputSchema.properties.actions.items.allOf[0].then,{required:['x','y']});
  assert.equal(chrome.inputSchema.properties.plan.properties.steps.maxItems,24);
  assert.match(chrome.inputSchema.properties.tabTag.description,/本会话snapshotId/);
  for (const key of ['anyOf','oneOf','allOf']) assert.equal(chrome.inputSchema[key],undefined,'Keep the tool compatible with Claude Code');
  assert.equal(chrome.inputSchema.properties.plan.properties.maxActions.default,32);
  assert.equal(chrome.inputSchema.properties.plan.properties.inputs.maxItems,8);
  assert.deepEqual(chrome.inputSchema.properties.plan.properties.steps.items.required,['action']);
  for(const tool of [chrome,jianlai]) {
    assert(!tool.inputSchema.properties.operation.enum.includes('advise'));
    assert.equal(tool.inputSchema.properties.advice,undefined);
    assert(tool.inputSchema.properties.operation.enum.includes('run'));
    assert.equal(tool.inputSchema.properties.vision,undefined);
    assert.equal(tool.inputSchema.properties.look,undefined);
  }
});
