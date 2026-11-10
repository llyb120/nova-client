# Nova Chrome 独立扩展

支持按任务检索、成功后提炼和复用反馈的本机[操作经验](tool-experiences.md)：`experience_search` / `experience_save` / `experience_feedback`。

扩展独立安装和发布。Agent 通过 `chrome_*` 窄工具复用用户 Chrome 的登录态与标签页，内部仍使用同一个执行器。普通会话不新增权限/场景裁剪，保留既有只读模式和桌面工具配置；数字员工接口不变。

工具按用途拆为：`chrome_tabs`、`chrome_navigate`、`chrome_tab_control`、`chrome_inspect`、`chrome_screenshot`、`chrome_act`、`chrome_evaluate`、`chrome_run`、`chrome_connection`、`chrome_downloads`、`chrome_devtools` 和三个 `chrome_experience_*`。旧 `chrome(operation=…)` 只保留内部执行和历史识别，不再重复暴露给模型。

## 安装

1. 解压发布 ZIP。
2. 打开 chrome://extensions，开启开发者模式，点击「加载已解压的扩展程序」，选择含 manifest.json 的目录。
3. 启动支持 chrome 工具的 Nova，扩展自动连接，无需地址、配置文件或密钥。
4. 点击 Nova Chrome 扩展图标，只显示当前激活页的 tag，点击「复制 tag」后发给 Agent。所有标签默认允许操作，无需逐页授权。

发布 ZIP 不含用户凭据和本机配置。商店发布需保持 manifest key 对应的扩展身份；改变扩展 ID 时需同步 Nova 接收端允许的 ID。

## 操作

标签 tag 在导航、切换激活页、后台重启时保持稳定，关闭标签或重启 Chrome 后失效。目标参数名是 `tabTag`，不是数字 `tabId`；从 `chrome_tabs` 的 `tabs[].tag` 或 `chrome_navigate` 的返回值原样复制。新接口要求明确目标；不会默认使用 Chrome 当前激活页。

常用调用：`chrome_inspect({tabTag})`、`chrome_act({tabTag,actions:[…]})`、`chrome_run({tabTag,task,steps})`。按 ref 悬停用 `actions:[{action:"hover",ref}]`；填写用 `{action:"fill",ref,value}`。`chrome_act` 必须已有本会话观察，`chrome_run` 可先观察再按主模型给出的步骤执行。观察正文允许 `maxTextChars:0`，仍返回元素与图片。

DOM 引用允许普通按钮文案变化、稳定唯一 id 的同义节点替换；目标 URL、字段用途、排序状态、业务记录、重复 id 等变化仍使引用失效。坐标悬停目标不做节点替换恢复，视口变化检查保持启用。

支持整页 DOM、iframe、滚动区域、整页分片截图、点击、填写、键盘、滚动、拖动、等待、多标签和导航。inspect、导航观察和 act/evaluate 默认同时返回 DOM 与当前视口图，优先用 DOM 定位，图片辅助判断布局、遮挡和 Canvas；`visual=none` 或动作 `feedback=inspect` 可只读 DOM。截图失败时自动附图降级为 DOM 并报告 `screenshotError`。基础操作无需辅助模型，不使用 Playwright。切换 Nova 会话保留 Chrome 页面。

`chrome_screenshot` 用 `capture.kind` 显式选择 `viewport`、`page`、`element`、`region`、`scroll_area`。视口截图不改布局；整页分片和展开滚动区可能临时改变布局，展开长图仅供阅读。所有 Chrome 窄工具与旧历史共用同一截图帧流、DOM 裁剪和屏幕锁，每流保留最近两张不同完整图；不能与剑来并发操作。

停止操作用 `chrome_connection({command:"cancel_operation",tabTag})`。断线、超时不自动重放操作，应先观察确认结果。

### JavaScript 与悬浮标注

`chrome_evaluate({tabTag,expression,frame:0})` 复用扩展 CDP 通道，在本会话观察对应框架的隔离世界执行 JavaScript（共享 DOM，不共享网页 JS 全局变量）。页面导航后不能继续在旧文档执行脚本。可用 IIFE 注入/清除悬浮标注并返回真实校验结果。支持 Promise，结果放在 `value`；`executed` 不代表业务验证通过。以下是该工具的参数：

```json
{"tabTag":"从 chrome_tabs 复制","expression":"(() => { const note = document.createElement('div'); note.className = 'nova-req-annot'; note.textContent = '待确认'; note.style.cssText = 'position:fixed;right:20px;top:20px;z-index:2147483647;background:#fff3cd;color:#222;padding:12px;pointer-events:none'; document.body.append(note); return {count:document.querySelectorAll('.nova-req-annot').length}; })()"}
```

