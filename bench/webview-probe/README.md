# WebView2 原生控制验证（Windows）

独立 Tauri 程序，在窗口右侧创建真正的 WebView2 子视图。通过
`with_webview → CoreWebView2 → CallDevToolsProtocolMethod` 获取可访问性树、
截图、发送鼠标与文字输入。不引入或调用 Playwright，不开放远程调试端口。
测试命令仅由父进程通过 stdin 传入，网页没有 Tauri capability。
跨域独立进程 iframe 使用 `Target.attachToTarget` 与 WebView2 原生
`ICoreWebView2_11::CallDevToolsProtocolMethodForSession` 读取。

## 运行

在仓库根目录执行（需要 Rust、Node.js 和 WebView2 Runtime）：

```powershell
cargo build --manifest-path bench/webview-probe/Cargo.toml
node bench/webview-probe/check.mjs
```

运行时短暂出现独立验证窗口，完成后自动关闭。使用独立临时浏览器配置目录，
不读取用户日常浏览器登录状态。构建目录独立，避免覆盖正在运行的 Nova 的 DLL。

默认输出到 `src-tauri/target/webview-probe-results/`：

- `report.json`：每项检查结果、耗时及观察数据体积。
- `page.png`：右侧 WebView 原生截图。
- `accessibility.json`、`observation.json`：原始 AX 树与简短结构观察。

可传入自定义输出目录：`node bench/webview-probe/check.mjs <目录>`。

## 验证边界

本程序验证真实 WebView 控制能力，使用本地可重复页面和确定性的测试定位，
没有调用小模型，不代表小模型定位准确率或真实站点端到端成功率。
中文验证是 CDP 原生文字插入，不代表人工中文输入法候选窗口已验证。
结构摘要是单页实验，不是生产级元素引用系统，也未实现遮挡检查、引用失效、
虚拟列表探索、弹窗管理或自动权限决策。

后续小模型实验应让模型只看到目标与观察结果，禁止提供测试选择器；
由验证端独立检查结果，记录误点率、任务成功率、模型 token 与端到端延迟。
先用相同任务比较结构观察和结构加截图两种输入，再决定模型及路由策略。

## 本机验证结果（2026-09-16）

8 项检查通过：右侧子视图位置、AX 树中的同名按钮与中文标签、原生中文输入与
区域内点击及延迟结果、开放 Shadow DOM、跨域 iframe、屏幕外目标、原生截图、
精简结构观察。原生鼠标事件的 `isTrusted` 为 true。

本次简单本地页面：20 次结构观察的 P50 为 0.55 ms，P95 为 0.76 ms；
截图约 83 ms。结构摘要 310 字节，页面 HTML 1269 字节。
该摘要只覆盖顶层候选元素，不含 iframe/Shadow DOM 的完整信息，字节数不是
模型 token 数；这些数字不包含推理和网站网络等待，也不能外推到复杂网站。

实际发现并修正：顶层 frame 列表不含独立进程跨域 iframe；滚动后立即按新 DOM
坐标发送输入会出现命中不同步。验证程序在滚动后等待两次动画帧再测量和点击。
生产执行器仍需补充遮挡命中检查和操作结果验证，不能认为两帧对所有网站都足够。

本机 Windows GNU 链接器报告 multiple non-default manifests 警告，构建及运行成功；
本次测试缩放为 100%，尚未覆盖高 DPI、多屏、人工 IME、登录和弹窗。
