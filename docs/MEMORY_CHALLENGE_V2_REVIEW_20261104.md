# diagnostic_postfreeze_v2：检索前语义审读与勘误

本轮逐项阅读六主题的 60 条来源、96 个问题及标签、12 个范围控制；未读取本集的模型或检索结果，未运行检索、模型或 Cargo。v2 是代理审读后的诊断集，不是人工盲评，也不声称穷尽了自然语言的全部合理证明路径。v1 的四份数据、schema 和 manifest 保持原字节。

创建工具为 `scripts/memory-lab/revise_diagnostic_postfreeze_v2.py`。它先核对冻结 v1 的六个哈希，拒绝已有输出目录，校验内存中的新版，再写新目录。逐字段 before/after、来源 ID 和理由保存在新版 `errata.json`；manifest 链接父版本与 errata 哈希。重建会产生新的创建时间和 manifest 哈希，因此运行比较必须显式绑定下列已冻结的实际 manifest。

## 冻结文件

目录：`rust/ai-tutor/tests/fixtures/diagnostic_postfreeze_v2/`

| 文件 | SHA-256 |
| --- | --- |
| manifest.json | `858ee969827d54d1687d1a3966e726576a66024f9b462683323732dde94fbb81` |
| sources.jsonl | `b96d0232e8927ae676345f9463e87a641160c0256a0d16a0c3aad60005da55fd` |
| queries.jsonl | `726a967f59fb03fe2fec47a9c888ed409f7608578fc16acf01b10d66ed7e2541` |
| labels.jsonl | `8735e034bd764ea42052cdcc34266de3d693bc2f39afbf13813a8a17a44d34fb` |
| scope_controls.jsonl | `7fffe45855e063a5e5e35748ee9e118d9e57ab24b2fe017acbe132d2459238cf` |
| record_schemas.json | `75946f114835b7604fce61dd841a96638f97d2b700458580d4cae79cd1a17f05` |
| errata.json | `bc63aaace182af23772f167b8b1a57e5030080f9cb3d3dc4de2f69bd194203ad` |

## 全部问题差异

表内 `sNN` 指与问题相同主题的来源。没有列出的记录字段保持不变；全部来源、12 控制和 24 个负例标签不改。

| 问题 ID | 证据与语义问题 | v2 处理 |
| --- | --- | --- |
| germination_q02 | s02 的 A/B 每日 10/5mL 已足以否定光照单因果；温度缺失是额外细节，题目未要求 | 充分集 `{s03}` 或 `{s02}`；claim 收窄为水量不同，不能单归因光照 |
| paper_bridge_q02 | s02 直接说跨度未保持一致，已能否定折法单因果 | `{s03}` 或 `{s02}`；claim 聚焦跨度不同 |
| fractions_q02 | s02 明确 E 原味、F 草莓味，单条已提供水比例之外的混杂条件 | `{s03}` 或 `{s02}`；不强制重报两杯完整配比；不保留包含 s02 的冗余双来源超集 |
| reading_q02 | s01 明确摘录未写是否淋雨，原始句已足以否定“直接证明刚在雨里走过” | `{s03}` 或 `{s01}`，原 claim 不变 |
| loops_q02 | s02 明确 H 与 G 初值不同；另有 s01/s09 的 G=0 与 s05 的 H=1 可组成独立证明 | 四个最小集：`{s03}`、`{s02}`、`{s01,s05}`、`{s09,s05}`；claim 只要求初值不同，末值不足判循环体错误 |
| evaporation_q02 | s02 明确气流与形状未匹配，单条即能否定面积单因果 | `{s03}` 或 `{s02}`；不额外要求复述未测流速/水温 |
| paper_bridge_q09 | 题目问为何不能把七枚写为成功承载量；s01 与 s09 均直接给出六枚成功、第七枚触桌 | `{s09}` 或 `{s01}`，不因题中出现 correction 就强迫只用更正来源 |
| loops_q11 | s03 已报告两程序实际执行三次加二，初值分别为 0/1；允许确定算术时不必同时检索两种语言 | 保留 `{s01,s02}`、`{s09,s02}`，增 `{s03}`；claim 明说 6/7 为已发生事实的算术推导；mode 改为 mixed_single |
| loops_q07 | s07“代码尚未改好”不能翻成“code changes have not happened” | 改为 changes are not complete，尚无 rerun/new output；证据集不变 |
| germination_q10 | s10 说明这是目标而非完成试验的证据，不是对全部现实活动作“从未完成”的断言 | 改为该目标记录没有提供已完成受控试验的证据；证据集不变 |
| reading_q09 | 原题只问正确时间且给出旧 six 前提，易与原始观察混同 | 检索前澄清题干为“更正说明，小岚那句中的 six o'clock 误记从哪来、正确几点，有无 morning or afternoon 的证据？”；由题意要求 s09 的误归来源，不改标签 |
| loops_q09 | 原题只问迭代次数和输出数字，却用标签强制追溯更正历史 | 检索前澄清为“G 的 correction 说把哪个数误记成了 output？正确输出是多少，这次更正是否新增运行？”；s09 提供误记关系及无新增运行，不改标签 |

