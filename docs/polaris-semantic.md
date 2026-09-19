# 北极星：中文语义定位与可复现 A/B

## 默认行为

通用入口使用 `polaris({"task":"停止按钮为什么不能立即取消当前生成任务"})`。MCP/自定义工具也可使用 `query`。完整中文句子按自然语言处理，不再以有没有空格判断中文是否为任务。明确的符号（keywords/query）或通过 files 指定的相对路径仍走原有精确检索；单个猜测名字不存在，不会截断一个同时给出了自然语言任务的请求。

未配置语义模型时，启用的是改进的词法／双语概念／结构检索，输出会明确标记 `backend: lexical`。**这不是模型语义能力，也不会自动下载模型或上传代码。** 启用下述本地服务，且索引构建完成后，才是 `backend: hybrid` 的真实语义向量检索。

Rust、TypeScript、TSX 和 JavaScript/JSX 的声明由 Tree-sitter 解析；其他已支持语言保留原有扫描回退，不宣称全部语言都有完整语法／类型解析。函数正文、签名、真实注释和标识符词汇参与检索，不生成未经源码证明的功能摘要。字典别名只是检索元数据，不会写入原文件或冒充源码注释。

结果优先返回实现正文和有界的调用／注册线索。仅转发的包装函数可向其唯一源码内被调用实现传递排名。链接仍是源码线索，不是编译器级类型绑定或根因证明。返回前重新校验源文件哈希；修改、删除和同大小同时间戳变更被发现后，至多进行一次有界重召回。无法完整覆盖时会返回 PARTIAL 和 next_reads，不把截断正文当成完整实现。

## 启用真实本地模型

服务使用 Python 3.11；源码检索本身仍是 Rust，不通过 Python 重写检索引擎。模型下载和运行是显式选项。

1. 在独立虚拟环境安装测试过的依赖：

```powershell
python -m venv .venv
.\.venv\Scripts\Activate.ps1
python -m pip install torch==2.6.0 --index-url https://download.pytorch.org/whl/cpu
python -m pip install transformers==4.57.6 sentencepiece==0.2.0
```

2. 显式下载固定版本 encoder（只有这一步联网下载公开模型，不发送仓库代码）：

```powershell
python -c "from huggingface_hub import snapshot_download; print(snapshot_download('intfloat/multilingual-e5-small', revision='614241f622f53c4eeff9890bdc4f31cfecc418b3', local_dir='C:/Models/polaris-e5', allow_patterns=['*.json','*.safetensors','*.model','vocab.txt'], ignore_patterns=['onnx/*','openvino/*','*.bin']))"
```

3. 在 PowerShell 中设置同一份配置，再启动服务和 Nova；已运行的 Nova 必须退出并重新启动才能继承环境变量：

```powershell
$env:NOVA_POLARIS_SEMANTIC_URL = 'http://127.0.0.1:48771'
$env:NOVA_POLARIS_SEMANTIC_MODEL = 'e5:614241f622f53c4eeff9890bdc4f31cfecc418b3|meanpool256-v1'
$env:NOVA_POLARIS_SEMANTIC_TOKEN = python -c 'import secrets; print(secrets.token_urlsafe(32))'
$env:NOVA_POLARIS_RERANK = '0'
$worker = Start-Process python -PassThru -ArgumentList @('scripts/polaris-semantic-server.py','--model-path','C:/Models/polaris-e5','--identity',$env:NOVA_POLARIS_SEMANTIC_MODEL,'--port','48771','--threads','2','--max-tokens','256')
# 把下面的路径改成你的 Nova 可执行文件位置。
& '.\src-tauri\target\release\Nova.exe'
```

模型服务器仅监听字面回环地址，校验随机 Bearer token，拒绝带 Origin 的浏览器请求；模型只从本地 safetensors 加载，禁用远程代码和隐式模型下载。没有请求正文／源码日志。服务结束可用 `Stop-Process -Id $worker.Id`，这只结束本次启动的模型进程。

