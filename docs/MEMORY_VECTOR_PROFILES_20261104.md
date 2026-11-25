# 固定向量 profile 与真实 Store 诊断

本片保留生产默认召回和 E5 推理路径，只让派生向量缓存明确区分固定模型输入合同。没有把全局 384 维改成 1024 维，没有启用 BGE 生产推理，也没有使用相似度代替来源等级或可信度。

## 已实现的边界

`memory_embedding/profile.rs` 注册四个固定 profile：E5 ONNX INT8、E5 ONNX FP32、E5 native FP32、BGE-M3 native FP32。每个 key 绑定固定 revision、权重 SHA256、tokenizer SHA256、pooling、query/document 前缀与维度；profile 注册表同时绑定模型名称。E5 使用 mask mean + L2、query/passage 前缀、384 维；BGE 使用 CLS + L2、空前缀、1024 维。原来两个 E5 ONNX key 保持逐字一致；同维度的不同权重路径也不共享缓存。

`store/embeddings.rs` 在写入和读取时按注册 profile 检查维度、有限值和 L2 范数。来源版本、记忆内容、learner epoch、namespace 与 owner 继续由真实 Store CAS 和 `memory_evidence` 核对。未知 query profile 保留既有词面回退；未知索引 profile 被拒绝。候选仍标为 `candidate_requires_relevance_review`。

迁移只重建派生 `memory_embeddings` 表，旧 384 维行原样复制，新增 dimension 默认 384；旧显式八列写入仍兼容。全部已有失效触发器重新指向最终表名，迁移处于既有 Store open 事务内。没有改写来源事件或用户记忆。

根代理统一验证报告：本轮 workspace 650 passed、0 failed、5 ignored、无警告；其中包含新增 2 个 profile 单测和 5 个 Store 集成回归。覆盖旧表迁移与旧写入兼容、四 profile 分离、错维度/错 key/损坏 blob、真实更正/忘记/epoch/删除失效、跨 owner 与 whole-source 预算。随后独立 probe 的 release 编译和首次运行也分别以 exit 0 完成；其实际结果另列如下。

## 冻结向量进入真实 Store

独立源码 `scripts/memory-lab/bge_store_probe.rs` 不加载模型，不访问实际教学数据库；只在自建临时目录打开合成数据库。方法文件为同目录 `bge_store_method_v1.json`。根代理已完成注册、编译并首次运行：

```powershell
target/release/bge-store-probe.exe --root 'C:\Users\RBMS\Desktop\AI TUTOR' --output runtime/goal-baselines/20261104/bge-store-first/report.json
```

新 bin 注册路径为 `../../scripts/memory-lab/bge_store_probe.rs`，名称 `bge-store-probe`，`test=false`。报告必须使用新路径，不能覆盖已有结果。

冻结输入包括 challenge v2 manifest `858ee969827d54d1687d1a3966e726576a66024f9b462683323732dde94fbb81`、原 BGE/E5 报告及两份全部 252 条 encoding 文件。每条浮点向量重新核对来源文本及前缀的 SHA、f32 little-endian SHA、维度与归一化。原 60 来源、96 问、标签与结果均不修改。

实际入库仅 18 条：12 条 reported observation 经现有 `add_memory` 形成 `reported_progress`，6 条已接受但未做的 plan 形成 `pending_experiment`；正文逐字不变。其余 42 条保留原事件类型并逐项列出未索引理由。临时请求不会伪装持续偏好，未接受计划不会伪装观察或完成结果，其他类型不绕过 typed promotion 的正文前缀。排除是本诊断的合法原文适配范围，不能推断 Store 必然拒绝所有其他类型。

原逻辑 workshop 的来源被映射到仅此合成数据库已有的 modular_rover activity API，报告明示此限制。它不证明跨活动 scope 放宽是正确的，也不代替原 12 个 scope control。真实 Store 的 namespace/owner/CAS 控制独立于语义排名计分。

对两个 native profile 分别计算 current question 和原样 question + goal 两种条件，保留全部 96 问。每组分别返回：

- 实际 `recall_candidates(..., None)`：当前 Store 词面路径。
- 实际 `eligible_memory_embeddings` 经 cosine 排序：18 条完整纯向量排序和 whole-source top 5。
- 实际 `recall_candidates(..., Some(query))`：现有 legacy 词面 + RRF 候选，不能称作纯 BGE 或冻结的新词面方法。

保留原最小充分证据集合和替代集，分别列出原始 72 正例分母与至少存在一套完整入库证据的正例分母；未入库证据不会被删出标签或变成新负例。24 个负例只报告候选返回，不声称拒答通过。此 18 来源诊断不继承原 60 来源实验的质量门槛。

Dense 与最终 RRF 的相同分数使用真实随机 memory ID 排序，报告保留 ID 映射；legacy 词面沿用既有时间/行序规则。完整 dense 排名以及 RRF 返回范围内的同分组会记录；五条返回以外的 RRF 边界同分候选未暴露，不能声称完全消除 tie 影响。短原文未必耗尽 2400 字预算，报告只验证不截断和不超限；真正超限由上述独立 fake-vector 回归覆盖。

真实向量控制复用 Store 更正和忘记 API，并将直接 SQL 的来源版本/删除、epoch、session/learner 删除触发器检查明确标为 SQL fixture。另核对跨 learner 读取/伪造回执、跨 namespace、错维度/错 profile 和未知 profile。它们不意味着所有对应操作都有公开 API。

