import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
function load(file, dependencies = {}, extra = {}) {
  const code = ts.transpileModule(readFileSync(new URL(file, import.meta.url), 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const exports = {};
  vm.runInNewContext(code, { exports, require: name => { assert.ok(name in dependencies, name); return dependencies[name]; }, ...extra });
  return exports;
}
const display = load('../src/threadDisplay.ts');
const { nextRunningThread } = load('../src/nextRunningThread.ts');
const thread = (id, extra = {}) => ({ id, title:id, createdAt:1, ...extra });
const threads = [thread('a'), thread('b'), thread('c')];
assert.equal(nextRunningThread(threads, null, {}), undefined);
assert.equal(nextRunningThread(threads, null, {a:true,c:true}).id,'a');
assert.equal(nextRunningThread(threads, 'a', {a:true,c:true}).id,'c');
assert.equal(nextRunningThread(threads, 'c', {a:true,c:true}).id,'a');
assert.equal(nextRunningThread(threads, 'b', {a:true,c:true}).id,'a');
assert.equal(nextRunningThread(threads, 'a', {a:true}).id,'a');
assert.equal(nextRunningThread([thread('training',{experienceThread:true})],null,{training:true}),undefined);
const chain = [thread('root'),thread('other'),thread('stage1',{parentThreadId:'root',stageSourceThreadId:'root',createdAt:2}),thread('stage2',{parentThreadId:'stage1',stageSourceThreadId:'stage1',createdAt:3})];
assert.equal(nextRunningThread(chain,null,{stage1:true,stage2:true}).id,'stage2');
assert.equal(nextRunningThread(chain,'stage1',{stage2:true,other:true}).id,'other');
assert.equal(nextRunningThread(chain,'other',{stage2:true,other:true}).id,'stage2');
assert.ok(nextRunningThread([thread('x',{parentThreadId:'y'}),thread('y',{parentThreadId:'x'})],null,{x:true,y:true}));
// Execute the real store action rather than a duplicate implementation.
const ast = ts.createSourceFile('store.ts',readFileSync(new URL('../src/store.ts',import.meta.url),'utf8'),ts.ScriptTarget.Latest,true);
const selected = ast.statements.filter(node => ts.isFunctionDeclaration(node) && ['openNextUnreadThread','chainUnreadTurns'].includes(node.name?.text));
assert.equal(selected.length,2);
const code = ts.transpile(selected.map(node => node.getText(ast).replace(/^export /,'')).join('\n'));
async function navigate(state,hidden=[]) {
  const opened=[];
  await vm.runInNewContext(`${code}\nopenNextUnreadThread()`,{state:{threads,currentId:null,running:{},unreadTurns:{},...state},virgoHiddenThreads:()=>new Set(hidden),nextRunningThread,isPendingThreadId:id=>id.startsWith('pending:'),latestFireStage:display.latestFireStage,setView:()=>{},openThread:async id=>opened.push(id)});
  return opened;
}
assert.deepEqual(await navigate({running:{a:true},unreadTurns:{b:1}}),['b']);
assert.deepEqual(await navigate({running:{a:true,c:true},currentId:'a'}),['c']);
assert.deepEqual(await navigate({running:{a:true}},['a']),['a']);
assert.deepEqual(await navigate({}),[]);
assert.deepEqual(await navigate({threads:[thread('pending:new')],running:{'pending:new':true}}),[]);
assert.deepEqual(await navigate({threads:chain,currentId:'root',running:{other:true,stage2:true},unreadTurns:{stage1:2}}),['stage1']);
let handler,stops=0,toggles=0;
class Element { constructor(terminal=false) { this.terminal=terminal; this.tagName='TEXTAREA'; } closest() { return this.terminal?this:null; } }
const state={settings:{sessionShortcuts:[]}};
const shortcuts=load('../src/sessionShortcuts.ts',{'solid-js':{onMount:cb=>cb(),onCleanup:()=>{}},'./store':{ALL_AGENT_KINDS:[],state}},{HTMLElement:Element,window:{addEventListener:(_event,cb)=>handler=cb,removeEventListener:()=>{}}});
const event=(key,patch={})=>({key,code:'',ctrlKey:false,altKey:false,shiftKey:false,metaKey:false,target:new Element(),preventDefault(){this.defaultPrevented=true;},stopPropagation(){},...patch});
const defaults=shortcuts.withDefaultSessionShortcuts([]);
assert.equal(shortcuts.findSessionShortcut(event('`',{ctrlKey:true,code:'Backquote'}),defaults).action,'toggleTerminal');
assert.equal(shortcuts.findSessionShortcut(event('~',{ctrlKey:true,shiftKey:true,code:'Backquote'}),defaults).action,'toggleTerminal');
assert.equal(shortcuts.findSessionShortcut(event('`'),defaults),null);
assert.equal(shortcuts.findSessionShortcut(event('`',{ctrlKey:true}),shortcuts.withDefaultSessionShortcuts([{id:'custom',keys:'Alt+T',action:'toggleTerminal',target:''}])),null);
assert.equal(shortcuts.findSessionShortcut(event('`',{ctrlKey:true}),shortcuts.withDefaultSessionShortcuts([{id:'custom',keys:'Ctrl+`',action:'newSession',target:''}])).action,'newSession');
shortcuts.mountSessionShortcuts({allowedActions:['stopSession','toggleTerminal'],onStopSession:()=>{stops++;return true;},onToggleTerminal:()=>toggles++});
handler(event('Escape',{target:new Element(true)}));assert.equal(stops,0);
handler(event('Escape'));assert.equal(stops,1);
handler(event('~',{ctrlKey:true,shiftKey:true,code:'Backquote',target:new Element(true)}));assert.equal(toggles,1);
shortcuts.setShortcutCaptureActive(true);handler(event('`',{ctrlKey:true}));assert.equal(toggles,1);
console.log('Terminal and unread/running shortcut regression checks passed');
