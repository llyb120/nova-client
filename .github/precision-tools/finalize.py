from pathlib import Path

def change(path, old, new):
    p=Path(path);s=p.read_text(encoding='utf8')
    assert s.count(old)==1,(path,old,s.count(old))
    p.write_text(s.replace(old,new),encoding='utf8')

change('scripts/native-browser-smoke.mjs',"const targets = async () => (await fetch(`http://127.0.0.1:${debugPort}/json/list`)).json();",'''// Separate WebView2 profiles have separate browser processes. CI gives the
// sidebar's test-only debugging endpoint its own port; neither is a product setting.
const targets = async () => (await Promise.all([...new Set([debugPort, Number(process.env.TEST_CHILD_CDP_PORT || debugPort)])].map(async port => {
  try { return await (await fetch(`http://127.0.0.1:${port}/json/list`, { signal: AbortSignal.timeout(1000) })).json(); }
  catch { return []; }
}))).flat();''')
change('scripts/native-browser-smoke.mjs',"  // Partial center occlusion should use a visible edge without an extra model or retry.",'''  // A click can succeed while its focus handler redirects elsewhere. Such a
  // partially performed fill must be needs_review, never a replayable failure.
  await page.evaluate(`document.body.insertAdjacentHTML('afterbegin','<input id="focus-trap" aria-label="不应修改" value="preserve">');document.querySelector('label input').addEventListener('focus',()=>document.querySelector('#focus-trap').focus(),{once:true})`);
  observation=await ui('inspect');
  const hijacked=await act({action:'fill',...find(observation,'订单号'),text:'must not reach focus trap'},observation);
  assert.equal(hijacked.status,'needs_review',JSON.stringify(hijacked));
  assert.match(hijacked.reason,/焦点/);
  assert.equal(await page.evaluate('document.querySelector("#focus-trap").value'),'preserve');
  await page.evaluate('document.querySelector("#focus-trap").remove()');
  // Partial center occlusion should use a visible edge without an extra model or retry.''')
change('src-tauri/src/jianlai.rs','region超出原始窗口截图范围，必须使用originalWidth/originalHeight像素坐标','region超出原始截图范围，必须使用originalWidth/originalHeight像素坐标')
change('src-tauri/src/jianlai.rs','''        // Simulate a stale foreground identity without actually changing the user's focus.''','''        // Exercise a real monitor crop and preflight without sending a click.
        // Only the saved witness is changed to simulate stale target pixels.
        let monitor = Monitor::all().unwrap().remove(0);
        let cropped = run("test".into(), json!({"operation":"screenshot","monitorId":monitor.id().unwrap(),
            "maxEdge":0,"region":{"x":0,"y":0,"width":128,"height":128}})).unwrap();
        assert_eq!(cropped["images"][0]["width"],128);
        let click: Action = serde_json::from_value(json!({"action":"click","x":64,"y":64})).unwrap();
        {
            let snapshot = DESKTOP.lock().unwrap();
            let snapshot = snapshot.as_ref().unwrap();
            let original = &snapshot.shots[0];
            check_target(snapshot,original,&click).unwrap();
            check_pixels(snapshot,original,&click).unwrap();
            let mut changed = original.clone();
            let mut pixels = changed.guard.as_ref().unwrap().as_ref().clone();
            for y in 57..72 { for x in 57..72 {
                let p=pixels.get_pixel_mut(x,y);
                for c in 0..3 {p[c]=255-p[c];}
            }}
            changed.guard=Some(std::sync::Arc::new(pixels));
            assert!(check_pixels(snapshot,&changed,&click).unwrap_err().contains("画面已变化"));
        }
        let _=std::fs::remove_file(cropped["images"][0]["path"].as_str().unwrap());
        eprintln!("desktop guard: real monitor crop + unchanged target accepted + stale witness rejected; no click sent");
        // Restore the feedback snapshot used by the stale-focus test below.
        let result = run("test".into(),json!({"operation":"screenshot"})).unwrap();
        // Simulate a stale foreground identity without actually changing the user's focus.''')
change('src-tauri/src/native_browser.rs','''        assert!(parse_action(r#"{"action":"eval","code":"alert(1)"}"#).is_err());''','''        assert!(parse_action(r#"{"action":"eval","code":"alert(1)"}"#).is_err());
        for action in [
            r#"{"action":"click_at","imageId":"image-1","x":1.25,"y":2.5}"#,
            r#"{"action":"double_click_at","imageId":"image-1","x":1,"y":2}"#,
            r#"{"action":"drag","imageId":"image-1","x":1,"y":2,"to_x":3,"to_y":4}"#,
            r#"{"action":"scroll_at","imageId":"image-1","x":1,"y":2,"delta":0,"delta_x":40}"#,
            r#"{"action":"wait_for","frame":0,"ref":"v1:0","state":"enabled","ms":500}"#,
        ] { assert!(parse_action(action).is_ok(), "{action}"); }''')
for path in ['scripts/chrome-tool.json','scripts/webview-tool.json']:
    import json
    p=Path(path);v=json.loads(p.read_text(encoding='utf8'))
    v['inputSchema']['properties']['maxItems']['description']='回复最多元素数，默认60；优先可见未遮挡元素。documentPath保存全部已采集元素，query存在时仅保存匹配元素。'
    p.write_text(json.dumps(v,ensure_ascii=False,indent=2)+'\n',encoding='utf8')
