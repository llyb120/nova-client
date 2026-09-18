from pathlib import Path
p=Path('scripts/chrome-precision-smoke.mjs');s=p.read_text(encoding='utf8')
old="  const invoke=(operation,args={})=>main.evaluate(`window.__TAURI_INTERNALS__.invoke('chrome_browser_ui',${JSON.stringify({operation,args})})`);"
assert s.count(old)==1
s=s.replace(old,"""  await main.call('Emulation.setFocusEmulationEnabled',{enabled:true});
  const native=(command,args={})=>main.evaluate(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)},${JSON.stringify(args)})`);
  // Exercise the UI entry point under a real local thread, without starting an agent.
  const thread=await native('create_thread',{cwd:process.cwd(),agentKind:'lyra',model:'',mode:'build',ephemeral:false});
  await until(()=>main.evaluate('!!document.querySelector(".thread-item")'),'fixture thread');
  await main.evaluate('document.querySelector(".thread-item").click()');
  await native('report_activity',{threadId:thread.id});
"""+old)
old="o=await inspect();const hover=find(o,'查询','订单筛选');"
assert s.count(old)==1
s=s.replace(old,"await page.call('Input.dispatchMouseEvent',{type:'mouseMoved',x:0,y:0});"+old)
p.write_text(s,encoding='utf8')
