//! 剑谱的纯数据部分：原始事件 → 语义步骤、点击截图标注。不依赖 Tauri，便于单独测试。
use serde::{Deserialize, Serialize};

const IMAGE_EDGE: u32 = 1280;

/// 截图里点击位置的换算：图片像素 = (屏幕物理像素 - origin) * scale。
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Frame { pub file: String, pub origin: (i32, i32), pub scale: f32 }

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Ctx { pub app: String, pub title: String, /** Chrome 页面地址（地址栏读到的），其它应用为空。 */ pub url: String }

/// worker 补上上下文与截图后的事件，`fold` 的输入（纯数据，便于测试）。
#[derive(Clone, Debug)]
pub(super) enum Ev {
    Down { at: i64, button: u8, pt: (i32, i32), ctx: Ctx, frame: Option<Frame> },
    Up { button: u8, pt: (i32, i32) },
    Wheel { at: i64, delta: i32, horizontal: bool, ctx: Ctx },
    Key { at: i64, name: String, text: Option<String>, mods: [bool; 4], ctx: Ctx },
    /// 一段输入结束时从焦点输入框读到的内容（输入法输入只能靠它拿到原文）。
    Value { text: String },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Step {
    /// 距录制开始的毫秒数。
    pub at: i64,
    /// click / double_click / right_click / middle_click / drag / scroll / type / key
    pub kind: String,
    pub app: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub x: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub y: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub to_x: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub to_y: Option<i32>,
    /// type：按键得到的文字。
    #[serde(default, skip_serializing_if = "Option::is_none")] pub text: Option<String>,
    /// type：输入结束时输入框的内容。
    #[serde(default, skip_serializing_if = "Option::is_none")] pub value: Option<String>,
    /// key：组合键；scroll：方向 up/down/left/right。
    #[serde(default, skip_serializing_if = "Option::is_none")] pub key: Option<String>,
    /// scroll：滚轮格数。
    #[serde(default, skip_serializing_if = "Option::is_none")] pub amount: Option<i32>,
    /// 点击时 Chrome 地址栏的页面地址：这一步属于 chrome 工具的网站经验。
    #[serde(default, skip_serializing_if = "Option::is_none")] pub url: Option<String>,
}


fn image_point(frame: &Option<Frame>, pt: (i32, i32)) -> (Option<i32>, Option<i32>) {
    frame.as_ref().map_or((None, None), |f| (Some(((pt.0 - f.origin.0) as f32 * f.scale).round() as i32), Some(((pt.1 - f.origin.1) as f32 * f.scale).round() as i32)))
}

fn combo(name: &str, [ctrl, alt, shift, win]: [bool; 4]) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if ctrl { parts.push("Ctrl"); }
    if alt { parts.push("Alt"); }
    if shift { parts.push("Shift"); }
    if win { parts.push("Win"); }
    parts.push(name);
    parts.join("+")
}

