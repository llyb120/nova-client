use serde_json::{json, Value};
use std::io::{self, BufRead};
use tauri::{LogicalPosition, LogicalSize, WebviewBuilder, WebviewUrl, WindowBuilder};
use webview2_com::CallDevToolsProtocolMethodCompletedHandler;
use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_11;
use windows::core::{Interface, HSTRING};

fn reply(id: &Value, result: Result<Value, String>) {
    let message = match result {
        Ok(value) => json!({"id": id, "result": value}),
        Err(error) => json!({"id": id, "error": error}),
    };
    println!("{message}");
}

fn main() {
    let url = std::env::var("NOVA_PROBE_URL").expect("NOVA_PROBE_URL required");
    let profile = std::env::var("NOVA_PROBE_PROFILE").expect("NOVA_PROBE_PROFILE required");
    tauri::Builder::default()
        .setup(move |app| {
            let window = WindowBuilder::new(app, "probe")
                .title("Nova WebView2 native probe")
                .inner_size(1200.0, 800.0)
                .visible(false)
                .build()?;
            window.add_child(
                WebviewBuilder::new("shell", WebviewUrl::App("index.html".into())),
                LogicalPosition::new(0.0, 0.0),
                LogicalSize::new(400.0, 800.0),
            )?;
            let webview = window.add_child(
                WebviewBuilder::new("page", WebviewUrl::External(url.parse()?))
                    .data_directory(profile.into()),
                LogicalPosition::new(400.0, 0.0),
                LogicalSize::new(800.0, 800.0),
            )?;
            // A visible child is needed for realistic rendering/input. Show without requesting focus.
            window.show()?;
            println!("{}", json!({"ready": true, "backend": "WebView2 native COM"}));
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                for line in io::stdin().lock().lines() {
                    let Ok(line) = line else { break };
                    let command: Value = match serde_json::from_str(&line) {
                        Ok(value) => value,
                        Err(error) => { reply(&Value::Null, Err(error.to_string())); continue; }
                    };
                    let id = command["id"].clone();
                    let method = command["method"].as_str().unwrap_or("").to_owned();
                    if method == "probe.close" { break; }
                    if method == "probe.bounds" {
                        reply(&id, (|| {
                            let scale = window.scale_factor().map_err(|e| e.to_string())?;
                            let pos = webview.position().map_err(|e| e.to_string())?;
                            let size = webview.size().map_err(|e| e.to_string())?;
                            Ok(json!({"scale":scale,"x":pos.x,"y":pos.y,"width":size.width,"height":size.height}))
                        })());
                        continue;
                    }
                    if !matches!(method.as_str(), "Runtime.evaluate" | "Accessibility.getFullAXTree"
                        | "Page.captureScreenshot" | "Page.getFrameTree" | "Page.createIsolatedWorld"
                        | "Input.dispatchMouseEvent" | "Input.insertText" | "Input.dispatchKeyEvent"
                        | "Target.getTargets" | "Target.attachToTarget") {
                        reply(&id, Err("Method not allowed by probe".into()));
                        continue;
                    }
                    let params = command.get("params").cloned().unwrap_or(json!({})).to_string();
                    let session = command["sessionId"].as_str().map(str::to_owned);
                    let callback_id = id.clone();
                    let dispatch = webview.with_webview(move |native| unsafe {
                        let callback = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |status, body| {
                            reply(&callback_id, status.map_err(|e| e.to_string())
                                .and_then(|_| serde_json::from_str(&body).map_err(|e| e.to_string())));
                            Ok(())
                        }));
                        let result = native.controller().CoreWebView2().and_then(|core| {
                            if let Some(session) = session {
                                core.cast::<ICoreWebView2_11>()?.CallDevToolsProtocolMethodForSession(
                                    &HSTRING::from(session), &HSTRING::from(method), &HSTRING::from(params), &callback)
                            } else {
                                core.CallDevToolsProtocolMethod(&HSTRING::from(method), &HSTRING::from(params), &callback)
                            }
                        });
                        if let Err(error) = result { reply(&id, Err(error.to_string())); }
                    });
                    if let Err(error) = dispatch { reply(&command["id"], Err(error.to_string())); }
                }
                handle.exit(0);
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("WebView2 probe failed");
}
