# 北极星：中文语义定位与可复现 A/B

## 默认行为

`polaris({"query":"停止按钮为什么不能立即取消当前生成任务"})` 会按自然语言处理，不再以有没有空格判断中文是否为任务。明确的符号（keywords/query）或通过 files 指定的相对路径仍走原有精确检索；单个猜测名字不存在，不会截断一个同时给出了自然语言任务的请求。

未配置语义模型时，启用的是改进的词法／双语概念／结构检索，输出会明确标记 `backend: lexical`。**这不是模型语义能力，也不会自动下载模型或上传代码。** 启用下述本地服务，且索引构建完成后，才是 `backend: hybrid` 的真实语义向量检索。

Rust、TypeScript、TSX 和 JavaScript/JSX 的声明由 Tree-sitter 解析；其他已支持语言保留原有扫描回退，不宣称全部语言都有完整语法／类型解析。函数正文、签名、真实注释和标识符词汇参与检索，不生成未经源码证明的功能摘要。字典别名只是检索元数据，不会写入原文件或冒充源码注释。

结果优先返回实现正文和有界的调用／注册线索。仅转发的包装函数可向其唯一源码内被调用实现传递排名。链接仍是源码线索，不是编译器级类型绑定或根因证明。返回前重新校验源文件哈希；修改、删除和同大小同时间戳变更被发现后，至多进行一次有界重召回。无法完整覆盖时会返回 PARTIAL 和 next_reads，不把截断正文当成完整实现。

## 启用真实本地模型

服务使用 Python 3.11；源码检索本身仍是 Rust，不通过 Python 重写检索引擎。模型下载和运行是显式选项。

1. 在独立虚拟环境安装测试过的依赖：

```powershell
python -m venv .polaris-venv
.\.polaris-venv\Scripts\Activate.ps1
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

冻结语料为 `3da28d30f1adfd0da3b993813a47aa3fff20fdeb`。用例/标准答案保存在语料之外；原始精确引擎内容哈希保持不变。先在 8 个开发问题修复，再对 12 个预留问题、4 个精确符号控制、2 个不存在的符号完成验证。预留标签文件可被维护者读取，不将其宣传为保密盲测或多仓库泛化证明。

五组同语料实验分别为旧 query 接口、旧 task 接口、新词法、新真实语义向量、新语义加精排；执行顺序轮换。round 0 单列，round 1/2 作为同问题重复查询的 warm 数据，不能把缓存命中耗时冒充从未查询过的新问题。模型服务实际调用次数、固定模型版本/哈希、环境、逐题返回源码及源码未变校验均保留。

命中要求实际返回指定函数定义及标注的正文语句，文件名、元数据或仅调用该函数不算命中。Top1 只统计主结果；被调用关系中返回核心实现只计正文覆盖，不计首位。当前 support 标注为空，所以 evidenceSufficientProxy 等同核心正文覆盖，**不是完整任务成功率或调用链完整率**。按唯一问题说明分母，不能把重复执行当成额外独立样本。

质量脚本不把退出码为零当作检索成功：检查自然语言首位/核心正文覆盖、预留题表现、精确检索不退化、无答案符号不编造、完整真实语义索引参与和热查询预算；失败时保持草稿。完整结果和未命中问题保存在 PR 的 Actions 产物中。

在首次运行完整预留集之前固定的默认混合检索门槛：开发集 Top1 至少 50%、核心正文至少 87.5%；预留集 Top1 至少 50%、核心正文至少 75%；合计 20 个自然语言任务 Top1 至少 60%、核心正文至少 80%；warm P95 不超过 750ms。精确符号结果不得劣于旧引擎，不存在的精确符号不得返回伪造正文。该门槛是本次合并标准，不是任意用户问题的成功率保证。
