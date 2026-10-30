// Verify all adapter registrations retain the new fields and automatic visual feedback.
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
const tool = async name=>JSON.parse(await readFile(new URL(`./${name}-tool.json`,import.meta.url),'utf8'));
test('password book uses one freeform secret and never derives public names from it', async()=>{
  const [employee,view] = await Promise.all([
    tool('employee'),readFile(new URL('../src/components/VaultPop.tsx',import.meta.url),'utf8')
  ]);
  const fields = employee.inputSchema.properties;
  assert.equal(fields.content.type,'string');
  assert.equal(fields.content.maxLength,16000);
  assert.ok(!('username' in fields) && !('password' in fields));
  assert.match(view,/<textarea rows=\{5\} value=\{d\(\)\.content\}/);
  assert.doesNotMatch(view,/\busername\b|\bpassword\b|vault-pair/);
  const source = view.match(/  const save = async \(\) => \{[\s\S]*?\n  \};/)?.[0];
  assert.ok(source);
  const ts = (await import('typescript')).default;
  const compiled = ts.transpileModule(source,{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
  let current = {condition:'  调用测试 API 时  ',content:'  API_KEY=secret\nPRIVATE_KEY=line1\nline2\n',share:false};
  const calls = [];
  let refreshes = 0;
  const save = new Function('draft','invoke','setDraft','refresh','showToast',`${compiled};return save;`)(
    ()=>current,async(command,args)=>{calls.push({command,...args});},value=>{current=value;},()=>{refreshes++;},error=>assert.fail(error));
  const content = current.content;
  await save();
  assert.equal(calls[0].command,'vault_save');
  assert.equal(calls[0].entry.content,content,'multiline credentials and whitespace must remain unchanged');
  assert.equal(calls[0].entry.name,'调用测试 API 时','names are public metadata, not a secret preview');
  assert.equal(current,null);
  current = {id:'v1',condition:'使用旧凭据时',content:'',share:true};
  await save();
  assert.equal(calls[1].entry.content,'','editing metadata leaves stored credentials untouched');
  assert.equal(refreshes,2);
});
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
  assert.match(employee,/dispatch_active\(app, &thread_id, text, images\)\?/);
  assert.match(employee,/DISPATCH_LOCK\.lock\(\)/);
  assert.match(employee,/load\(\)\?\.require_live_task\(&item\)\?/);
  assert.match(employee,/action == "sequence_stop"/);
  for (const call of ['dispatch_active(app, &thread_id, text, images)', 'dispatch_active(app, root, ASK_NUDGE.into(), Vec::new())']) {
    assert.ok(employee.includes(call), `missing cancellation gate: ${call}`);
  }
});
test('employee tasks reuse two isolated sessions, mark progress and share one desktop', async()=>{
  const schema = JSON.parse(await readFile(new URL('../scripts/employee-tool.json',import.meta.url),'utf8'));
  const [employee, bridge] = await Promise.all(['src-tauri/src/employee.rs', 'src-tauri/src/lyra/bridge.rs']
    .map(path=>readFile(new URL(`../${path}`,import.meta.url),'utf8')));
  const actions = schema.inputSchema.properties.action.enum;
  assert.ok(actions.includes('progress') && actions.includes('defer'));
  assert.ok(actions.includes('relay') && !actions.includes('context'));
  assert.deepEqual(schema.inputSchema.properties.scene.enum, ['computer']);
  assert.match(schema.description,/最多两个固定会话/);
  assert.ok(schema.inputSchema.properties.key);
  assert.match(employee,/segment_prompt\(e, &prompt, head\)/);
  assert.match(employee,/Outcome::DeskBusy/);
  assert.match(employee,/t\.session_thread\(computer\)/);
  assert.match(employee,/t\.finish_segment\(computer, outcome, task\.started_ms, &full\)/);
  assert.match(bridge,/crate::employee::same_session\(scope\.scene, \*scene\)/);
  assert.match(bridge,/if employee \{ auto\.as_ref\(\)\.is_some_and\(crate::AutoScope::desktop\) \}/);
  assert.doesNotMatch(bridge,/computer_gate|desktop_gate/);
  assert.match(employee,/root\.as_deref\(\) != Some\(thread\.as_str\(\)\)/, 'idle computer sessions must not borrow their parent task');
  const launch = employee.match(/fn launch\([\s\S]*?\n\}/)?.[0];
  assert.ok(launch);
  assert.doesNotMatch(launch,/stage_source_thread_id|fork_thread/);
  assert.doesNotMatch(employee,/只做下面这一件事，最后用一句话总结结果/);
});
test('employee development runs ordinary branch operations unless the user requires review', async()=>{
  const schema = await tool('employee');
  const employee = await readFile(new URL('../src-tauri/src/employee.rs',import.meta.url),'utf8');
  const runtime = employee.split('#[cfg(test)]')[0];
  assert.ok(!schema.inputSchema.properties.action.enum.includes('worktree'));
  assert.match(schema.description,/只有用户明确要求时才用常规 Git 工具创建或复用 worktree/);
  assert.doesNotMatch(runtime,/crate::gitwt::|fn (?:ensure_worktree|sync_worktree|commit_worktree|remove_worktrees|git_ident)\(/);
  assert.doesNotMatch(runtime,/employee action=worktree|本轮结束时系统自动提交/);
  assert.match(runtime,/未要求 worktree 就在用户指定的项目目录工作/);
  assert.match(runtime,/记忆中的.*不是用户授权/);
  for (const prompt of [runtime,schema.description]) {
    assert.match(prompt,/开发任务默认自行创建或复用开发分支、提交本需求改动并向对应远程普通推送，无需逐项授权/);
    assert.match(prompt,/只有用户明确要求先审核或确认时，才暂停等待审批/);
    assert.match(prompt,/不能仅因用户没有逐项授权就 ask/);
    assert.match(prompt,/用户明确禁止或限制的操作必须遵守/);
    assert.doesNotMatch(prompt,/用户已要求完成开发并将开发分支构建到测试环境时|普通推送属于流程内授权/);
    assert.match(prompt,/只提交本需求改动，不混入用户或其它任务的修改/);
    assert.match(prompt,/强推、合并或改写 release\/test 等共享分支、删除分支、发布生产不属于常规开发分支操作，仍须单独授权/);
  }
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
test('employee approvals keep the original task visibly paused and resume its full scope', async()=>{
  const ts = (await import('typescript')).default;
  const [view,employee,schema] = await Promise.all([
    readFile(new URL('../src/components/EmployeeView.tsx',import.meta.url),'utf8'),
    readFile(new URL('../src-tauri/src/employee.rs',import.meta.url),'utf8'),tool('employee')
  ]);
  const labels = view.match(/const NODE:[\s\S]*?\n\};/)?.[0];
  const classifier = view.match(/function nodeState\([\s\S]*?\n\}/)?.[0];
  assert.ok(labels && classifier);
  const js = ts.transpileModule(`${labels}\n${classifier}`,{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
  const stateOf = new Function(`${js};return nodeState;`)();
  const task = {id:'original',threadId:'work',approval:'c1'};
  assert.equal(stateOf(task,[task],[],1),'confirm');
  assert.equal(stateOf(task,[task],[{itemId:task.id,queued:false}],1),'confirm','approval can arrive before the asking turn ends');
  assert.equal(stateOf({...task,approval:''},[task],[],1),'resume');
  assert.equal(stateOf({...task,approval:''},[task],[{itemId:task.id,queued:false}],1),'running');
  assert.equal(stateOf({...task,status:'cancelled'},[task],[],1),'cancelled');
  assert.match(labels,/confirm: \{ label: "等待批准"/);
  assert.match(employee,/let waiting = todo\.awaits_user\(\) && outcome != Outcome::Stopped/);
  const approve = employee.slice(employee.indexOf('"approve" => {'),employee.indexOf('\n        _ => Err(format!("未知操作：{action}"'));
  assert.match(approve,/e\.approve_confirmation\(&id/);
  assert.match(employee,/完成获批事项后，按原任务已有授权继续后续未完成步骤/);
  assert.match(schema.description,/执行中的原任务暂停而非完成/);
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
  assert.equal(branched.placed.length,nodes.length-1,'deep children stay visible without merging independent sequences');
  const at = id=>branched.placed.find(p=>p.t.id===id);
  assert.equal(at('a').y,at('b').y,'parallel steps occupy the same row');
  assert.notEqual(at('a').x,at('b').x);
  assert.ok(at('join').y>at('deep14').y && at('join').y>at('b').y,'join waits for entire predecessor subflows');
  assert.deepEqual(branched.links.filter(l=>l.to==='join').map(l=>l.from).sort(),['b','deep14']);
  assert.ok(!branched.placed.some(p=>p.t.id==='detached'));
  assert.deepEqual(place(nodes.find(t=>t.id==='detached'),nodes).placed.map(p=>p.t.id),['detached']);
  for(const a of branched.placed) for(const b of branched.placed) if(a!==b) {
    assert.ok(a.x+w<=b.x || b.x+w<=a.x || a.y+h<=b.y || b.y+h<=a.y,'cards do not overlap');
  }
  const relayed = place(root,[root,{id:'step1',parent:'root',createdAt:1,relayed:true},
    {id:'relay',parent:'step1',createdAt:2},{id:'step2',parent:'root',after:['step1'],createdAt:3}]);
  assert.deepEqual(relayed.links.map(l=>`${l.from}->${l.to}`).sort(),['root->step1','step1->relay','relay->step2'].sort());
  assert.equal(new Set(relayed.placed.map(p=>p.x)).size,1,'relay segments are inline, not side branches');
  assert.deepEqual(dash({status:'done'}),[]);
  assert.deepEqual(dash({threadId:'live'}),[]);
  assert.deepEqual(dash({}),[5,4]);
  assert.equal(rootOf('deep14',new Map(nodes.map(t=>[t.id,t]))).id,'root');
  assert.equal(rootOf('detached',new Map(nodes.map(t=>[t.id,t]))).id,'detached','stopping does not follow origin');
  assert.deepEqual(place(undefined,nodes),{placed:[],links:[]});
  const cycleRoot = {id:'root',parent:'a',createdAt:0};
  assert.equal(place(cycleRoot,[cycleRoot,nodes[1]]).placed.length,2);
  assert.match(view,/props\.act\("sequence_stop", root\.id\)/);
  assert.match(view,/停止任务/);
  assert.match(view,/disabled=\{stopping\(\)\}/);
});
test('employee sequences stay separate even with a shared duty or derived origin', async()=>{
  const ts = (await import('typescript')).default;
  const view = await readFile(new URL('../src/components/EmployeeView.tsx',import.meta.url),'utf8');
  const helpers = ['sequenceRoot','indexSequences'].map(name=>view.match(new RegExp(`function ${name}\\([\\s\\S]*?\\n\\}`))?.[0]);
  assert.ok(helpers.every(Boolean));
  const js = ts.transpileModule(helpers.join('\n'), {compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
  const index = new Function(`${js}\nreturn indexSequences;`)();
  const nodes = [
    {id:'first',dutyId:'duty',status:'done'},
    {id:'second',dutyId:'duty'},
    {id:'derived',origin:'first',fromDuty:'duty'},
    {id:'nested',origin:'derived',fromDuty:'duty'},
    {id:'step',parent:'derived'},
  ];
  const grouped = index(nodes);
  assert.deepEqual(grouped.tops.map(t=>t.id),['first','second','derived','nested']);
  assert.deepEqual(grouped.members.get('first').map(t=>t.id),['first'],'finished sources are not kept active by independent tasks');
  assert.deepEqual(grouped.members.get('derived').map(t=>t.id),['derived','step']);
  assert.equal(grouped.rootById.get('step').id,'derived');
  assert.doesNotMatch(view,/familyTop|familyRoots|familyNodes|sequenceNodes/);
  assert.match(view,/const sequences = createMemo\(\(\) => indexSequences\(nodes\(\)\)\)/);
  assert.match(view,/const layout = createMemo\(\(\) => layoutTree\(top\(\), displayNodes\(\)\)\)/);
  for (const name of ['taskRows','recentRows','current','currentKey']) {
    assert.match(view,new RegExp(`const ${name} = createMemo\\(`),'row bindings must reuse computed lists and selection');
  }
  let reads = 0;
  const many = Array.from({length:300},(_,i)=>({get id(){reads++;return `t${i}`;},createdAt:i}));
  const full = index(many);
  for (const root of full.tops) assert.equal(full.members.get(root.id).length,1);
  assert.ok(reads < many.length * 12,`flat sequences must build one index, not scan the full list per row (${reads} reads)`);
  const orphan = {id:'orphan',parent:'gone'};
  assert.equal(index([orphan]).rootById.get('orphan'),orphan);
  assert.deepEqual(index([]).tops,[]);
  const cycle = index([{id:'a',parent:'b'},{id:'b',parent:'a'}]);
  assert.equal(cycle.rootById.size,2,'malformed legacy data must not loop forever');
});
test('employee plans draw future steps inside one task without pretending logs are a plan', async()=>{
  const ts = (await import('typescript')).default;
  const view = await readFile(new URL('../src/components/EmployeeView.tsx',import.meta.url),'utf8');
  const schema = await tool('employee');
  assert.ok(schema.inputSchema.properties.plan);
  assert.deepEqual(schema.inputSchema.properties.plan.items.required,['content','status']);
  const constants = [view.match(/const NODE:[\s\S]*?\n\};/)?.[0],view.match(/const \[NODE_W,[^\n]+;/)?.[0]];
  const helpers = ['nodeState','executionNodes','sortSiblings','layoutTree'].map(name=>view.match(new RegExp(`function ${name}\\([\\s\\S]*?\\n\\}`))?.[0]);
  assert.ok(constants.every(Boolean) && helpers.every(Boolean));
  const js = ts.transpileModule([...constants,...helpers].join('\n'),{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
  const {nodesFor,place,stateOf} = new Function(`${js}\nreturn {nodesFor:executionNodes,place:layoutTree,stateOf:nodeState};`)();
  const top = {id:'t',text:'整个任务',threadId:'work',createdAt:1,plan:[
    {content:'读需求',status:'completed'},{content:'编码',status:'in_progress'},
    {content:'构建',status:'pending'},{content:'验证',status:'pending'}]};
  const original = JSON.stringify(top);
  const nodes = nodesFor(top,[top]);
  assert.equal(nodes.length,5);
  assert.equal(JSON.stringify(top),original,'diagram projection does not create or mutate scheduled tasks');
  const flow = place(top,nodes);
  assert.equal(flow.placed.length,5);
  assert.deepEqual(flow.links.map(l=>[l.from,l.to]),[['t','t/plan/0'],['t/plan/0','t/plan/1'],['t/plan/1','t/plan/2'],['t/plan/2','t/plan/3']]);
  assert.deepEqual(nodes.slice(1).map(t=>stateOf(t,nodes,[{itemId:'t',queued:false}],1)),['done','running','future','future']);
  top.waiting = '验证码';
  assert.equal(stateOf(nodes[2],nodes,[],1),'help');
  top.waiting = '';
  top.status = 'done';
  assert.equal(stateOf(nodes[2],nodes,[],1),'unverified','task completion cannot invent unreported step completion');
  assert.equal(stateOf(nodes[4],nodes,[],1),'future');
  const legacy = [{id:'old',text:'一项任务',progress:[{text:'3/3 某次操作结束'}]}];
  assert.equal(nodesFor(legacy[0],legacy),legacy,'no invented plan from a free-text progress counter');
  assert.match(view,/尚未登记步骤计划/);
  assert.match(view,/setSelected\(hit\.t\.planStep === undefined \? hit\.t\.id : top\(\)!\.id\)/,'step clicks still act on the single task');
});
test('employee task deletion confirms once, covers active and finished details, and revokes runtime before cancellation', async()=>{
  const ts = (await import('typescript')).default;
  const [view,employee] = await Promise.all(['src/components/EmployeeView.tsx','src-tauri/src/employee.rs']
    .map(path=>readFile(new URL(`../${path}`,import.meta.url),'utf8')));
  const component = view.match(/function DeleteTaskButton\([\s\S]*?\n\}/)?.[0];
  assert.ok(component);
  const logic = `${component.slice(0,component.indexOf('  return <button'))}\n  return {remove,deleting};\n}`;
  const js = ts.transpileModule(logic,{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
  const signal = value=>[()=>value,next=>{value=next;}];
  const createButton = new Function('createSignal','confirm','showToast',`${js}\nreturn DeleteTaskButton;`);
  let requested = 0;
  const cancelled = createButton(signal,async()=>false,()=>assert.fail('unexpected toast'))({id:'t',act:async()=>{requested++;}});
  await cancelled.remove();
  assert.equal(requested,0,'cancelling confirmation must not delete');
  assert.equal(cancelled.deleting(),false);
  let confirmCount = 0, approve, finish;
  const calls = [];
  const active = createButton(signal,()=>{confirmCount++;return new Promise(resolve=>{approve=resolve;});},()=>assert.fail('unexpected toast'))({
    id:'task',act:(...args)=>{calls.push(args);return new Promise(resolve=>{finish=resolve;});},
  });
  const pending = active.remove();
  assert.equal(active.deleting(),true);
  await active.remove();
  assert.equal(confirmCount,1,'double clicks must not open multiple dialogs');
  approve(true);
  await new Promise(setImmediate);
  assert.deepEqual(calls,[['task_delete','task']]);
  assert.equal(active.deleting(),true,'disable the button until deletion finishes');
  finish(true);
  await pending;
  assert.equal(active.deleting(),false);
  let selectedId = 'confirmed-task', decide;
  const refreshing = createButton(signal,()=>new Promise(resolve=>{decide=resolve;}),error=>assert.fail(error))({
    get id(){return selectedId;},act:async(...args)=>{calls.push(args);},
  });
  const deleting = refreshing.remove();
  selectedId = 'another-task';
  decide(true);
  await deleting;
  assert.deepEqual(calls.at(-1),['task_delete','confirmed-task'],'refreshing the selected task during confirmation must not change the authorized deletion target');
  const errors = [];
  const failed = createButton(signal,async()=>{throw new Error('dialog unavailable');},error=>errors.push(error))({id:'t',act:async()=>assert.fail('no confirmation')});
  await failed.remove();
  assert.equal(failed.deleting(),false);
  assert.match(errors[0],/dialog unavailable/);
  assert.match(view,/<Show when=\{!peer\}><DeleteTaskButton id=\{x\(\)\.threadId\} act=\{act\}/,'legacy run details have deletion too');
  assert.match(view,/<Show when=\{!props\.readonly && top\(\)\}>\s*<details class="employee-more employee-task-more">[\s\S]*?<DeleteTaskButton id=\{top\(\)!\.id\}/,'finished tasks retain deletion under the more menu');
  assert.match(component,/disabled=\{deleting\(\)\}/);
  const remove = employee.match(/async fn delete_task\([\s\S]*?\n\}/)?.[0];
  assert.ok(remove);
  assert.match(remove,/DISPATCH_LOCK\.lock\(\)/);
  assert.ok(remove.indexOf('e.delete_task(id, who, now_ms())') < remove.indexOf('rt.release('));
  assert.ok(remove.indexOf('rt.release(') < remove.indexOf('crate::cancel_turn('),'in-flight tools must lose authorization before async cancellation');
  assert.match(remove,/crate::vault::drop_temp/);
  assert.doesNotMatch(remove,/remove_threads|std::fs::remove|delete_thread/,'task deletion must preserve conversations and workspace files');
  assert.match(employee,/action == "task_delete"/);
});
test('employee history filters before paging and recovery actions confirm and submit once', async()=>{
  const ts = (await import('typescript')).default;
  const [view,employee,schema] = await Promise.all(['src/components/EmployeeView.tsx','src-tauri/src/employee.rs','scripts/employee-tool.json']
    .map(path=>readFile(new URL(`../${path}`,import.meta.url),'utf8')));
  const filtered = view.match(/const filteredRows = createMemo\([^\n]+;/)?.[0];
  assert.ok(filtered);
  const rows = [...Array.from({length:35},(_,i)=>({key:`done${i}`,state:'done'})),{key:'cancelled',state:'cancelled'},{key:'failed',state:'failed'},{key:'stopped',state:'stopped'}];
  const filterJs = ts.transpileModule(filtered,{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
  const filter = new Function('createMemo','recentRows','historyFilter',`${filterJs}\nreturn filteredRows();`);
  for(const state of ['done','cancelled','failed','stopped']) {
    assert.deepEqual(filter(fn=>fn,()=>rows,()=>state),rows.filter(r=>r.state===state));
  }
  assert.deepEqual(filter(fn=>fn,()=>rows,()=>'all'),rows);
  assert.match(view,/filteredRows\(\)\.slice\(0, historyLimit\(\)\)/);
  assert.doesNotMatch(view,/\.slice\(0, SHOWN\)\.map\(\(x\) => x\.row\)/);
  assert.match(view,/任务记录/);
  assert.match(view,/cancelled: \{ label: "已取消"/);
  const component = view.match(/function TaskRecordActions\([\s\S]*?\n\}/)?.[0];
  assert.ok(component);
  const logic = `${component.slice(0,component.indexOf('  return <>'))}\n  return {restore,pending};\n}`;
  const js = ts.transpileModule(logic,{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
  const signal = value=>[()=>value,next=>{value=next;}];
  const button = new Function('createSignal','confirm','showToast',`${js}\nreturn TaskRecordActions;`);
  const calls = [];
  const cancelled = button(signal,async()=>false,()=>assert.fail('unexpected toast'))({task:{id:'t'},me:'',act:async(...args)=>calls.push(args)});
  await cancelled.restore(true);
  assert.deepEqual(calls,[]);
  assert.equal(cancelled.pending(),'');
  const resume = button(signal,()=>assert.fail('continuation is not a restart'),()=>assert.fail('unexpected toast'))({task:{id:'t'},me:'',act:async(...args)=>calls.push(args)});
  await resume.restore(false);
  assert.deepEqual(calls,[['task_resume','t']]);
  let confirmed = 0,approve,finish;
  const restart = button(signal,()=>{confirmed++;return new Promise(resolve=>{approve=resolve;});},()=>assert.fail('unexpected toast'))({task:{id:'t'},me:'',act:(...args)=>{calls.push(args);return new Promise(resolve=>{finish=resolve;});}});
  const pending = restart.restore(true);
  await restart.restore(true);
  await restart.restore(false);
  assert.equal(confirmed,1);
  approve(true);
  await new Promise(setImmediate);
  assert.deepEqual(calls,[['task_resume','t'],['task_restart','t']]);
  assert.equal(restart.pending(),'restart');
  finish(true);
  await pending;
  assert.equal(restart.pending(),'');
  const denied = button(signal,()=>assert.fail('not owner'),()=>assert.fail('unexpected toast'))({task:{id:'t',owner:{token:'alice'}},me:'bob',act:()=>assert.fail('not owner')});
  await denied.restore(false);
  await denied.restore(true);
  const errors = [];
  const failed = button(signal,async()=>{throw new Error('dialog unavailable');},error=>errors.push(error))({task:{id:'t'},me:'',act:()=>assert.fail('no confirmation')});
  await failed.restore(true);
  assert.equal(failed.pending(),'');
  assert.match(errors[0],/dialog unavailable/);
  const reopen = employee.match(/^fn reopen_task\([\s\S]*?^\}/m)?.[0];
  assert.ok(reopen);
  for(const guard of ['DISPATCH_LOCK.lock()', 'rt.tasks.iter().any', 'crate::running_by_id', 'store.get(th).is_none()']) {
    assert.ok(reopen.indexOf(guard)>=0 && reopen.indexOf(guard)<reopen.indexOf('e.reopen_task('),`${guard} must run before reopening the record`);
  }
  assert.match(employee,/action, "task_resume" \| "task_restart"/);
  assert.match(JSON.parse(schema).description,/done 取消待办\/驳回待确认并保留记录/);
});
test('employee scene retention is separate from ordinary temporary routing', async()=>{
  const [bridge,scene,turn,employee] = await Promise.all(['src-tauri/src/lyra/bridge.rs','src-tauri/src/lyra/scene.rs','src-tauri/src/lyra/turn.rs','src-tauri/src/employee.rs']
    .map(path=>readFile(new URL(`../${path}`,import.meta.url),'utf8')));
  assert.match(bridge,/routing\.retain_scene = employee/);
  assert.match(bridge,/if employee \{[\s\S]*?crate::employee::persist_scene\(&app, &id, scene, model\)/);
  assert.match(turn,/scene::finish_turn\(self\.0\)/);
  assert.match(scene,/retain_scene: false/);
  const finish = scene.match(/pub fn finish_turn\([\s\S]*?\n\}/)?.[0];
  assert.match(finish,/routing\.stack\.clear\(\)/);
  assert.match(finish,/while restore\(session\)/);
  const persist = employee.match(/pub\(crate\) fn persist_scene\([\s\S]*?\n\}/)?.[0];
  assert.match(persist,/store\.save_thread\(id\)/);
  assert.doesNotMatch(persist,/set_thread_agent|fork_thread|new_thread/);
});
test('employee refreshes coalesce in flight without dropping the last update', async()=>{
  const ts = (await import('typescript')).default;
  const view = await readFile(new URL('../src/components/EmployeeView.tsx',import.meta.url),'utf8');
  const start = view.indexOf('  let fetching =');
  assert.ok(start>=0);
  const code = view.slice(start,view.indexOf('  void refetch();',start));
  const js = ts.transpileModule(code,{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
  const pending = [], snapshots = [], errors = [];
  let failure = '';
  const store = {};
  const {refresh,dispose} = new Function('call','setStore','reconcile','setNow','setFailed','peer','showToast','failed',
    `${js}\nreturn {refresh:refetch,dispose:()=>{disposed=true;}};`)(
      ()=>new Promise((resolve,reject)=>pending.push({resolve,reject})),
      update=>{update(store);snapshots.push(structuredClone(store.snap));},value=>target=>{Object.assign(target,value);},()=>{},v=>{failure=v;},false,v=>errors.push(v),()=>failure);
  const first = refresh();
  await Promise.all(Array.from({length:5},()=>refresh()));
  assert.equal(pending.length,1,'polls and change events share the current request');
  pending[0].resolve({version:1});
  await first;
  assert.equal(pending.length,2,'events during a request get exactly one trailing refresh');
  pending[1].resolve({version:2});
  await new Promise(setImmediate);
  assert.deepEqual(snapshots,[{version:1},{version:2}]);
  const failing = refresh();
  pending[2].reject(new Error('offline'));
  await failing;
  assert.equal(errors.length,1);
  assert.match(failure,/offline/);
  const retry = refresh();
  pending[3].resolve({version:3});
  await retry;
  assert.equal(failure,'','a failed request must not lock future refreshes');
  const leaving = refresh();
  await refresh();
  dispose();
  pending[4].resolve({version:4});
  await leaving;
  await refresh();
  assert.equal(pending.length,5,'unmount cancels the trailing refresh');
  assert.equal(snapshots.length,3,'late replies cannot mutate a disposed board');
});
test('employee desktop cooldown is enforced before dispatch and each tool claim', async()=>{
  const employee = await readFile(new URL('../src-tauri/src/employee.rs',import.meta.url),'utf8');
  const respond = employee.match(/pub\(crate\) fn respond_takeover\([\s\S]*?\n\}/)?.[0];
  assert.ok(respond.indexOf('e.pause_desktop(') < respond.indexOf('tx.send('));
  const claim = employee.match(/pub\(crate\) async fn claim_desktop\([\s\S]*?\n\}/)?.[0];
  assert.ok(claim.indexOf('take_desktop(&root)?') < claim.indexOf('Step::Go => return Ok(())'));
  const take = employee.match(/fn take_desktop\([\s\S]*?\n\}/)?.[0];
  assert.match(take,/now_ms\(\) < e\.desktop_ask_after/);
  assert.match(take,/!cooling && !locked/);
  assert.match(take,/e\.defer_desktop_request\(now_ms\(\)\)/);
  const relay = employee.slice(employee.indexOf('"relay" => {'),employee.indexOf('"progress" => {'));
  assert.match(relay,/e\.defer_desktop_request\(now_ms\(\)\)/);
  for (const name of ['next_due','triggers','sleep_for']) {
    const scheduler = employee.match(new RegExp(`fn ${name}[\\s\\S]*?\\n\\}`))?.[0];
    assert.ok(scheduler);
    assert.doesNotMatch(scheduler,/pause_desktop|defer_desktop_request/);
  }
  assert.match(employee,/if outcome == Outcome::Yielded \{ e\.pause_desktop\(now\); \}/);
});
test('employee supports unscheduled duties and matches delivered tasks without duplicate dispatch', async()=>{
  const [schema,view,employee] = await Promise.all([
    tool('employee'), ...['src/components/EmployeeView.tsx','src-tauri/src/employee.rs']
      .map(path=>readFile(new URL(`../${path}`,import.meta.url),'utf8'))
  ]);
  assert.equal(schema.inputSchema.properties.manual.type,'boolean');
  assert.match(schema.description,/不定时.*已有任务仍自愈、续做/);
  assert.match(schema.description,/当前任务内执行适用流程.*不扩大本次授权/);
  assert.match(view,/<option value="manual">不定时（不自动触发）<\/option>/);
  assert.match(view,/props\.act\("duty_manual", duty\.id\)/);
  const form = view.slice(view.indexOf('function DutyForm('),view.indexOf('function ConfirmCard('));
  const submit = form.match(/  const submit = async \(\) => \{[\s\S]*?\n  \};/)?.[0];
  assert.ok(submit);
  const ts = (await import('typescript')).default;
  const compiled = ts.transpileModule(submit,{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
  const calls = [];
  let closed = 0, mode = 'manual';
  const save = new Function('ok','text','note','d','mode','times','every','events','props',`${compiled};return submit;`)(
    ()=>true,()=> ' 测试流程 ',()=> ' 补充要求 ',undefined,()=>mode,()=> '09:00,14:30',()=>60,()=>['orbit.added'],
    {run:async(command,args)=>{calls.push({command,args});return true;},onClose:()=>closed++});
  for(const [selected,rule] of [['manual',{manual:true}],['times',{times:['09:00','14:30']}],
    ['every',{everyMinutes:60}],['events',{events:['orbit.added']}]]) {
    mode = selected;
    await save();
    assert.equal(calls.at(-1).command,'employee_do');
    assert.equal(calls.at(-1).args.action,'duty_add');
    assert.deepEqual(JSON.parse(calls.at(-1).args.text),{text:'测试流程',note:'补充要求',...rule});
  }
  assert.equal(closed,4);
  const launch = employee.match(/fn launch\([\s\S]*?\n\}/)?.[0];
  assert.match(launch,/if computer \{ "" \} else \{ todo\.map_or\("", \|t\| task_duties_prompt\(&data, t\)\) \}/);
  assert.match(employee,/for key in \["text", "note", "times", "everyMinutes", "events", "manual"\]/);
});
test('employee visual hierarchy keeps recovery prominent and auxiliary details collapsed', async()=>{
  const [view,css] = await Promise.all(['src/components/EmployeeView.tsx','src/components/EmployeeView.css']
    .map(path=>readFile(new URL(`../${path}`,import.meta.url),'utf8')));
  const tree = view.slice(view.indexOf('function TaskTree('),view.indexOf('function DutyForm('));
  const toolbar = tree.slice(tree.indexOf('aria-label="任务操作"'),tree.indexOf('<details class="employee-tree-guide">'));
  assert.match(toolbar,/<TaskRecordActions task=\{top\(\)!\}/,'continuation stays above the scrollable details');
  assert.match(toolbar,/<details class="employee-more employee-task-more">[\s\S]*?<summary aria-label="更多任务操作">/);
  assert.match(toolbar,/<DeleteTaskButton/,'deletion is retained but no longer a primary action');
  assert.match(tree,/<details class="employee-tree-guide">/,'the full state legend is collapsed by default');
  assert.match(tree,/<details class="employee-progress-log">/,'old progress logs do not bury the current result');
  assert.match(view,/<label for="employee-message">交给员工一件事<\/label>/);
  assert.match(view,/employee-current-section[\s\S]*?>当前任务/);
  assert.match(view,/<EmployeeTabs \/>/);
  assert.doesNotMatch(view,/employee-workspace-head|任务工作台/,'titlebar replaces the redundant dashboard heading');
  assert.match(view,/class="employee-nav" aria-label="工作分类"/);
  assert.match(view,/section\(\) === "history"/);
  assert.match(view,/section\(\) === "duties"/);
  const navigation = view.match(/const selectSection = \(value:[\s\S]*?\n          \};/)?.[0];
  assert.ok(navigation);
  const ts = (await import('typescript')).default;
  const compiled = ts.transpileModule(navigation,{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
  let section,pick;
  const select = new Function('setSection','setPick','taskRows','duties','recentRows',`${compiled};return selectSection;`)(
    value=>{section=value;},value=>{pick=value;},()=>[{pick:{kind:'task',id:'active'}}],()=>[{id:'duty'}],()=>[{pick:{kind:'task',id:'history'}}]);
  select('history');
  assert.equal(section,'history');
  assert.deepEqual(pick,{kind:'task',id:'history'});
  select('duties');
  assert.deepEqual(pick,{kind:'duty',id:'duty'});
  select('tasks');
  assert.deepEqual(pick,{kind:'task',id:'active'});
  const empty = new Function('setSection','setPick','taskRows','duties','recentRows',`${compiled};return selectSection;`)(
    ()=>{},value=>{pick=value;},()=>[],()=>[],()=>[]);
  for(const value of ['tasks','duties','history']) { empty(value); assert.equal(pick,null); }
  assert.match(css,/\.employee-page \.employee-row\[aria-current=true\] \{[^}]*var\(--accent\)/);
  assert.match(css,/:focus-visible \{[^}]*outline: 2px solid var\(--accent\)/);
});
test('employee board and tree details use separate responsive horizontal panes', async()=>{
  const css = await readFile(new URL('../src/components/EmployeeView.css',import.meta.url),'utf8');
  assert.match(css,/\.employee-layout \{[^}]*grid-template-columns: clamp\(250px, 24%, 290px\) minmax\(0, 1fr\)/);
  assert.match(css,/@container employee-board \(max-width: 640px\)/);
  assert.match(css,/\.employee-tree \{[^}]*display: grid;[^}]*grid-template-columns: minmax\(248px, 30%\) minmax\(0, 1fr\)/);
  assert.match(css,/@container employee-detail \(max-width: 520px\)/);
  assert.match(css,/\.employee-detail \{[^}]*overflow-y: auto/);
  assert.match(css,/\.employee-detail \.employee-duty-body \{ max-height: none; overflow: visible; \}/);
  assert.doesNotMatch(css.match(/\.employee-tree-detail \{[^}]*\}/)?.[0] ?? '',/overflow:\s*auto|max-height:/,'no nested vertical scrollbar inside the task details');
  assert.doesNotMatch(css.match(/\.employee-tree-canvas \{[^}]*\}/)?.[0] ?? '',/max-height:|overflow:\s*auto/,'the full flow shares the detail scroll');
  assert.match(css,/\.employee-detail-head \{[^}]*position: sticky/);
});
test('Chrome rolls DOM history and does not gate input on snapshot IDs or age', async()=>{
  const chrome=await tool('chrome');
  const source=await readFile(new URL('../src-tauri/src/native_browser.rs',import.meta.url),'utf8');
  assert.match(chrome.description,/DOM按标签只保留最近两次观察/);
  assert.match(chrome.inputSchema.properties.snapshotId.description,/可选.*不校验ID新旧或180秒时限/);
  assert.match(chrome.inputSchema.properties.feedback.description,/evaluate传none.*不清除已有观察.*无需先inspect/);
  const start=source.indexOf('async fn control_session(');
  const end=source.indexOf('let mut result = tokio::select!',start);
  assert.ok(start>=0 && end>start);
  const control=source.slice(start,end);
  assert.doesNotMatch(control,/snapshotIdNote|refs_in\(|captured\.elapsed\(\)|__delta|observations\.lock\(\)\.unwrap\(\)\.(remove|retain)/);
  assert.doesNotMatch(source,/snapshotId 不是该标签最新观察或观察已过期|截图已过期：/);
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