## 首次真实 Store 结果

报告为 `runtime/goal-baselines/20261104/bge-store-first/report.json`，SHA256 为 `8c5966d3a6033c4f30a61e07d94dd8fa7cee1aa3f95a396418707b35f071fef3`。根代理记录 release 编译 15.89 秒、探针运行 4.469 秒，均 exit 0；运行时间包括读取/核验冻结向量与临时数据库操作，**不是模型编码耗时**。

实际 60 条来源中 18 条入库、42 条排除；两个 profile 分别提交 18 条缓存，正文均未变。原始 72 个正题中，仅 31 题至少有一套完整证据仍在这 18 条里，另 41 题在此入库范围下不可能完成原始证据要求。下表分母统一为这 31 题，保留原始分母时相同分子应写作 `/72`。

| 查询条件 | 当前 Store 词面 | E5 纯 eligible cosine | E5 实际 Store RRF | BGE 纯 eligible cosine | BGE 实际 Store RRF |
|---|---:|---:|---:|---:|---:|
| 当前问题 | 13/31 | 25/31 | 22/31 | 31/31 | 31/31 |
| 当前问题 + 目标 | 13/31 | 26/31 | 24/31 | 31/31 | 29/31 |

这些数值表示 top 5 包含至少一套原标注的完整最小证据，不代表回答正确率。18 来源的候选池也减少了原 60 来源实验中的干扰项，因此不能把 31/31 当作原 96 问通过、真实场景准确率或生产启用依据。

加入目标后，BGE 实际 RRF 相对纯 cosine 丢掉了两道同语中文因果边界题：

- `paper_bridge_q02`：入库充分来源 `paper_bridge_s02` 指出两座桥的跨度未保持一致；另一充分来源 `paper_bridge_s03` 未入库。BGE 将 s02 排第 4，cosine 约 0.475709。它不在 legacy 词面 top 5，仅得到语义单路 RRF 分数 `1/64 = 0.015625`。词面与语义共同命中的计划占据前列，词面第 3 的 `reading_s07` 以 `1/63 ≈ 0.015873` 进入返回第 5，s02 因五条数量上限被挤出。
- `evaporation_q02`：入库充分来源 `evaporation_s02` 指出容器形状和气流未匹配；另一充分来源 `evaporation_s03` 未入库。BGE 将 s02 排第 5，cosine 约 0.514821。它同样不在词面 top 5，只有 `1/65 ≈ 0.015385`；词面第 3 的 `reading_s07` 以约 0.015873 占返回第 5，充分来源被挤出。

两题实际返回都只有 319 个 Unicode 字符，远未达到 2400 字预算；这是现有两路各取五条、RRF 融合后再返回五条的排序/数量边界，不能归因为向量缓存丢失或正文截断。两条被挤出的充分来源分数均严格低于返回第 5，不能用随机 memory ID 的同分先后解释这两个失败。`paper_bridge_q02` 的返回第 4/5 本身存在同分，但不改变上述分数差。

两个 profile、两种查询条件的 24 个原始负题，纯 dense 与实际 RRF 均返回候选（24/24）。当前 Store 词面为当前问题 22/24、加目标 24/24。所有候选仍需相关性审查；本次没有运行答案生成或拒答判定，不能宣称无证据问题已被正确拒绝。

10 组真实向量控制全部通过：API 路径为来源更正与忘记；直接 SQL fixture 路径为来源版本变化、来源删除、learner epoch、session 删除、learner 删除；另三组为跨 learner 读取/伪造回执、跨 namespace 回执、错维度/错 profile/未知 profile。前七个失效场景均观察到缓存行归零且迟到提交被拒绝。SQL 控制只证明实际触发器和 CAS 边界，不声称对应公开端点或所有降级编辑都已验证。

本次支持“固定 BGE/E5 profile 与真实 Store 缓存/来源失效链可以协同工作”这一窄结论。它同时显示 legacy RRF 会在加入目标时抵消纯 BGE 的部分召回优势；生产默认仍不变，不继承原 60 来源实验的质量门槛。

独立审计 `bge-store-audit-second.json` 从原v2标签、事件字段与来源原文重算384个案例、1152组候选，单题与汇总全部一致；最大入选原文为1078/2400个Unicode字符。首份audit把集合成员按顺序比较，产生28条假阳性；保留首份，第二份按无序集合逐项纠正，原probe/数据/报告未改。

控制证据另有明确边界：真实BGE跨namespace探针把simulator回执提交到全新空real库，owner/来源同时不存在，故不属于只改变namespace的单变量对照。既有 `snapshots_bind_owner_namespace_epoch_model_and_content` 测试在同Store中只改namespace并拒绝提交，已随650项通过，但使用E5假单位向量。两者分别记录，不互相冒充。

## 冻结记录与剩余边界

- Probe 源 SHA256：`d22f36202bf64d266b713a26fed2951079351a2cdf6674e57910378d52471173`。
- 方法 SHA256：`dc4abf69fa5d02370f8ecdc14e250623a5bcdecf8ddc83d4ec939931d89e0981`。
- 本代理完成 UTF8 静态读取与 rustfmt；根代理统一编译及首次执行后，本代理只读复核报告与两道失败题，并仅更新本文件。源码、方法、原数据与首次报告不变。
- 本片不接主程序、不改变默认召回、不重新推理、不涉及设备，不对完整 96 问给出生产通过结论。
