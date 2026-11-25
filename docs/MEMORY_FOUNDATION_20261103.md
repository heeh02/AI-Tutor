# 教育记忆基础与可复现基线（2026-11-03）

本报告按两批记录 P0/P3 的记忆正确性基础。全部语料为合成文本，测试只创建临时 SQLite；不读取真实学习数据，不启动设备，不调用远程模型。首批不新增数据库结构；第二批的旁表、来源版本与偏好设置见后文。完整异步 LLM 整理器不在本模块交付范围。

## 已实现

1. `Store::memories()` 继续返回最新 100 条有效记忆，保持现有 HTTP/UI 合同。
2. 新增 `Store::memory_page(learner_id, before, limit)`，返回 `(items, next_cursor)`；每页 1–100 条，通过 SQLite 行号游标向旧记录翻页。翻页间新插入记录不会挤动或重复已读旧页；归属过滤始终保留。游标不混入原记忆 JSON。
3. 召回、重复候选检查、模型更正的旧来源匹配改用独立的全部有效记忆集合。显示页的 100 条上限不再截断这些业务判断。
4. 召回遇到超出剩余字符预算的候选时继续检查后续候选，直到足够条数或候选耗尽。仍保留最多 5 条、共 2400 字符的现有限额。

修复前，可构造“旧记忆 + 105 条新记录”：旧记忆存在于数据库，却无法通过召回找到；重复提交会产生重复记录；引用该旧来源的模型更正会因首屏内没有被更正对象而被拒绝。新测试覆盖数据库重开、教学上下文、去重和更正来源失效。

## 活动范围决定

本轮保留 `modular_rover` 边界。当前会话表没有活动类别字段，候选协议、写入路径与 `context.activity` 都固定为该类别。直接去掉召回过滤会把其他课程记录混进来，不能当作通用教育支持。

后续需要先建立明确的活动类别来源、迁移旧活动的规则、写入校验与按课程召回合同，再支持一般教学主题。不得根据模型临时生成的活动名称自动扩大读取范围。

## 基线语料与执行

- 语料：`rust/ai-tutor/tests/fixtures/education_memory_v1.json`。
- 测试：`rust/ai-tutor/tests/education_memory.rs`。
- 数据集声明 `version=1`、`synthetic_only=true`。
- 8 个检索用例：中文、英文、英文大小写、混合表达、英问中记、中问英记、无关主题、明确活动续接。
- 2 个更正与删除用例：中英文分别验证来源撤销、旧内容不返回、删除后重启与迟到候选不可复活。
- 2 个计划用例：中英文分别验证已显示建议不能被保存成已完成试验；明确的下次计划只作为 `pending_experiment`。
- 另外验证 101 条以上、稳定分页、上下文证据与跨档案/空间/活动隔离、预算跳过。

