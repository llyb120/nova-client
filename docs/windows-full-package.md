# Windows 全环境 ZIP

Windows 上原有 `npm run package` 默认生成两个文件，无需修改 workflow：

- `release/nova-{ver}.zip`：只有 `Nova.exe`，保留现有普通更新包格式。
- `release/nova-full-{ver}.zip`：`Nova.exe`、`runtimes/node/`、`runtimes/python/`。

打包脚本原有版本递增规则不变；本次实现不修改项目版本。

## 来源与校验

不复制开发机的 Node/Python。固定下载官方 Node **22.14.0** Windows ZIP（包含 npm/npx），以及 Python Software Foundation 发布的 NuGet **3.13.2** 完整包：x64 使用 `python`，arm64 使用 `pythonarm64`。来源分别为 `nodejs.org/dist` 和 `api.nuget.org/v3-flatcontainer`；这不是缺少正常 pip 支持的 embeddable 包。

辅助脚本固定四个 SHA-256：Node 来自对应版本官方 `SHASUMS256.txt`，Python 来自下载的官方 NuGet 文件。下载使用 HTTPS、超时和大小上限；哈希不匹配立即失败，不引入缓存、镜像、重试或第三方下载依赖。升级运行时需一起更新版本、来源哈希并重新验证。

按 `Nova.exe` 的 PE 架构选择 x64/arm64，检查运行时关键文件、ensurepip 内置 wheel，以及全部 EXE/DLL/PYD 的 PE 架构和文件完整性。不支持的架构、来源不可用或错误架构均明确失败，绝不回退到 x64 或开发机安装。解包拒绝路径穿越、链接、Windows 特殊路径和大小写重复文件，并限制展开大小。

全包必须在能够运行目标架构程序的 Windows 构建机打包。脚本执行内置 `python.exe -I -m ensurepip --upgrade --default-pip`（使用随包 wheel，不访问 PyPI），复制 `python.exe` 为 `python3.exe`，再将 ensurepip 生成的包含临时绝对路径的 pip EXE 启动器换成相对路径 CMD 启动器。随后移动整个运行时目录，在包含空格的新路径下实际运行：

- 内置 `node.exe --version`、`npm.cmd --version`
- 内置 `python.exe -I -m pip --version`、`python3.exe -I --version`
- 内置 `pip.cmd --version`

这些命令失败或 DLL 缺失导致无法启动时，打包失败。全包在临时目录完成并检查 ZIP CRC 后才替换目标文件；失败时普通包可能已经生成，已有旧全包不会更新，勿将旧包当作本次结果。

## 使用与限制

**必须将 ZIP 完整解压到可写目录后使用**，不能只取出 EXE 或直接在 ZIP 内运行。保留以下相对布局：

```text
Nova.exe
runtimes/
  node/       # node.exe、npm.cmd、npx.cmd、node_modules/ 等
  python/     # python.exe、python3.exe、DLL、Lib/、Scripts/ 等
```

应用在启动和刷新环境时，把上述 Node、Python 和 Python/Scripts 目录前置到自身 PATH，子进程默认继承；不修改系统 PATH。用户显式指定解释器绝对路径、自定义 MCP 环境或 shell profile 改写 PATH 时，仍尊重这些配置。独立终端如需使用，可指定完整路径，例如 PowerShell：

```powershell
& .\runtimes\node\node.exe --version
& .\runtimes\node\npm.cmd --version
& .\runtimes\python\python3.exe --version
& .\runtimes\python\python.exe -m pip --version
& .\runtimes\python\Scripts\pip.cmd --version
```

优先使用 `python.exe -m pip`；`pip`/`pip3`/`pip3.13` 由相对路径 CMD 包装器提供，不提供 `pip.exe`。npm/npx 和 pip 安装第三方包仍可能需要网络；原生扩展可能需要匹配架构的 wheel、额外 DLL 或编译工具，全包不附带这些开发工具。不要通过 pip 自升级覆盖可移动包装器；pip 安装的第三方命令启动器、虚拟环境可能记录绝对路径，移动目录后需重建。应用内 PATH 不代表外部已打开终端的 PATH 会改变。

**内置运行时随全包手动更新**，不会自动跟随 Node/Python 上游版本升级。应用自动更新继续精确选择普通 `nova-{ver}.zip`，替换 EXE 而不清空安装目录，因此保留已有 runtimes，不更新其中的 Node/Python（已静态核对更新器，尚未 Windows 实机验证）。不要把全环境 ZIP 当作现有自动更新协议的普通 ZIP 替代品。

## 离线测试

```sh
python3 -m unittest discover -s scripts -p 'test_package_full.py'
```

测试使用合成 ZIP/PE 和模拟下载/子进程，覆盖两种架构、错误哈希、下载失败、安全解包、缺文件/错架构、python3/pip 准备及失败时不覆盖旧全包。离线测试不替代实际 Windows 执行验证；后者每次 Windows 全包打包时强制执行。
