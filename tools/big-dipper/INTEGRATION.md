# Big Dipper ↔ polaris 集成协议

两阶段架构：**Big Dipper（北斗七星，big-dipper）负责召回与排序，polaris 负责精确定位与依赖展开**。
两者通过一个 JSON 句柄契约衔接，互不渗透职责。

## 工具边界

| | Big Dipper（本工具） | polaris（北极星，精确查询） |
|---|---|---|
| 输入 | 自然语言 / 残缺符号名 / 错别字 / 中文业务词 | 精确句柄：path+symbol、path+行范围 |
| 输出 | 候选句柄 + 证据 + 置信度（可能有错） | 完整代码单元 + 依赖 + IMPACT（必须确定） |
| 索引 | 符号级词法索引（BM25F+标识符+n-gram），易变 | 精确符号表 + AST，按 snapshot 校验 |
| 禁止行为 | 不做精确解析、不替调用方选定唯一目标 | 不做语义猜测、不回退到模糊检索 |

## 交接流程

```
用户自然语言
    ↓
big-dipper "查询" --handoff --root <repo>
    ↓
{ snapshot, verdict, decision, targets, next }
    ↓ decision 分流（调用方执行）
    ├─ expand        → polaris_exact(targets[0], expand={definitions,directCallers}, budget)
    ├─ disambiguate  → 展示 targets（≤3）给调用方，选定后再调 polaris_exact
    ├─ clarify       → 反问补充 目录/模块/符号类型，重新 big_dipper_search
    └─ none          → 明确报未找到，不猜测
```

## 契约细则

1. **句柄格式**：`{ path, symbol, kind, lines }`。`lines` 仅同进程即时交接有效；
   跨会话必须退回 `path + symbol`，由 polaris 重新定位（行号会漂移）。
2. **snapshot**：git 仓库为 HEAD commit，非 git 目录为 `content:<符号数>` 内容戳。
   polaris 侧应校验 snapshot 不一致时按 `STALE_HANDLE` 处理。
3. **北极星错误语义**：找不到 `NOT_FOUND`、多义 `AMBIGUOUS`（可返回精确消歧选项）、
   过期 `STALE_HANDLE`。调用方收到后决定是否重走模糊层——北极星自身永不回退。
4. **快捷路径**：用户一开始就给出准确 path+symbol 时，跳过 big_dipper_search 直接调 polaris。

## 置信度门控当前阈值（待真实查询集校准）

- `high`：存在精确证据（引号命中 / ident:exact）或 Top1/Top2 margin ≥ 2.0
- `medium`：margin ≥ 1.2 或 Top1 有多通道证据
- `low`：其余有候选情形
- `none`：零候选

## CLI

```sh
big-dipper "鉴权失败后的重试" --handoff --root <repo>   # 交接载荷（推荐）
big-dipper "查询" --json --limit 10                     # 完整候选+证据
big-dipper "查询"                                        # 人类可读
big-dipper --bench --root <repo>                         # 延迟基准
```

## 性能基线（本实现）

- 200 文件 / 7.8k 符号：查询 6～17ms；2799 文件 / 15k 符号：查询 13～35ms
- 索引热缓存 12ms；磁盘缓存按 size+mtime 逐文件失效，遵守 .gitignore