统一构建环境下运行：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-rust.ps1 test --locked -p ai-tutor --test education_memory -- --nocapture
```

检索用例会输出 `EDUCATION_MEMORY_BASELINE` 单行 JSON，包含每例的期望、实际、语言、是否通过及是否为硬门槛。纯跨语两例是已知语义能力缺口，允许作为失败的评测项报告；测试进程通过不能解释为跨语能力通过。其他检索项与独立安全回归必须满足断言。

本轮不调用 LLM，所以结果仅衡量存储、检索和构造上下文；不衡量模型生成的教学内容、儿童学习效果、自动重要性判断或实际音视频体验。

## 验证状态

首批已由根代理实测：`runtime/goal-baselines/20261103/education-memory-first.log` 中 8 个测试通过；基线 JSON 显示 6 个必过检索项通过，英问中记与中问英记两项实际均未召回。这两个缺口明确保留。对应整套 `workspace-second.log` 为 285 通过、0 失败、1 忽略；这些数字只对应首批当时版本，不能自动用于后续迁移代码。

测试名称：

- `memory_recall_and_deduplication_reach_beyond_the_display_page`
- `memory_pages_are_stable_across_new_inserts_and_keep_legacy_shape`
- `model_correction_invalidates_memory_older_than_one_hundred_records`
- `recall_budget_skips_oversized_candidates_but_keeps_searching`
- `bilingual_retrieval_corpus_reports_semantic_gaps_without_claiming_model_quality`
- `bilingual_corrections_revoke_sources_and_deleted_memories_do_not_return`
- `bilingual_plans_and_displayed_suggestions_are_not_completed_experiments`
- `full_history_search_still_isolates_learner_namespace_and_activity`

## 首批后续接口与限制（第二批进展见下文）

1. `memory_page` 已提供 Rust 接口；HTTP 游标参数、界面“加载更早记录”和 CLI 分页尚未接入。现有界面仍只显示最新 100 条。
2. 全量检索当前是完整有效集合扫描，不是向量检索。修正了静默漏记录，但总成本仍随该档案的记忆数量增长；后续需评测索引方案，不能恢复静默截断。
3. 中英跨语语义召回、前台优先的异步整理队列、重要性判定、版本化候选、结构化事实/推断类型尚未实施。
4. 现有小车活动名称匹配只能作为范围提示，不能替代语义相关性。本基线故意使用不含活动名称的跨语问题，避免靠“rover/小车”范围放宽伪装跨语能力。
5. 尚未新增记忆结构或整理任务 schema。后续设计必须先与前台回复协议和 epoch 删除链路对齐：事件 → 快状态 → 后台候选 → Rust 校验 → 事务写入；后台结果不能绕过来源、归属、撤销检查。

修改前原件保存在 `runtime/goal-backups/20261103/backend_memory/`。

## 第二批：教育事件与状态版本基础

经根代理确认，已增加三个旁表，不改变原 `learners/sessions/events/memories` 的列数或旧插入合同：

- `education_event_meta`：事件的档案、会话、空间、明确类型、版本、状态与被替代事件 ID；不复制原文。旧事件只标记为 `legacy_report`，不补人格、情绪、能力等推断。
- `memory_source_links`：记忆与事件版本的来源边，包含档案、空间和有效状态。更正/删除会使相关边失效；重开回填使用 `INSERT OR IGNORE`，不会把已失效边恢复为有效。
- `session_state_versions`：只保留当前状态修订号，不保存可能包含已删除信息的历史快状态副本。数据库触发器覆盖所有 `sessions.state` 更新，包括旧 `checkpoint` 和遗忘清理路径。

基础表初始化、旧数据回填、旁表建立和启动恢复统一事务提交。来源读取同时验证事件、会话与档案归属、空间、状态；记忆解析额外检查事件版本。原事件内容不可原位改写；明确更正创建新事件 ID/版本并失效旧来源派生内容。

新增可供后续前台和异步整理接入的 Rust API：

- `record_education_event`：写入明确的学习者陈述、回答、观察、计划、教师设置或更正；教师设置须由教师确认，模拟空间仍保留模拟来源等级。
- `revise_education_event`：创建下一事件版本，撤销旧来源关联的快状态和慢记忆；不把更正过程变成已完成实验。
- `memory_evidence`：返回当前有效记忆、实时原始来源及版本边，拒绝错档案、错空间、过期或不完整来源。
- `checkpoint_if_current`：在同一事务中验证档案、会话 generation、state_revision、epoch 和来源再更新；适合后台结果提交，拒绝排队期间被新输入、删除或更正替代的旧状态。

`add_memory` 继续兼容旧候选；新候选如携带 `source_event_versions`，必须与所有引用事件版本一致。明确的 `plan` 事件不能升格为 `reported_progress`，也不能通过新 CAS API 生成已完成状态。旧协议没有事件语义类型，因此不声称已解决所有自然语言“计划/完成”判断问题。

选择性遗忘由“清空事件再插回保留项”改为“只删除应删除事件”，避免保留来源被误失效或重新创建。对话、模型运行记录与快状态仍按原合同清理。已失效来源元数据不包含原文；遗忘时清除该档案已删除事件的元数据。

`context.rs` 已将来源类型/版本/状态与慢记忆来源版本放入上下文，并通过 `memory_evidence` 检查来源。前台事件写入和快状态更新尚未全部切换到新 typed/CAS API；本段交付的是可接线基础，不代表异步整理已上线。

第二批测试位于 `tests/education_provenance.rs`，共 9 项，覆盖：旧库迁移、迁移失败完整回滚与重试、重新开启不复活、版本/归属/空间、显式更正派生清理、选择性删除与迟到 CAS、并发状态修订、计划非完成、偏好设置、首次学习前的空间绑定。根代理已实测 `runtime/goal-baselines/20261103/workspace-third.log`：304 通过、0 失败、1 忽略；其中本文件 9 项全部通过。

## 独立偏好设置基础

随后按根代理要求增加 `learner_preferences` 旁表，保留旧学习者列结构。`reading_level` 为 `plain/standard/technical`，默认 `standard`；`speech_speed` 为 `slow/normal/fast`，默认 `normal`。`learner()/learners()` 返回有效值，`update_learner_preferences` 支持部分更新，未提供字段保持原值，非法值不产生部分写入。

上下文加入阅读复杂度与表达规则：plain 短句、少术语；standard 常规解释；technical 可以术语并解释必要概念。不得把设置解释成儿童能力、年龄或人格判断。工作台界面语言不写入学习者偏好，也不传给教学模型；教学语言仍来自原学习者语言设置。实际播报速度如何应用由音频/服务层另行接线，本模块只提供持久设置。

第二批修改前备份在 `runtime/goal-backups/20261103/backend_memory/stage2/`。第二批新增代码在 `rust/ai-tutor/src/store/education.rs`，不修改 `lib.rs` 或 Cargo 依赖。
