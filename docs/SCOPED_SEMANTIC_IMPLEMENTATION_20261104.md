# 显式活动范围语义检索切片

本片实现 `CandidateRanking::{DenseOnly, HybridRrf}` 与 `Store::recall_candidates_for_session_ranked`。DenseOnly 只按语义相似度排序；HybridRrf 保持原 `1/(60+rank)`、相同分数按 memory ID 排序。分数仅表示候选排序，不是事实可信度、掌握程度或拒答阈值。

显式接口拒绝未注册 profile、维度错误、非有限或未归一化的 query vector，即使没有可用缓存也不回退。合法向量但无有效缓存时 DenseOnly 返回空列表，HybridRrf 可保留词面候选。旧 optional embedding API 的签名、无效向量词面回退及默认 `build_context` 不变。

缓存读取原本已通过 `memory_embedding_snapshot` 验证允许类型及 live 来源。本片将 dense 排名前的允许类型复核写在活动过滤旁边，使 owner、namespace、epoch、来源版本、活动父链及持久偏好资格均在 top5 前生效。最终仍按 5 条/2400 字保留完整记录并重新验证来源；未改变缓存 schema、写入 CAS 或原词面规则。

`build_context_with_semantic` 显式接受 query text、query vector 与排序模式，核对 query text 等于 `memory_recall_query(current_input, current_goal)`，只替换慢记忆选取。与默认入口共用当前输入、必要事实、continuation、来源关系、证据投影与最终上下文预算；不会把排名字段传给模型。该文本检查只能防止调用方传错查询字段，不能数学证明外部向量来自该文本。受控诊断另以冻结 `current_query` ID、原文 SHA 和向量文件补充可审计绑定。

没有改 server、运行策略、生产默认开关或在线 BGE 编码器。代码实施阶段没有启动编码、模型或设备；随后独立六题诊断的实际结果见文末。旧 helper、方法和结果保持冻结。

## 核心回归与当前验证状态

新增 `rust/ai-tutor/tests/scoped_semantic_context.rs`，7 项临时 SQLite/合成单位向量回归：

- `explicit_hybrid_matches_legacy_on_the_same_snapshot_and_default_stays_lexical`
- `dense_only_ignores_lexical_plan_bonus_and_never_adds_uncached_records`
- `explicit_invalid_queries_error_even_without_cache_while_legacy_keeps_fallback`
- `activity_chain_and_owner_filter_precede_dense_top_five`
- `semantic_context_keeps_continuation_current_input_and_requires_the_exact_query`
- `source_revision_forget_and_epoch_invalidate_dense_cache_and_late_commit`
- `dense_candidates_preserve_whole_records_and_fixed_count_and_character_budgets`

根代理已完成 workspace44：807 通过、0 失败、9 忽略，含 UI 128，无 warning；本节 7 项新增回归全部通过。日志 `runtime/goal-baselines/20261104/workspace-forty-fourth.log`。合成向量测试验证范围、失效、兼容和预算，不证明语义质量。定向命令：`cargo test -p ai-tutor --test scoped_semantic_context --test memory_embeddings --test activity_continuation`。

修改前备份位于 `runtime/goal-backups/20261104/backend_review/scoped-semantic-core/`。

## 六题同快照诊断

独立新入口 `scripts/memory-lab/scoped_semantic_probe.rs` 与 `scoped_semantic_vectors.rs` 复用冻结 `typed48_common.rs`，不修改旧探针、旧方法或旧结果。方法为 `scoped_semantic_method_v1.json`。每题在一个真实临时 Store 内创建原 60 条来源/48 条类型记忆，通过真实 32+16 work/CAS 写入冻结 BGE 向量，逐项核对正文、类型、来源版本、元数据与向量往返。保存每条 CAS 快照摘要、query 原文/向量 hash 和实际 source 映射。

同一题的 DenseOnly、HybridRrf 和默认词面对照共用 Store、memory IDs、source snapshot、当前问题和 generation。独立调用原生产 context，不在最终 context 内添删来源。审计另重建实际选中来源活动的候选池、dense 排名与完整 RRF union，并核对生产返回一致；保存全部同分组、第五位跨组截断及每种合法选择的原证据覆盖。假设其他同分选择的完整 context 不重建、不冒充实际调用；本夹具最大五条正文预算先验证可容纳，避免把字数跳过误当普通 top5。

固定原六题、每分支 5 阳性/1 阴性；新六个父链边界题没有冻结向量，因此不进入此次模型比较。每个问题按 DenseOnly→HybridRrf 顺序，无自动重试。分数、别名、标签和期望答案只进独立审阅文件。真实模型阶段不投递、不应用候选；这不证明应用护栏已经拒绝候选，护栏的真实 Store 回归需另外引用。

准备入口（根代理执行，prepare 不调用模型）：

```text
cargo test -p ai-tutor --bin scoped-semantic-probe
target/release/scoped-semantic-probe.exe --root . --output <新的准备目录>
```

准备后先冻结 `manifest.json` 的 SHA，再显式执行一对或全部十二个请求：

```text
scoped-semantic-probe.exe --root . --prepared <准备目录> --prepared-manifest-sha256 <SHA> --real-synthetic --batch 1
scoped-semantic-probe.exe --root . --prepared <准备目录> --prepared-manifest-sha256 <SHA> --real-synthetic --all
```