首次查询会开始后台增量建索引，期间明确返回 warming/partial，保留即时词法结果；首次全仓建索引不是几十毫秒操作。A/B 把模型下载、模型加载、首次建索引、首次查询和重复查询分别记录。文件变化仅重建相关单元。默认限制最多两个后台建索引任务，不能据查询热延迟承诺应用冷启动或首次索引耗时。

`NOVA_POLARIS_RERANK=1` 是可选实验性精排，需要服务显式加载 `--rerank-path` 对应模型。当前 CPU 对照显示它可能更慢且未必更准，因此**默认关闭**，不要把“多一个模型”当成质量保证。改变模型、版本、pooling/token 配置时同时改变 model identity，防止旧向量混用。

远程 HTTPS 服务必须额外显式设置 `NOVA_POLARIS_ALLOW_REMOTE=1`；这会向用户配置的服务发送检索文本／代码片段。默认不允许远程，不通过重定向、系统代理或 URL 内凭据绕过限制。不得把服务 token 提交进仓库。

## 预算与边界

语义/词法候选有数量上限，源码索引尊重忽略规则和仓库边界；数据文件不作为普通源码隐式纳入。预览结果默认 600 行、32 KiB，超大函数说明需要进一步读取的区间。显式给出的配置文件可按限定路径检索，不放宽整个目录。精确符号沿用旧引擎，避免明确函数名的请求强制支付模型开销。

词法、语义和静态引用都不是程序执行证明。没有要求模型伪造唯一正确答案；同名函数、动态注册、复杂宏、跨语言调用、未加载的子模块仍可能需要额外上下文。不能承诺任意仓库的任意自然语言问题都一次命中。

## A/B 与质量门禁

```powershell
node --test scripts/polaris-query.test.mjs scripts/nova-tools-mcp.test.mjs
cargo test --release --manifest-path bench/polaris-harness/Cargo.toml -- --test-threads=1
cargo build --release --manifest-path bench/polaris-harness/Cargo.toml
python scripts/polaris-eval.test.py
python scripts/polaris-acceptance.test.py
# --corpus 必须是干净的冻结源码 checkout，不是本 PR 工作目录。
python scripts/polaris-ab.py --binary bench/polaris-harness/target/release/polaris-harness.exe --corpus C:/Tests/nova-polaris-baseline --out polaris-ab-results --models --rounds 3 --split all
python scripts/polaris-acceptance.py polaris-ab-results/report.json
```

冻结语料为 `3da28d30f1adfd0da3b993813a47aa3fff20fdeb`。用例/标准答案保存在语料之外；原始精确引擎内容哈希保持不变。共有 8 个开发问题、12 个回归问题、4 个精确符号控制、2 个不存在的符号。12 个问题仍沿用 JSON 中的 `heldout` 标签，但其失败已用于排查，**不再是未参与调试的独立盲测**。不存在符号用例也不能证明自然语言无答案判断能力。

五组同语料实验分别为旧 query 接口、旧 task 接口、新词法、新真实语义向量、新语义加精排；执行顺序轮换。round 0 单列，round 1/2 作为同问题重复查询的 warm 数据，不能把缓存命中耗时冒充从未查询过的新问题。模型服务实际调用次数、固定模型版本/哈希、环境、逐题返回源码及源码未变校验均保留。

命中要求实际返回指定函数定义及标注的正文语句，文件名、元数据或仅调用该函数不算命中。Top1 只统计主结果；被调用关系中返回核心实现只计正文覆盖，不计首位。当前 support 标注为空，所以 evidenceSufficientProxy 等同核心正文覆盖，**不是完整任务成功率或调用链完整率**。按唯一问题说明分母，不能把重复执行当成额外独立样本。

质量脚本不把退出码为零当作检索成功：检查自然语言首位/核心正文覆盖、回归题表现、精确检索不退化、无答案符号不编造、完整真实语义索引参与和热查询预算；失败时保持草稿。完整结果和未命中问题保存在 PR 的 Actions 产物中。

