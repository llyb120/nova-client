from pathlib import Path
p=Path('scripts/native-browser-smoke.mjs');s=p.read_text(encoding='utf8')
old="  const invoke=(command,args={})=>main.evaluate(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)},${JSON.stringify(args)})`);"
assert s.count(old)==1
s=s.replace(old,old+'''
  // Optional driver compiled ONLY into the isolated CI copy. This avoids
  // assuming every WebView2 profile exposes the same remote target list.
  const nativeDriver=label=>{
    const call=(method,params={},sessionId)=>invoke('precision_smoke_cdp',{label,method,params,sessionId:sessionId??null});
    const evaluate=async expression=>{const value=await call('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true});if(value.exceptionDetails)throw Error(JSON.stringify(value.exceptionDetails));return value.result.value;};
    return {call,evaluate};
  };
''')
old="  const page=await attach(await until(async()=> (await targets()).find(t=>t.url.includes('/fixture')),'fixture'));"
assert s.count(old)==1
s=s.replace(old,"  const page=process.env.TEST_NATIVE_CDP_DRIVER ? nativeDriver((await ui('status')).activeTab) : await attach(await until(async()=> (await targets()).find(t=>t.url.includes('/fixture')),'fixture'));\n  console.log('fixture driver:',await page.evaluate('location.href'));")
old="  const delayed=await attach(await until(async()=> (await targets()).find(t=>t.url.includes('/delayed')),'delayed target'));"
assert s.count(old)==1
s=s.replace(old,"  const delayed=process.env.TEST_NATIVE_CDP_DRIVER ? nativeDriver((await ui('status')).activeTab) : await attach(await until(async()=> (await targets()).find(t=>t.url.includes('/delayed')),'delayed target'));")
p.write_text(s,encoding='utf8')
