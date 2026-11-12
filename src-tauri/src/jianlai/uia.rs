//! Windows 控件树观察（UIAutomation）：只读矩形与名称，不参与任何输入。
//! 矩形是物理屏幕像素；由调用方（jianlai.rs）映射到当前快照的图片像素。
use std::{
    ptr::null_mut,
    time::{Duration, Instant},
};
use windows::{
    core::Interface,
    Win32::{
        Foundation::HWND as UiaHwnd,
        System::Com::{
            CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
        },
        UI::Accessibility::*,
    },
};

/// 跨进程 UIA 在慢应用上会长时间阻塞：与任务栏布局同一上限，超时按「结果不完整」报告。
const DEADLINE: Duration = Duration::from_millis(900);
/// 访问节点上限，避免在超宽列表里边遍历边分配。
const VISIT_LIMIT: usize = 800;

pub(super) struct Item {
    pub kind: UIA_CONTROLTYPE_ID,
    /// 本地化控件类型（按钮 / 菜单项 …）；跨语言不稳定，仅作模型辨认的提示。
    pub role: String,
    pub name: String,
    pub automation_id: String,
    /// 物理屏幕像素：left / top / width / height。
    pub rect: (i32, i32, i32, i32),
    pub depth: usize,
}

pub(super) struct Report {
    pub items: Vec<Item>,
    pub truncated: bool,
    /// 第一次截断的原因：deadline / visitLimit / itemLimit / queryFailed。
    pub truncated_by: Option<&'static str>,
    pub visited: usize,
    pub elapsed_ms: u128,
}

impl Report {
    /// 只记第一次截断原因：更深的后续原因不影响模型该怎么调整参数。
    fn mark_truncated(&mut self, reason: &'static str) {
        self.truncated = true;
        self.truncated_by.get_or_insert(reason);
    }
}

/// 从顶层窗口 hwnd 的 ControlView 树收集有名称的可见控件。
/// UIA 走自己的 MTA 线程：tokio blocking 线程的 COM 单元状态不受我们控制，
/// 在 STA 线程上对别的进程控件树做跨进程查询会和它自己的消息循环互相等待。
pub(super) fn collect(
    hwnd: u32,
    depth: usize,
    max_items: usize,
    filter: Option<&str>,
) -> Result<Report, String> {
    let filter = filter.map(str::to_owned);
    std::thread::Builder::new()
        .name("nova-uia".into())
        .spawn(move || query(hwnd, depth, max_items, filter.as_deref()))
        .map_err(|e| format!("启动 UIA 线程失败：{e}"))?
        .join()
        .map_err(|_| "UIA 查询线程异常结束".to_string())?
}

fn query(
    hwnd: u32,
    depth: usize,
    max_items: usize,
    filter: Option<&str>,
) -> Result<Report, String> {
    let started = Instant::now();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let automation: IUIAutomation2 = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("UIAutomation 不可用：{e}"))?;
        // 慢应用的控件树不拖住整次调用：连接/事务超时与本地 deadline 双重兜底。
        let _ = automation.SetConnectionTimeout(300);
        let _ = automation.SetTransactionTimeout(300);
        let root = automation
            .ElementFromHandle(UiaHwnd(hwnd as _))
            .map_err(|e| format!("读不到该窗口的控件树：{e}"))?;
        // 不可见的窗口没有可点的控件；返回空列表会被误读成「窗口里没有控件」。
        if root.CurrentIsOffscreen().map(|v| v.as_bool()).unwrap_or(false) {
            return Err("窗口当前不可见（已最小化或被隐藏），先让它显示在桌面上再观察".into());
        }
        let walker = automation.ControlViewWalker().map_err(|e| e.to_string())?;
        let filter = filter.map(str::to_lowercase);
        let mut report = Report { items: Vec::new(), truncated: false, truncated_by: None, visited: 0, elapsed_ms: 0 };
        walk(&walker, &root, 0, depth, max_items, filter.as_deref(), started, &mut report);
        report.elapsed_ms = started.elapsed().as_millis();
        Ok(report)
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn walk(
    walker: &IUIAutomationTreeWalker,
    node: &IUIAutomationElement,
    level: usize,
    depth: usize,
    max_items: usize,
    filter: Option<&str>,
    started: Instant,
    report: &mut Report,
) {
    report.visited += 1;
    if report.visited > VISIT_LIMIT {
        report.mark_truncated("visitLimit");
        return;
    }
    if started.elapsed() > DEADLINE {
        report.mark_truncated("deadline");
        return;
    }
    // 不可见子树整枝跳过：滚出视图或被隐藏的项既截不到也点不到。
    if node.CurrentIsOffscreen().map(|v| v.as_bool()).unwrap_or(true) {
        return;
    }
    if let Some(item) = describe(node, level, filter) {
        report.items.push(item);
        if report.items.len() >= max_items {
            report.mark_truncated("itemLimit");
            return;
        }
    }
    if level >= depth {
        return;
    }
    let mut child = match relative(walker, node, true) {
        Some(child) => child,
        None => {
            report.mark_truncated("queryFailed");
            return;
        }
    };
    while let Some(element) = child {
        walk(walker, &element, level + 1, depth, max_items, filter, started, report);
        if report.truncated {
            return;
        }
        if started.elapsed() > DEADLINE {
            report.mark_truncated("deadline");
            return;
        }
        child = match relative(walker, &element, false) {
            Some(next) => next,
            None => {
                report.mark_truncated("queryFailed");
                return;
            }
        };
    }
}

