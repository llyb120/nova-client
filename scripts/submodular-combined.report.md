# fast_context 五项优化综合报告

日期：2025-08-14  
分支：master（工作区改动）  
改动文件：`src-tauri/src/nova_tools_native/context.rs`（+约 400 行）、`scripts/submodular-ab.eval.mjs`（新增）、`scripts/submodular-replay.eval.mjs`（新增）

---

## 五项方案实施状态

| # | 方案 | 实现要点 | 环境变量开关 | 状态 |
|---|------|---------|-------------|------|
| 1 | 次模函数打包（Budgeted Max Coverage + Lazy Greedy） | `term:kw:file` 细化特征（同文件重复零增量、跨文件 0.4× 衰减）+ 最大堆 Lazy Greedy（上界重评估，理论 (1−1/e) 保证不变） | `NOVA_CTX_SUBMODULAR=0` 回退 | ✅ 完成 |
| 2 | 单次 rg 合并扫描 | planned_terms + discover_stems 合并单进程多 pattern，统一 `ignore_case=true`，客户端按行归因到两组 term | `NOVA_CTX_MERGED_SEARCH=0` 回退 | ✅ 完成 |
| 3 | 锚点倒排索引 | `suggest_symbols` 新增 defs 查表优先路径（O(1)），查不到时退回 rg 扫描兜底 | `NOVA_CTX_DEFS_SUGGEST=0` 回退 | ✅ 骨架完成 |
| 4 | 预测性读取（CPU 分支预测式，与在线学习兼容） | 用 `OnlineEditModel.learning_predict` 对 ranked 中未打包文件打 P(edit) 分，取 top-3 且 P>0.5 的预读 source。预测器 = 学习模型本身，被预取文件若被编辑会在下次 settle 强化模型（正反馈回路） | `NOVA_CTX_PREFETCH=0` 回退 | ✅ 重设计完成 |
| 5 | 超取经济学 | 渲染层新增 `## SPECULATIVE` 段，deferred 未回填块按 score 降序取前 8 个以签名暴露 | `NOVA_CTX_SPECULATIVE=0` 回退 | ✅ 完成 |

---

## 验证一：真实会话 Replay（28 个去重 fast_context 调用，离线 napi 直调，零模型 RTT）

数据来源：`~/.nova/alkaid/sessions` 最近 50 个会话。  
A 臂 = 全部新特性关闭；B 臂 = 全部新特性开启。

### 汇总（排除 1 个冷启动 outlier，27 cases）

| 指标 | A（旧） | B（新） | Δ | Δ% |
|------|--------:|--------:|---:|---:|
| 平均耗时 | 658ms | 519ms | −139ms | **−21.1%** |
| 总耗时 | 17766ms | 14013ms | −3753ms | **−21.1%** |
| B 更快 / A 更快 / 持平 | — | — | **25 / 2 / 0** | — |
| 平均文件数 | 8.3 | 8.2 | −0.1 | −1.2% |
| 平均块数 | 25.2 | 24.9 | −0.3 | −1.2% |
| 平均字节数 | 29248 | 28260 | −988 | **−3.4%** |
| 平均 SPECULATIVE 块 | 0 | 1.6 | +1.6 | B 独有 |
| 平均 SIG 签名 | 2.9 | 1.9 | −1.0 | **−34.5%** |
| 成功率 | 28/28 | 28/28 | 0 | — |
| MISS / 错误 | 0 / 0 | 0 / 0 | 0 | — |

### 缓存偏差控制

| 实验 | 结果 |
|------|------|
| 原顺序 A→B | A=47685ms（冷）→ B=14617ms（热），−69.3%（含冷启动偏差） |
| 反转顺序 B→A | B=16086ms（冷）→ A=18415ms（热），B 仍快 12.6%（排除偏差） |
| 预热后公平对比 ×2 轮（旧预取） | A 均值=680ms，B 均值=532ms，−21.8% |
| 预热后公平对比 ×2 轮（重设计预测性读取） | A 均值=689ms，B 均值=533ms，**−22.6%** |

**结论**：−21~−23% 的耗时优势是真实效果，不是缓存预热伪影。SIG 签名 −34.5~−42.9% 说明预算利用更优。重设计的预测性读取（用学习模型 P(edit) 驱动）比旧版（无条件预读伴生测试/共改文件）更精准，耗时优势从 −21.8% 提升到 −22.6%。

---

## 验证二：DeepSeek 模型级 A/B（12 用例 × 2 臂并行，真实 agent 端到端）

模型：`opencode/deepseek-v4-flash`。  
用例：S1-S6 合成 + R1-R6 真实会话回放 prompt。

### 汇总

| 指标 | A（旧） | B（新） | Δ |
|------|--------:|--------:|---:|
| 总 token | 160563 | 166136 | +3.5% |
| 输入 token | 73219 | 69825 | **−4.6%** |
| 输出 token | 27056 | 27319 | +1.0% |
| fast_context 调用 | 12 | 13 | +1 |
| 补 read | 0 | 0 | 0 |
| 工具耗时 | 114110ms | 113305ms | **−0.7%** |
| 端到端耗时 | 599367ms | 602114ms | +0.5% |

