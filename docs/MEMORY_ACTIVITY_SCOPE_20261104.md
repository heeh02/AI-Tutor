# 实际活动归属与生产召回隔离（2026-11-04）

## 状态与范围

本片实现了基于真实 Store 来源的活动隔离。根侧 `workspace-thirty-seventh.log` 已实际通过完整742项、0失败、7忽略（含UI122），无编译警告，耗时29.282秒；六项新增活动归属测试全部通过。它们使用真实临时Store和合成向量，不含模型或设备质量评测。首次 workspace36 的一个旧库连续更正测试期望过强，原失败日志保留；随后只调整该夹具以区分可恢复与已丢信息，生产代码没有猜测归属或放宽约束。

原生release及HTTP检查随后通过。`database-activity-compat-first` 以实际新程序→旧安装→新程序运行同一个新建合成库，三进程均正常退出，schema及比较/未接受计划的语义保持；它没有慢记忆样本，因此不替代上面的更正/旁表迁移回归。旧诊断硬编码测试数566的标签已在独立元数据勘误文件中说明，本次正确软件计数是742，原始报告不改。

这不是题目专用过滤，也不是新的相关性评分器。没有改变冻结的 60 源、96 问、typed42/typed48 投影、编码、Store 诊断、36 条教学请求或其实际结果。不能据此宣称此前 q06 错答已解决或 36 题语义通过。

## 身份与召回

原 `sessions.activity_id` 是创建活动时生成的 UUID；`memories.activity_scope="modular_rover"` 只是旧领域类别，不能区分两次实际活动。`events` 和 `education_event_meta` 保存来源会话，可关联真实活动 ID。

新增旁表 `memory_activity_bindings(memory_id, learner_id, namespace, activity_id)`，不增加旧 `memories`/`sessions` 表的列，保留旧程序使用的 positional INSERT 形状。普通来源必须全部有归属且同属一个活动。绑定不读取主题词、目标文本、相似度或模型提供的活动 ID。

- `recall_for_session(sid, query, top_k)` 是当前教学的词面入口。
- `recall_candidates_for_session(sid, query, embedding, top_k)` 是同样受约束的可选向量/RRF入口；此入口不启用生产向量检索。
- 两路都在词面/向量前五名选择及全文预算之前过滤活动，其他活动不能先占掉名额。
- `context::build_context` 和 `Store::session().recalled_memories` 已改用受约束的入口。
- 旧 `recall` / `recall_candidates` 继续用于明确的历史/lab用途，生产上下文和会话快照不调用它们。

来源、版本、namespace、owner、learner epoch 和删除失效校验继续存在；活动归属不替代这些检查。跨活动只允许现有规则证明的 `help_preference + cross_session_explicit`，并逐条重新检查来源的明确持续性；混有临时请求、计划或不合法来源不会因一个 persistent 标记获得通行。

上下文增加当前 `activity_id`。来源 evidence 投影实际记录的 `session_id/activity_id`；记忆另带 `activity_binding`。更正事件的记录会话可能不同于被更正记忆的归属，两个字段的含义不能混为一谈。

## 更正与迁移

手工更正优先使用实际 `corrected_memory_id` 的归属。一个来源失效可能同时使多条记忆失效，受影响闭包不能冒充用户选中的单一更正目标。没有单一目标的模型更正要求整个目标集合归属一致，并在旧来源失效之前验证。

教育来源修订通过 `supersedes_event_id` 回溯。旧事件正文可能已被真实删除，保留的 `education_event_meta` 加真实 session 仅用于恢复身份；这些已删除/失效来源不能成为有效内容证据。没有显式旧目标的 typed correction 仍归属于它实际记录的活动，表示本次更正声明的来源，不推断它改正了哪条其他活动记录。

原有持续偏好经手工编辑时，记录明确的 teacher setting 和 persistent 标记。模拟档案的来源等级仍为 `simulated`，不冒充真实教师确认；两种 namespace 都保持持续偏好可召回。

