const status = document.querySelector('#status');
const button = document.querySelector('#run');
const retry = document.querySelector('#retry');
const names = {attachExistingTabWithoutRestart:'接管已有标签页',reusedNovaWholeDomObserver:'整页 DOM',trustedChineseInputAndClick:'原生中文输入和点击',screenshot:'截图',popupAndTabState:'新标签及状态保留',detachAndReattach:'断开重连'};
function render(value) {
  button.disabled = value?.running === true;
  retry.hidden = !value || value.running || value.uploaded;
  retry.disabled = value?.running === true;
  if (!value) { status.textContent = '就绪：打开测试页后，点击“运行验证”。'; return; }
  if (value.running) { status.textContent = '正在验证，请保持测试页打开…'; return; }
  status.textContent = [value.passed ? '验证通过' : '验证未通过', ...Object.entries(value.checks || {}).map(([key, ok])=>`${ok ? '✓' : '✗'} ${names[key] || key}`), value.error || '', value.uploaded ? '报告已回传，助手可以直接读取。' : `报告未回传：${value.uploadError || '请确认本地验证服务正在运行'}`].filter(Boolean).join('\n');
}
async function refresh() { render((await chrome.storage.local.get('probeStatus')).probeStatus); }
fetch(chrome.runtime.getURL('config.json')).then(r=>r.json()).then(config=>document.querySelector('#fixture').href=config.fixtureUrl).catch(error=>status.textContent=String(error));
refresh();
chrome.storage.onChanged.addListener((changes, area)=>{if(area==='local' && changes.probeStatus)render(changes.probeStatus.newValue);});
button.addEventListener('click',async()=>{
  button.disabled = true;
  try { const result=await chrome.runtime.sendMessage({type:'run-probe'}); if(result?.error)throw Error(result.error); await refresh(); }
  catch(error){status.textContent=String(error);button.disabled=false;}
});
retry.addEventListener('click',async()=>{
  retry.disabled=true;
  try { const result=await chrome.runtime.sendMessage({type:'retry-report'}); if(result?.error)throw Error(result.error); await refresh(); }
  catch(error){status.textContent=String(error);}
  finally {retry.disabled=false;}
});
