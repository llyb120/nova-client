// Only Nova's authenticated loopback receiver supplies commands. Never infer a target from the active tab.
let model, loading;
const loops = new Set(), connections = new Set();
// ponytail: serialize individual browser commands across instances; use per-tab queues if contention grows.
let commands = Promise.resolve();
const attached = new Set();
const frameTargets = new Map();
const ready = () => loading ??= chrome.storage.session.get('model').then(({model: saved}) => {
  model = saved ?? { clientId: crypto.randomUUID(), next: 1, tabs: {} };
  return model;
});
const save = () => chrome.storage.session.set({model});
const permittedUrl = url => /^https?:\/\//i.test(url || '') || url === 'about:blank';
async function inventory() {
  await ready();
  const tabs = await chrome.tabs.query({});
  const ids = new Set(tabs.map(t=>String(t.id)));
  for (const id of Object.keys(model.tabs)) if (!ids.has(id)) delete model.tabs[id];
  for (const tab of tabs) if (!model.tabs[tab.id]) {
    model.tabs[tab.id] = {tag:`C${model.next++}-${model.clientId.replaceAll('-','').slice(0,16)}`};
  }
  await save();
  // `loading` lets Nova wait for a navigation to settle instead of costing the model another inspect.
  return tabs.map(tab=>({tag:model.tabs[tab.id].tag,title:tab.title || '',url:tab.url || tab.pendingUrl || '',active:tab.active,incognito:!!tab.incognito,windowId:tab.windowId,allowed:true,controllable:permittedUrl(tab.url || tab.pendingUrl),openerTag:model.tabs[tab.openerTabId]?.tag,status:tab.status || 'unknown',loading:tab.status!=='complete' || Boolean(tab.pendingUrl)}));
}
async function resolveTag(tag) {
  await ready();
  if (typeof tag !== 'string' || !tag) throw Error('缺少明确的 tabTag，请先 tabs 获取标签；不会使用当前激活页');
  const entry = Object.entries(model.tabs).find(([,value])=>value.tag===tag);
  if (!entry) throw Error('tabTag不存在或已失效，请重新 tabs');
  const tab = await chrome.tabs.get(Number(entry[0]));
  return tab;
}
const checkUrl = value => {
  const url = new URL(value);
  if (!['http:','https:'].includes(url.protocol) || url.username || url.password) throw Error('只允许不含凭据的 HTTP(S) 地址');
  return url.href;
};
async function detach(tabId) {
  frameTargets.delete(tabId);
  if (attached.delete(tabId)) await chrome.debugger.detach({tabId}).catch(()=>{});
}
async function debuggerCall(command, invoke) {
  const remaining = Math.min(3000, command.expiresAt - Date.now());
  if (!(remaining > 0)) throw Error('命令已过期，未执行');
  let timer;
  try {
    return await Promise.race([
      invoke(),
      new Promise((_, reject) => { timer = setTimeout(() => reject(Error('Chrome 调试命令响应超时；结果可能已执行，请先观察，不要重放动作')), remaining); }),
    ]);
  } finally { clearTimeout(timer); }
}
async function execute(command) {
  const {operation, args} = command;
  if (Date.now() > command.expiresAt) throw Error('命令已过期，未执行');
  if (operation === 'downloads') {
    if (!chrome.downloads) throw Error('下载查询需要 Nova Chrome 0.1.6，请更新并重新加载扩展');
    const query = {orderBy:['-startTime'],limit:100};
    if (args.downloadId !== undefined) {
      if (typeof args.downloadId !== 'string' || !/^\d+$/.test(args.downloadId) || !Number.isSafeInteger(Number(args.downloadId))) throw Error('downloadId 必须为下载查询返回的 ID');
      query.id = Number(args.downloadId);
    } else {
      const since = args.since ?? Date.now()-600000;
      if (!Number.isSafeInteger(since) || since < 0 || since > 8640000000000000) throw Error('since 必须为有效的 Unix 毫秒时间戳');
      query.startedAfter = new Date(since).toISOString();
    }
    if (args.incognito !== undefined && typeof args.incognito !== 'boolean') throw Error('incognito必须为boolean');
    const items = await chrome.downloads.search(query);
    return {scope:'browser',queriedAt:Date.now(),limit:100,downloads:items.filter(d=>args.incognito === undefined || d.incognito === args.incognito).map(d=>({
      id:String(d.id),url:d.url?.slice(0,4096),finalUrl:d.finalUrl?.slice(0,4096),urlTruncated:(d.url?.length || 0)>4096 || (d.finalUrl?.length || 0)>4096,path:d.filename,state:d.state,
      bytesReceived:d.bytesReceived,totalBytes:d.totalBytes,startTime:d.startTime,endTime:d.endTime,
      error:d.error || null,paused:d.paused,canResume:d.canResume,exists:d.exists,danger:d.danger,mime:d.mime,incognito:d.incognito,
    })),notice:'Chrome 下载记录为浏览器范围，不能按 tabTag 归属；按时间、URL、文件名确认目标。空列表不代表导出失败，网页可能尚在生成文件。只有 state=complete 才表示下载完成。'};
  }
  if (operation === 'status') return {incognitoAllowed:await chrome.extension.isAllowedIncognitoAccess()};
  if (operation === 'tabs') return {tabs:(await inventory()).filter(tab=>args.incognito === undefined || tab.incognito === args.incognito),incognitoAllowed:await chrome.extension.isAllowedIncognitoAccess()};
  if (operation === 'open' || operation === 'new_tab') {
    if (args.incognito !== undefined && typeof args.incognito !== 'boolean') throw Error('incognito必须为boolean');
    const url = args.url ? checkUrl(args.url) : 'about:blank';
    let tab;
    if (args.incognito === true) {
      if (!await chrome.extension.isAllowedIncognitoAccess()) throw Error('请在 chrome://extensions 中打开 Nova Chrome 详情，开启“允许在无痕模式下运行”，然后重试；未创建普通窗口');
      const created = await chrome.windows.create({url,incognito:true});
      [tab] = created.tabs || await chrome.tabs.query({windowId:created.id});
    } else {
      // Bind a normal window explicitly: the last focused window may be incognito.
      const normal = (await chrome.windows.getAll({windowTypes:['normal']})).find(w=>!w.incognito);
      if (normal) tab = await chrome.tabs.create({url,windowId:normal.id});
      else { const created = await chrome.windows.create({url,incognito:false}); [tab] = created.tabs || await chrome.tabs.query({windowId:created.id}); }
    }
    if (!tab) throw Error('窗口已创建但未取得标签，请tabs观察，不要重复创建');
    await inventory();
    return {tabTag:model.tabs[tab.id].tag,tabs:await inventory()};
  }
  const tab = await resolveTag(args.tabTag);
  switch (operation) {
    case 'select_tab': await chrome.tabs.update(tab.id,{active:true}); await chrome.windows.update(tab.windowId,{focused:true}); break;
    case 'close_tab': await detach(tab.id); await chrome.tabs.remove(tab.id); delete model.tabs[tab.id]; await save(); break;
    case 'goto': await chrome.tabs.update(tab.id,{url:checkUrl(args.url)}); break;
    case 'back': await chrome.tabs.goBack(tab.id); break;
    case 'forward': await chrome.tabs.goForward(tab.id); break;
    case 'reload': await chrome.tabs.reload(tab.id); break;
    case 'stop': await detach(tab.id); break;
    case 'cdp': {
      if (!attached.has(tab.id)) {
        await debuggerCall(command,()=>chrome.debugger.attach({tabId:tab.id},'1.3')); attached.add(tab.id);
      }
      // Recheck the tab after the asynchronous attach.
      await resolveTag(args.tabTag);
      if (Date.now() > command.expiresAt) throw Error('命令已过期，未执行');
      // Extension debugger sessions are auto-attach-only: direct target discovery/attach is forbidden.
      if(args.method==='Target.getTargets') {
        const params={autoAttach:true,waitForDebuggerOnStart:false,flatten:true};
        await debuggerCall(command,()=>chrome.debugger.sendCommand({tabId:tab.id},'Target.setAutoAttach',params));
        const visited=new Set();
        for(;;){
          const child=[...(frameTargets.get(tab.id)?.values() || [])].find(value=>!visited.has(value.sessionId));
          if(!child || visited.size>=12)break;
          visited.add(child.sessionId);
          await debuggerCall(command,()=>chrome.debugger.sendCommand({tabId:tab.id,sessionId:child.sessionId},'Target.setAutoAttach',params));
        }
        return {targetInfos:[...(frameTargets.get(tab.id)?.values() || [])].map(value=>value.targetInfo)};
      }
      if(args.method==='Target.attachToTarget') {
        const child=frameTargets.get(tab.id)?.get(args.params?.targetId);
        if(!child)throw Error('子框架已变化，请重新观察');
        return {sessionId:child.sessionId};
      }
      try {return await debuggerCall(command,()=>chrome.debugger.sendCommand({tabId:tab.id,...(args.sessionId ? {sessionId:args.sessionId} : {})},args.method,args.params || {}));} catch(error){throw Error(`${args.method}: ${error?.message || error}`);}
    }
    default: throw Error('未知 Chrome 操作');
  }
  return {tabTag:args.tabTag,tabs:await inventory()};
}
function loop() {
  for (let port=47653; port<=47662; port++) void connect(`http://127.0.0.1:${port}`);
}
async function connect(origin) {
  if (loops.has(origin)) return;
  loops.add(origin);
  try {
    await ready(); await inventory();
    const paired=await fetch(`${origin}/pair`,{method:'POST',signal:AbortSignal.timeout(3000)});
    if(!paired.ok)throw Error(`Nova 连接失败 HTTP ${paired.status}`);
    const {token}=await paired.json();
    if(typeof token!=='string' || token.length<32)throw Error('无效的本机连接令牌');
    const config={origin,token};
    const post = (path,body) => fetch(`${config.origin}${path}`,{method:'POST',headers:{'Content-Type':'application/json',Authorization:`Bearer ${config.token}`},body:JSON.stringify({...body,clientId:model.clientId}),signal:AbortSignal.timeout(25000)});
    for (;;) {
      const response = await post('/poll',{});
      if (!response.ok) throw Error(`连接被拒绝 HTTP ${response.status}；检查是否连接了另一个 Chrome 实例`);
      connections.add(origin);
      const {command} = await response.json();
      if (!command) continue;
      let reply;
      const result = commands.then(()=>execute(command));
      commands = result.catch(()=>{});
      try { reply={id:command.id,result:await result}; }
      catch(error){reply={id:command.id,error:String(error?.message || error)};}
      // Never repeat a command if delivery of its result fails.
      const delivered = await post('/reply',reply);
      if(!delivered.ok)throw Error(`结果回传失败 HTTP ${delivered.status}；不重放操作`);
    }
  } catch {
    // Retry discovery on the next alarm; never replay a command after a failed reply.
  } finally { connections.delete(origin); loops.delete(origin); }
}
chrome.debugger.onEvent.addListener((source,method,params)=>{
  if(method==='Target.attachedToTarget' && params.targetInfo?.type==='iframe') {
    if(!frameTargets.has(source.tabId))frameTargets.set(source.tabId,new Map());
    frameTargets.get(source.tabId).set(params.targetInfo.targetId,{targetInfo:params.targetInfo,sessionId:params.sessionId});
  }else if(method==='Target.detachedFromTarget'){
    const frames=frameTargets.get(source.tabId);
    for(const [id,frame] of frames || [])if(frame.sessionId===params.sessionId)frames.delete(id);
  }
});
chrome.debugger.onDetach.addListener(source=>{attached.delete(source.tabId);frameTargets.delete(source.tabId);});
chrome.tabs.onRemoved.addListener(id=>{attached.delete(id);frameTargets.delete(id);void ready().then(async()=>{delete model.tabs[id];await save();});});
chrome.tabs.onReplaced.addListener((added,removed)=>{void ready().then(async()=>{
  if(model.tabs[removed]){model.tabs[added]=model.tabs[removed];delete model.tabs[removed];await save();}attached.delete(removed);frameTargets.delete(removed);
});});
chrome.runtime.onMessage.addListener((message,sender,reply)=>{
  if(sender.id!==chrome.runtime.id)return;
  const task=async()=>{
    await ready();
    if(message.type!=='status' && message.type!=='reconnect')throw Error('未知请求');
    void loop();
    const tabs=await inventory();
    const [active]=await chrome.tabs.query({active:true,lastFocusedWindow:true});
    return {tab:active ? tabs.find(tab=>tab.tag===model.tabs[active.id]?.tag) : null,connection:{connected:connections.size>0,count:connections.size}};
  };
  task().then(reply,error=>reply({error:String(error?.message || error)}));return true;
});
chrome.alarms.onAlarm.addListener(alarm=>{if(alarm.name==='nova-reconnect')void loop();});
chrome.runtime.onInstalled.addListener(()=>{void chrome.alarms.create('nova-reconnect',{periodInMinutes:.5});void loop();});
chrome.runtime.onStartup.addListener(()=>void loop());
void loop();
