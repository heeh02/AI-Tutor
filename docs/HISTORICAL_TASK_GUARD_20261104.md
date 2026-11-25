# 历史证据与当前快状态写入边界

先增加回归并调整旧测试期望，根代理运行 `--lib tutor::` 保存 `historical-task-guard-before.log`：6 项通过、5 项失败，exit 1，无构建错误；五项失败均是预期的历史来源写入边界。随后实施下述窄修复，新增回归断言不变。根代理的 `workspace-fortieth.log` 已完整通过：775 项通过、0 失败、8 项默认忽略，含 UI 128，49 个测试汇总，无警告，耗时 88.47 秒。

真实诊断 `tutor-store-context-first/result-descendant_completed.json`（SHA256 `850121a0e4c6c8b30453ecf5cc343c8fbbd9ee714993da890877ce1a6f917964`）正文能够区分旧计划和后来完成报告，但同时提出两个 task_update_candidates，仅引用旧 B 的完成来源，把新 C 的 stage 写为 completed、reported_result 写为 B 的完成报告。诊断没有应用候选，真实用户数据未受影响。

修复前 apply_reply 会校验 owner/live source，却允许跨 session 来源写当前 goal、stage、reported_result、pending_step；仅加 historical 标记不足以阻止 current stage=completed。混入当前 question 后，all historical 判断还会变 false，把相同旧完成报告标成当前。current_session help_preference 同样只核来源存在，存在相同归属缺口。

已实施生产窄规则：所有当前 task 字段和 current_session help_preference 的支持来源都必须属于当前 sid；混合旧/新来源整体拒绝，不用主题、完成词或其他文本规则推断。两处共用一个小型纯检查函数，检查来源非空、引用完整且每条实际 session_id 相等。继续保留现有 owner/namespace/epoch、计划接受状态等检查；保留输出协议的 historical:false 字段，去掉不可达的历史快状态赋值分支。该规则只解决写入位置，不宣称证明当前问题或报告与候选语义相符。

历史事实仍可用于回答和来源绑定记忆。合法 B 来源的 reported_progress/current_activity 必须保留 B 的实际 activity_binding，不能因为正在 C 回答就改绑 C，也不能随 task 拒绝一并丢弃。明确持续偏好的跨会话规则由现有 Store 单独维护，不改成当前临时偏好。

新增真实临时模拟 Store A→B完成→C续做六项回归：实际两 task 候选及合法历史 memory；四个当前 task 字段全部拒绝历史支持；历史完成混当前 question 仍拒绝；有效当前报告可更新并保留 simulated；当前临时偏好拒绝旧/混合来源；有效当前偏好和 B 历史记忆保持各自作用域。实际候选原文保持，仅将 opaque event IDs 映射到新临时来源；未读取或改写冻结请求/响应。

旧 `historical_task_candidate_retains_original_simulation_grade` 改为“不复制历史报告进当前快状态”的契约。来源级别保留改由有效当前报告和 B 历史记忆控制组验证，不再用允许历史覆盖当前来证明 grade 正确。

原 tutor.rs 备份：`runtime/goal-backups/20261104/architecture_audit/historical-task-guard/tutor.rs`，SHA256 `777aa4b23a998abd37e60133f1f8d00b494ce2c9f50134e832b38246c4b5521e`。含失败回归但未修 guard 的版本另存同目录 `pre-fix-tutor.rs`，SHA256 `9276df96abb436f1cfd2ed54df969fcc04ee269902da3b67ff847a967f5f3d3e`。本片只修改 tutor.rs、新增 tutor_historical_tests.rs 和本文，未改 Store 慢记忆路径、冻结评测请求/响应或测试断言来规避失败。
