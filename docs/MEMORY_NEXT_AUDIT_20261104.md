# 教育记忆现状与下一片建议（2026-11-04）

本次为只读源码与既有文档审计；仅新增本文。未运行 Cargo、模型或设备，未改源码、冻结方法或旧证据。根代理报告当前 workspace43 为 786 通过、0 失败、9 忽略；这是软件回归结果，不是学习者效果或长期教学质量结论。本文不提前报告尚在调度的真实边界/长测结果，也未引用论文。

## 当前实际实现

| 方面 | 已有实现与直接证据 | 仍不能据此声称的能力 |
|---|---|---|
| 重要性与来源等级分离 | `store/memory_policy.rs:158` 校验四项 0–3 分；`:172` 按实际来源重新计算 source_grade/source_count，模型不能凭高分提高等级。`memory_contract.rs:24` 固定分项；`tutor-ui/src/memory_evidence.rs:330` 明确说明不是可信度/成绩。 | 当前四分项主要是元数据与展示，不是已验证的保留价值预测。未见这些分项被生产召回排序或自动淘汰使用；`store.rs:585` 仍是词面重合及待做计划固定加分。source_grade 是出处类别，不是事实正确概率；support_level 是提示情境，也不是可信度分。 |
| 快→慢 LLM 总结 | `store/consolidation.rs:127` 绑定 session generation、state_revision、learner epoch、来源 ID/version；`:167` 入队；`memory_worker.rs:291` 构造有界原始来源请求；`:249` 验证后 stage；`store/consolidation.rs:327` 保存 shadow；`:368` 显式 enabled_kinds 下再校验并晋升。 | 队列绑定了快状态修订，但当前 LLM 请求主要含原事件，不传整份快状态，也不递归把旧摘要当新证据。worker 只产候选，不自动把模型判断变长期事实。四项重要性中 goal_relevance 也未获得独立目标评分验证。 |
| 调度与成本 | `memory_schedule.rs:31` 缺少策略/disabled 不启动；仅 shadow 模式。`:65` 要求无前台/音频活动且输入后至少 10 秒空闲；`:128` 单循环 2 秒检查。`store/memory_discovery.rs:79` 每次最多 4 个 dirty session、每个最近 12 条 typed 来源。`consolidation.rs:16` 默认 pending16、最多3次、日20/会话6次预留、120秒租约。 | 不是“始终后台总结全部历史”。发现窗口有界、当前普通聊天不会被自行标成 learner_answer、已接受计划或独立完成。没有产品层自动晋升策略；也未核查用户运行目录当前是否启用策略，不能用代码默认值替代部署状态。 |
| 前台抢占 | `memory_worker.rs:123` 前台 guard 立即发取消；`:199` 模型 await 不持 Store 锁，90 秒截止及取消清理；`:246` 提交与前台进入共用短锁。`kimi.rs:943` 单 gate 注册时前台取消后台，后台只 try_acquire、不排在前台队列前。`server.rs:358` 与 `tutor_delivery.rs:175` 将 guard 保留到投递边界。 | 控制与 fake 并发回归证明不抢占提交、迟到结果不应用；真实一次 shadow 成功或计数并不证明所有负载下的抢占时延/学生体验。需要独立同时负载测量，不能把本轮非记忆长测自动算作这项验收。 |
| 更正、忘记与迟到结果 | `store/education.rs:197` 创建新 ID/version 并失效旧来源闭包；`store.rs:712` 撤销派生记忆、清相关快状态并真实删除旧正文；`:691` forget 提高 epoch。`store/consolidation.rs:73` 等触发器清失效 job snapshot/candidate payload；`store/embeddings.rs:219` CAS、`:251` 读取时核实来源版本和 digest。 | 不是所有语义错误都能自动识别。纠正需明确来源/目标；旧链精确身份丢失时保持 unresolved，不能靠文本猜。已保存的旧快状态未被最新 current-sid guard 自动迁移；关系投影已对此如实标注。 |
| 跨代连续性 | `store/activity_continuation.rs:63` 只沿有效显式选择链；`context.rs:123` 起读取真实关系并逐条 current/selected_ancestor/outside 标注。新活动 ID、新空快状态，历史目标/计划单独投影。 | 目前支持祖先阅读，**不支持新的跨代合并摘要**。`consolidation.rs:150/189` 要求每个 job 来源同 sid；`memory_policy.rs:208` 和 `memory_activity.rs:173` 要求单活动来源。A→B 合法读取 A，不代表能把 A+B 共同写成一条 B 总结。未来若开放需新 job 绑定有效链及最深包含来源的 B，不能回绑 A 使旁支 C 看见 B。 |

