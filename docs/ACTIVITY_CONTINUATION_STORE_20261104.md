# 明确选择已结束活动续做：Store 与上下文（2026-11-04）

## 验证状态

代码和七项无模型 Store 回归已冻结，等待根代理统一 Cargo/HTTP 验证。作者只运行 rustfmt 解析，没有运行 Cargo、模型或设备。本文不把静态审读当成实际测试通过，也不继承此前记忆挑战集或教学 36 题的成绩。

## 用户选择与数据归属

续做总是创建新的 session_id 和 activity_id。当前未结束活动按既有规则结束，旧硬件配置、观察结果、完成阶段、临时偏好不复制到新快状态；新状态仍是 empty_state。选择旧活动本身记录为 operator-confirmed 的 learner_statement，模拟档案仍为 simulated。该事件只证明用户选择了来源活动，不证明实验完成、当前硬件连接或理解程度。

旁表 `session_continuations` 同时保存显式父关联和窄幂等回执：新 session/activity、owner/namespace、来源 session/activity、client_submission_id、原 expected_current_session_id/generation，以及实际选择事件 ID/version。它不存旧正文，不改旧表列数。

当前活动可读取自己和有效显式祖先链的记忆；链中每一步必须满足实际 owner/namespace、源/新活动身份、来源活动已结束、有效选择事件及版本，并且来源 rowid 小于新记录。断链即停止，不跳过失效关联继续读更早祖先。A→B、A→C 时，C 不会因为共享祖先而读取 B 的新事实。

每次 scoped 召回先解析一次活动集合，随后词面/向量均在排名与预算前作集合过滤。持续偏好沿既有严格规则跨活动。精确更正仍继承被更正记录的活动归属。

本片开放的是明确父链的读取，不支持把不同实际活动来源合并成一条新跨代摘要。原 sources_share_activity 准入仍要求单一活动，后台任务仍处理当前 session 来源；以后如需跨代综合，应单独实现最深明确后代的绑定规则，不能把 B 的新事实回绑到 A 而泄露给旁支 C。

## API 合同

`session_catalog(lid, before, limit)` 是 owner/namespace 内的新到旧稳定分页，limit 为 1–50，before 为正 i64。返回：

```text
{learner_id, namespace, items:[
  {session_id, activity_id, created_at, status, goal:String,
   continued_from:null|{session_id, activity_id, goal:String}}
], next_before:i64|null}
```

无有效来源的目录 goal 是空字符串，不是 null。目录不直接复制旧 state.goal；只显示可回到当前有效来源的历史字段。

服务端操作顺序：

1. 在控制锁内调用 `continuation_replay(lid, source_sid, nonce, expected_current_sid, expected_generation)`。首次返回 None 前已完成源 finished/owner、当前 owner/generation、nonce 等校验，没有写入或取消。
2. 若返回 Some，服务端仍必须确认运行时 current 正是该操作创建的新 session，然后无副作用返回，不能取消已经开始的新轮。数据库回执核对原 source/current/generation 完全一致；同 nonce 不同 payload 拒绝。
3. 首次操作才停止旧轮；服务端 cancel 恰好推进一次 generation。随后调用 `continue_finished_session(..., current_generation_after_cancel)`；Store 用 checked_add 核对原代次加一，在同一事务中再核来源/当前身份、结束旧活动、创建新活动、记录选择事件和回执。

返回：

```text
{learner_id, session_id, activity_id, client_submission_id,
 replayed:bool, session:<完整当前session>}
```

nonce 为 1–96 个 ASCII 字母、数字、下划线或连字符。新活动已经 finished 但运行时 current 仍指向它时，也允许只读重放确认；不重新选中活动。服务端运行时检查是必要组成，Store 不持有工作台的 current 指针。

## 历史信息的显示与教学上下文

每个 session 回显：

```text
continued_from: null | {session_id, activity_id, goal:String}
continuation: null | {
  client_submission_id, source_session_id, source_activity_id,
  selection_event_id, selection_event_version,
  historical:true, hardware_verified:false,
  requires_inventory_confirmation:true, ancestor_count,
  goal:<sourced|null>, pending_step:<sourced|null>,
  pending_resolution:<sourced-state-resolution|null>
}
```

sourced 字段含 text、source_session_id/activity_id、source_event_ids/versions、historical:true、hardware_verified:false。它是旧状态字段的有来源历史投影，不是新状态事实。待做计划另含 `as_of_session_id` 和 `completion_status:"not_revalidated"`，表示当时记录，当前完成状态未核实。