/// 原始事件 → 语义步骤：按下/抬起配成点击或拖拽，连续两次近处点击并成双击，连续同向滚动合并，
/// 连续打字合并（退格删掉上一个字），读到的输入框内容挂到这段输入上；修饰键组合记为 key。
pub(super) fn fold(events: &[Ev]) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    let mut down: Option<(i64, u8, (i32, i32), Ctx, Option<Frame>)> = None;
    let mut typing = false;
    let mut last_click: Option<(i64, u8, (i32, i32))> = None;
    for ev in events {
        if !matches!(ev, Ev::Key { .. } | Ev::Value { .. }) { typing = false; }
        match ev {
            Ev::Down { at, button, pt, ctx, frame } => down = Some((*at, *button, *pt, ctx.clone(), frame.clone())),
            Ev::Up { button, pt, .. } => {
                let Some((start, b, from, ctx, frame)) = down.take().filter(|d| d.1 == *button) else { continue };
                let far = (pt.0 - from.0).abs().max((pt.1 - from.1).abs());
                let (x, y) = image_point(&frame, from);
                let url = Some(ctx.url).filter(|u| !u.is_empty());
                let mut step = Step { at: start, app: ctx.app, title: ctx.title, url, x, y, image: frame.as_ref().map(|f| f.file.clone()), ..Step::default() };
                if far > 8 {
                    let (to_x, to_y) = image_point(&frame, *pt);
                    step = Step { kind: "drag".into(), to_x, to_y, ..step };
                    last_click = None;
                } else if let Some(prev) = steps.last_mut().filter(|_| last_click.is_some_and(|(t, pb, p)| pb == b && start - t <= 500
                        && (p.0 - from.0).abs().max((p.1 - from.1).abs()) <= 4)) {
                    // 第二次点击并入上一步；保留第一次的截图，那才是双击前的画面。
                    prev.kind = "double_click".into();
                    last_click = None;
                    continue;
                } else {
                    step.kind = match b { 1 => "right_click", 2 => "middle_click", _ => "click" }.into();
                    last_click = Some((start, b, from));
                }
                steps.push(step);
            }
            Ev::Wheel { at, delta, horizontal, ctx } => {
                last_click = None;
                let dir = match (*horizontal, *delta > 0) { (false, true) => "up", (false, false) => "down", (true, true) => "right", (true, false) => "left" };
                if let Some(prev) = steps.last_mut().filter(|s| s.kind == "scroll" && s.key.as_deref() == Some(dir) && s.app == ctx.app && at - s.at <= 1500) {
                    prev.amount = Some(prev.amount.unwrap_or(0) + delta.abs());
                    prev.at = *at;
                    continue;
                }
                steps.push(Step { at: *at, kind: "scroll".into(), app: ctx.app.clone(), title: ctx.title.clone(), key: Some(dir.into()), amount: Some(delta.abs()), ..Step::default() });
            }
            Ev::Key { at, name, text, mods, ctx } => {
                last_click = None;
                let plain = !mods[0] && !mods[1] && !mods[3];
                let open = typing && steps.last().is_some_and(|s| s.kind == "type" && s.app == ctx.app);
                if plain && name == "Backspace" && open {
                    if let Some(text) = steps.last_mut().unwrap().text.as_mut() { text.pop(); }
                    continue;
                }
                if let (true, Some(text)) = (plain, text) {
                    if open { steps.last_mut().unwrap().text.get_or_insert_default().push_str(text); } else {
                        steps.push(Step { at: *at, kind: "type".into(), app: ctx.app.clone(), title: ctx.title.clone(), text: Some(text.clone()), ..Step::default() });
                    }
                    typing = true;
                    continue;
                }
                typing = false;
                steps.push(Step { at: *at, kind: "key".into(), app: ctx.app.clone(), title: ctx.title.clone(), key: Some(combo(name, *mods)), ..Step::default() });
            }
            Ev::Value { text } => {
                if let Some(last) = steps.last_mut().filter(|s| s.kind == "type") { last.value = Some(text.clone()); }
            }
        }
    }
    steps
}

/// 停止录制的那一下快捷键本身不是演示内容。
pub(super) fn drop_trailing_shortcut(steps: &mut Vec<Step>, shortcut: &str) {
    let norm = |s: &str| s.split('+').map(|p| match p.trim().to_ascii_lowercase().as_str() {
        "control" => "ctrl".to_string(), "super" | "meta" | "cmd" => "win".to_string(), "option" => "alt".to_string(), other => other.to_string(),
    }).collect::<Vec<_>>().join("+");
    if !shortcut.is_empty() && steps.last().is_some_and(|s| s.kind == "key" && s.key.as_deref().map(norm) == Some(norm(shortcut))) { steps.pop(); }
}


/// 点击截图：红圈标出落点，长边缩到 1280，存 JPEG。
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(super) fn save_frame(mut image: image::RgbaImage, origin: (i32, i32), source_width: u32, pt: (i32, i32), path: &std::path::Path) -> Option<f32> {
    let mut scale = image.width() as f32 / source_width.max(1) as f32;
    let long = image.width().max(image.height());
    if long > IMAGE_EDGE {
        let k = IMAGE_EDGE as f32 / long as f32;
        image = image::imageops::resize(&image, (image.width() as f32 * k) as u32, (image.height() as f32 * k) as u32, image::imageops::FilterType::Triangle);
        scale *= k;
    }
    let (cx, cy) = ((pt.0 - origin.0) as f32 * scale, (pt.1 - origin.1) as f32 * scale);
    for y in 0..image.height() {
        for x in 0..image.width() {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            if (9.0..12.0).contains(&d) { image.put_pixel(x, y, image::Rgba([239, 68, 68, 255])); }
        }
    }
    let rgb = image::DynamicImage::ImageRgba8(image).into_rgb8();
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 80)
        .encode(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8).ok()?;
    std::fs::write(path, bytes).ok()?;
    Some(scale)
}


