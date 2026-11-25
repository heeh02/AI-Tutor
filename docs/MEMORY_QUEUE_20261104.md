# 教育记忆整理队列：第三批可接线基础

日期：2026-11-04。范围为 Store 持久任务、来源校验、候选门禁与上下文元数据；没有调用模型，没有加入运行中的调度器，也没有默认开启任何慢记忆升格类型。仅使用合成临时数据库测试。修改前备份：`runtime/goal-backups/20261104/backend_review/`。

## 模块与数据

`store/consolidation.rs` 管理具体的记忆整理任务和租约；`store/memory_policy.rs` 管理教育候选语义。两个私有子模块共享 Store 事务，不新增 Cargo 依赖或通用作业框架。旧事件、记忆和状态表结构兼容，启动时在原有迁移事务内创建以下三张旁表，可重复开启：

| 表 | 用途 |
| --- | --- |
| `memory_jobs` | namespace、learner/session、generation、epoch、state_revision、来源 ID/版本、prompt_version、幂等键、限额、状态及租约 |
| `memory_job_attempts` | 每次预算预留与 lease token、Shanghai 日期、开始时间和终止原因；不保存提示词或模型原始输出 |
| `education_memory_candidates` | 最小结构化 shadow 候选、归属、对应 job 和应用后的 memory ID |

同一学习者的来源也必须属于任务绑定的 session。已结束的 session 可以整理，前提是 generation、revision、epoch 和来源仍然有效。幂等键包含全部上述快照字段及 prompt_version；前台变更使旧任务失效后，调度器必须按新快照重新入队。

来源删除/更正通过已有事件元数据触发器同步使任务失效，清空 source_snapshot，并将关联候选 payload 置 NULL；不会保留可读原文或模型改写在失效候选中。旧任务和候选不会在重新开启数据库时回填复活。来源删除后应用过的慢记忆继续服从已有来源链接失效/遗忘合同。状态版本或 generation 更新使尚未应用的任务失效；已应用记忆仍按实际来源有效性审计。

## 调度接线 API

所有操作是同步短事务；不要持有 Store 锁等待模型。所有 `now_ms` 必须由服务端可信时钟产生，不能取客户端请求。返回值使用现有项目的 `serde_json::Value` 风格。

1. `enqueue_memory_job(lid, sid, source_ids, prompt_version, &MemoryJobLimits, now_ms)`：事务内获取当前快照、生成幂等键；同键返回已有任务。最多 32 条不同来源。
2. `claim_memory_job(worker, &limits, now_ms)`：返回 `Option<{job,receipt,sources}>`。receipt 含 `job_id/lease_owner/lease_token/attempt_no/lease_until_ms`；sources 是当前原始证据，job/status 不暴露 token。每次 claim 原子预留预算。
3. `mark_memory_job_started(&receipt, now_ms)`：仅在真正开始 provider 调用时记录。claim 不等于模型调用成功，未调用的预留仍显示 started_at_ms=NULL。
4. 前台请求到来时，worker 应先取消/等待模型子进程退出，再调用 `preempt_memory_job`；提供方失败调用 `fail_memory_job`。两者用 receipt 验证租约，当前快照仍有效才进入 pending；达到次数上限为 exhausted。旧 receipt 不能取消或提交新租约。
5. `reap_memory_jobs(now_ms)` 回收超时租约；claim 也会回收。`recover_memory_jobs(now_ms)` 只由持有应用独占启动锁的调度器在启动时调用。单纯 `Store::open` 不抢走存活 worker 的租约。
6. `stage_memory_candidates(&receipt, &[candidate], now_ms)`：最多 5 个；同一事务内重新验证完整快照并验证所有候选。有效候选默认 shadow，任务为 shadow_ready；零候选为 completed。模型调用回执为 response_received，不记录为事实成立或教学成功。
7. `memory_job_status(lid, job_id)`：按学习者返回任务、去 token 的尝试记录和候选。`apply_memory_candidate(lid, candidate_id, &enabled_kinds, now_ms)` 只有显式允许的 kind 可以升格，提交事务内再次核对快照、原文和版本，建立正常慢记忆来源链接，重复应用不重复写入。

