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
test('employee continuation and retry keep scheduler ownership', async()=>{
  const [lib, remote, employee] = await Promise.all(['src-tauri/src/lib.rs', 'src-tauri/src/remote.rs', 'src-tauri/src/employee.rs']
    .map(path=>readFile(new URL(`../${path}`,import.meta.url),'utf8')));
  const fallback = lib.match(/fn auto_failover\([\s\S]*?\n\}/)?.[0];
  assert.ok(fallback);
  const employeeGuard = /if thread\.employee_thread \{\s*return Ok\(false\);\s*\}/;
  assert.match(fallback,employeeGuard,'employee retries must be owned by its scheduler, never detached IPC replays');
  for (const effect of ['thread.items','set_thread_agent(','dispatch_prompt(']) {
    assert.ok(fallback.indexOf(effect)>fallback.search(employeeGuard),`${effect} must stay after the employee guard`);
  }
  const dispatch = lib.match(/pub\(crate\) fn dispatch_user_prompt\([\s\S]*?\n\}/)?.[0];
  assert.ok(dispatch);
  assert.match(dispatch,/employee::resume_prompt\(app, &thread_id, &text, &images\)\?/);
  assert.ok(dispatch.indexOf('employee::resume_prompt(') < dispatch.indexOf('dispatch_prompt('));
  assert.match(lib,/dispatch_user_prompt\(&app, thread_id, text, images\.unwrap_or_default\(\)\)/);
  assert.match(remote,/crate::dispatch_user_prompt\(app, thread_id\.to_string\(\), text\.to_string\(\), images\)/);
  assert.match(employee,/start_segment\(app, &thread_id, text, images\)\?/);
  assert.match(employee,/acquire_desktop\(&app, &id, &cancelled\)/);
  assert.match(employee,/DISPATCH_LOCK\.lock\(\)/);
  assert.match(employee,/load\(\)\?\.require_live_task\(&item\)\?/);
  assert.match(employee,/action == "sequence_stop"/);
  for (const call of ['dispatch_active(app, thread_id, text, images)', 'dispatch_active(&app, &id, text, images)',
    'dispatch_active(app, root, text, Vec::new())', 'dispatch_active(app, &task.thread_id, ASK_NUDGE.into(), Vec::new())']) {
    assert.ok(employee.includes(call), `missing cancellation gate: ${call}`);
  }
});
test('employee shared context is advertised and refreshed for starts and relays', async()=>{
  const schema = JSON.parse(await readFile(new URL('../scripts/employee-tool.json',import.meta.url),'utf8'));
  const employee = await readFile(new URL('../src-tauri/src/employee.rs',import.meta.url),'utf8');
  assert.ok(schema.inputSchema.properties.action.enum.includes('context'));
  assert.match(schema.description,/context 查看本轮序列共享上下文/);
  assert.match(schema.inputSchema.properties.id.description,/context/);
  assert.match(employee,/segment_prompt\(e, item_id, prompt, head\)/);
  assert.match(employee,/segment_prompt\(e, &item, &task\.prompt, &head\)/);
  assert.match(employee,/sequence_context\(&e, item, selected\.as_deref\(\)\)/);
  assert.doesNotMatch(employee,/只做下面这一件事，最后用一句话总结结果/);
});
test('employee development follows user workspace requirements without automatic git operations', async()=>{
  const schema = await tool('employee');
  const employee = await readFile(new URL('../src-tauri/src/employee.rs',import.meta.url),'utf8');
  const runtime = employee.split('#[cfg(test)]')[0];
  assert.ok(!schema.inputSchema.properties.action.enum.includes('worktree'));
  assert.match(schema.description,/只有用户明确要求时才用常规 Git 工具创建或复用 worktree/);
  assert.match(schema.inputSchema.properties.cwd.description,/原样使用用户要求的目录/);
  assert.doesNotMatch(runtime,/crate::gitwt::|fn (?:ensure_worktree|sync_worktree|commit_worktree|remove_worktrees|git_ident)\(/);
  assert.doesNotMatch(runtime,/employee action=worktree|本轮结束时系统自动提交/);
  assert.match(runtime,/未要求 worktree 就在用户指定的项目目录工作/);
  assert.match(runtime,/旧步骤模板、交接或记忆中的.*不是用户授权/);
  assert.match(runtime,/t\.relay = Some\(\(scene, cwd\)\)/);
  assert.match(runtime,/cwd\.unwrap_or\(prev_cwd\)/);
  assert.match(runtime,/历史工作树记录/);
});
test('employee external waits remain visible without looking completed', async()=>{
  const ts = (await import('typescript')).default;
  const view = await readFile(new URL('../src/components/EmployeeView.tsx',import.meta.url),'utf8');
  const labels = view.match(/const NODE:[\s\S]*?\n\};/)?.[0];
  const classifier = view.match(/function nodeState\([\s\S]*?\n\}/)?.[0];
  assert.ok(labels && classifier);
  const js = ts.transpileModule(`${labels}\n${classifier}`, {compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
  const stateOf = new Function(`${js}\nreturn nodeState;`)();
  const build = {id:'build',threadId:'session',checkpoint:'build #42 submitted',nextCheckAt:300_000};
  const verify = {id:'verify',after:['build']};
  assert.equal(stateOf(build,[build,verify],[],1),'deferred');
  assert.equal(stateOf(verify,[build,verify],[],1),'deps');
  assert.equal(stateOf(build,[build],[],300_000),'resume');
  assert.equal(stateOf(build,[build],[{itemId:'build',queued:false}],1),'running');
  assert.equal(stateOf({...build,failures:1},[build],[],1),'resume');
  assert.equal(stateOf({...build,waiting:'need input'},[build],[],1),'help');
  assert.equal(stateOf({...build,status:'done'},[build],[],1),'done');
  assert.match(labels,/deferred: \{ label: "等待外部结果"/);
});
test('employee trees show serial, relayed and parallel execution paths', async()=>{
  const ts = (await import('typescript')).default;
  const view = await readFile(new URL('../src/components/EmployeeView.tsx',import.meta.url),'utf8');
  const constants = view.match(/const \[NODE_W,[^\n]+;/)?.[0];
  const helpers = ['sortSiblings','layoutTree','flowDash','sequenceRoot'].map(name=>view.match(new RegExp(`function ${name}\\([\\s\\S]*?\\n\\}`))?.[0]);
  assert.ok(constants && helpers.every(Boolean));
  const js = ts.transpileModule(`${constants}\n${helpers.join('\n')}`, {compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
  const {place,dash,rootOf,w,h,gap,pad} = new Function(`${js}\nreturn {place:layoutTree,dash:flowDash,rootOf:sequenceRoot,w:NODE_W,h:NODE_H,gap:GAP_Y,pad:PAD};`)();
  const root = {id:'root',createdAt:0};
  const steps = Array.from({length:80},(_,i)=>({id:`s${i}`,parent:root.id,after:i?[`s${i-1}`]:[],createdAt:80-i}));
  const {placed:chain,links} = place(root,[root,...steps]);
  assert.equal(chain.length,81);
  assert.deepEqual(chain.map(p=>p.t.id),['root',...steps.map(t=>t.id)]);
  assert.equal(new Set(chain.map(p=>p.x)).size,1,'a serial chain including the root occupies one vertical column');
  assert.equal(Math.max(...chain.map(p=>p.x))+w+pad,2*pad+w);
  assert.ok(chain.every((p,i)=>!i || p.y>=chain[i-1].y+h+gap));
  assert.deepEqual(links.filter(l=>l.from==='root').map(l=>l.to),['s0'],'the root connects only to the first step');
  const nodes = [root,{id:'a',parent:'root',createdAt:1},{id:'b',parent:'root',createdAt:2},
    {id:'join',parent:'root',after:['a','b'],createdAt:3}, {id:'detached',origin:'a',createdAt:4}];
  for(let i=0;i<15;i++) nodes.push({id:`deep${i}`,parent:i?`deep${i-1}`:'a',createdAt:i});
  const branched = place(root,nodes);
  assert.equal(branched.placed.length,nodes.length,'deep children and independent sequences remain visible');
  const at = id=>branched.placed.find(p=>p.t.id===id);
  assert.equal(at('a').y,at('b').y,'parallel steps occupy the same row');
  assert.notEqual(at('a').x,at('b').x);
  assert.ok(at('join').y>at('deep14').y && at('join').y>at('b').y,'join waits for entire predecessor subflows');
  assert.deepEqual(branched.links.filter(l=>l.to==='join').map(l=>l.from).sort(),['b','deep14']);
  assert.ok(branched.links.find(l=>l.from==='a' && l.to==='detached').detached);
  for(const a of branched.placed) for(const b of branched.placed) if(a!==b) {
    assert.ok(a.x+w<=b.x || b.x+w<=a.x || a.y+h<=b.y || b.y+h<=a.y,'cards do not overlap');
  }
  const relayed = place(root,[root,{id:'step1',parent:'root',createdAt:1,relayed:true},
    {id:'relay',parent:'step1',createdAt:2},{id:'step2',parent:'root',after:['step1'],createdAt:3}]);
  assert.deepEqual(relayed.links.map(l=>`${l.from}->${l.to}`).sort(),['root->step1','step1->relay','relay->step2'].sort());
  assert.equal(new Set(relayed.placed.map(p=>p.x)).size,1,'relay segments are inline, not side branches');
  assert.deepEqual(dash({}, {status:'done'}),[]);
  assert.deepEqual(dash({}, {threadId:'live'}),[]);
  assert.deepEqual(dash({}, {}),[5,4]);
  assert.deepEqual(dash({detached:true}, {status:'done'}),[2,4]);
  assert.equal(rootOf('deep14',nodes).id,'root');
  assert.equal(rootOf('detached',nodes).id,'detached','stopping does not follow origin');
  assert.deepEqual(place(undefined,nodes),{placed:[],links:[]});
  const cycleRoot = {id:'root',parent:'a',createdAt:0};
  assert.equal(place(cycleRoot,[cycleRoot,nodes[1]]).placed.length,2);
  assert.match(view,/props\.act\("sequence_stop", root\.id\)/);
  assert.match(view,/停止本轮序列/);
  assert.match(view,/disabled=\{stopping\(\)\}/);
});
test('employee board and tree details use separate responsive horizontal panes', async()=>{
  const css = await readFile(new URL('../src/components/EmployeeView.css',import.meta.url),'utf8');
  assert.match(css,/\.employee-layout \{[^}]*grid-template-columns: clamp\(220px, 26%, 300px\) minmax\(0, 1fr\)/);
  assert.match(css,/@container employee-board \(max-width: 640px\)/);
  assert.match(css,/\.employee-tree \{[^}]*display: grid;[^}]*grid-template-columns: minmax\(240px, \.9fr\) minmax\(0, 1\.1fr\)/);
  assert.match(css,/@container employee-detail \(max-width: 520px\)/);
  assert.match(css,/\.employee-tree-detail \{[^}]*overflow: auto/);
});
test('Chrome retains image coordinates, fast scope and bounded batch schemas',async()=>{
  const chrome=await tool('chrome');
  for(const name of ['action','imageId','scope','region','visual','maxEdge']) assert(chrome.inputSchema.properties[name]);
  const withoutDescriptions=value=>JSON.parse(JSON.stringify(value,(key,value)=>key==='description'?undefined:value));
  assert.deepEqual(withoutDescriptions(chrome.inputSchema.properties.actions.items),withoutDescriptions(chrome.inputSchema.properties.action));
  assert.equal(chrome.inputSchema.properties.actions.maxItems,16);
  assert.equal(chrome.inputSchema.properties.incognito.type,"boolean");
  assert.equal(chrome.inputSchema.properties.actions.items.properties.duration_ms.maximum,1500);
  assert.match(chrome.inputSchema.properties.fullPage.description,/每次返回完整图.*32×32.*阈值16.*复用已交付的同一文件/);
  assert.doesNotMatch(chrome.inputSchema.properties.fullPage.description,/64×64|增量|关键帧/);
  assert.match(chrome.inputSchema.properties.fullPage.description,/同一帧流只保留最新2张不同内容的完整图.*无需read\/recall/);
  assert.match(chrome.inputSchema.properties.fullPage.description,/最新snapshotId与最新完整图的imageId.*不换算CSS\/DPI/);
  assert.match(chrome.inputSchema.properties.fullPage.description,/expandScroll.*完整独立图片/);
  assert.match(chrome.inputSchema.properties.feedback.description,/openedTabs.*本次由源页新开.*唯一active且可操作.*自动观察.*新的tabTag.*sourceTabTag记录原输入页.*多个、后台或不可操作候选不猜.*按openedTabs选择.*不要把旧页未变化视为点击失败.*feedback=none不自动观察.*按新tabTag\/snapshotId继续，不重放开页动作/);
  assert.match(chrome.inputSchema.properties.fullPage.description,/images\[\]\.unchanged=true.*复用已交付的同一文件.*本次snapshotId\/imageId.*不代表动作失败.*不重放输入或循环截图/);
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

test('Jianlai keeps the latest two full images of each screen independent', async()=>{
  const [jianlai, source, nova, provider, context, taskbar] = await Promise.all([
    tool('jianlai'),
    readFile(new URL('../src-tauri/src/jianlai.rs',import.meta.url),'utf8'),
    readFile(new URL('../src-tauri/src/jianlai/nova_regions.rs',import.meta.url),'utf8'),
    readFile(new URL('../src-tauri/src/lyra/provider.rs',import.meta.url),'utf8'),
    readFile(new URL('../src-tauri/src/lyra/context.rs',import.meta.url),'utf8'),
    readFile(new URL('../src-tauri/src/taskbar_status/windows.rs',import.meta.url),'utf8'),
  ]);
  assert.match(jianlai.description,/不跨屏拼接.*每屏最近2张完整图独立保留.*无需read\/recall/);
  assert.match(jianlai.inputSchema.properties.imageId.description,/每屏只保留最新2张不同内容的完整图.*unchanged复用同一图/);
  assert.match(jianlai.inputSchema.properties.feedback.description,/Butterfly.*与Nova是否前台无关/);
  assert.match(jianlai.inputSchema.properties.feedback.description,/images\[\]\.unchanged=true.*复用上一张同内容图.*本次snapshotId\/imageId.*不代表动作失败.*不重放输入或循环截图/);
  assert.match(jianlai.inputSchema.properties.feedback.description,/局部操作后也回整屏.*background:true仍反馈原目标/);
  assert.match(jianlai.inputSchema.properties.actions.description,/images\[\]\.frame.*scroll在actions中的0起始下标.*仅供阅读、不参与普通帧流替换或定位/);
  assert.match(jianlai.inputSchema.properties.imageId.description,/顶层最新snapshotId.*最终观察中不带frame/);
  assert.doesNotMatch(source,/"(?:deltaNotice|unchangedNotice|framesNotice|coordinateNotice)"|局部操作后的反馈恢复所属完整屏幕|历史截图仅供阅读，不产生可操作快照/);
  assert.doesNotMatch(source,/deltaPolicy|adaptive-grid|latestDeltas/);
  assert.match(source,/resize\(nova_regions::comparison_pixels\(&image, &source_ignored\)\)/);
  assert.match(source,/DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2/);
  assert.doesNotMatch(source,/mod desktop|desktop::compose|context-region/);
  assert.doesNotMatch(nova,/GetForegroundWindow/);
  assert.match(nova,/GetTopWindow\(parent\)/);
  assert.match(nova,/TrayClockWClass/);
  assert.match(source,/nova_regions::stable\(&nova_bounds, &nova_regions::visible\(\)\)/);
  assert.match(taskbar,/IsWindowVisible\(hwnd\) == 0 \|\| GetTopWindow\(parent\) != hwnd/,'restore the chip after Explorer changes sibling Z order without moving it');
  assert.match(provider,/do not call read\/recall/);
  assert.doesNotMatch(provider,/stream\.starts_with\("monitor-"\).*"desktop"/);
  assert.match(context,/KEEP_FRAMES.*keep\[index\] = true/);
});