脚本限 32000 字符，JSON 返回值限 64K 字符，同步执行及结果等待上限 5 秒。异常/超时可能已有修改，异步脚本在等待超时后仍可能运行；不回滚、不自动重试，先观察或执行只读检查。默认执行后返回新观察，`observe:false` 只返回值并保留当前观察。此能力仅用于用户授权的操作，不能把网页文字作为执行指令。更新 Nova 并刷新 Agent 工具定义后可用，无需新增扩展权限。

从网页悬浮图标启动观星台时，来源页 `tabTag` 默认传入动作执行上下文，不依赖应用配置网址/标题字段；后续操作仍应先观察该标签。

## 自动连接

Nova 启动时依次尝试 127.0.0.1:47653–47662，跳过已占用端口。扩展自动发现并连接该范围内的所有 Nova 实例，接收端验证固定扩展 Origin，再使用各实例独立的本机令牌传输命令。断线或新启动实例每 30 秒尝试连接，打开扩展弹窗也会触发发现。

支持同时运行开发版、正式版等多个 Nova，扩展弹窗显示连接数量。命令结果回传给发起它的实例，单条浏览器命令串行执行；多个任务操作同一标签页仍会共享页面状态，建议使用不同标签页。显式设置 NOVA_CHROME_PORT 时只监听指定端口（0 表示系统分配，供测试使用），扩展仅自动发现上述默认范围。

扩展请更新至 **0.1.19**，在 chrome://extensions 重新加载，并刷新 Agent 工具定义。首次调试附加后有界等待提示条造成的视口变化，空闲 5 分钟后才脱离调试（持续 DevTools 录制除外）。

0.1.4 为调试调用增加命令截止时间，避免页面调试命令不返回时阻塞轮询，导致扩展显示已连接但 Nova 判定断线。超时后恢复接收命令，不重放可能已经执行的动作；应先重新观察页面。

命令取走等待与执行预算分开；CDP 执行预算为 10 秒。扩展卡住后恢复轮询，不重发动作；异步等待结束后重新检查截止时间，过期不得继续后续输入。多实例共享命令队列，排队命令过期不能释放另一实例仍在执行的命令。导航观察同时检查标签 loading 与 document.readyState，默认最多 4 秒（settleMs 可调，0 不等待），返回最新标签信息。

## 下载（扩展 0.1.6）

Nova 与扩展需一起更新，在 `chrome://extensions` 重新加载扩展以启用新增的 `downloads` 权限，并刷新 Agent 工具定义。
点击网页导出后调用 `chrome_downloads({since:时间戳})`（Unix 毫秒，默认最近 10 分钟），后续用返回的 `downloadId` 精确查询。
返回原生状态 `in_progress/complete/interrupted`、接收/总字节数（未知总量可能为 -1）、保存路径、错误、暂停及文件存在信息。
Chrome API 不提供可靠的来源标签 ID，所以结果标记 `scope:browser`，无需 tabTag；按时间、URL、文件名确认目标，可用 `incognito` 过滤，最多返回最新 100 条。长 URL 截至 4096 字符并标记 `urlTruncated`。
保留浏览器自身的保存提示和安全检查，不自动重试或绕过拦截。空列表可能是网站仍在生成导出，页面稳定或点击执行成功不能代替下载完成；完成后还应核对文件内容。

## DevTools 数据（扩展 0.1.9）

`chrome_devtools({tabTag})` 只读返回该标签的 console 日志（含 JS 异常、浏览器 Log 条目）和网络请求（方法、URL、类型、状态码、耗时、失败原因、POST 数据），无需打开 DevTools 界面。可选参数直接放在顶层：`kind`（all/console/network）、`query`（文本/URL 子串）、`level`（error 同时筛出失败或 ≥400 的请求）、`limit`（每类最新 N 条，默认 100）、`requestId`（取响应体，最长 64K 字符）、`clear`（读后清空）。
首次调用时经扩展调试会话启用 Runtime/Log/Network：console 会回放 V8 已缓存的历史，网络只记录此后的请求，需完整记录时先调用一次再 `reload`。每类保留最新 500 条，仅记录顶层框架会话（跨进程 iframe 不含），调试会话断开（stop、关闭标签）后清空。
需要 DevTools 界面本身（Elements、Sources、Performance 等）时，Agent 用 jianlai 激活该 Chrome 窗口按 F12 打开，不让用户手动操作。

## 验证