迁移只按 rowid 顺序补缺少绑定的记忆，使更正能引用已处理的原记录。缺失或混合归属保存为 NULL，不删除旧历史，也不在重启时重新猜测。新普通/typed 混合活动候选在持久化前拒绝；对归属未明的既有记录进行手工更正，结果仍可能未解且不自动召回。

旧库连续更正还存在明确的信息缺口：中间更正事件的正文可能已删除，其精确 `corrected_memory_id` 随之丢失，而 memory.supersedes 仅留下包含混合来源记录的失效闭包。这时不能恢复唯一归属。只有仍保留精确目标、可证明一致的 lineage，或已有旁表绑定时才可恢复/保留；其余旧链保持 unresolved，不把失效闭包当成精确目标。

实际更改来源、owner、scope、kind、supersedes 或会话活动身份会清空相关绑定；同值 UPDATE 不清空。正文更正走新记录，删除记忆同时删除旁表记录。绑定失效不恢复事实或向量缓存。

## 明确的行为变化与待办

未结束会话继续使用原 session/activity ID。本片没有实现恢复已结束活动的 server/UI：新建活动后输入“继续”，不会再凭领域类别自动选择旧活动。旧来源、历史查看和明确的 broad 历史查询仍保留。以后应通过明确选择旧活动建立经过 owner/namespace/代次校验的关联，并创建新的当前 session/activity ID；不要复用旧活动路由 ID，避免迟到请求写入新会话。

同一次活动内若讨论了多个未显式分开的实验，本片不能按文字自动拆分它们。活动隔离也不等于同活动内的语义充分性、正确引用或拒答验证。既有快状态与已显示对话没有被本片重写或清理；其历史来源标记和既有证据约束继续适用。

## 已运行的回归范围

新 `tests/memory_activity.rs` 共六项综合测试：

1. 六条高排名旧活动记录不能挤掉当前记录，覆盖词面/真实 Store 合成向量、非空双 owner 和上下文身份投影。
2. real/simulator 持续偏好跨活动、混合临时来源拒绝，以及手工偏好更正后的来源等级和召回。
3. 新混合活动来源拒绝；旧未解记录可检查和历史查询，但不自动进入任一活动。
4. 明确目标更正不被混合失效闭包污染；首次精确目标尚在的旧库迁移、正常旁表连续更正/重启分别保留原活动；旧链精确目标已丢且闭包混合时保持未解，旧表仍为 13 列。
5. typed 修订沿已删除来源的身份回溯；无显式目标的新更正声明仍归自身活动。
6. 同值来源写入保留绑定、真实来源变更清空绑定，forget 后删除绑定且迟到向量 CAS 不复活。

旧测试的合同调整是有意行为变化：`context_retrieval.rs` 两项继续检验 query/goal 挤占，但使用同一实际活动；`context.rs` 的“继续”测试改为不隐式选择已结束活动；`product_workflow.rs` 两项服务重启/模拟完成测试继续验证数据保留与空间隔离，同时改为新活动不自动混入旧记录。

## 文件与备份

修改：

- `rust/ai-tutor/src/store.rs`
- `rust/ai-tutor/src/store/education.rs`
- `rust/ai-tutor/src/store/memory_policy.rs`
- `rust/ai-tutor/src/store/embeddings.rs`
- `rust/ai-tutor/src/context.rs`
- `rust/ai-tutor/tests/context_retrieval.rs`
- `rust/ai-tutor/tests/product_workflow.rs`

新增：

- `rust/ai-tutor/src/store/memory_activity.rs`
- `rust/ai-tutor/tests/memory_activity.rs`
- 本文档。

原文件备份在 `runtime/goal-backups/20261104/architecture_audit/memory-activity/`。没有修改 server、UI、Cargo、模型配置、教学 policy 或冻结实验脚本/数据/结果。