合计修改 10 条标签、2 条题干、1 条 mode；涉及 12 个不同问题。充分集调整同时更新精确全文引文，并从 `context_only_ids` 移除新成为充分证据的来源。其他问题正文、候选池、goal、范围和标签不变。题干确有两条变化，不能将 queries 称作字节未改。

## 算术与分类规则

允许从明确已发生或已测事实做确定性算术，例如 `50mL - 42mL = 8mL`、初值 0 经三次实际加 2 得 6；答案须说明属于推导。不能从计划、代码意图、缺失记录推断执行、实际观测、能力或单位。推导不增加独立试验数量，也不是一条新测量。

loops_q11 因存在 `{loops_s03}` 单来源充分集，不能继续算作“必须双语多证据”。新版 mandatory multi 为 11，mixed_single 为 13；mixed_single 表示至少存在一个单来源充分集，并不删除不可约的两来源替代。原 `tags` 不作为必要证据数量或排名依据；分母使用 mode。

五个 same_zh 的 q02 保留同语充分集，同时接受语义有效的英文来源替代；这不把它们增加到“必须跨语”分母。原来的 24 个单一跨语问题仍严格核对目标证据语言。具有替代集的问题总数由 12 变为 19。

验收阈值原样保持：完整证据至少 65/72，单一跨语至少 20/24，多证据至少 10/11，范围泄漏为 0；每主题和总体回退限制也未降低。没有为了保留 12 的分母排除算术，也没有因分母变小下调 10 的门槛。

## 负例、提示与范围审读

没有发现 24 个负例里已写出所问具体值/人物事实却被标成不可回答的案例。loops_q13 明确未保存耗时，evaporation_q13 明确未测流速，fractions_q13 明确未测含糖量：这些来源仍能支持“不提供该数值的原因”，所以保留 context_only。`answerable=false` 不等于没有任何可引用的限制说明。

已接受/未接受计划未执行、提示后回答、无提示但未实际执行的建议、临时请求与永久偏好，各自原文限定词均保留。未将分数/垫圈/循环计数混成质量、失败阈值、程序耗时或能力；未从年龄、外貌或请求推断能力/障碍。12 个范围控制在这个逻辑 fixture 的 owner/namespace/activity/status/version 规则下均应排除；同文异主仍只是再次检查 owner，不宣称生产 Store 接线或第六种授权机制已通过。

## 实际静态验证

`check_diagnostic_postfreeze.py` 现在明确按版本校验：v1 分布保持原样，v2 仅应用上述审读差异。发现原 v1 schema 限制替代集最多 3 组，但检查器漏查总组数；现在实际执行该约束。v2 为表达 loops_q02 的四个不可约集合，将该唯一数组上限改为 4，并更新 schema 标题。单集合最多 3 个来源、引用最多 6 条、来源状态/owner/scope/version、引用原文及去重/冗余超集约束不放松。

已运行以下两份报告，均通过结构/引用检查、原有 10 个内存坏数据回归及新增的替代集总数超限回归（11/11）：

- `runtime/goal-baselines/20261104/diagnostic-postfreeze-v2-static-first/report.json`
- `runtime/goal-baselines/20261104/diagnostic-postfreeze-v1-after-review/report.json`

这不是语义自动证明。报告继续保留 `semantic_quality_pass=false`、`model_called=false`、`retrieval_run=false`、`human_annotated=false` 和 `blind_evaluation=false`。后续检索必须由 root 显式绑定本页 manifest SHA；本轮没有运行任何检索。
