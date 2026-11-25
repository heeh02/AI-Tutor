# 当前问题与教学目标分离

本片修正生产 `context::build_context` 的检索输入。之前每次把当前输入、活动目标拼成同一查询，真实 Store 词面检索因此也会受历史目标中的词影响。教学上下文中 `task.goal` 及其来源本来就独立保留，无需把目标重复加进检索字符串。

代码核对同时确认：`server.rs` 通过 `build_context` 调用默认 `Store::recall`；生产没有调用 `E5Embedder` 或 `Store::recall_candidates`。后者和 BGE 缓存仍是独立诊断能力，E5 `embed_query` 仅添加模型规定的 `query: ` 前缀。此次不启用语义检索、不修改 RRF 排序，也不把诊断中当前问题 57/60 的结果称为生产召回成绩。

## 行为

`context::memory_recall_query(text, goal)` 提供单一、小型输入选择函数：

| 输入 | 检索内容 | 教学上下文 |
|---|---|---|
| 有具体内容的问题 | 当前输入逐字保留，不追加 goal | `user_input`、`task.goal` 和原来源都保留 |
| 整句是 `继续 / 上次 / 接着 / resume / continue / last time` | 有目标则仅用原 goal；无目标则保留原有“小车活动”显式恢复入口 | 仍保留原命令，不改写用户来源 |
| 空或纯空白 | 不构造查询，`build_context` 返回“请输入内容” | HTTP 原先就拒绝空输入；直接调用现在也一致 |

命令匹配仅忽略首尾空白、末尾句号/问号/叹号/省略号、英文大小写和命令内部空白。它不是自然语言意图分类器。“继续解释为什么结果不同”“我不想继续”“Do not continue…”“What changed last time?” 都仍使用完整当前输入，不因包含某个词而改用目标。其它表达也按完整输入处理，目标继续在教学上下文中可见。未增加按诊断题目写的领域词、同义词或相似度阈值。

未来如果接入查询编码器，应向它传入同一个选择结果，再由对应模型添加其规定前缀；不得另造一条拼接目标的向量查询。当前没有新增无调用者的编码适配器或配置开关。旧冻结 probe 的 `query_plus_goal` 仍准确表示旧实验条件，原方法、标签、向量和报告全部保持不变。

## 验证范围

新增 `tests/context_retrieval.rs` 五项回归，仅用临时 SQLite 和合成来源，无模型/设备：

1. 中英具体问题及否定、包含 continuation 词的句子逐字保留，不随 goal 改变。
2. 仅完整 continuation 命令借目标；没有目标保留原活动恢复；空白不借目标。
3. 真实 Store 中六条目标词记忆把旧拼接查询的唯一当前问题证据挤出前五；新 `build_context` 保留该证据，改变目标也不改变当前问题召回，同时目标及其来源仍在上下文。实际候选 API 的无向量回退使用同一查询得到相同证据。
4. 真实有来源目标支持 continuation 检索，原输入不改写；空白直接上下文调用被拒绝。
5. 同 namespace 两个非空学习者及非空 simulator 都保存相同正文，逐一只召回自己的记忆；实际修订 A 的来源使 A 旧记忆不再入上下文，B 和 simulator 不受影响；错配 session/learner 被拒绝。

已有 `context::tests::continue_recalls_finished_report_as_history_without_current_inventory` 继续覆盖无目标恢复不会把历史硬件当当前配置。整体字符预算、整条记忆选择、live source 版本校验与 epoch/owner 隔离沿用原流程。

根代理应执行：

```powershell
cargo test -p ai-tutor --test context_retrieval
cargo test -p ai-tutor context::tests
```

根代理统一执行的 `workspace-thirty-fourth.log` 已通过全部731项测试、0失败、6忽略，含本文五项真实Store回归和既有context测试，无编译警告。新的真实 Store 用例验证查询行为与来源边界，不作为全面教育检索质量或无证据拒答评测。旧词面自身的字符 gram、待做计划加分及默认偏好候选规则没有在本片修改，负例拒答仍未验证。

仅修改 `rust/ai-tutor/src/context.rs`，新增上述独立测试及本文档。原文件备份于 `runtime/goal-backups/20261104/architecture_audit/context-query-separation/context.rs`。不修改 Store schema、默认模型、Kimi、音频、Cargo 或原冻结实验。
