# 实验比较：冻结合同与 UI 实现

2026-11-04：由活动报告、模块确认、教育来源和记忆规则审阅形成方案，现已落实独立 Rust/Yew 组件及纯逻辑回归。后端由主任务另行挂接与验证；本文件不把未运行的编译、浏览器或实验任务写成通过。

## 边界与问题

现有 `POST /api/activities/{aid}/events` 把非空 `report` 一律记录为 `observation`，更新 `reported_result` 和 `attempts`。不能将计划或“尚未测试”伪装成该入口的观察。已有记忆规则将 `observation` 与 `plan + accepted=true` 区分，应复用该来源边界。

首片仅做两个条件的结构化记录、保存与回看，不增加实验完成推断、自动评分或长期记忆自动升格。不根据年龄推断能力；界面语言与教学语言维持独立。

## 模块与接口建议

- `tutor-ui/src/comparison_state.rs`：独立草稿、校验、归属代次、幂等提交快照与有界回看。
- `tutor-ui/src/experiment_comparison.rs`：活动页 A/B 卡片、单在途 GET/POST、显示与用户交互。
- `store/experiments.rs`：事务保存来源事件，按有效事件投影比较列表；不在教学 state 复制一份无界比较记录。
- `GET/POST /api/learners/{lid}/sessions/{sid}/comparisons`：GET 返回当前会话最近 10 组；POST 校验归属、命名空间、会话可写性；完成会话只读。

冻结的提交合同：

```json
{
  "submission_id":"comparison-synthetic-001",
  "question":"停车阈值怎样影响停车位置？",
  "changed_variable":"停车阈值",
  "conditions":[{"name":"小车、速度、地面、墙的位置","status":"reported_same"}],
  "trials":[
    {"label":"A","setting":"20 cm","status":"reported","result":"碰到墙","trial_count":1},
    {"label":"B","setting":"30 cm","status":"not_tested","result":null,"trial_count":null}
  ],
  "pending_plan":{"text":"保持其他条件，试30 cm","accepted":false},
  "source_type":"learner_report"
}
```

状态必须显式选择，默认未选择；次数允许未知，不默认一次。条件仅可声明为报告相同或尚未核对。`reported` 要求原始观察；`not_tested` 不允许结果。

边界：`submission_id` 最多 96 字符；问题、比较变量、条件名与 A/B 设置各 1–200 字符；共同条件 1–8 项；恰好 A/B 两项；原始结果和可选计划各 1–1000 字符；次数 `null` 或 1–10000 整数。`not_tested` 的 `result` 和 `trial_count` 必须均为 `null`。非空计划的 `accepted` 默认 `false`；空计划不能被接受。界面在切换至“尚未测试”时保留已写结果，明确提示用户清空或恢复“已有观察”，不静默丢弃输入。

GET 返回 `{learner_id,session_id,items:[item]}`，当前会话最新在前、最多 10 项；POST 返回 `{learner_id,session_id,item}`。`item` 包含 `comparison_id,created_at,source_type,hardware_verified:false` 与业务提交字段。界面不展示内部 ID。模拟档案由服务端强制 `source_type=simulated`；真实档案只接受 `learner_report|operator_confirmed`。`operator_confirmed` 文案为“人工报告确认”，不表示硬件验证；`hardware_verified` 始终为 `false`。

客户端将同一 payload 的 `submission_id` 保留至回执确认或实际编辑；网络失败原样重试使用同一 ID。实际编辑才换 ID。服务端先只读验证并查幂等结果；首次提交在控制锁内撤销旧候选并原子保存，重复提交不能再次取消较新的教学。该后端锁序由主任务接线，客户端不拆出额外 cancel 请求。

一次事务共享服务器生成的 `comparison_id`，拆成独立来源事件：观察事件仅包含已报告行；计划事件仅包含待测项和计划，只有显式接受才设置 `accepted:true`，执行状态保持 `not_started`。保留原始 `content.text` 和结构化条件，便于现有来源解释和引用。不改模块清单、模块确认、能力判断或完成阶段。后来补测应新增观察并关联原计划，不能把原计划改写为已完成记录。

## UI

“比较两种做法 / Compare two setups”：问题、改变什么、哪些条件保持相同，接 A/B 设置、观察或待测状态、次数。窄屏上下排列。原始观察不自动翻译；根据明确设置的阅读程度补充术语解释，不以年龄推断。

“保存记录”不等于接受计划；接受项默认关闭。来源保留学习者自述、人工报告确认、模拟。错误保留草稿；成功仅清理与提交版本完全一致的草稿，提交后的新编辑保留。切会话后的迟到回执丢弃；A→B→A 也不能恢复旧回执。

已确认的最小实现使用组件自有草稿和父层导航门控：草稿未保存时，结束/新建活动和切换/创建档案会定位记录卡，提示先明确保存或清空。**不自动拼接多表单保存事务。** 保存期间阻止其他非停止操作，撤销旧本地自动音频意图，暂停教学显示确认；POST 成功或失败后都取回当前 state，再恢复显示，以覆盖“已经提交但响应丢失”。保存本身不自动调用模型、发送新问题或写长期记忆。停止入口仍可用。

草稿仅在当前页面内存保留，界面明确告知刷新会丢失；没有宣称跨刷新或外部强制切会话恢复。完成活动只允许回看。GET 和 POST 各最多一个请求在途；旧 GET 不能覆盖保存后的列表或恢复更正后已失效的来源；列表始终最多 10 组，不在教学 state 追加无界历史。

UI 文案覆盖中文和英文，观察文本保持原文。plain/standard 使用“改变什么”，technical 使用“比较变量”，状态与校验不因阅读程度而放宽，也不根据年龄推断能力。窄栏卡片按可用宽度堆叠 A/B，真实布局尚待人工操作验证。

## 验收与边界

1. 临时数据库：观察/待测严格分离，待测不入结果；跨档案拒绝、完成会话拒写、事务原子、模块状态不变。
2. 中英 UI fixture：`rust/tutor-ui/fixtures/experiment-comparison-20261104.json` 已提供 12 项合成验收脚本，全部 `NOT_RUN`；覆盖失败重试、提交期间新编辑、迟到回执、导航门控、来源、阅读程度与窄屏。
3. 更正/删除后的来源失效及教学上下文：保留完整条件、次数与不确定性。复用 `memory_education_probe_v1.json` 的 20/30 cm 各一次样例，不能由一次观察推断“30 cm 总可靠”，也不能把已接受但未开始的计划计为完成。

软件验收不证明学习效果或现场实验可靠性。

本 UI 切片已写 8 项纯逻辑测试，覆盖：显式状态/未知次数/计划默认；改成待测不丢观察；幂等重试/新编辑保留；切会话与返回旧会话；非法次数/空计划接受；迟到列表/来源失效；10 项上限/重复回执/归属与硬件标记；当前会话导航门控。已运行 `rustfmt` 并通过；原生测试与 WASM 由主任务统一执行，记录结果前均视为待验证。本切片没有操作浏览器、真人数据、摄像头、麦克风、模型或云台。

修改前备份：`runtime/goal-backups/20261104/ui_review/experiment-comparison/`。