默认限额是待调优的保守值：namespace 内 pending/leased/shadow_ready 合计 16，单任务最多 3 次 claim，namespace 每 Shanghai 自然日 20 次预算预留，每 session 总计 6 次，租约 120 秒。父调用方可以配置更低限额；已入队任务的限制不能通过 claim 放宽。取消、超时、崩溃前未调用的预留不返还，以保守约束费用，不能把预留数当模型成功数。检测到时钟早于既有最大 claim 时间，拒绝新 claim，时间回退不会重置日预算。

后台 worker 的提供方 gate、前台优先调度和取消子进程由服务层接线；本批没有引入并行模型调用或后台抢占前台的锁。有限 shadow 队列需要产品策略消费、应用或在新快照时失效；当前无无条件自动升格，也未增加没有实际使用者的清理平台。

## 候选与教育规则

候选必须有 `kind/content/scope/claim_type/source_event_ids/source_event_versions/source_quotes/support_level/importance`。content 最多 600 字，来源 1–8 个唯一 ID，每条原文引用必须是相应证据文本中的实际片段。importance 为 `goal_relevance/continuity/novel_change/explicit_request` 四项 0–3 分，与证据可信度分开。source_grade/source_count 由实际唯一来源重算，模型不能自行提升；重复原文或摘要不能增加独立证据强度。

| kind | claim_type | 升格条件 |
| --- | --- | --- |
| learning_goal | stated_goal | 明确 learner_statement 或 teacher_setting |
| trial_observation | reported_observation | observation；旧存储映射 reported_progress，保留“据报告”限定 |
| concept_evidence | task_specific_response | learner_answer；限定本次回答，不写成已掌握 |
| revisable_misconception | tentative_interpretation | learner_answer；可修订、待确认的理解线索，不贴能力或人格标签 |
| help_preference | explicit_preference | 明确 persistent 偏好证据及 cross_session_explicit 范围；临时偏好不得升长期 |
| accepted_pending_experiment | accepted_plan | plan 且原证据 accepted=true；映射 pending_experiment，不能写完成事实 |
| correction | explicit_correction | 版本化 correction 来源；原事实失效由事件更正机制完成 |

助手建议、模型摘要、记忆摘要不能当原始学习证据。计划不能成为试验结果或理解证据。support_level 只接受 unknown/independent/hinted/scaffolded；非 unknown 必须有原始结构化记录，不能从语气推断。规则不从声音、脸、文字风格推年龄或能力。语义忠实性仍需后续 prompt 和评估审核，原句存在不代表模型改写一定正确，因此默认 shadow 是必要门禁。

已升格的记忆通过 `memory_evidence.education_metadata` 和 context 中同名字段提供 kind、claim_type、support_level、importance、source_grade/source_count，方便模型区分事实报告和有限推断。保留旧活动 `modular_rover` 协议；本批没有扩大活动范围，也没有硬编码中英词典来掩盖之前的两个跨语召回缺口。

## 验证范围

新增 `tests/memory_jobs.rs` 共 11 项：幂等/队列上限/抢占旧回执、租约超时/显式重启恢复、上海日预算/回退、结束后整理/类型门禁、遗忘清除候选/重开、前台 revision 失效、新版本/真实引用/提示证据/可信度、计划非完成/临时偏好、显式更正清除派生、概念回答证据、跨活动/跨档案/跨空间隔离。

本批编写代理未运行 cargo、模型或设备。根代理已经执行 `runtime/goal-baselines/20261104/memory-jobs-first.log`，本文件对应 11 项全部通过。前两批已验证结果见 `MEMORY_FOUNDATION_20261103.md`。

后续 shadow worker 接线基础见 `MEMORY_WORKER_20261104.md`；本文的队列持久化合同保持不变。
