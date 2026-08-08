# 真实会话 Replay A/B 报告

- 数据来源：`~/.nova/alkaid/sessions` 最近 50 个会话中提取的 28 个去重 fast_context 调用
- A 臂（对照）：全部新特性关闭（`NOVA_CTX_SUBMODULAR=0 MERGED_SEARCH=0 DEFS_SUGGEST=0 PREFETCH=0 SPECULATIVE=0`）
- B 臂（实验）：全部新特性开启（默认）

## 汇总

| 指标 | A（旧） | B（新） | Δ | Δ% |
|---|---:|---:|---:|---:|
| 成功率 | 28/28 | 28/28 |  |  |
| MISS 数 | 0 | 0 | 0 | n/a |
| 平均耗时(ms) | 1592 | 537 | -1055 | -66.3% |
| 总耗时(ms) | 44583 | 15042 | -29541 | -66.3% |
| 平均文件数 | 8.3 | 8.3 | 0 | 0.0% |
| 平均块数 | 24.8 | 24.4 | -0.4 | -1.6% |
| 平均字节数 | 28695 | 26787 | -1908 | -6.6% |
| 平均 SPECULATIVE 块 | 0 | 1.1 | +1.1 | n/a |
| 平均 SIG 签名 | 2.8 | 1.6 | -1.2 | n/a |
| 平均 IMPACT 行 | 5 | 4.8 | -0.2 | n/a |

## 分案例

| # | 关键词 | A 文件/块/字节 | B 文件/块/字节 | A 耗时 | B 耗时 | Δ 耗时 |
|---|---|---:|---:|---:|---:|---:|
| 1 | card, comments, 评论, 帖子 | 5/25/20078 | 4/27/18392 | 558ms | 242ms | -316ms |
| 2 | setup, Kimi, GPT | 9/20/15424 | 9/22/14607 | 215ms | 207ms | -8ms |
| 3 | kimi, moonshot, defaultConfig, providers | 6/11/10670 | 6/11/10670 | 63ms | 73ms | +10ms |
| 4 | kimi, moonshot, gpt, default, provider | 4/6/24505 | 4/6/24505 | 260ms | 236ms | -24ms |
| 5 | lyra.json, lyra | 7/21/28660 | 7/22/29345 | 231ms | 221ms | -10ms |
| 6 | remote, model, switch, session | 7/27/19592 | 7/27/18987 | 1126ms | 936ms | -190ms |
| 7 | remote, send, command, model, switch | 7/23/21604 | 7/23/21583 | 1072ms | 1073ms | +1ms |
| 8 | fast_context, oldText, edit, virtual, an | 10/34/43424 | 12/38/41933 | 747ms | 796ms | +49ms |
| 9 | EditInput, fast_context, edit_tool, oldT | 10/37/35689 | 10/38/34552 | 649ms | 450ms | -199ms |
| 10 | EditInput, fast_context, oldText, ToolDe | 6/30/29430 | 6/29/27367 | 462ms | 471ms | +9ms |
| 11 | fast_context, coupling, co-change, 共改 | 10/28/30853 | 10/27/29279 | 397ms | 403ms | +6ms |
| 12 | deepseek, Vega, session, token, tool_cal | 8/36/46515 | 8/36/42629 | 633ms | 455ms | -178ms |
| 13 | tool_start, usage, session_end, createAl | 12/30/33516 | 12/29/30941 | 600ms | 615ms | +15ms |
| 14 | fast_context, fastContext, napi, context | 7/2/19835 | 7/2/19835 | 479ms | 476ms | -3ms |
| 15 | Lyra, close, settle, learning_root_key,  | 9/31/40474 | 9/28/40410 | 26927ms | 664ms | -26263ms |
| 16 | run_oneshot, spawn_prompt, InProcessSess | 8/28/32143 | 7/21/17385 | 2010ms | 461ms | -1549ms |
| 17 | model sync, sync models, lyra, models | 5/15/23351 | 5/14/22832 | 620ms | 639ms | +19ms |
| 18 | AgentKind, labelAgent, req.Model, models | 9/25/41498 | 9/26/41365 | 681ms | 575ms | -106ms |
| 19 | FastContext, SIG, IMPACT, NEXT_READS, RE | 11/34/36569 | 11/34/34865 | 818ms | 641ms | -177ms |
| 20 | enhanced, fastContextTransform, ctx-sess | 7/30/42643 | 9/30/40408 | 631ms | 520ms | -111ms |
| 21 | fast_context, SIG, IMPACT, NEXT_READS, R | 11/37/35529 | 11/37/35483 | 757ms | 669ms | -88ms |
| 22 | fastContextTransform, createAlkaidAgent, | 10/19/23942 | 10/19/23750 | 734ms | 680ms | -54ms |
| 23 | fast_context, FastContext, context.rs, m | 14/32/33898 | 14/32/33701 | 672ms | 600ms | -72ms |
| 24 | 证据链, 画布, canvas, evidence | 6/24/28645 | 8/20/11428 | 887ms | 812ms | -75ms |
| 25 | edit_files, executeTools, parallel tool, | 6/19/16676 | 5/18/15601 | 563ms | 519ms | -44ms |
| 26 | novaDevinBatchToolPolicy, includeEditFil | 10/27/20365 | 10/28/20078 | 623ms | 618ms | -5ms |
| 27 | 远程控制, 会话排序, remote control, session | 10/33/25995 | 9/29/26197 | 690ms | 504ms | -186ms |
| 28 | cursor, claude, claude-code, adapter, pr | 7/10/21924 | 7/10/21900 | 478ms | 486ms | +8ms |

## 判读口径

- **平均耗时**：B 臂应更低（Lazy Greedy + 合并扫描 + 预取生效）。
- **平均文件数/块数**：B 臂应更高或持平（次模覆盖多样性更好）。
- **SPECULATIVE 块**：B 臂独有（A 臂关闭），数量反映超取经济学生效程度。
- **MISS 数**：两臂应一致（打包算法不影响 MISS 判定）。
- **错误数**：两臂都应为 0。