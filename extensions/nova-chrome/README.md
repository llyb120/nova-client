# Nova Chrome 独立扩展

扩展独立安装和发布。Agent 的浏览器网页操作统一使用 chrome，复用用户 Chrome 的登录态与标签页。

## 安装

1. 解压发布 ZIP。
2. 打开 chrome://extensions，开启开发者模式，点击「加载已解压的扩展程序」，选择含 manifest.json 的目录。
3. 启动支持 chrome 工具的 Nova，扩展自动连接，无需地址、配置文件或密钥。
4. 点击 Nova Chrome 扩展图标，只显示当前激活页的 tag，点击「复制 tag」后发给 Agent。所有标签默认允许操作，无需逐页授权。

发布 ZIP 不含用户凭据和本机配置。商店发布需保持 manifest key 对应的扩展身份；改变扩展 ID 时需同步 Nova 接收端允许的 ID。

## 操作

标签 tag 在导航、切换激活页、后台重启时保持稳定，关闭标签或重启 Chrome 后失效。操作必须明确指定 tabTag。

支持整页 DOM、iframe、滚动区域、整页分片截图、点击、填写、键盘、滚动、拖动、等待、多标签和导航。act 返回最新观察供验证并继续。无辅助模型，不使用 Playwright。切换 Nova 会话保留 Chrome 页面。

需要停止操作时使用 chrome 的 stop 操作。断线、超时不自动重放操作，应先观察确认结果。

## 自动连接

Nova 启动时依次尝试 127.0.0.1:47653–47662，跳过已占用端口。扩展自动发现并连接该范围内的所有 Nova 实例，接收端验证固定扩展 Origin，再使用各实例独立的本机令牌传输命令。断线或新启动实例每 30 秒尝试连接，打开扩展弹窗也会触发发现。

支持同时运行开发版、正式版等多个 Nova，扩展弹窗显示连接数量。命令结果回传给发起它的实例，单条浏览器命令串行执行；多个任务操作同一标签页仍会共享页面状态，建议使用不同标签页。显式设置 NOVA_CHROME_PORT 时只监听指定端口（0 表示系统分配，供测试使用），扩展仅自动发现上述默认范围。

Nova 和 Chrome 扩展需要同步更新，已安装的扩展请更新至 0.1.9，并在 chrome://extensions 重新加载。

0.1.9 新增 `operation=devtools`：经已有调试会话读取标签的 console 日志、JS 异常与网络请求（可按 requestId 取响应体），无需新增权限。网络只记录首次调用之后的请求；需要 DevTools 界面时由 Agent 用 jianlai 按 F12 打开。详见 docs/chrome-browser.md。

0.1.8 支持观星台「网页激活」：已发布应用配置了 browser.match 时，网址匹配的页面右下角挂载可拖动的应用图标（位置会记住），点击后在 Nova 打开该应用，并按配置填入网页网址/标题、执行指定动作。页面网址仅在点击时发送给 Nova。启动来源的稳定 `tabTag`、网址和标题会自动作为执行上下文交给模型，无需配置字段绑定；同次启动后的后续动作继续使用该页，新启动会替换上下文，非网页启动会清除它。旧扩展没有提供 `tabTag` 时不会猜当前激活页，需先查询确认来源标签。此能力需同步更新 Nova 与扩展并重新加载扩展。新增内容脚本需要访问所有 http(s) 网站，更新时 Chrome 会提示新增权限。

0.1.7 截图前自动还原最小化窗口并切到目标标签；此前窗口最小化或标签在后台时截图会一直超时。

0.1.6 新增 downloads 权限。更新 Nova 并刷新 Agent 工具定义后，可用 operation=downloads 查询下载进度、保存路径和失败原因；支持 downloadId 精确查询或 since（Unix 毫秒）过滤，默认最近 10 分钟。记录属于整个浏览器，需按时间、URL、文件名确认目标。只有 state=complete 表示下载完成，页面稳定或点击成功不能代替完成验证。原生保存提示和安全检查保持生效。

0.1.4 为调试调用增加命令截止时间，避免页面调试命令不返回时阻塞轮询，导致扩展显示已连接但 Nova 判定断线。超时后恢复接收命令，不重放可能已经执行的动作；应先重新观察页面。

## 验证

运行 node --test extensions/nova-chrome/worker.test.mjs scripts/nova-tools-mcp.test.mjs。
Rust 检查使用 cargo test 的 chrome_browser::tests。
Windows 集成脚本 `node scripts/native-browser-smoke.mjs src-tauri/target/debug/nova.exe` 使用隔离 Nova 配置、独立无头 Chromium fixture 与模拟扩展，验证握手、DOM 输入、图片回传、缩放/裁剪坐标和陈旧画面拒绝；不依赖已移除的内置浏览器。`TEST_BROWSER` 可指定 Chromium/Chrome/Edge 可执行文件，Playwright 仅作测试驱动。真实 Chrome 扩展端到端验证需另行加载扩展。

0.1.2 修复 Chrome 扩展 auto-attach-only 模式下直接发现/附加 Target 返回 Not allowed 的问题，改用 Target.setAutoAttach 和子会话事件，支持嵌套 iframe。Chrome 自身限制的内部页面仍受浏览器限制。
