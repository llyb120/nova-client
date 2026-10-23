// Only Nova's authenticated loopback receiver supplies commands. Never infer a target from the active tab.
let model, loading;
const loops = new Set(), connections = new Set();
// Paired instances (origin -> authenticated post), available before the first long poll returns.
const instances = new Map();
// ponytail: serialize individual browser commands across instances; use per-tab queues if contention grows.
let commands = Promise.resolve();
const attached = new Set();
const frameTargets = new Map();
// DevTools data per tab, recorded only after the first `devtools` call enables the domains.
// ponytail: top-frame session only, ring buffer of DEVTOOLS_LIMIT per kind; OOPIF iframes need per-session enable.
const devtools = new Map();
const DEVTOOLS_LIMIT = 500;
const clip = (text, max = 2000) => { text = String(text ?? ''); return text.length > max ? text.slice(0, max) + `…(+${text.length - max})` : text; };
const preview = p => p?.properties && (p.subtype === 'array' ? `[${p.properties.map(x => x.value).join(', ')}${p.overflow ? ', …' : ''}]` : `{${p.properties.map(x => `${x.name}: ${x.value}`).join(', ')}${p.overflow ? ', …' : ''}}`);
const remoteText = arg => arg.type === 'string' ? arg.value : arg.unserializableValue ?? preview(arg.preview) ?? arg.description ?? (arg.value === undefined ? arg.type : JSON.stringify(arg.value));
const frameAt = trace => { const f = trace?.callFrames?.[0]; return f ? {url:clip(f.url,2048),line:f.lineNumber + 1} : {}; };
function recordDevtools(tabId, method, params) {
  const log = devtools.get(tabId);
  if (!log) return;
  const push = entry => { log.console.push(entry); if (log.console.length > DEVTOOLS_LIMIT) log.console.shift(); };
  if (method === 'Runtime.consoleAPICalled') push({source:'console',level:params.type,text:clip((params.args || []).map(remoteText).join(' ')),time:params.timestamp,...frameAt(params.stackTrace)});
  else if (method === 'Runtime.exceptionThrown') { const d = params.exceptionDetails || {}; push({source:'exception',level:'error',text:clip(d.exception?.description || d.text),time:params.timestamp,url:clip(d.url,2048),line:d.lineNumber + 1}); }
  else if (method === 'Log.entryAdded') { const e = params.entry; push({source:e.source,level:e.level,text:clip(e.text),time:e.timestamp,url:clip(e.url,2048),line:e.lineNumber === undefined ? undefined : e.lineNumber + 1}); }
  else if (method === 'Network.requestWillBeSent') {
    log.network.delete(params.requestId); // Redirects reuse the id: keep only the final hop.
    log.network.set(params.requestId, {requestId:params.requestId,method:params.request.method,url:clip(params.request.url,2048),type:params.type,startedAt:Math.round(params.wallTime * 1000),start:params.timestamp,...(params.request.postData ? {postData:clip(params.request.postData)} : {})});
    if (log.network.size > DEVTOOLS_LIMIT) log.network.delete(log.network.keys().next().value);
  } else {
    const request = log.network.get(params.requestId);
    if (!request) return;
    if (method === 'Network.responseReceived') Object.assign(request, {status:params.response.status,statusText:params.response.statusText,mimeType:params.response.mimeType,type:params.type});
    else if (method === 'Network.loadingFinished') Object.assign(request, {finished:true,encodedBytes:params.encodedDataLength,durationMs:Math.round((params.timestamp - request.start) * 1000)});
    else if (method === 'Network.loadingFailed') Object.assign(request, {failed:true,errorText:params.errorText,canceled:params.canceled,blockedReason:params.blockedReason,durationMs:Math.round((params.timestamp - request.start) * 1000)});
  }
}
async function devtoolsData(command, tab) {
  const options = command.args.devtools ?? {};
  if (typeof options !== 'object' || Array.isArray(options)) throw Error('devtools 必须为对象');
  const kind = options.kind ?? 'all';
  if (!['all','console','network'].includes(kind)) throw Error('devtools.kind 必须为 all/console/network');
  const limit = options.limit ?? 100;
  if (!Number.isInteger(limit) || limit < 1 || limit > DEVTOOLS_LIMIT) throw Error(`devtools.limit 必须为 1–${DEVTOOLS_LIMIT} 的整数`);
  for (const key of ['query','level','requestId']) if (options[key] !== undefined && typeof options[key] !== 'string') throw Error(`devtools.${key} 必须为字符串`);
  await attach(command, tab);
  let log = devtools.get(tab.id);
  if (!log) {
    // Runtime.enable replays console history V8 already holds; network has no history before enable.
    devtools.set(tab.id, log = {since:Date.now(),console:[],network:new Map()});
    try { for (const method of ['Runtime.enable','Log.enable','Network.enable']) await debuggerCall(command,()=>chrome.debugger.sendCommand({tabId:tab.id},method,{})); }
    catch (error) { devtools.delete(tab.id); throw error; }
  }
  if (options.requestId) {
    if (!log.network.has(options.requestId)) throw Error('requestId 不在已记录的网络请求中，请重新 devtools 查询');
    const {body, base64Encoded} = await debuggerCall(command,()=>chrome.debugger.sendCommand({tabId:tab.id},'Network.getResponseBody',{requestId:options.requestId}));
    return {tabTag:command.args.tabTag,requestId:options.requestId,base64Encoded,bodyLength:body.length,body:clip(body,65536)};
  }
  const query = options.query?.toLowerCase();
  const keep = entry => (!query || [entry.text, entry.url].some(value => value?.toLowerCase().includes(query)))
    && (!options.level || entry.level === options.level || (options.level === 'error' && entry.failed) || (options.level === 'error' && entry.status >= 400));
  const pick = list => { const matched = list.filter(keep); return {total:matched.length,items:matched.slice(-limit).map(({start, ...entry}) => entry)}; };
  const result = {tabTag:command.args.tabTag,recordingSince:log.since,notice:'console 含启用前 V8 已缓存的日志；网络只记录 recordingSince 之后的请求，需完整记录可先 devtools 再 reload。每类最多保留最新 500 条，不含跨进程 iframe。'};
  if (kind !== 'network') result.console = pick(log.console);
  if (kind !== 'console') result.network = pick([...log.network.values()]);
  if (options.clear === true) { log.console = []; log.network.clear(); }
  return result;
}
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
// Visible marker: tabs under debugger control sit in a blue "Nova" tab group; idle tabs detach so Chrome's debugging bar goes away.
// ponytail: tabs with an active devtools recording never idle-detach (detaching would drop the log); idle timers die with the worker.
const IDLE_DETACH_MS = 30000;
const grouped = new Set(), idle = new Map();
async function mark(tab) {
  if (tab.groupId !== -1 || grouped.has(tab.id)) return;
  try {
    const [group] = await chrome.tabGroups.query({windowId:tab.windowId,title:'Nova'});
    const groupId = await chrome.tabs.group(group ? {tabIds:tab.id,groupId:group.id} : {tabIds:tab.id,createProperties:{windowId:tab.windowId}});
    if (!group) await chrome.tabGroups.update(groupId,{title:'Nova',color:'blue'});
    grouped.add(tab.id);
  } catch { /* Popup windows cannot be grouped; control still works. */ }
}
function forget(tabId) {
  attached.delete(tabId); frameTargets.delete(tabId); devtools.delete(tabId); clearTimeout(idle.get(tabId)); idle.delete(tabId);
  if (grouped.delete(tabId)) void chrome.tabs.ungroup(tabId).catch(()=>{});
}
async function detach(tabId) {
  const was = attached.has(tabId);
  forget(tabId);
  if (was) await chrome.debugger.detach({tabId}).catch(()=>{});
}
async function debuggerCall(command, invoke) {
  const remaining = Math.min(10000, command.expiresAt - Date.now());
  if (!(remaining > 0)) throw Error('命令已过期，未执行');
  let timer;
  try {
    return await Promise.race([
      invoke(),
      new Promise((_, reject) => { timer = setTimeout(() => reject(Error('Chrome 调试命令响应超时；结果可能已执行，请先观察，不要重放动作')), remaining); }),
    ]);
  } finally { clearTimeout(timer); }
}
async function attach(command, tab) {
  clearTimeout(idle.get(tab.id));
  idle.set(tab.id,setTimeout(()=>{ if(!devtools.has(tab.id)) void detach(tab.id); },IDLE_DETACH_MS));
  if (attached.has(tab.id)) return;
  await debuggerCall(command,()=>chrome.debugger.attach({tabId:tab.id},'1.3')); attached.add(tab.id);
  await mark(tab);
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
  if (operation === 'status') return {incognitoAllowed:await chrome.extension.isAllowedIncognitoAccess(),version:chrome.runtime.getManifest?.().version};
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
    case 'devtools': return devtoolsData(command, tab);
    case 'cdp': {
      await attach(command, tab);
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
      if(args.method==='Page.captureScreenshot') {
        // A minimized window or background tab never paints: the capture would hang until timeout.
        if((await chrome.windows.get(tab.windowId)).state==='minimized')await chrome.windows.update(tab.windowId,{state:'normal'});
        if(!tab.active)await chrome.tabs.update(tab.id,{active:true});
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
const escapeRegExp = text => text.replace(/[.+?^${}()|[\]\\]/g, '\\$&');
// `*` is the only wildcard; the whole URL (including query) must match.
const globMatch = (pattern, url) => new RegExp('^' + pattern.split('*').map(escapeRegExp).join('.*') + '$').test(url);
async function miniappMatches(url) {
  if (!/^https?:\/\//i.test(url || '')) return [];
  if (!instances.size) { void loop(); for (let i = 0; i < 15 && !instances.size; i++) await new Promise(resolve => setTimeout(resolve, 200)); }
  const found = [];
  await Promise.all([...instances].map(async ([origin, post]) => {
    try {
      const response = await post('/miniapp/triggers', {});
      if (!response.ok) return;
      const {triggers} = await response.json();
      for (const trigger of Array.isArray(triggers) ? triggers : []) {
        if (typeof trigger?.appId === 'string' && Array.isArray(trigger.match) && trigger.match.some(pattern => typeof pattern === 'string' && globMatch(pattern, url))) found.push({origin, appId: trigger.appId, name: String(trigger.name || '')});
      }
    } catch { /* An offline instance simply contributes no launcher. */ }
  }));
  return found;
}
async function launchMiniapp(message, tab) {
  const post = instances.get(message.origin);
  if (!post || !tab) throw Error('Nova 未连接');
  await inventory();
  const tabTag = model.tabs[tab.id]?.tag;
  if (!tabTag) throw Error('启动应用的标签页已关闭，请重新打开页面');
  const response = await post('/miniapp/launch', {appId: message.appId, url: tab.url, title: tab.title || '', tabTag});
  if (!response.ok) throw Error((await response.json().catch(() => ({}))).error || `HTTP ${response.status}`);
  return {ok: true};
}
function loop() {
  for (let port=47653; port<=47662; port++) void connect(`http://127.0.0.1:${port}`);
}
async function connect(origin, attempt = 0) {
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
    instances.set(origin, post);
    for (;;) {
      const response = await post('/poll',{});
      if (!response.ok) throw Object.assign(Error(`连接被拒绝 HTTP ${response.status}；检查是否连接了另一个 Chrome 实例`),{conflict:response.status===409});
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
  } catch (error) {
    // Reloading the extension clears session storage, so Nova sees a new clientId and keeps
    // the old one for 45s: retry for ~60s instead of waiting for alarms. Never replay a command.
    if (error?.conflict && attempt < 12) setTimeout(() => void connect(origin, attempt + 1), 5000);
  } finally { connections.delete(origin); instances.delete(origin); loops.delete(origin); }
}
chrome.debugger.onEvent.addListener((source,method,params)=>{
  if(!source.sessionId)recordDevtools(source.tabId,method,params);
  if(method==='Target.attachedToTarget' && params.targetInfo?.type==='iframe') {
    if(!frameTargets.has(source.tabId))frameTargets.set(source.tabId,new Map());
    frameTargets.get(source.tabId).set(params.targetInfo.targetId,{targetInfo:params.targetInfo,sessionId:params.sessionId});
  }else if(method==='Target.detachedFromTarget'){
    const frames=frameTargets.get(source.tabId);
    for(const [id,frame] of frames || [])if(frame.sessionId===params.sessionId)frames.delete(id);
  }
});
chrome.debugger.onDetach.addListener(source=>forget(source.tabId));
chrome.tabs.onRemoved.addListener(id=>{grouped.delete(id);forget(id);void ready().then(async()=>{delete model.tabs[id];await save();});});
chrome.tabs.onReplaced.addListener((added,removed)=>{void ready().then(async()=>{
  if(model.tabs[removed]){model.tabs[added]=model.tabs[removed];delete model.tabs[removed];await save();}grouped.delete(removed);forget(removed);
});});
chrome.runtime.onMessage.addListener((message,sender,reply)=>{
  if(sender.id!==chrome.runtime.id)return;
  const task=async()=>{
    await ready();
    if(message.type==='miniapp-match')return {apps:await miniappMatches(sender.tab?.url)};
    if(message.type==='miniapp-launch')return launchMiniapp(message,sender.tab);
    if(message.type!=='status' && message.type!=='reconnect')throw Error('未知请求');
    void loop();
    const tabs=await inventory();
    const [active]=await chrome.tabs.query({active:true,lastFocusedWindow:true});
    return {tab:active ? tabs.find(tab=>tab.tag===model.tabs[active.id]?.tag) : null,connection:{connected:connections.size>0,count:connections.size}};
  };
  task().then(reply,error=>reply({error:String(error?.message || error)}));return true;
});
// Single-page apps change URLs without reloading the content script.
chrome.tabs.onUpdated.addListener((id,change)=>{if(change.url)void chrome.tabs.sendMessage(id,{type:'miniapp-refresh'}).catch(()=>{});});
chrome.alarms.onAlarm.addListener(alarm=>{if(alarm.name==='nova-reconnect')void loop();});
chrome.runtime.onInstalled.addListener(()=>{void chrome.alarms.create('nova-reconnect',{periodInMinutes:.5});void loop();});
chrome.runtime.onStartup.addListener(()=>void loop());
void loop();