在首次运行完整预留集之前固定的默认混合检索门槛：开发集 Top1 至少 50%、核心正文至少 87.5%；回归集 Top1 至少 50%、核心正文至少 75%；合计 20 个自然语言任务 Top1 至少 60%、核心正文至少 80%；warm P95 不超过 750ms。精确符号结果不得劣于旧引擎，不存在的精确符号不得返回伪造正文。该门槛是本次合并标准，不是任意用户问题的成功率保证。


## 本轮修复与测量结果

实现提交 `6c10b4dba8ce3a04eb98953ff282e284451ecdc1` 的完整验证：
https://github.com/llyb120/nova-client/actions/runs/35434236538

这是 26 个独立问题、5 个方案、每题 3 次，共 390 次执行。下表仅汇总 20 个自然语言问题，round 0 单列于原始报告，round 1/2 作为重复查询计时。命中数的分母是 20，不把两次重复当成 40 个独立问题。

| 方案 | 首位实现命中 | 核心正文覆盖 | warm 中位 / P95 |
|---|---:|---:|---:|
| 原 query | 0/20 | 1/20 | 8.7 / 26.5 ms |
| 原 task | 0/20 | 1/20 | 20.0 / 26.8 ms |
| 新词法、概念和结构 | 13/20 | 18/20 | 42.7 / 48.7 ms |
| 新词法＋真实语义向量 | 12/20 | 18/20 | 44.8 / 48.9 ms |
| 新语义＋实验精排 | 5/20 | 16/20 | 1043.3 / 1095.9 ms |

不能据此说模型必然提高首位准确率：该数据集上新词法略优于混合检索，精排更慢且更差。因此额外模型仍是可选配置，默认不开精排。原版短耗时主要伴随没有返回标注实现，不能当成等质量提速对照。

混合检索开发集首位 6/8、正文 8/8；回归集首位 6/12、正文 10/12。固定门槛全部通过，没有修改金标或降低阈值。精确符号控制前后均为 3/4，不把“没有退化”说成 100%；两个不存在符号均正确不返回伪造正文。完整输出、分组计数、失败问题和 acceptance.json 保存在本次产物 `polaris-ab-results`。

混合检索本轮未覆盖正文的两题是 `terminal-retain` 和 `terminal-shell`；未排在首位但返回了相关正文的还有 `stop`、`theme-home`、`queue-hold`、`channel`、`restore-window`、`remote-img`。词法方案能覆盖 terminal-shell，但未覆盖 channel，两个通道有所互补。结果是用于代码定位的候选证据，不是已经完成整个修改任务。

本次 CPU 首次完整语义索引（272 个文件、4,810 个实现单元）约 290.4 秒，模型下载约 12.3 秒、模型加载约 3.9 秒；首次词法查询包含索引建立，约 1.33 秒。模型已就绪后的首遍混合查询中位约 56.8 ms，不能与从零建索引混为一谈。warm 查询还会复用查询向量。所有数字只对应这一台测试机、这一份冻结仓库，不是用户电脑的固定延迟保证。

Windows/Linux release 检索模块分别通过 89 项测试（另外 2 项原有实验测试保留 ignored，不计为通过）；Node 参数/MCP 回归 14 项，评测器 4 项、质量门禁 6 项通过。Windows 完整 Tauri 库与前端构建通过，并在原生库内实际执行 36 项语义模块测试。这里没有宣称完成每一种外部 agent 或实际用户项目的桌面端到端验收。

续修包含：合并同函数不同片段的检索票数，保留源码操作/约束校验的分数差距，补足泛用 tab/panel 概念，并根据解析后的 Rust 测试属性及祖先配置作用域隔离测试实现。名字不是 tests 的测试模块也不会当成生产代码；明确查单元测试仍保留这些正文。`cfg` 解析有长度和递归预算。

仅显式设置 `NOVA_POLARIS_TRACE_RANK` 才输出排名诊断（问题、路径、符号、分数，不包含源码正文），A/B 使用它排查；普通用户默认关闭。诊断可能包含用户问题和项目路径，不应随意公开用户真实项目日志。
