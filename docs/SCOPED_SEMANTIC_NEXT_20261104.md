# 下一片：活动范围内语义候选与真实教学上下文

本文件仅设计，未改 Rust、运行 Cargo、编码或调用模型。当前历史快状态来源修复已由根代理验证 workspace40：775 通过、0 失败、8 忽略；本文实验不属于该成绩。

## 现有入口与最小改动

`store/embeddings.rs` 已有 `embedding_work`（每批最多 32）、`commit_memory_embedding` CAS 和 `eligible_memory_embeddings`。CAS 绑定 namespace、learner、epoch、memory/source digest、正文及完整 profile。缓存读取再核 live 来源版本；向量不是来源权威。

`recall_candidates_for_session(sid, query, embedding, top_k)` 已先解析当前活动及有效显式祖先，再分别排名，但输出固定词面+语义 RRF。`eligible_memory_embeddings(lid, key)` 本身只是 owner 范围，不能直接作为教学候选池。`context.rs::build_context` 当前只调用 `recall_for_session`，没有外部候选注入入口。

建议只加一个排序枚举 `CandidateRanking::{DenseOnly, HybridRrf}`，在现有 scoped 候选方法内部复用。保留全部旧方法签名和行为，新增：

```rust
recall_candidates_for_session_ranked(
    sid, query, QueryEmbedding, top_k, CandidateRanking
) -> Result<Vec<RecallCandidate>>
```

新增入口要求有效且归一化的已注册 profile 向量，缺失或不匹配直接报错，不静默把 DenseOnly 降为词面。DenseOnly 不参与词面分数；HybridRrf 保持现有 `1/(60+rank)` 和 ID 同分规则，不为本次题目调规则。仍先 owner/live/活动链过滤，再做各自 top5、融合及 5 条/2400 字完整记录预算，最终再核来源。持续偏好的既有严格跨活动规则不变。

上下文只做一处共用实现拆分：旧 `build_context` 委托内部函数、默认仍走原 `recall_for_session`；另给显式 `build_context_with_semantic(..., query_text, embedding, ranking)`。新入口必须核 `query_text == memory_recall_query(current_input, current_goal)`，仅替换选取慢记忆那一步；证据投影、历史/continuation、当前事实、来源级别、预算与最终长度检查共用原逻辑。不要让调用方传任意 memory IDs 或自行改最终 context。排名分数/别名/标签留诊断侧，不喂模型。

这不接 server、不添加生产默认开关、不启用在线 BGE 推理。`BgeM3NativeFp32` 目前是冻结 native 向量 profile，实际 Rust 编码器仍只有 E5；在线服务生命周期另片处理。

## 固定输入与真实 Store 重建

使用以下现存证据，不重新编码：

| 输入 | SHA256 |
|---|---|
| typed48-projection-first/report.json | `8f36bf7d19f598c2ac6ee7eeb06e825915d556f84fbf77d01da3961381acfe65` |
| typed48-projection-first/encoder_inputs.jsonl | `6f003d9ad4d2fdf14f0978f683b9dd5487ef527e362a54d1f40cf4b23d6cc7e7` |
| typed48-encode-first/report.json | `1c1b261e4ed3d74d14fbacc713a9f0e2c32bbeca6cf03f1368c29024a3fa1a44` |
| typed48-encode-first/bge_m3-encodings.json | `01ad3da86852d64bab2c8d21b366b8e3bc0a74f170a15660d45320b3aa7c63e9` |

每题重建与前一真实 Store 探针相同的 60 来源/48 类型记忆及六活动映射，同规则恢复有来源历史 s10/s07，再合法继续所选活动。复用冻结 typed48 helper；新的诊断文件不修改旧 helpers、方法、结果。

按实际 memory.content、来源正文/kind/grade/support/version 映射冻结文本，不依赖 UUID 相同。通过真实 `embedding_work` 两批 32+16、逐条 CAS 导入 48 个 1024 维向量；再从 eligible 读取逐项核正文和向量精确相等、剩余工作为零。scope 不由 pack 名称决定，pack 只用于夹具映射真实来源 session；允许范围由 Store 实际选择链决定。

六个原问题及原文不变：germination_q06、paper_bridge_q02、fractions_q16、reading_q09、loops_q07、evaporation_q06。只取精确 `current_query:<id>` 向量，不拼 goal；逐条核查询文字 hash。每题同一 Store、同一 memory IDs 和来源快照构建 DenseOnly、HybridRrf 两份上下文，避免用重新随机 UUID 偷换 RRF 同分顺序；各调用使用独立 request_id。另保存同一 Store 的默认词面 context 作结构对照。

## 准备、执行与判定

先冻结新方法、全部准备请求、真实 CAS/候选审计及审阅映射，再由根代理调度。两个明确语义分支各六次，按冻结原题顺序逐题 DenseOnly→HybridRrf，每个 arm/case 只有一次 attempt，无自动重试或挑结果重跑。原词面已保存回答只是历史参照；若 policy/guard 版本不同，不把跨版本差异全归因于排序。当前新 policy/schema、provider 源与实际 executable 均重新记录 hash，旧产物不改。

调用仍是生产 KimiProvider probe/generate/close，无投递/候选应用。结构验证后保存完整合成答复。source ID 合法不等于语义充分；审阅同时检查正文、候选更新及四个当前 task/current_session 偏好来源契约。新增 guard 是否实际拒绝候选用独立真实 Store 回放验证，不把“没有调用 apply”当作拒绝成功。

固定分母五阳性、一阴性。阳性用原完整最小证据替代集：分别记录召回前可用、候选 top5、最终完整 context（含 continuation）以及回复引用的集合覆盖，不裁掉漏召回来源、不把阳性改为阴性。阴性 fractions_q16 返回任何候选仍只算候选；必须审阅模型没有编造蜂蜜数量/品牌、没有无依据更新，并说明证据不足。相似度不设“可信度/拒答”硬阈值。

记录 RRF 所有同分及第五名截断组。已知离线植物题 s06（语义第 4）与 s09（词面第 4）均为 1/64，不能把本轮 UUID 恰好保留 s06 当作稳定通过；同时报告该同分组不同合法截断下的证据覆盖，不按标签选择 tie。

必要无模型回归限：默认词面输出不变；同快照 HybridRrf 与旧 API 相同；DenseOnly 不受词面计划加分影响；高相似度无关活动在 top-k 前排除；真实来源修订/删除/epoch 使旧缓存/CAS 失效；缺失或错 profile 不静默回退。父链/旁支范围可用已有真实 Store 合成向量回归，但六个新增教学边界含新观察文本，**没有冻结 BGE 向量，本片不声称其语义教学评测已覆盖**。

即使六题两分支都通过，也只能支持下一阶段受控 shadow，不代表原 24 阴性、全部 96 问、现场长期记忆或默认启用已通过。
