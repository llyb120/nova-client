# Polaris 精确符号优先实验

采用：保留显式 files 最优先；在同一优先级内让已召回的精确/忽略大小写精确符号先于部分名称匹配单元分配正文名额。复用已有 seed_weight 和稳定排序，无新增检索或源码解析。

## 根因与边界

调试轨迹显示 polaris 的 context.rs 已进入候选，真实定义也已生成单元，但四个其它文件先占满文件名额，定义被结构上限跳过。原选择过程偏好小单元，部分名称匹配的函数因此胜过精确定义。

这是打包顺序修复，不是全局召回修复。构造 8 个干扰文件时发现目标还可能在更早的候选截断中遗漏；本次测试用 4 个干扰文件隔离已确认的打包问题。没有宣称解决所有精确符号漏召回。

## 真实仓库 A/B

沿用 6 个固定查询，每组交错 4 对，共 48 次。排除每组第一对，剩余 3 次取中位数。debug 测试构建；非生产 deadline、非严格冷启动测试。

| 查询 | 旧排序 ms | 精确优先 ms | 目标正文文件排名（旧→新） |
|---|---:|---:|---|
| lyra agent | 1044 | 1176 | 1→1 |
| polaris | 403 | 448 | 未展开→1 |
| subject_match | 121 | 121 | 1→1 |
| read govern | 322 | 274 | 2→2 |
| compact reasonix | 192 | 174 | 1→1 |
| build_system_prompt | 184 | 186 | 1→1 |

目标文件正文覆盖从 5/6 到 6/6。其它五组输出字节数未变；polaris 从 29895 到 29540 字节。延迟有升有降，不能据此承诺搜索提速；主要价值是减少漏掉目标后补读的可能。

文件覆盖只计 ### 正文标题，不证明任务依赖全部覆盖。回归测试同时断言旧路径漏掉真实函数、新路径返回完整函数，预算仍守住硬上限。

## 复现

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib exact_definition_is_not_displaced --offline
cargo test --manifest-path src-tauri/Cargo.toml --lib exact_symbol_repository_ab --offline -- --ignored --nocapture
```

`_legacyUnitOrder` 仅测试构建可切回旧顺序；生产始终使用精确优先。BM25 仍仅为测试实验，未启用。