/// 无可读名称的容器对点击没有意义，只继续遍历它的子树。
unsafe fn describe(
    node: &IUIAutomationElement,
    depth: usize,
    filter: Option<&str>,
) -> Option<Item> {
    // 先只读名称：无名容器占大多数，不必为它们再读类型/矩形。
    let name = node.CurrentName().map(|v| v.to_string().trim().to_string()).unwrap_or_default();
    let automation_id = node.CurrentAutomationId().map(|v| v.to_string()).unwrap_or_default();
    if name.is_empty() && automation_id.is_empty() {
        return None;
    }
    // UIA 矩形来自别的进程，异常值不能在这抡溢出。
    let kind = node.CurrentControlType().ok()?;
    let rect = node.CurrentBoundingRectangle().ok()?;
    let (width, height) = (rect.right.saturating_sub(rect.left), rect.bottom.saturating_sub(rect.top));
    if width <= 0 || height <= 0 {
        return None;
    }
    let role = node.CurrentLocalizedControlType().map(|v| v.to_string()).unwrap_or_default();
    if let Some(filter) = filter {
        if ![&name, &automation_id, &role].iter().any(|v| v.to_lowercase().contains(filter)) {
            return None;
        }
    }
    Some(Item { kind, role, name, automation_id, rect: (rect.left, rect.top, width, height), depth })
}

/// 保留 S_OK + null（遍历结束）与查询失败的区别：生成的包装会把两者都变成 Err，
/// 失败时不能当作「没有子元素」，否则不完整的控件树会被报告成完整结果。
unsafe fn relative(
    walker: &IUIAutomationTreeWalker,
    node: &IUIAutomationElement,
    first: bool,
) -> Option<Option<IUIAutomationElement>> {
    let mut raw = null_mut();
    let call =
        if first { walker.vtable().GetFirstChildElement } else { walker.vtable().GetNextSiblingElement };
    call(walker.as_raw(), node.as_raw(), &mut raw).ok().ok()?;
    Some((!raw.is_null()).then(|| IUIAutomationElement::from_raw(raw)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_keeps_the_first_reason() {
        let mut report = Report { items: Vec::new(), truncated: false, truncated_by: None, visited: 0, elapsed_ms: 0 };
        report.mark_truncated("itemLimit");
        // 更深层的后续原因不覆盖第一个：模型按第一原因调参就够。
        report.mark_truncated("deadline");
        assert!(report.truncated);
        assert_eq!(report.truncated_by, Some("itemLimit"));
    }

    #[test]
    #[ignore = "needs a real desktop: cargo test --lib uia_reads_a_real_window -- --ignored"]
    fn uia_reads_a_real_window() {
        // 用真实窗口（不是桌面根窗口：它只能通过 RootElement 枚举子节点）验证读取链。
        let own = std::process::id();
        let target = xcap::Window::all().ok().and_then(|all| {
            all.into_iter().find(|w| {
                w.pid().ok() != Some(own)
                    && w.is_minimized().ok() == Some(false)
                    && w.width().ok().unwrap_or(0) > 200
                    && w.height().ok().unwrap_or(0) > 200
            })
        });
        let Some(window) = target else {
            eprintln!("skipped: 没有其它可见窗口可观察");
            return;
        };
        let report = collect(window.id().unwrap(), 3, 40, None).expect("UIA query");
        assert!(report.visited > 1, "only the root was reachable");
        assert!(report.elapsed_ms < 3000, "query took {} ms", report.elapsed_ms);
        eprintln!(
            "title={:?} visited={} items={} truncated={} by={:?}",
            window.title().unwrap_or_default(),
            report.visited,
            report.items.len(),
            report.truncated,
            report.truncated_by
        );
        for item in report.items.iter().take(5) {
            eprintln!("  {:?} {:?} {:?}", item.role, item.name, item.rect);
        }
    }
}