新活动当前字段为空时可以从明确祖先读取历史目标/计划，但不会回写成当前 goal、reported_result 或 completed。计划必须有已接受来源；对 typed plan，摘要文字须是实际记录的计划原文或结构化 pending_experiment。存在较后活动的有效来源支持 completed，或明确清空 pending_step 时，不再回填更早祖先的待办；单纯 session.status=finished 不代表实验已完成。

该规则只阻止从更早祖先回填计划。同一来源会话仍保留非空、有效 pending_step 时，即使其 stage 已为 completed，也会优先展示这条计划，并标为当时记录、完成状态未重验；不能把本文理解成所有 completed 会话的历史计划必为空。

教学上下文有独立 continuation，引用到的实际选择/目标/计划来源加入 evidence，保留历史与当前的区别。历史是可选上下文：全文放不下时尝试只保留选择身份；紧凑身份也放不下时只将初始化已有的 omitted_continuation 从 0 改为 1，不挤掉当前完整必需输入。

旧计划本身仍是有效历史，不因历史摘要停止回填而删除原记录。`pending_resolution` 回显较后来源支持的 completed_activity 或 cleared_pending_step、对应状态字段和值、真实来源 ID/version 及历史标记；context 同时包含这些来源的原始内容，使模型能看见停止回填的依据。它只证明历史状态字段的完成/清空来源，不推断某个具体实验已经执行；本片不是计划语义完成推断器，也不宣称教学端已通过证据充分性评测。

## 忘记、删除与重试

每次投影和父链解析均验证 live source/version。忘记操作会删掉不再保留的选择事件，因此旧关联可能失效，session.continuation 变 null；旁表保留不含正文的无效 receipt，阻止旧 nonce 重新创建选择。显式使用新 nonce 继续一个已断父链的活动，只能关联它仍有效的来源，不能跳过断链恢复更早活动。

若历史 source session 被删除、owner/activity 身份变化、选择来源被撤销，关联同样失效。来源内容与缓存仍受现有 memory evidence、learner epoch、forget 和 CAS 规则约束。

## 七项新增 Store 回归（已通过）

1. 接近上下文上限时，可选续做信息不能挤掉原本合法的当前完整输入。
2. real/simulator 的真实选择来源、历史 goal/plan、新状态清空、精确 ID 和后续轮次/finished 幂等读取。
3. A→B 与 A→C 的祖先/旁支隔离、跨代新摘要拒绝，以及合成向量 scoped 检索。
4. 非法来源、owner、nonce、代次在首次预验无副作用拒绝；故障注入使创建/选择/回执事务整体回滚。
5. partial forget 后保留记忆仍存在但父链失效；重启和旧 nonce 不复活，新 nonce 也不能跳断链。
6. owner 内稳定目录分页、空 goal 字符串、失效来源和未接受计划不作为历史摘要。
7. 有来源的完成/清空阻止祖先待办回填；中间选择事件删除后不再跨过该节点读取祖先。

根代理统一执行 `workspace-thirty-eighth.log`：760通过、0失败、7忽略，无警告；上述7项在真实临时SQLite中通过。原生全部bin及新UI WASM构建通过。实际进程检查 `activity-continuation-first/report.json` 在独立模拟库运行新版→已安装旧版→新版→新版，全部正常退出。目录三页、10类拒绝不改状态、两代精确祖先记忆集合、有效关联重启、忘记后断链重启均通过；旧版只做启动、档案读取和独立档案创建，前后六张来源/会话/记忆/关联表摘要相同。不是所有降级操作的兼容承诺。

该HTTP夹具仅有观察报告，没有非空历史目标/计划，目标、计划及完成/清空依据的覆盖来自上述Store回归。没有运行模型、设备或浏览器，也没有因此重评此前36题模型结果。混合不同实际活动的新摘要仍拒绝入库，跨代自动摘要尚未在本片开放。

## 文件与备份

本代理修改 `src/store.rs`、`src/store/memory_activity.rs`、`src/store/embeddings.rs`、`src/context.rs`；新增 `src/store/activity_continuation.rs`、`tests/activity_continuation.rs` 和本文档。路径均位于 rust/ai-tutor（本文档除外）。原文件备份：`runtime/goal-backups/20261104/architecture_audit/activity-continuation/`。

服务端和 UI 由协作代理分别实现。未修改 Cargo、冻结旧评测方法、模型配置或教学 policy。