`--batch 1..6` 各含一个原问题的两分支；`--case` 接原 query ID 时同样运行两分支，也可接完整 arm ID 单选尚未尝试的分支。单选 Hybrid 必须已有对应 Dense 的 attempt（成功或失败都算），以便 Dense 失败后显式补做 Hybrid 首次调用；不会重跑 Dense。已存在任一选中 arm 的 attempt 文件即拒绝隐式重试。每次显式调用都保留 provider probe/close 结果；结构成功不自动填写语义质量通过。旧词面回答的 policy/guard 版本若不同，不把全部差异归因于检索。

新增 4 项独立 probe 测试：固定顺序/分母、六个真实 Store 的 32+16 导入与同快照 context、第五位同分保留不利合法选择、查询文字改变拒绝旧向量。根代理已完成定向测试：4 通过、0 失败，exit0；含构建总耗时 28.486 秒，测试执行 8.62 秒。日志 `runtime/goal-baselines/20261104/scoped-semantic-probe-tests-first.log` 与同名 `-exit.json`。这是 workspace44 之后的独立验证，不回填其统计；没有运行编码器、教学模型或设备，也不代表语义质量已通过。

应用护栏的单独实际 Store 证据来自已有 `tutor_historical_tests.rs`，例如 `actual_historical_completion_candidates_cannot_mark_fresh_continuation_completed` 实际执行 `validate_reply` 与 `apply_reply`，确认旧完成报告的两项当前任务更新被拒，同时合法历史慢记忆仍可按其真实来源保存；`all_current_task_fields_reject_historical_support_even_on_explicit_parent_chain` 验证四类当前任务字段。它们与本片“模型结果未应用”是两种不同证据，不互相替代。

## 十二次真实调用与独立语义审阅

证据目录 `runtime/goal-baselines/20261104/scoped-semantic-first/`，准备 manifest SHA `171867663cabf4b87517f84de88d2b985d7324ed9275dac4e6386c721d649364`，实际 probe 二进制 SHA `03c2372b27eefb13bba7c44a210181ef2828993b25bf53610e6b7b6977dd2f99`。固定原六题各两 arm、各唯一一次尝试：12 results，11 validated，1 协议失败。第六批 Dense 蒸发题发生来源引用校验错误，`reply=null`；之后仅用既有 `--case` 首次执行此前未尝试的 Hybrid，未重试 Dense。七次 run 均保存 provider close 成功与 running=0，仅证明现有 provider 的关闭范围，不虚构远端取消确认。

方法知情、非盲的独立审阅检查实际 request、原最小证据集、全部正文和候选；把核心必需答案与附加陈述忠实度分开：

| 口径 | DenseOnly | HybridRrf |
| --- | --- | --- |
| 原阳性 context 含完整最小来源集 | 5/5 | 5/5 |
| 原阳性核心答案完整 | 4/5 | 5/5 |
| 原阳性整段忠实、含候选检查 | 3/5 | 5/5 |
| 原阴性正确说明缺证据、无编造 | 1/1 | 1/1 |
| 全六例整段及候选通过 | 4/6 | 6/6 |

Dense 发芽题核心回答“每盘起始种子数量相同”正确，但附加句将来源 s05 的“提示后指出两盘每天的水量不同”写成“每天浇水量相同则是在老师提醒后才指出的”。这改变了已记录的学习者发言，不能由后来接受等水量计划补足；因此核心答对仍不计整段忠实通过。Dense 蒸发题虽然 context 已有 s06，协议失败仍保留为失败；没有被拒正文，不能推断具体错引 ID 或内容泄漏。

其余有效回答保留桥梁跨度混杂条件、故事人物及时间边界、未执行计划与旧输出的区别、量筒建议不能证明过去已执行，以及蜂蜜克数/品牌缺失。纸桥 10 厘米方案在实际 continuation 可见，回答是将来建议而非完成事实；这不意味着 10 厘米是唯一合法控制值或其他条件已全部受控。11 个有效回复的 task/memory candidates 均为空且未应用，不证明运行过候选应用护栏。

同分边界仍存在：发芽 Hybrid 第五位 s06 与 s09 都为 `1/(60+4)=0.015625`，一个席位。本次随机 memory ID 的升序取舍选中所需 s06；另一合法取舍会选 s09 而丢失最小来源集。两种 top5 均满足正文预算，但假设取舍的完整 context 与模型回答没有运行，不能把本次 Hybrid 5/5 来源覆盖说成稳定保证。

独立记录 `independent-review-ui.json` SHA `887f25664bb45659c68c0cc7341a5eb621f6d0f121eb6101a7cd6587fca94d27`，整体严格口径为 **10/12**，不是结构成功 11/12，也不是来源覆盖全过。原阴性只有 fractions_q16 一个问题、两个 arm，不能扩称原 24 个负题或 96 题验收。模型身份/采样未独立冻结，按固定 Dense→Hybrid 顺序，旧词面调用的 policy/guard 也可能不同；不作严格因果 AB 或算法优劣定论，不改旧结果，不默认启用生产语义召回。
