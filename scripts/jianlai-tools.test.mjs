import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {AjvJsonSchemaValidator} from '@modelcontextprotocol/sdk/validation/ajv';
const tools=JSON.parse(await readFile(new URL('./jianlai-tools.json',import.meta.url),'utf8'));
const old=JSON.parse(await readFile(new URL('./jianlai-tool.json',import.meta.url),'utf8')).inputSchema.properties;
const byName=Object.fromEntries(tools.map(t=>[t.name,t]));
const validator=new AjvJsonSchemaValidator();
const valid=(name,args)=>validator.getValidator(byName[name].parameters)(args).valid;

test('nine narrow desktop tools cover the executor without exposing the old run alias',()=>{
  const cases={
    jianlai_windows:{query:'Editor',observe:true,background:false,maxEdge:0},
    jianlai_screenshot:{capture:{kind:'desktop'},background:true},
    jianlai_activate:{windowId:42},
    jianlai_act:{snapshotId:'s',imageId:'i',background:false,actions:[{action:'click',x:0,y:0,glide:false},{action:'type',text:''},{action:'press',key:'Ctrl+C'}]},
    jianlai_elements:{snapshotId:'s',imageId:'i',x:10,y:20,depth:3,maxItems:20,filter:'save'},
    jianlai_recall:{imagePath:'old.webp'},
    jianlai_experience_search:{scope:'Editor',task:'Open'},
    jianlai_experience_save:{recordingId:'demo',scope:'Editor',task:'Open',conditions:['Home'],steps:['Open'],checks:['Visible'],evidence:'Visible',redacted:true},
    jianlai_experience_feedback:{snapshotId:'s',routeId:'r',scope:'Editor',task:'Open',outcome:'transient',reason:'Loading',evidence:'Spinner',redacted:true},
  };
  assert.deepEqual(Object.keys(byName).sort(),Object.keys(cases).sort());
  for(const [name,args] of Object.entries(cases)){
    assert.equal(valid(name,args),true,name);
    assert.equal(valid(name,{...args,operation:'act'}),false,name+' rejects unrelated operation');
  }
  assert.equal(valid('jianlai_act',{actions:[{action:'wait',ms:0}],feedback:'none'}),true,'pure wait keeps its snapshot-free path');
  assert.equal(valid('jianlai_act',{actions:[{action:'press',key:'Enter'}]}),true,'omitted snapshot is bound by the executor, not filled by the model');
});

test('waits and input batches accept omitted snapshot IDs while keeping action constraints',()=>{
  const inputs=[{action:'click',x:0,y:0},{action:'double_click',x:0,y:0},{action:'move',x:0,y:0},
    {action:'drag',x:0,y:0,toX:1,toY:1},{action:'type',text:''},{action:'press',key:'Enter'},{action:'scroll',x:0,y:0,delta:0}];
  for(const input of inputs)for(const actions of [[input],[{action:'wait',ms:0},input]]) {
    for(const snapshotId of [undefined,''])assert.equal(valid('jianlai_act',{actions,...(snapshotId===undefined?{}:{snapshotId})}),true,JSON.stringify({actions,snapshotId}));
    assert.equal(valid('jianlai_act',{actions,snapshotId:'s'}),true,JSON.stringify(actions));
  }
  assert.equal(valid('jianlai_act',{actions:[{action:'wait'},{action:'wait',ms:0}]}),true);
  for(const args of [{actions:[]},{actions:[{action:'wait',ms:2001}],snapshotId:'s'},
    {actions:[{action:'wait'}],feedback:'invalid'},{actions:[{action:'wait'}],unexpected:true},
    {actions:Array(17).fill({action:'wait'})}])assert.equal(valid('jianlai_act',args),false,JSON.stringify(args));
});

test('screenshot targets keep image/source coordinates and background explicit',()=>{
  const region={x:0,y:1,width:2,height:3};
  for(const capture of [{kind:'desktop'},{kind:'window',windowId:42},{kind:'monitor',monitorId:0},
    {kind:'image_region',snapshotId:'s',imageId:'i',region},{kind:'window_region',windowId:42,region},{kind:'monitor_region',monitorId:0,region}])
    assert.equal(valid('jianlai_screenshot',{capture,background:false,maxEdge:1200}),true,JSON.stringify(capture));
  for(const capture of [{kind:'monitor',monitorId:0,windowId:42},{kind:'window'},{kind:'image_region',region},{kind:'desktop',region}])
    assert.equal(valid('jianlai_screenshot',{capture}),false,JSON.stringify(capture));
  for(const maxEdge of [0,640,1200,3840])assert.equal(valid('jianlai_windows',{maxEdge}),true);
  for(const maxEdge of [-1,1,639,3841])assert.equal(valid('jianlai_windows',{maxEdge}),false);
  assert.equal(valid('jianlai_activate',{windowId:42,background:true}),false,'activation never falls back from background');
  const captures=byName.jianlai_screenshot.parameters.properties.capture.anyOf;
  assert.match(captures.find(s=>s.properties.kind.enum[0]==='window').properties.kind.description,/普通模式.*显示器整屏.*macOS.*background=true.*窗口自身/);
  assert.match(captures.find(s=>s.properties.kind.enum[0]==='window_region').properties.kind.description,/不是窗口客户区坐标/);
  assert.equal(valid('jianlai_screenshot',{capture:{kind:'image_region',snapshotId:'',imageId:'i',region}}),false);
});