普通教学回复还可经 `tutor.rs` 提出旧协议慢记忆候选，活动结束的 `store.rs:277` 则按明确来源做有界原文保留；这两条不应被描述成后台 LLM 自主整理。最新 task guard 要求所有当前快字段及本次临时偏好来源属于当前 sid，历史记忆仍按源活动独立保存。

现有 `MEMORY_WORKER`/`MEMORY_SCHEDULER` 文档保留了各批当时版本和结果，其中早期七类型、prompt v1 等描述不是当前总表。当前源码为八种教育候选、schema educational-memory-v2、prompt educational-memory-prompt-v3。本文不回写历史文档或扩大其结果含义。

## 最有价值的下一片：多来源作答的提示情境保真

建议先收紧**单个教育候选的 support_level 必须与全部引用证据一致**，不先开放跨代自动合并或扩大自动晋升。原因是一个已定位、范围很窄的真实准入缺口：

`store/memory_policy.rs:149` 对非 unknown 的 support_level 仅要求 `sources.any` 有一条来源匹配。候选可以同时引用同活动两个合法 learner_answer（一个 independent、一个 hinted），写 independent；其他校验只要求两条都是可用作答来源、原句/版本准确。`persist_education_candidate` 的同活动/类型门槛并不消除这个混合提示情境。当前 `tests/memory_jobs.rs:655` 是单个 hinted 来源的正例，没有覆盖这种集合冲突。

这是静态可构造的准入路径，**本次没有运行新的反例，也没有声称此前真实模型已造成这类错误**。既有中英偏好/提示四例把独立错误答案和提示后修正分开保存，说明那些样本未暴露该混合问题，不代表任意多来源候选安全。

最小行为应是：非 unknown 单值只在每条被引用来源都明确声明同一值时允许。混合 independent/hinted/scaffolded/unknown 或缺失支持字段，应拆成分别绑定证据的候选，或采用 unknown 并在正文保留各次作答的实际提示情境。不要按“最大/最小等级”归并，因为这不是一条可信度或能力高低轴。context_constraint 继续固定 unknown；不根据关键词把原来源改类。

修改范围可限 `store/memory_policy.rs` 的小型集合检查、`prompts/memory_consolidation.md` 的明确拆分规则及 prompt version、`tests/memory_jobs.rs`/新独立回归。无新表、服务、模型、向量或 UI 框架，不改默认 disabled/shadow，不启用自动晋升。stage 与 apply 已共用 validate_education_candidate，因此旧 shadow 在晋升前也会重新校验；已晋升的旧记录不在这片追溯重写。

## 可执行验收

先新增真实临时 Store 反例，保持旧 guard 运行并保存失败日志；再修生产集合门限，原断言不改。采用同 owner/sid 的真实 typed 原事件和 job→claim→stage→apply API，不伪造数据库候选行绕过校验。

1. 独立错误作答 + 提示后修正，同时引用并标 independent 或 hinted 均不能晋升为该单一情境；同源正文、引用原句和版本保持真实。两个分别引用自己的候选可保留 independent/hinted；混合集合可按明确 unknown 合法保留，但不因此认可改写正文正确。
2. independent + unknown/缺失字段，不能靠一个 independent 洗成全候选独立；同为 independent 的多个来源、同为 hinted 的多个来源仍可通过，unknown 不强行猜测。
3. 把 importance 四项都设 3、重复引用 ID、增加来源数量，均不能改变上述判定或模拟/学习者报告 grade。重复 ID 沿现有约束拒绝；不是通过重复计算支持数投票。
4. 两候选拆分后，真实来源修订/删除/epoch 变化继续使旧 stage/apply 或向量 CAS 失效；保留当前 same-activity、计划不是结果、current_session 不长期化规则。
5. 默认关闭零调用、前台抢占、完整 schema 和现有单来源正例继续回归；只增加必要的新集合反例，不重写一套调度框架。

随后若根代理安排真实诊断，先冻结中英各两个新混合情境：一次无提示原答案与一次提示后修正；一次明确提示程度与一次未记录程度。每例一次 shadow，无自动重试或 apply，保留完整合成候选供逐项审阅。正确拆分、提示情境/原错误答案/未测新题限制都要核对；零候选虽结构合法但记为覆盖缺失。该结果是合成、方法已知的开放审阅，不是独立盲测、提示因果效果、儿童学习进步或长期能力评估。

完成此片后再考虑跨代综合：先通过单活动多证据情境保真，再设计明确 A→B 来源集合及 B 归属，仍只 shadow。scoped BGE 作为独立检索片继续原计划，不能代替来源类型和作答情境的准入检查。