#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(app: &str) -> Ctx { Ctx { app: app.into(), title: "t".into(), url: if app == "Chrome" { "https://a.com/x".into() } else { String::new() } } }
    fn key(at: i64, name: &str, text: Option<&str>, mods: [bool; 4], app: &str) -> Ev {
        Ev::Key { at, name: name.into(), text: text.map(Into::into), mods, ctx: ctx(app) }
    }
    const NONE: [bool; 4] = [false; 4];

    #[test]
    fn fold_merges_clicks_typing_scrolls_and_keys() {
        let frame = Some(Frame { file: "001.jpg".into(), origin: (100, 50), scale: 0.5 });
        let events = vec![
            // 双击：两次近处点击并成一步，坐标换算到截图像素。
            Ev::Down { at: 0, button: 0, pt: (300, 250), ctx: ctx("WeChat"), frame: frame.clone() },
            Ev::Up { button: 0, pt: (300, 250) },
            Ev::Down { at: 200, button: 0, pt: (302, 251), ctx: ctx("WeChat"), frame: None },
            Ev::Up { button: 0, pt: (302, 251) },
            // 打字合并，退格删字，IME 只开段，读值挂上去。
            key(300, "A", Some("a"), NONE, "WeChat"),
            key(310, "B", Some("b"), NONE, "WeChat"),
            key(320, "Backspace", None, NONE, "WeChat"),
            key(330, "IME", Some(""), NONE, "WeChat"),
            Ev::Value { text: "a你好".into() },
            key(400, "Enter", None, NONE, "WeChat"),
            key(500, "C", Some("c"), [true, false, false, false], "WeChat"),
            // 连续同向滚动合并，换方向另起一步。
            Ev::Wheel { at: 600, delta: -1, horizontal: false, ctx: ctx("WeChat") },
            Ev::Wheel { at: 700, delta: -2, horizontal: false, ctx: ctx("WeChat") },
            Ev::Wheel { at: 800, delta: 1, horizontal: false, ctx: ctx("WeChat") },
            // 拖拽；没配上按下的抬起被忽略。
            Ev::Down { at: 900, button: 0, pt: (100, 50), ctx: ctx("Explorer"), frame: frame.clone() },
            Ev::Up { button: 0, pt: (140, 90) },
            Ev::Up { button: 1, pt: (0, 0) },
            Ev::Down { at: 1000, button: 1, pt: (120, 70), ctx: ctx("Explorer"), frame },
            Ev::Up { button: 1, pt: (120, 70) },
            key(1100, "R", None, [false, true, false, false], "Explorer"),
        ];
        let mut steps = fold(&events);
        drop_trailing_shortcut(&mut steps, "alt + r");
        let kinds: Vec<_> = steps.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(kinds, ["double_click", "type", "key", "key", "scroll", "scroll", "drag", "right_click"]);
        assert_eq!((steps[0].x, steps[0].y, steps[0].image.as_deref()), (Some(100), Some(100), Some("001.jpg")));
        assert_eq!((steps[1].text.as_deref(), steps[1].value.as_deref()), (Some("a"), Some("a你好")));
        assert_eq!((steps[2].key.as_deref(), steps[3].key.as_deref()), (Some("Enter"), Some("Ctrl+C")));
        assert_eq!((steps[4].key.as_deref(), steps[4].amount, steps[5].key.as_deref()), (Some("down"), Some(3), Some("up")));
        assert_eq!((steps[6].x, steps[6].to_x, steps[6].to_y, steps[6].app.as_str()), (Some(0), Some(20), Some(20), "Explorer"));
        // 别的应用里的打字不并入上一段。
        let split = fold(&[key(0, "A", Some("a"), NONE, "A"), key(1, "B", Some("b"), NONE, "B")]);
        assert_eq!(split.len(), 2);
        // Chrome 里的点击带上页面地址，供整理成 chrome 网站经验。
        let web = fold(&[Ev::Down { at: 0, button: 0, pt: (1, 1), ctx: ctx("Chrome"), frame: None }, Ev::Up { button: 0, pt: (1, 1) }]);
        assert_eq!((web[0].url.as_deref(), steps[0].url.as_deref()), (Some("https://a.com/x"), None));
    }

    #[test]
    fn frame_marks_the_click_and_scales_to_the_saved_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("001.jpg");
        let scale = save_frame(image::RgbaImage::from_pixel(2560, 1440, image::Rgba([255, 255, 255, 255])), (-2560, 0), 2560, (-1280, 720), &path).unwrap();
        assert_eq!(scale, 0.5);
        let saved = image::open(&path).unwrap().into_rgb8();
        assert_eq!(saved.dimensions(), (1280, 720));
        let ring = saved.get_pixel(640 + 10, 360);
        assert!(ring[0] > 200 && ring[1] < 120, "红圈落在点击处: {ring:?}");
    }
}