test('control observation stays read-only and bounded',()=>{
  // 观察只接定位参数：动作批次/后台模式不属于它。
  for(const args of [{depth:0},{depth:7},{maxItems:0},{maxItems:121},{x:-1},{filter:'x'.repeat(61)},
    {background:true},{actions:[{action:'click',x:0,y:0}]}])
    assert.equal(valid('jianlai_elements',args),false,JSON.stringify(args));
  for(const args of [{},{x:0,y:0},{windowId:0},{windowId:42},{snapshotId:'s',imageId:'i',depth:6,maxItems:120,filter:'保存'}])
    assert.equal(valid('jianlai_elements',args),true,JSON.stringify(args));
  assert.equal(byName.jianlai_elements.parameters.properties.x.description.includes('需与 y 同时给出'),true,'omitting both x and y must be an explicit mode');
  // 没有快照也要能读树（只读、便宜）：windowId 是唯一不依赖图片坐标的目标。
  const windowId=byName.jianlai_elements.parameters.properties.windowId;
  assert.equal(windowId.type,'integer');
  assert.equal(windowId.minimum,0,'与其它工具的 windowId 参数保持一致');
});

test('act can target screen pixels without a screenshot',()=>{
  const space=byName.jianlai_act.parameters.properties.coordinateSpace;
  assert.deepEqual(space.enum,['image','screen']);
  // 屏幕坐标不需要观察，且可为负（多显示器在左/上）。
  assert.equal(valid('jianlai_act',{coordinateSpace:'screen',actions:[{action:'click',x:-5,y:7}]}),true);
  assert.equal(valid('jianlai_act',{coordinateSpace:'screen',actions:[{action:'drag',x:-5,y:7,toX:-1,toY:9}]}),true);
  assert.equal(valid('jianlai_act',{coordinateSpace:'px',actions:[{action:'click',x:0,y:0}]}),false);
  // 坐标范围改由运行时按坐标空间校验：schema 不再把负值当参数错误。
  assert.equal(valid('jianlai_act',{actions:[{action:'click',x:-5,y:7}]}),true);
  assert.equal(valid('jianlai_act',{actions:[{action:'click',y:7}]}),false);
});

test('screenshot targets keep image/source coordinates and background explicit',()=>{
  const cases=[{action:'click',x:0,y:0,button:'right',glide:false},{action:'double_click',x:0,y:0},
    {action:'move',x:0,y:0,glide:false},{action:'drag',x:0,y:0,toX:1,toY:2,button:'middle',glide:true},
    {action:'type',text:''},{action:'press',key:'Enter'},{action:'scroll',x:0,y:0,delta:0,axis:'horizontal'},{action:'wait',ms:0}];
  for(const action of cases)assert.equal(valid('jianlai_act',{snapshotId:'s',actions:[action]}),true,JSON.stringify(action));
  for(const action of [{action:'click',x:0},{action:'click',x:0,y:0,text:''},{action:'press',key:'Enter',x:0},
    {action:'scroll',x:0,y:0,delta:1,glide:false},{action:'wait',ms:2001},{action:'move',x:0.5,y:0}])
    assert.equal(valid('jianlai_act',{snapshotId:'s',actions:[action]}),false,JSON.stringify(action));
  assert.equal(valid('jianlai_act',{actions:Array(17).fill({action:'wait'})}),false);
  assert.equal(valid('jianlai_windows',{actions:[{action:'type',text:'never'}]}),false);
  const type=byName.jianlai_act.parameters.properties.actions.items.anyOf.find(s=>s.properties.action.enum[0]==='type');
  assert.deepEqual(type.properties.text,old.actions.items.properties.text,'typing/readback safety guidance stays intact');
  for(const name of ['jianlai_windows','jianlai_screenshot','jianlai_act'])assert.deepEqual(byName[name].parameters.properties.background,old.background);
});

test('schemas stay within the shared runtime validator vocabulary and require observed feedback evidence',()=>{
  const keys=new Set(['type','properties','required','additionalProperties','description','enum','minimum','maximum','exclusiveMinimum','minLength','maxLength','anyOf','minItems','maxItems','items']);
  const visit=s=>{for(const key of Object.keys(s))assert(keys.has(key),key);for(const v of Object.values(s.properties||{}))visit(v);for(const v of s.anyOf||[])visit(v);if(s.items)visit(s.items);};
  for(const tool of tools)visit(tool.parameters);
  for(const name of ['jianlai_experience_save','jianlai_experience_feedback']){
    assert(byName[name].parameters.required.includes('evidence'));
    assert.deepEqual(byName[name].parameters.properties.redacted.enum,[true]);
  }
});
