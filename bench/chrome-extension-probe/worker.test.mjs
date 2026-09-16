import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { runInNewContext } from 'node:vm';

test('failed upload retains evidence; retry sends it without any browser action, including old-version summaries', async () => {
  const data = { pendingReport: {passed:true,checks:{screenshot:true},screenshot:'saved-png'} };
  let listener, online = false, sent, browserCalls = 0;
  const chrome = {
    runtime:{id:'probe',getURL:path=>`extension://${path}`,onMessage:{addListener:fn=>listener=fn}},
    storage:{local:{get:async()=>({...data}),set:async values=>Object.assign(data,values),remove:async key=>{delete data[key];}}},
    action:{setBadgeText:async()=>{}},
    debugger:{attach:async()=>{browserCalls++;throw Error('Must not replay actions');}},
  };
  runInNewContext(await readFile(new URL('./worker.js',import.meta.url),'utf8'),{
    chrome,AbortSignal,fetch:async(url,options)=>{
      if(url==='extension://config.json')return {json:async()=>({origin:'http://127.0.0.1:1234',token:'test'})};
      if(!online)throw Error('Failed to fetch');
      sent=JSON.parse(options.body);return {ok:true};
    },
  });
  const retry=()=>new Promise(resolve=>listener({type:'retry-report'},{id:'probe'},resolve));
  assert.equal((await retry()).done,true);
  assert.equal(data.probeStatus.uploaded,false);
  assert.equal(data.pendingReport.screenshot,'saved-png');
  online=true;await retry();
  assert.equal(sent.screenshot,'saved-png');assert.equal(data.pendingReport,undefined);
  assert.equal(data.probeStatus.uploaded,true);assert.equal(browserCalls,0);
  data.probeStatus={passed:true,checks:{screenshot:true},uploaded:false};
  await retry();
  assert.equal(sent.screenshotUnavailable,true);assert.equal(sent.screenshot,undefined);
  assert.equal(browserCalls,0);
});
