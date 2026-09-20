# Nova Chrome 独立扩展

支持按任务检索、成功后提炼和复用反馈的本机[操作经验](tool-experiences.md)：`experience_search` / `experience_save` / `experience_feedback`。

扩展独立安装和发布，右侧浏览器始终是 WebView。Agent 使用 webview 操作内置页面，使用 chrome 操作 Chrome，两者可同时使用。

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

支持同时运行开发版、正式版等多个 Nova，扩展弹窗显示连接数量。命令结果回传给发起它的实例，单条浏览器命令串行执行；多个任务操作同一标签页仍会共享页面状态，建议使用不同标签页。WebView 不依赖这些端口。显式设置 NOVA_CHROME_PORT 时只监听指定端口（0 表示系统分配，供测试使用），扩展仅自动发现上述默认范围。

多实例连接需要 Nova 和 Chrome 扩展同步更新，已安装的扩展请更新至 0.1.4，并在 chrome://extensions 重新加载。

0.1.4 为调试调用增加命令截止时间，避免页面调试命令不返回时阻塞轮询，导致扩展显示已连接但 Nova 判定断线。超时后恢复接收命令，不重放可能已经执行的动作；应先重新观察页面。

## 验证

运行 node --test extensions/nova-chrome/worker.test.mjs scripts/nova-tools-mcp.test.mjs。
Rust 检查使用 cargo test 的 chrome_browser::tests。
集成脚本 node scripts/native-browser-smoke.mjs 使用独立 Nova/WebView2 与模拟扩展，验证握手和共享操作引擎。真实 Chrome 端到端验证需加载扩展后进行。

0.1.2 修复 Chrome 扩展 auto-attach-only 模式下直接发现/附加 Target 返回 Not allowed 的问题，改用 Target.setAutoAttach 和子会话事件，支持嵌套 iframe。Chrome 自身限制的内部页面仍受浏览器限制。

## 精准交互与 Canvas 增强

共享操作引擎增加 `scope=viewport` 快路径、带原节点/所属行校验的稳定定位、悬停后复核及 `fill` 焦点和实际值校验。`act` 可传互斥的 `action` 或 1–16 项 `actions`；只合批已确认、不依赖中间画面判断的步骤。

Canvas 观察包含显示尺寸和绘图尺寸；大面积 Canvas 的 `inspect` 默认自动附可操作视口图。小目标可按 `frame/ref` 或 `region` 裁剪。传回 `imageId` 后坐标以对应 PNG 的实际像素为准，服务端处理缩放和偏移；拖动、右键/双击、横向滚动以及落点附近画面变化检查共用原生 CDP 路径。没有新增生产 Playwright、辅助模型或扩展权限。

接口例子、失败恢复和明确的测试范围见 [精准交互说明](automation-precision.md)。本轮浏览器层测试不等于 Rust/Tauri、真实扩展及 WebGL 桌面流程已经验收。

### 无痕模式

在 chrome://extensions → Nova Chrome → 详情开启“允许在无痕模式下运行”，更新扩展后点击重新加载。工具 status/tabs 返回 incognitoAllowed；open/new_tab 传 incognito:true 新建无痕窗口并返回 tabTag，后续 inspect/act 使用该标签。tabs(incognito:true) 仅列出无痕标签，每项包含 incognito。未授权会报错，不降级到普通窗口；省略 incognito 创建普通标签。扩展使用 spanning，共用连接和标签身份。无痕浏览不意味着 Nova 不记录会话和截图。
