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
  assert.match(context,/默认由目标场景完成剩余任务并直接交付/);
  assert.match(context,/resume 参数写明具体后续工作/);
  assert.match(context,/仅还原初始模型配置，不额外请求模型/);
  assert.doesNotMatch(context,/自动恢复之前的模型继续用户任务/);
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
  assert.match(chrome.inputSchema.properties.fullPage.description,/3×3九宫格.*最多9块.*先横后竖.*不跨未变格/);
  assert.doesNotMatch(chrome.inputSchema.properties.fullPage.description,/32×32/);
  assert.match(chrome.inputSchema.properties.fullPage.description,/只保留当前关键帧组.*各流独立保留.*压缩后自动补回.*无需read\/recall/);
  assert.match(chrome.inputSchema.properties.fullPage.description,/deltaOf.*x\/y一次/);
  assert.match(chrome.inputSchema.properties.fullPage.description,/expandScroll.*完整独立图片/);
  assert.match(chrome.inputSchema.properties.feedback.description,/openedTabs.*本次由源页新开.*唯一active且可操作.*自动观察.*新的tabTag.*sourceTabTag记录原输入页.*多个、后台或不可操作候选不猜.*按openedTabs选择.*不要把旧页未变化视为点击失败.*feedback=none不自动观察.*按新tabTag\/snapshotId继续，不重放开页动作/);
  assert.match(chrome.inputSchema.properties.fullPage.description,/images\[\]\.unchanged=true.*未重复附图.*本次snapshotId\/imageId.*不代表动作失败.*不重放输入或循环截图/);
  assert.doesNotMatch(await readFile(new URL('../src-tauri/src/native_browser.rs',import.meta.url),'utf8'),/"(?:deltaNotice|unchangedNotice)"/);
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
  assert.match(jianlai.description,/executed只表示输入已发送，不代表界面加载完成或业务成功/);
  assert.match(jianlai.description,/禁止再截图、查状态/);
  assert.match(jianlai.inputSchema.properties.feedback.description,/截图只采一帧.*显式wait后观察/);
  assert.match(jianlai.inputSchema.properties.notes.description,/原样返回，未经工具验证/);
  assert.match(jianlai.inputSchema.properties.experience.description,/返回accepted即可收尾/);
  assert.match(jianlai.inputSchema.properties.observe.description,/无匹配或有歧义/);
  assert.match(jianlai.inputSchema.properties.observe.description,/普通模式查询不激活窗口.*focused\/observedWindowFocused=false.*先显式activate\(windowId\)并核对反馈.*nextCall.*不得把命中窗口等同当前输入目标/);
  assert.match(jianlai.inputSchema.properties.windowId.description,/不激活窗口.*observe中的focused\/observedWindowFocused规则/);
  assert.match(jianlai.inputSchema.properties.actions.description,/普通模式指针操作不因焦点隐式activate/);
  assert.match(jianlai.inputSchema.properties.region.description,/新snapshotId\/imageId.*左上角\(0,0\).*images中的width\/height.*不加region偏移或使用显示放大后的坐标/);
  assert.match(jianlai.inputSchema.properties.imagePath.description,/仅供阅读，不产生可操作快照.*当前截图的snapshotId\/imageId/);
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

test('Jianlai keeps each screen and its nine-grid image context independent', async()=>{
  const [jianlai, source, nova, provider, context, taskbar] = await Promise.all([
    tool('jianlai'),
    readFile(new URL('../src-tauri/src/jianlai.rs',import.meta.url),'utf8'),
    readFile(new URL('../src-tauri/src/jianlai/nova_regions.rs',import.meta.url),'utf8'),
    readFile(new URL('../src-tauri/src/lyra/provider.rs',import.meta.url),'utf8'),
    readFile(new URL('../src-tauri/src/lyra/context.rs',import.meta.url),'utf8'),
    readFile(new URL('../src-tauri/src/taskbar_status/windows.rs',import.meta.url),'utf8'),
  ]);
  assert.match(jianlai.description,/不跨屏拼接.*每屏最近基线及增量独立保留.*无需read\/recall/);
  assert.match(jianlai.inputSchema.properties.imageId.description,/3×3九宫格.*最多9块.*先横后竖/);
  assert.match(jianlai.inputSchema.properties.feedback.description,/Butterfly.*与Nova是否前台无关/);
  assert.match(jianlai.inputSchema.properties.feedback.description,/images\[\]\.unchanged=true.*未重复附图.*本次snapshotId\/imageId.*不代表动作失败.*不重放输入或循环截图/);
  assert.match(jianlai.inputSchema.properties.feedback.description,/局部操作后也回整屏.*background:true仍反馈原目标/);
  assert.match(jianlai.inputSchema.properties.actions.description,/images\[\]\.frame.*scroll在actions中的0起始下标.*仅供阅读、不参与普通帧流替换或定位/);
  assert.match(jianlai.inputSchema.properties.imageId.description,/顶层最新snapshotId.*最终观察中不带frame/);
  assert.doesNotMatch(source,/"(?:deltaNotice|unchangedNotice|framesNotice|coordinateNotice)"|局部操作后的反馈恢复所属完整屏幕|历史截图仅供阅读，不产生可操作快照/);
  assert.match(source,/"deltaPolicy":"nine-grid"/);
  assert.match(source,/resize\(nova_regions::comparison_pixels\(&image, &source_ignored\)\)/);
  assert.match(source,/DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2/);
  assert.doesNotMatch(source,/mod desktop|desktop::compose|context-region/);
  assert.doesNotMatch(nova,/GetForegroundWindow/);
  assert.match(nova,/GetTopWindow\(parent\)/);
  assert.match(nova,/TrayClockWClass/);
  assert.match(source,/nova_regions::stable\(&nova_bounds, &nova_regions::visible\(\)\)/);
  assert.match(taskbar,/IsWindowVisible\(hwnd\) == 0 \|\| GetTopWindow\(parent\) != hwnd/,'restore the chip after Explorer changes sibling Z order without moving it');
  assert.match(provider,/Do not call read\/recall/);
  assert.doesNotMatch(provider,/stream\.starts_with\("monitor-"\).*"desktop"/);
  assert.match(context,/screens\.insert\(stream\).*keep\[index\] = true/);
});
