# 条件与推断边界记忆：设计与最小实现

最初建议来自已冻结 60 条挑战来源和类型策略的交叉检查；现已获授权完成下述最小实现，五项新增 Store 回归已通过统一测试。没有改变现有 42 条投影、编码、标签、报告或生产自动晋升默认，也没有运行模型。新增 `context_constraint / condition_limit` 这一对 kind/claim_type，用于明确表述的特定任务条件、未控制变量与文本推断边界。它记录“来源说了什么限制”，不升级为学习者能力、一般科学结论或长期偏好。

## 现有类型不能无损代替

原 `store/memory_policy.rs` 的七类均有不同含义：learning_goal 是学习目标；trial_observation 晋升必须来自 observation；concept_evidence 与 revisable_misconception 必须来自 learner_answer；accepted_pending_experiment 必须来自已接受 plan；help_preference 要有明确持续偏好标记；correction 必须来自更正事件。现有六条 condition_limit 原始事件均是 learner_statement。改成 observation/learner_answer 或把条件写作 learning_goal 会伪造来源含义。旧 reported_progress 的宽泛正文也不能替代一个清楚的类型合同。

六条缺口并不全是物理实验：发芽涉及水量与温度未控制；纸桥涉及跨度和折法同时变化；分数饮料涉及配比与口味同时变化；阅读涉及线索不足以确定原因；循环涉及初值不同；蒸发涉及位置、容器、温度和气流边界。因此 `context_constraint` 比“实验结果”或“能力限制”更准确。

## 最小类型合同

- kind=`context_constraint`，claim_type=`condition_limit`；正文只表达原来源明确给出的任务条件、缺测/未控制项或推断限制，保留具体对象、数值和否定。首次独立诊断直接保留原文，不新增推断或改写查询。
- 初片只接受真实 `learner_statement` 来源，沿用 exact quote、source ID/version、有效状态及 owner 检查。不要允许 plan、assistant_suggestion、model_summary 混入该类型；教师设置以后若有具体需要再独立扩展。
- scope 仅 `current_activity`，support_level 固定 `unknown`：该类型本身不记录独立作答、提示效果或掌握程度。不得写入 reported_result、completed、pending_experiment 或帮助偏好，也不触发教学状态完成。
- 持久正文可用简短限定“特定任务的条件与推断边界：…”。该前缀表示报告的边界，不意味着已做传感器验证。正文需要足以在单独召回时定位原任务，不把“此处”“另一个”变成无上下文断言。
- `source_grade` 和 `source_count` 继续由 Store 从真实来源计算；importance 四项 0..3 只表示用途。高重要性、重复召回、模型相似度都不提高可信度。
- 继续通过 `apply_memory_candidate(..., enabled_kinds)` 显式启用；新类型可以先在独立合成诊断允许，实际 worker 仍只产生 shadow，不顺便打开生产自动晋升。

来源校验能验证出处、版本和类型，不能证明候选正文的科学正确性。现有 prompt 仍须明确禁止把一句能力/人格描述包装为条件限制；本片不建立关键词“真伪识别”或新的通用分类器。

## 复用范围与必须保留的边界

`memories.kind` 和候选 kind 是 TEXT，无枚举 CHECK，不需要新增表或重建缓存。现有 source links、source version、learner epoch、修订/忘记/删除触发器及 embedding CAS 已按来源关系工作，可直接复用。撤销一个来源应使相关记忆失效、旧 embedding receipt 被拒；不能靠新增 kind 绕过这些关系。

目前活动仍固定为 `modular_rover`，`current_activity` 不是“只在这一次比较中始终成立”。Store 与 embedding eligibility 都按这个活动范围过滤；不能据此宣称已支持六种真实活动的独立授权。现有 context 已把 slow_memory 标为 historical、保留 kind/education_metadata/来源版本，并注明不是当前硬件。条件记忆也必须保持这个历史含义，不自动把旧水量、旧文本线索或旧初值应用到新任务。若未来需要真正多活动隔离，应作为独立活动 ID 改动处理，本片不顺带扩展。

更正使用既有 `revise_education_event`：旧条件来源失效，新更正仍是 correction 来源，先按 correction 类型记录，不伪装成新的 learner_statement。用户明确新表述才能产生新的条件候选。重复 reopen、晚 CAS 或重复 apply 都不能复活旧条件。

## 修改边界