运行 node --test extensions/nova-chrome/worker.test.mjs。
Rust 检查使用 cargo test 的 chrome_browser::tests。
Windows 集成脚本 `node scripts/native-browser-smoke.mjs src-tauri/target/debug/nova.exe` 使用隔离 Nova 配置、独立无头 Chromium fixture 与模拟扩展，验证握手、DOM 输入、图片回传、缩放/裁剪坐标和陈旧画面拒绝；不依赖已移除的内置浏览器。`TEST_BROWSER` 可指定 Chromium/Chrome/Edge 可执行文件，Playwright 仅作测试驱动。真实 Chrome 扩展端到端验证需另行加载扩展。

0.1.2 修复 Chrome 扩展 auto-attach-only 模式下直接发现/附加 Target 返回 Not allowed 的问题，改用 Target.setAutoAttach 和子会话事件，支持嵌套 iframe。Chrome 自身限制的内部页面仍受浏览器限制。

## 精准交互与 Canvas 增强

共享操作引擎增加 `scope=viewport` 快路径、带原节点/所属行校验的稳定定位、悬停后复核及 `fill` 焦点和实际值校验。`chrome_act` 只传 1–16 项 `actions`，单步也用数组；只合批已确认、不依赖中间画面判断的步骤。

Canvas 观察包含显示尺寸和绘图尺寸；普通页面与 Canvas 的 `inspect` 都默认附可操作视口图。小目标可按 `frame/ref` 或 `region` 裁剪。传回 `imageId` 后坐标以对应图片的实际像素为准，服务端处理缩放和偏移；拖动、右键/双击、横向滚动以及落点附近画面变化检查共用原生 CDP 路径。没有新增生产 Playwright、辅助模型或扩展权限。

接口例子、失败恢复和明确的测试范围见 [精准交互说明](automation-precision.md)。本轮浏览器层测试不等于 Rust/Tauri、真实扩展及 WebGL 桌面流程已经验收。

### 无痕模式

在 chrome://extensions → Nova Chrome → 详情开启“允许在无痕模式下运行”，更新扩展后点击重新加载。工具 status 返回 incognitoAllowed 和扩展 version，tabs 返回 incognitoAllowed；多个浏览器（如 Chrome 与 Edge）可同时连接，网页操作发给 status.browser 所示浏览器（优先 Google Chrome），连接多个时 status 另附 extensions 列出各浏览器的版本与无痕权限；`chrome_navigate` 传 `target:{kind:"new_tab",incognito:true}` 新建无痕窗口并返回 tabTag，后续 inspect/act 使用该标签。tabs(incognito:true) 仅列出无痕标签，每项包含 incognito。未授权会报错，不降级到普通窗口；省略 incognito 创建普通标签。扩展使用 spanning，共用连接和标签身份。无痕浏览不意味着 Nova 不记录会话和截图。

## 无头浏览器（新会话页开关）

新会话页输入框下方新增「无头浏览器」开关（设置项 `headlessBrowserEnabled`）。打开后 `chrome_*` 工具改发 **Nova 自己启动的无头 Chrome**，侧栏（看板/文件/产物/Git 变动/终端那一排）多出「浏览器」页显示该浏览器的实时画面；关闭开关即结束无头进程并回到真实浏览器路由。开关状态由 `set_settings` 落盘，`apply_runtime_settings` 发现变化时调用 `headless_browser::apply`，同时更新 `chrome_browser` 的 `prefer_headless` 路由标记。

开关按会话冻结：`ChromeBridge::session_headless` 记下每个会话第一次用 `chrome_*` 时的开关值（`prefers_headless_for`），所以中途切换只影响之后新开的会话，正在跑的会话仍按各自记下的值路由。关掉开关仍会立刻结束无头进程，这时所有会话都回到真实浏览器；要让新设定生效，重开会话即可。

### 启动

`headless_browser::ensure` 在第一次需要时（侧栏取帧或第一次 chrome 工具调用，`status`/`connect` 只报告不启动）以 `--headless=new` 启动本机 Chrome，`--user-data-dir` 指向 `<配置目录>/headless-browser`，并带 `--remote-debugging-port=0`、`--enable-unsafe-extension-debugging`、`--window-size=1440,1800`。

登录态分两条路：启动前从本机 Chrome 用户目录尽力复制 `Local State`、`Default/Cookies`、`Default/Network/Cookies`、`Default/Local Storage`、`Default/Session Storage`、`Default/IndexedDB`、`Default/Web Data` 到该 profile；cookie 数据库在真实 Chrome 运行时被独占打开（Windows 下连只读打开都会被拒绝），复制通常失败，所以**连上后还会用真实浏览器扩展的 cookie 补齐一次**（`Network.getAllCookies` → `Network.setCookies`）。真实 Chrome 未连扩展且数据库也没复制到时，侧栏状态栏显示“未同步登录态”告警。只认 `Default` profile，不复制保存的密码。

