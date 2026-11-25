# 小型 A/B 实验比较记录持久化（2026-11-04）

`store/experiments.rs` 提供来源绑定的比较记录，不推断硬件连接、试验成功、活动完成或模块清单。`store.rs` 仅增加模块声明和迁移入口；原文件备份在 `runtime/goal-backups/20261104/architecture_audit/store.before-experiments.rs`。没有调用模型或设备。

## API 对接

路由由 server 接线，复用同源、认证、当前活动和前台 control 约束：

- `GET /api/learners/{lid}/sessions/{sid}/comparisons` → `Store::comparisons(lid,sid)`，返回 `{learner_id,session_id,items}`，最新有效 10 条。
- `POST` 同路径 → 在前台 control 内先调用 `comparison_replay(lid,sid,data)`。该只读方法对首次提交也完整校验格式、归属和会话未结束；返回 `Some` 时直接重放原响应，不反复取消正在进行的新教学。首次返回 `None` 后，由 server 取消旧教学、推进代次、使旧候选失效，再调用 `save_comparison(lid,sid,data)`，保存时事务内重检全部条件。
- 保存和幂等响应均为 `{learner_id,session_id,item}`。`item` 包含固定业务字段、`comparison_id`、`created_at`、实际 `source_type`、`hardware_verified:false`、`submission_id`。GET 可读已结束会话，POST/幂等写入入口拒绝已结束会话。

载荷界限：问题与改变变量各 1–200 字；共同条件 1–8 项、名称 1–200 字、状态 `reported_same|not_confirmed`；试验固定唯一 A/B、设置 1–200 字。`reported` 要 1–1000 字原文结果，次数可未知 null 或整数 1–10000；`not_tested` 的结果、次数都必须 null。`pending_plan` 为 null 或 `{text:1..1000,accepted:bool}`，省略 accepted 等于 false。提交标识 1–96 字，同 session 同规范化载荷幂等；同标识不同内容拒绝。未知字段拒绝，不能自行提交 `hardware_verified:true`。

真实空间只允许 `learner_report|operator_confirmed`，模拟空间强制持久化为 `simulated`；人工确认仍不等于硬件验证。规范化只补明确默认值、将 A/B 排序，不改写报告原文；digest 保留请求原 source 类型，不能用改变来源的请求冒充同一重试。

## 三种来源各自保存

最多生成三个既有教育事件，各自有原文 `text` 与结构化内容：

1. `reported` 试验进入 `observation`，仅包含已报告试验的设置、结果、次数及共同条件。无 reported 项时不创建 observation，不修改当前报告结果。
2. `not_tested` 试验进入另一个 `plan`，只含待测设置、结果/次数为 null，`accepted:false`，不混入观察原文。
3. 用户另填的下一步计划进入独立 `plan`，保留 explicit accepted；接受该计划不会同时接受第 2 项的所有待测条件。仅 accepted=true 的这一个来源更新 `pending_step` 和来源绑定，accepted=false 不进入已接受步骤。

保存 reported 事件时更新 `reported_result` 与有界 attempts（最多 12 条），明确未硬件核验；不改 stage、modules、活动完成状态，不立即创建慢记忆。

`experiment_comparisons` 只保存轻量 receipt：owner/session/namespace、提交及比较标识、规范化载荷 SHA-256、三个来源 ID/版本、时间、active 标志。业务正文只有 events 一份。GET 与幂等响应每次从有效来源重建 A/B 与待测计划，不从 receipt 复制摘要。

来源删除、失效、版本改变会令 receipt 永久失效。恢复旧 metadata 的 active 值或重开数据库不能恢复比较；同 submission_id 的重试也拒绝重建已失效来源。删除学习数据后 receipt 仅保留无业务正文的幂等失效标记，原文随 events 删除；session/learner 行删除则清 receipt。

## 验证边界

新增 11 个不调用模型/设备的临时数据库回归：观察/未测/未接受计划分离；明确接受只绑定其自身计划、不推断完成；幂等不重复 source/快状态；首次非法载荷无副作用；归属/完成状态/模拟隔离；删源/遗忘/重开不复活；来源版本和删除后的最新 10 条；receipt 写入失败回滚全部 source 与快状态；上下文保留 plan+accepted=false 后拒绝模型虚构完成；混合引用拒绝且精确观察/已接受来源仍可写入；结束会话不能将旧快状态中的混合或未接受计划固化。

对应收紧了原有候选入口：`tutor.rs` 拒绝引用集合包含任一 plan 的 `reported_result`/`completed` 更新；显式 `accepted:false` 的 plan 不能支持 `pending_step`。`Store::add_memory` 同样拒绝它升格为 pending_experiment；结束会话的来源集合检查会拒绝这种混合证据，不再逐个挑其中可接受的来源绕过。旧未类型化报告的原文/未来意图验证继续保留。备份 `store.before-plan-evidence-guards.rs` 与 `tutor.before-plan-evidence-guards.rs` 记录修改前状态。

本模块已 rustfmt，Cargo 由 root 统一执行。Store 测试不代替 HTTP 当前活动锁、真实 DOM 提交或硬件验收；server 和 UI 各自完成对应边界验证后才报告整体功能通过。
