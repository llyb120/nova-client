from pathlib import Path

def replace_once(path, old, new):
    p=Path(path); s=p.read_text(encoding='utf-8')
    assert s.count(old)==1, f'{path}: expected exactly one match, found {s.count(old)}'
    p.write_text(s.replace(old,new), encoding='utf-8')

replace_once('src-tauri/build.rs',
'''        } else if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {''',
'''            // Normal binaries already contain Tauri's RT_MANIFEST resource.
            // Do not generate a second resource with the same ID for them.
            println!("cargo:rustc-link-arg-bins=/MANIFEST:NO");
        } else if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {''')
replace_once('src/terminalSessions.ts',
'''      return terminalApi.resize(tab.id, tab.terminal.cols, tab.terminal.rows).catch(error => { if (tab.status() !== "exited") throw error; });''',
'''      // Creation, not resize, gates input. ConPTY may need xterm's cursor
      // response to finish a resize; putting resize in ready deadlocks input.
      void terminalApi.resize(tab.id, tab.terminal.cols, tab.terminal.rows).catch(error => {
        if (!tab.disposed && tab.status() !== "exited") tab.setError(String(error));
      });''')
replace_once('src/terminalSessions.ts',
'''          if (!tab.disposed) void terminalApi.ack(tab.id, event.data.length).catch(error => tab.setError(String(error)));''',
'''          if (!tab.disposed && tab.status() !== "exited") void terminalApi.ack(tab.id, event.data.length).catch(error => {
            if (!tab.disposed && tab.status() !== "exited") tab.setError(String(error));
          });''')
replace_once('src-tauri/src/workspace_terminal.rs',
'''        flow.closed = true;
        self.ready.notify_all();
        if !flow.reaped {
            if let Some(pid) = self.pid {
                crate::acp::kill_process_tree(pid);
            }
            let _ = self.killer.lock().unwrap_or_else(|e| e.into_inner()).kill();
        }
    }
    fn finish(&self)''',
'''        flow.closed = true;
        let reaped = flow.reaped;
        self.ready.notify_all();
        // The output thread must drain while process termination/ConPTY close
        // runs. Never hold its flow-control mutex across blocking OS calls.
        drop(flow);
        if !reaped {
            if let Some(pid) = self.pid {
                crate::acp::kill_process_tree(pid);
            }
            let _ = self.killer.lock().unwrap_or_else(|e| e.into_inner()).kill();
        }
        // A startup query may already have reached a now-disposed renderer.
        // Complete that handshake even when no further output is delivered.
        #[cfg(windows)]
        self.finish_cursor_handshake();
    }
    #[cfg(windows)]
    fn finish_cursor_handshake(&self) {
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writer.write_all(b"\\x1b[1;1R").and_then(|_| writer.flush());
    }
    fn finish(&self)''')
replace_once('src-tauri/src/workspace_terminal.rs',
'''            let mut buffer = [0u8; 8192];
            loop {''',
'''            let mut buffer = [0u8; 8192];
            #[cfg(windows)]
            let mut cursor_query = 0usize;
            loop {''')
replace_once('src-tauri/src/workspace_terminal.rs',
'''                    Ok(count) => {
                        let mut flow = live.flow.lock().unwrap_or_else(|e| e.into_inner());
                        if flow.closed {
                            continue;
                        }''',
'''                    Ok(count) => {
                        // Preserve a partial DSR across reads. During shutdown
                        // the renderer no longer answers, but ConPTY still needs
                        // its inherited cursor response before it can close.
                        #[cfg(windows)]
                        let mut queried_cursor = false;
                        #[cfg(windows)]
                        for &byte in &buffer[..count] {
                            if byte == b"\\x1b[6n"[cursor_query] {
                                cursor_query += 1;
                                if cursor_query == 4 {
                                    queried_cursor = true;
                                    cursor_query = 0;
                                }
                            } else {
                                cursor_query = usize::from(byte == 0x1b);
                            }
                        }
                        let mut flow = live.flow.lock().unwrap_or_else(|e| e.into_inner());
                        if flow.closed {
                            drop(flow);
                            #[cfg(windows)]
                            if queried_cursor {
                                live.finish_cursor_handshake();
                            }
                            continue;
                        }''')
p=Path('src-tauri/src/workspace_terminal.rs'); s=p.read_text(encoding='utf-8')
assert s.rstrip().endswith('}')
s=s.rstrip()[:-1]+'''    #[test]
    fn closing_all_during_startup_releases_every_reader() {
        let dir = tempfile::tempdir().unwrap();
        let shell = if cfg!(windows) { "cmd.exe" } else { "/bin/sh" };
        for _ in 0..8 {
            let manager = TerminalManager::default();
            let id = uuid::Uuid::new_v4().to_string();
            let (tx, rx) = std::sync::mpsc::channel();
            start(
                manager.clone(), id.clone(),
                shell_command(shell, &[], dir.path()).unwrap(),
                size(80, 24).unwrap(),
                move |event| tx.send(event).map_err(|e| e.to_string()),
            ).unwrap();
            let session = manager.get(&id).unwrap();
            manager.close_all();
            let _ = collect_events(&manager, &id, &rx);
            assert!(session.flow.lock().unwrap().reaped);
            assert!(manager.get(&id).is_err());
        }
    }
}
'''
p.write_text(s,encoding='utf-8')
replace_once('scripts/workspace-terminal.test.mjs',
'''let deferred=false,fail=false,stops=0;''',
'''let deferred=false,fail=false,stops=0,gatedResize=false;
const resizeGates=new Map();''')
replace_once('scripts/workspace-terminal.test.mjs',
'''terminalApi.resize=async(id,cols,rows)=>{resized.push({id,cols,rows});};''',
'''terminalApi.resize=async(id,cols,rows)=>{
  resized.push({id,cols,rows});
  if(gatedResize){gatedResize=false;await new Promise(resolve=>resizeGates.set(id,resolve));}
};''')
replace_once('scripts/workspace-terminal.test.mjs',
''' paste:text=>active().terminal.paste(text),stopCount:()=>stops,''',
''' paste:text=>active().terminal.paste(text),stopCount:()=>stops,
 gateResize:()=>{gatedResize=true;},hasResizeGate:id=>resizeGates.has(id),
 releaseResize:id=>{resizeGates.get(id)?.();resizeGates.delete(id);},''')
replace_once('scripts/workspace-terminal.test.mjs',
'''  if(process.env.TEST_SCREENSHOT)await page.screenshot({path:process.env.TEST_SCREENSHOT});''',
'''  // Windows can wait for a cursor response inside the first resize. Input
  // and close must remain usable while that native resize is unresolved.
  await page.evaluate(()=>window.termTest.gateResize());
  await page.getByRole('button',{name:'新建终端',exact:true}).click();
  await page.waitForFunction(()=>window.termTest.hasResizeGate(window.termTest.id()));
  const gated=await page.evaluate(()=>window.termTest.id());
  const beforeGatedQuery=await page.evaluate(()=>window.termTest.writes.length);
  await page.evaluate(id=>window.termTest.send(id,{type:'data',data:[27,91,54,110]}),gated);
  await page.waitForFunction(before=>window.termTest.writes.slice(before).some(w=>w.data.at(-1)===82),beforeGatedQuery);
  await page.getByRole('button',{name:'关闭终端 终端 2',exact:true}).click();
  await page.waitForFunction(id=>window.termTest.closed.includes(id),gated);
  await page.evaluate(id=>window.termTest.releaseResize(id),gated);
  if(process.env.TEST_SCREENSHOT)await page.screenshot({path:process.env.TEST_SCREENSHOT});''')