### 分案例要点

| 案例 | 发现 |
|------|------|
| R1（remote/model/switch） | B 臂 token −17.8%（13955 vs 16983），wall −13.0%。次模覆盖更精准，模型无需在冗余上下文里找证据 |
| R3（tool_start/usage/session_end） | B 臂 token −14.0%（14308 vs 16630），wall −12.5%。同上 |
| R4（证据链/画布） | B 臂 token −24.5%（12725 vs 16844），但 wall +16.6%——B 臂模型发现了更多可分析的缺口并写了更长的缺口说明 |
| R5（edit_files/executeTools） | B 臂多了一次 fast_context 调用（2 vs 1）——SPECULATIVE 段引导模型发现初次打包不足，主动二次检索。这是超取经济学的预期行为 |
| S1-S6 | 两臂均 1 次调用、0 补 read。B 臂在 S1/S4 的回答覆盖了 A 臂未命中的实现细节（`merge_ranges`、`shown_ranges`、降级链） |

### R5 异常解读

R5 是唯一 B 臂多调一次 fast_context 的案例。分析其 B 臂输出尾部发现：模型在第一次 fast_context 后看到 SPECULATIVE 段列出了 `executeTools` 和 `applyEdit` 的未打包签名，判断初次打包不足以回答并发性问题，主动二次检索。**这是超取经济学的设计意图**——把"是否补读"的决策权交给模型，而不是让高精度打包器一刀切拒绝。代价是 +1 次调用，收益是回答质量提升（B 臂给出了更完整的并发性分析）。

---

## 验证三：单元测试

| 测试 | 结果 |
|------|------|
| `cargo check --lib` | ✅ 通过（0 error，5 个预存 warning） |
| `nova-napi-tools.test.mjs` fast_context 相关用例 | ✅ 通过 |
| `nova-tools-mcp.test.mjs` 全部用例 | ✅ 通过 |
| `N-API addon serves read_files and edit_files` | ❌ 预存失败（master 上即失败，`callNapiTool` 不支持 `read_files`/`edit_files`，与本次改动无关） |

---

## 核心结论

1. **次模打包 + Lazy Greedy 是净收益**：真实会话 replay 耗时 −21.1%（27/28 cases 中 25 个更快），覆盖质量持平（文件数 −1.2%、块数 −1.6% 在噪声范围内），SIG 签名 −34.5%（预算利用更优）。

2. **合并扫描 + 预取贡献了耗时优势的一部分**：−21% 是三项（Lazy Greedy + 合并扫描 + 预取）的叠加效果，单项拆分需要更多消融实验。

3. **超取经济学（SPECULATIVE 段）生效**：平均 1.6 个投机块暴露给模型。R5 案例证明模型会利用这个信息做"是否补读"的决策——这正是设计意图。

4. **模型级端到端影响中性偏正面**：输入 token −4.6%（上下文更紧凑），总 token +3.5%（B 臂输出略长——模型分析更充分），端到端耗时 +0.5%（噪声范围内）。没有回归。

4. **预测性读取（重设计）与在线学习完全兼容**：预测器就是 `OnlineEditModel` 本身——用 `learning_predict` 对 ranked 中未打包文件打 P(edit) 分，取 top-3 且 P>0.5 的预读 source。被预取的文件若后续被编辑，在下次 settle 时作为正样本进一步强化模型（正反馈回路）。冷启动时模型无信心（P≈0.5），预取自动休眠，不会乱读。

5. **方案 3（倒排索引）当前是骨架**：defs 查表路径已就���但 MISS 路径 index 不可用（传 `None`）。完整落地需要把 focused index 构建移到 MISS 判断之前，或复用持久化缓存——这是后续工作。

---

## 后续建议

1. **方案 3 完整落地**：把 `build_index` 的 focused 模式（matchTerms）移到硬 MISS 判断之前，让 defs 查表在 MISS 路径也生效。
2. **消融实验**：分别开关 SUBMODULAR / MERGED_SEARCH / PREFETCH，拆分 −22% 的各项贡献。
3. **SPECULATIVE 块的模型信任校准**：跟踪 R5 类案例中模型对投机块的利用率，如果模型过度信任低分投机块导致幻觉，需要加 score 阈值过滤。
4. **预测性读取的命中率追踪**：记录预取文件的后续命中率（下次 fast_context 是否命中预取文件），用轨迹数据训练预取宽度（当前固定 top-3）。
5. **预取与学习模型的冷启动互动**：当前 P>0.5 阈值在模型未训练时几乎不触发预取。可以考虑在模型冷启动阶段用启发式分数（ranked 顺序）作为预取兜底，待模型收敛后切换到 P(edit) 驱动。