| 文件 | 必要变更 |
|---|---|
| `rust/ai-tutor/src/store/memory_policy.rs` | 加 kind/claim 对、learner_statement-only 晋升条件、scope/support 要求和限定前缀；保留现有类别门禁 |
| `rust/ai-tutor/src/memory_contract.rs` | schema 枚举增加新对；为扩展合同更新 schema/prompt 版本，使 job 身份不沿用旧语义 |
| `prompts/memory_consolidation.md` | 增加新类型定义，窄改“learner_statement 仅能目标/偏好”的旧句；明确历史条件、未知信息、非能力/非完成 |
| `rust/tutor-ui/src/memory_evidence.rs` | 新增“任务条件与推断边界”标签，继续分别显示来源等级、活动范围、原句；不展示为验证结果 |
| `rust/ai-tutor/src/bin/memory-education-probe.rs` | 独立诊断认识新类型仅允许 learner_statement，并检查 scope/support/claim；不修改旧 fixture 的覆盖期待 |
| `rust/ai-tutor/tests/memory_jobs.rs` 或新的独立测试 | 真实 Store 类型晋升/拒绝/来源失效回归 |
| 新版本独立 projection 方法/探针 | 以后才允许六条新类型入库并冻结新增编码输入；不修改当前冻结 42 条产物 |

不需要改 `store/embeddings.rs`、主召回排序、teacher 默认生成、音频或新增数据库迁移。`context.rs` 已泛化投影类型元数据与 historical 标记，先补回归即可，不为此复制另一条上下文路径。

有意义的最小回归应覆盖：真实 learner_statement 的条件原文可以晋升；同内容的 plan、learner_answer、跨会话偏好范围或非 unknown support 被拒；全部 importance=3 仍保留 learner_report/simulated 等级；源修订、忘记、晚 embedding CAS 与重新开库不能复活；context 不产生 completed/掌握字段且仍标历史；同 namespace 两个非空 owner 不互相召回。另以阅读推断边界和循环初值为例，确认新类型不是仅凭实验词汇硬编码。

## 实现与验证

新增类型在 Store 的 stage 和 apply 再校验时均要求所有来源为 learner_statement、scope=current_activity、support_level=unknown，混入未接受计划也会被拒。正文前缀为“特定任务的条件与推断边界”；现有来源分级、引用版本、importance 四分项和撤销机制保持不变。没有增加关键词分类器，也没有假装通过规则验证正文的科学正确性。

schema 升为 `educational-memory-v2`，prompt 升为 `educational-memory-prompt-v3`。旧 schema 响应被拒；旧 prompt job 保留原身份，当前 worker 的既有版本门禁不变。UI 增加中英类型标签。`allowed_memory` 已通用接受 current_activity，调用方 `enabled_kinds` 仍决定晋升，没有额外自动启用列表要修改。

新 `rust/ai-tutor/tests/context_constraints.rs` 共五项回归：参数化真实 Store 晋升/显式启用/三种来源等级/历史 context 不改教学状态；错来源与混合计划拒绝；scope/support/claim/引用/版本拒绝；来源更正和重新开库后拒晚 CAS/重复 apply；两个实际非空且同正文 owner 的隔离及单 owner forget。缓存测试只用合法合成 384 维单位向量验证来源生命周期，不是模型性能测试。

另窄改 `tests/memory_worker.rs` 的版本回归和 UI 既有标签回归。根代理 `workspace-thirtieth` 已完整通过 710 项、0 失败、5 忽略（含 119 UI），其中新增五项 context_constraints 全部通过。随后补充教育 probe 的类型诊断和 prompt 尾部 unknown 例外，最终 `workspace-thirty-first.log` 再次 710 通过、0 失败、5 忽略、无警告，实际退出码 0，耗时58.828秒。`wasm-constraints-first.log` 的新中英标签界面构建成功，耗时20.359秒；构建不代表浏览器视觉验收。

本片不继承既有 55/60 成绩；没有验证 LLM 的新类型提取，也没有运行扩展为 48 条记忆的编码与召回。原投影 probe/method 的 SHA 仍为 `b76c7ef3…` / `3b059ba5…`，原 typed Store probe/method 仍为 `21e07f0c…` / `4d4a9fff…`。修改前备份在 `runtime/goal-backups/20261104/architecture_audit/context-constraint/`；最后两处在同级 `context-constraint-final/`。最终教育 probe SHA `6cfc6ba73e1ab6e591d6d499a2067930d2973dd582604375f7cf6a262ddcfe05`，prompt SHA `cae205092acea48afef434455ea7d50330a219939621f90f01bdb20792e84185`。