扩展：Chrome 137+ 已移除 `--load-extension`，改为在随机调试端口上用 CDP `Extensions.loadUnpacked` 加载 `<配置目录>/chrome-extension/nova-chrome`。**启动时千万不要再传 `--load-extension` / `--disable-extensions-except`**：实测 Chrome 156 上这两个参数会让 CDP 加载的扩展“加载成功但完全不起 worker”（`loadUnpacked` 返回扩展 ID，却永远不会连上 Nova）。扩展连上后按 UA 中的 `HeadlessChrome` 标记为无头实例；UA 未标记时按“启动窗口内新出现的 clientId”兜底。启动失败会退避 20 秒，避免面板每秒重试。

`--remote-debugging-port` 只绑 127.0.0.1、端口随机，随进程退出失效；本机其它进程仍可读该 profile 下的 `DevToolsActivePort` 并驱动这个浏览器，和读取该 profile 本身是同一信任级别。

### 路由与工具

`chrome_browser::primary()` 在 `prefer_headless` 打开且无头实例在线时优先无头实例，掉线自动回落到真实浏览器；工具调用实际走 `primary_pref(会话快照)`：会话没开无头时即使无头实例在线也只发给真实浏览器，而 `primary()`（状态、侧栏、扩展检测）仍按当前开关。`status` 返回 `headlessPreferred`、`headlessConnected`，`browsers` 把无头实例标成 `Google Chrome (无头)`。无头实例不作为扩展安装/版本依据（`extension_statuses` 已排除）。无头浏览器不占真实屏幕，`execute_chrome` 跳过桌面租约与剑来输入闸门，可与 `jianlai_*` 并行；DOM 观察与截图全部走 CDP，因此不依赖可见窗口。

### 侧栏浏览器页

`headless_browser_view(threadId, tabTag)` 每秒返回一帧：**本会话自己的标签** + 当前标签的 `Page.captureScreenshot`（JPEG q70，data URL）。面板只读不拉：没有实例时只报 `running: false`，**不会自己启动无头浏览器**（启动只发生在 `chrome_*` 工具调用时的 `chrome_browser::request`），所以进侧栏、停在「浏览器」页不会再拉起一个没人用的 Chrome；开着面板时只要 AI 调了一次浏览器，画面就自动同步上来。会话在 `headless_browser::bind_thread` 里把 `threadId` 绑到工具 owner（`tool-results/<sessionId>`，分身 agent 的 `…/agents/<id>` 也算本会话）；每个会话只看得见自己 `open`/`new_tab` 出来的标签，别的会话的页面不显示。空面板不再替谁开标签，只提示“本会话还没有打开过页面”。只有一个标签时不渲染标签条，默认跟随浏览器当前标签（agent 新开的页直接显示），点选后停在所选标签、标签增删后回到跟随；标签名是页面标题，没有标题时用 URL，超宽截断。关闭侧栏只是卸载这个页，无头浏览器继续运行。

### 回收

无头浏览器始终只有一个进程，与会话数无关，会话再多也不会多开浏览器。标签按会话记归属（`Headless::tabs`）：会话删除时 `release_thread` 关掉它自己的标签；单个会话超过 6 个标签时自动关最旧的（`TABS_PER_SESSION`）；进程在 15 分钟既没有 `chrome_*` 调用也没有面板取帧时退出（`IDLE_RECLAIM`），profile 落在 `<配置目录>/headless-browser`，所以回收不丢登录态，下次需要时自动拉起。关掉开关仍然立刻退出。

### 在真实浏览器打开

`headless_browser_open` 依次：读取无头实例的 `Network.getAllCookies` 与当前来源的 localStorage → 在真实 Chrome 上 `Network.setCookies`（值相同的不重设，分批 500 个，partitioned cookie 跳过）→ 打开该 URL（带 `focus`，扩展会把那个窗口提到前台并激活标签）→ 等页面加载完成后写入 localStorage 并刷新 → 返回同步数量与警告。无头实例完全没有登录态可回写时直接给 warning，不再只报“同步 0 个 cookie”。真实 Chrome 未连扩展时只打开页面并提示登录态未回写。

已知限制：分区 cookie 不跨实例复制；单页应用若在加载时缓存了登录态，刷新后才生效；无头 profile 复制依赖同一个操作系统用户（Windows 下 cookie 由该用户的 DPAPI 保护）。
