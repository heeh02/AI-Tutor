# 后台教育记忆 shadow worker

日期：2026-11-04。此批实现可调用的一轮 worker、提供方前台优先机制、教育候选 prompt/schema 和纯合成检验入口。未运行真实模型或设备。没有把真实学习者数据库默认交给后台模型；服务端调度与 typed 事件接线由根代理负责。

## 已实现

`memory_worker.rs` 是具体的一轮教育记忆任务，不启动自动循环。`MemoryWorkerPolicy::default()` 为 Disabled，立即返回且不 claim、不计预算、不调用 provider。SyntheticOnly 仅接受 simulator 数据空间且每条原证据显式有 `content.synthetic_fixture=true`。OptInReal 是单独的显式策略值，需要服务层明确启用；这个枚举本身不代表产品已经完成用户同意流程。

`MemoryWorker::run_once(stores, provider, namespace, policy)` 在 Store 短事务中 claim，释放锁后调用 `MemoryConsolidator`，随后在新事务中 stage。返回只有状态、任务 ID、数量和实际开始标志，不复制模型原文。没有自动 apply；即使模型响应全部通过，也只保存 shadow。来源、版本、session 快照、lease token 和类型约束最终仍由 Store 校验。

`MemoryWorker::foreground()` 返回拥有内部引用的 `ForegroundGuard`，可放入前台 task 或 Pending 并一直保留到投递结束。进入时在短 control 锁内递增计数并通知后台取消；worker 提交也持有这个短锁，保证前台已经进入时后台不能提交。Guard 的 Drop 只做原子减计数，不取 control 锁，因此在服务层持有 Store 锁移除 Pending 时不会产生 Store→control / control→Store 的锁序死锁。多个前台 guard 可以重叠；全部释放后才允许下一轮后台 claim。

worker 取消时通知 provider；已收到实际开始回执的调用最多等待 2 秒走正常取消/回收路径，未开始的 future 直接丢弃，避免为清理而反过来启动已取消任务。迟到响应被忽略，然后恢复有效的 pending 租约或保留 Store 已 invalidated 的状态。90 秒截止涵盖整个 provider await；worker 要求租约至少 100 秒，覆盖取消清理余量。不存在 Store 锁跨 await。若 worker task 被外部直接 abort，其未结租约由原队列的启动恢复/超时回收解决，不暗中复活旧快照。

## 同一 Kimi 通道

`KimiProvider::consolidate(request, started_signal)` 复用现有 invoke 的无工具环境审计、官方已登录提供方、单 semaphore、隔离工作目录、Windows Job Object、并发有界 stdout/stderr、90 秒截止和完整响应解析。没有第二个 provider，没有自动重试子进程，也没有绕过原核验。

私有 `register_call` 在同一个 calls 锁内完成以下操作：前台登记并通知所有后台取消；后台检查没有已登记前台后立即 try_acquire gate。后台不能进入 semaphore 等待队列，也不能越过已登记但尚未取到 gate 的前台。前台等正在退出的后台清理后继续。worker 的 foreground guard 与提供方本身的优先机制各自覆盖其负责边界。

Kimi 只有实际 spawn 并绑定 ProcessJob 后发送 started=true；worker 此时才写 started_at。预算预留、进程开始、响应收到和记忆成立仍分开。`consolidation_requests` 只在后台进程实际启动后增加。没有将预留或模型响应统计为儿童学习成功。

`prompts/tutor-agent.md` 只调整一行输出措辞：从固定 TutorReply 改为遵循本次输出 schema；tools/subagents 空列表保持。**文件 hash 已改变，真实诊断前必须按现有审计机制重新核验新 hash**，不能把旧 marker 视作新 agent 的验证结果。备份在 `runtime/goal-backups/20261104/backend_review/worker/`。

## 事件与候选合同

提供根代理的最小 typed 接线：普通 submit 使用 `record_education_event(..., "learner_statement", {text})`；显式 activity report 的非空 report 使用 observation，只有模块清单则仍为 learner_statement。直接取返回对象的 event_id 继续原状态流程。不能从自由文本自动判定 learner_answer、已接受 plan、独立完成、持久偏好、能力或年龄。

`memory_contract.rs` 提供版本 `educational-memory-v1`、JSON schema 与外层响应校验；`prompts/memory_consolidation.md` 定义七类候选、来源原句/版本、0–3 教育重要性、提示程度和保守语义。prompt 不将普通陈述升级成答题/计划/结果，不将助手建议写成儿童完成事实，不将讲解写成掌握。重要性分值不能改变来源可信度。中英原文按来源语言保留；没有添加跨语关键词词典。

## 合成真实检验入口

`memory_worker::run_synthetic_probe(&verified_provider, "zh" | "en")` 是显式调用入口，不被创建 worker 自动触发。每次新建临时 simulator 数据库，只使用内置合成观察，预算最多一轮、一个任务、一次尝试；正常结束或取消后删除已知临时 SQLite 文件。它不打开任何已有学习者数据库，也不把 model raw output 放入返回报告。真实调用需要已完成新 agent hash 审计的 KimiProvider。根代理可将它接入明确的开发诊断命令；此批尚未远程执行。

## 验证状态

此批新增 11 项测试：`tests/memory_worker.rs` 8 项（默认关闭/未标记数据拒绝、shadow-only、前台抢占迟到响应/恢复、首次 poll 前取消零调用、删除后不提交、提供方错误/版本错误、协议上限、中英纯合成入口）；worker unit 1 项真实 Store 锁顺序；Kimi unit 2 项同一 gate 前台优先/后台不得排队。

根代理已验证前 10 项，`runtime/goal-baselines/20261104/workspace-second.log` 整体 351 通过、0 失败、2 忽略，其中 memory_worker integration 7 项通过。随后新增的“首次 poll 前取消零调用”用例及其修复等待第三轮统一验证，不能沿用前一轮结果当作该修复已通过。

上述测试只用 fake provider、临时 DB 或本地门控，不读取认证、不启动模型、不打开设备。已用 rustfmt 解析和格式化新增/修改 Rust 文件。上一批 queue 的 11 项已由根代理验证全部通过，日志 `runtime/goal-baselines/20261104/memory-jobs-first.log`。

根代理后续实际执行中英单事件合成入口：`memory-shadow-real-zh-first.log` 产出 0 条候选，`memory-shadow-real-en-first.log` 产出 1 条；都实际开始 provider、完成 shadow_staged，未 apply。该结果只确认真实链路可用，不能算作教育连续性或召回质量通过。调度器和更有连续性的三事件评测建议见 `MEMORY_SCHEDULER_20261104.md`。
