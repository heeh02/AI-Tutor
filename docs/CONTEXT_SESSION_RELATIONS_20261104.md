# 上下文来源的真实会话关系

本片已经根代理统一验证：workspace43 为 786 通过、0 失败、9 忽略（含 UI 128），五项新来源关系测试均通过。作者仅运行 rustfmt，模型和设备由根代理统一调度；不继承旧教学诊断通过结论。

每条 evidence 在组成完整记录时增加两个派生字段：`session_relation` 为 current、selected_ancestor 或 outside_selected_chain；`ancestor_depth` 对当前会话为 0，明确选中的父活动为 1、更早祖先逐级增加，链外为 null。这些值由实际来源 session_id 和有效选择链决定，不由文字、记忆类型或 historical 布尔值猜测。同会话慢记忆虽然仍标 historical:true（摘要），其关系仍是 current/0。

Store 新增内部只读 selected_session_depths，每次上下文构建读取一次；复用 continuation_chain 的 owner/namespace、来源状态与版本、实际选择事件、身份和顺序校验。只返回当前与有效祖先，不查询、枚举或统计旁支。忘记使选择来源失效后即断链；随后用新编号选择仍有效的后代，也不跨过断链恢复更早祖先。

上下文 evidence_scope.activity_recall_scope 仅说明活动记忆召回范围为当前及用户明确选中的祖先；合法跨活动持续偏好由原 Store 规则独立授权，标 outside_selected_chain，不伪装祖先。该标记不表示旁支数量、其他活动存在或不存在，也不放宽召回权限。旧数据库已有 task.field_evidence/session_preferences 的必要来源可能在召回链外；existing_required_state 明确要求按每条 session_relation 审阅，不声称所有既存快状态事实都位于召回范围。本片不迁移或清除旧状态，也不改变原历史文本与当前硬件/完成状态的区分。

固定 scope 在第一次 fits 前放入初始上下文；每条来源关系在 source() 建立记录时加入，因此完整记录预算、去重和最终检查看到的是实际完整长度。没有先选完内容再追加元数据的路径。

五项真实临时 Store 回归覆盖：当前/父/祖父、同 sid 慢记忆和旁支不泄漏；独立合法持久偏好关系；forget 断链及新选择不能越级；大量完整来源记录的预算选择；兼容旧历史快状态时，链外必要来源仍如实 outside/null 标注，召回仍为空且没有虚假的全事实范围承诺。旧方法/数据/模型输入结果不修改，后续边界教学实验应新建并冻结准备目录。

改动范围：`src/context.rs`、`src/store/activity_continuation.rs`；新增 `tests/context_session_relations.rs` 和本文。原件备份在 `runtime/goal-backups/20261104/architecture_audit/context-session-relations/`。未改排序、prompt、server、UI 或 Cargo。

新的 `tutor-store-context-relations-first` 冻结十二项准备，只执行原第二批六边界。首批前四例结构及语义通过，第五例因引用本轮不可用来源而失败；用原 CLI 的 `--case` 仅首次执行尚未尝试的第六例，结果通过。完整分母为五通过、一协议失败，没有重试或遗漏失败例；候选均未应用，正文未投递。共同祖先和旁支此次区分正确，第六例没有越过忘记造成的断链，但不能据此追认所有六例或旧模型结果通过。失败例没有保留原始正文，无法定位具体引用 ID 或作出语义结论。最终方法知情独立审阅在 `independent-review-ui-batch2-final-six.json`，原 interim 审阅仍保留。
