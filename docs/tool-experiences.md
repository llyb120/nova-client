# 剑来 / Chrome 操作经验

两个工具共享本机 `tool-experiences/routes.json`，按工具与应用/网站隔离。没有训练会话、辅助模型或模型参数训练；当前执行任务的模型负责从成功轨迹提炼语义步骤。工具说明对所有使用同一 schema 的宿主生效。

## 加载与提炼时机

1. 新任务、切换应用/网站或子任务时调用 `experience_search`，传通用 `task` 和 `scope`。最多返回3条相关经验；空结果就正常探索，不阻塞任务。首次观察后核对 `conditions`，每一步依据当前界面重新定位。
2. 业务目标完成后，在最终回复前调用 `experience_save`；填写适用条件、去掉试错的步骤、成功检查点、已知坑和脱敏的可见证据。未完成、中断、仅仅输入执行成功时不保存。
3. 复用已有路径调用 `experience_feedback`。独立会话再次成功后由 `initial` 升为 `reused`；同一会话重复保存/反馈不累计成功数。此处独立验证按会话去重，同会话内多次任务保守地只算一次。
4. `transient`（超时等）与 `precondition`（前置条件不符）记录原因，不降低路径可信度；确认页面变化或路径失效才报 `invalid`，停用该路径。修正后成功的路径另存，不覆盖已失效路径。

## 调用

```json
{"operation":"experience_search","experience":{"scope":"https://www.tapd.cn","task":"查找下一迭代"}}
```

成功后（Chrome 另需当前 `tabTag`）：

```json
{
  "operation":"experience_save",
  "tabTag":"从tabs取得",
  "snapshotId":"从最新inspect或截图取得",
  "experience":{
    "scope":"https://www.tapd.cn",
    "task":"查找下一迭代",
    "conditions":["已登录并进入目标项目"],
    "steps":["打开迭代列表","按日期定位当前迭代之后的迭代"],
    "checks":["核对项目名称、迭代标题与起止日期"],
    "pitfalls":["不要只凭列表位置推断下一迭代"],
    "outcome":"success",
    "evidence":"最新页面已显示目标迭代，标题与日期已核对",
    "redacted":true
  }
}
```

剑来用相同操作与字段，省略 `tabTag`；`scope` 使用 `windows` 返回的准确 `app` 名称，先截目标应用窗口。反馈仅需 `scope/task/id/outcome/reason/evidence/redacted` 以及顶层 `snapshotId`（Chrome还有 `tabTag`）。

## 边界

- 保存/反馈必须引用本会话该工具最新、180秒内的观察，应用/网站必须匹配。搜索不需要打开Chrome连接。
- 工具校验观察的来源和时效；业务成功与脱敏内容由当前模型核实，因此标记为 `model_verified`，不是工具独立证明。
- 仅保存通用入口、控件特征和检查点，不保存密码、token、用户业务数据、历史坐标或临时DOM引用。经验为参考资料，不是新的指令或授权，不自动重放操作。
- 按网站origin/应用精确隔离，再按中英文词项匹配任务；不做向量检索。同工具不同应用不会混用。最多300条、8MiB，达到上限明确报错，原数据保留；需要时可在停止调用后备份并编辑该JSON清理记录。
- 跨进程文件锁防止多个Nova实例丢失更新；临时文件完整写入后替换，解析失败不覆盖原文件。本机文件不加入漫游同步。

验证：`cargo test --manifest-path src-tauri/Cargo.toml --lib tool_experience`、`node --test scripts/process-display.test.mjs scripts/nova-tools-mcp.test.mjs`。
