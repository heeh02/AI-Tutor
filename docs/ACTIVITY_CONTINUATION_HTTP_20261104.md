# 显式继续已结束活动：HTTP 接线

目录 `GET /api/learners/{lid}/sessions?before=<正整数>&limit=20` 只读、不切换档案。`limit` 支持 1–50，默认 20；返回 `learner_id`、`namespace`、`items` 和 `next_before`（整数或 null）。行包含 `session_id`、`activity_id`、`created_at`、`status`、有有效来源支持的 `goal` 文本及 `continued_from`。已有按档案/会话读取对话历史的接口保持原样。

`POST /api/learners/{lid}/sessions/{old_sid}/continue` 必须带 `client_submission_id`、`expected_current_session_id`、`expected_generation`。关联标识限 1–96 个 ASCII 字母、数字、下划线或连字符；原当前会话及代次构成精确意图，不能用活动名称、模型猜测或“继续”文本代替用户选择。来源必须是同一档案已结束活动。成功新建会话和活动编号，响应为 `{learner_id,session_id,activity_id,client_submission_id,replayed,session}`。

完整 `session` 和 `/api/state` 中的当前会话均回显 `continued_from`，以及 `continuation.client_submission_id/source_session_id/source_activity_id/selection_event_id`。历史目标/待做步骤由 Store 解析有效来源；新活动不复制旧的硬件配置，仍需确认。教学语言和指导设置沿用当前档案设置，不把历史活动字段当新设置。

服务端按 `camera_control → control` 获取现有控制锁。Store 的 `continuation_replay` 是完整只读预验：返回 None 前核对来源已结束、档案/空间归属及原当前会话代次；返回已有回执时还核对原载荷和有效来源。Workstation 再核对真正的 runtime current。所有无效请求和有效幂等回放都在 foreground、云台停止、采音/播报失效、轮换暂停或 Pending 清理之前返回。

首次有效选择才失效当前采音/播报并取消旧教学，原会话只通过既有 cancel 递增一次代次，再把实际取消后代次和未改写的原请求代次分别传入 Store 事务。Store 再校验后创建新身份/来源关联；提交成功后直接发布 current 和响应，不在提交后执行可失败操作。云台只沿用停止流程，不主动启用或移动。

同 nonce、同来源和同原载荷的重试，仅在 runtime current 仍是该操作创建的会话时只读返回；当前会话已有新教学进展、输出或已结束也不能因此被重置。切走其他活动后，旧重试拒绝并且不会重选旧结果。旧来源的 `activity_id` 和 `session_id` 仍可读历史，但不是写入新活动的别名。

客户端可将明确 4xx 与网络/5xx 未知结果分开。未知结果只能以同 nonce/同载荷核对，并可通过 state 中的 continuation nonce 认领；拒绝后刷新真实 state，不靠错误文案或旧响应重新选择。没有引入通用请求幂等或事务框架。

新增五项使用临时真实 Store 的测试（已纳入根代理统一回归并通过）：

- `explicit_continuation_creates_new_identity_and_replay_preserves_new_work`：真实 HTTP 首次续做、纯重放、冲突载荷、新 Pending/代次/音频 epoch 不受旧请求影响、旧 aid/sid 写入拒绝。
- `invalid_continuations_preserve_current_generation_pending_and_audio`：过期代次、错误当前会话、其他档案/空间、未结束来源、缺字段/额外字段/无效 nonce 与未认证请求均无副作用。
- `continuation_late_retry_after_new_activity_never_reselects_the_old_result`：实际新活动 dispatch 之后旧重试不重选。
- `continuation_replay_of_finished_current_activity_is_read_only`：实际 finish 后当前仍为同会话时幂等核对不重开。
- `activity_catalog_http_is_owner_paged_and_read_only`：实际路由认证、游标分页、来源档案隔离、无效分页参数以及只读状态保持。

根代理统一回归 `workspace-thirty-eighth.log` 为760通过、0失败、7忽略，无警告。新原生程序的 `activity-continuation-first/report.json` 通过10类无效请求及完整state不变检查；有效首次选择、后续新进展中的幂等回放、已结束当前会话的只读回放、切换后旧重试拒绝、旧sid/aid写入拒绝、有效关联与忘记后断链跨重启均通过。数据库仅含独立合成数据；新版→旧版→新版流程验证旧版启动/档案读写保留六张来源及关联表，不涵盖所有降级操作。四次实际进程均正常退出。

测试不请求模型，不打开输入/输出设备或相机，不启用PTZ。state回显关联已验证，但未模拟浏览器丢包；目标/计划字段的来源语义另由Store回归覆盖。修改前备份：`runtime/goal-backups/20261104/backend_review/activity-continuation/server.rs`。
