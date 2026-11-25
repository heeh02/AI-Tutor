# 可选 shadow 记忆调度器

日期：2026-11-04。实现位于 `src/memory_schedule.rs`（server 子模块）与 `src/store/memory_discovery.rs`。编写代理未运行 cargo、真实模型或设备；根代理负责接入主程序并验证。

## 部署合同

服务层需要 `#[path="memory_schedule.rs"] mod memory_schedule;`，Workstation 增加 `memory_schedule_started: AtomicBool`，初始 false。主程序取得应用独占锁并完成提供方 probe 后，调用 `app.start_memory_schedule()`，返回 `Result<Option<JoinHandle<()>>>`；保存句柄，在 begin_shutdown/preempt 后等待最多 3 秒，必要时 abort 并 await 回收。只有这次显式 startup 调用才执行 lease recover，重复启动被拒绝；单纯开启 Store 不回收活跃 worker。

策略只在启动时读取 `runtime/memory-policy.json`。**文件缺失或 mode=disabled 时返回 None，不执行 recover、发现、claim 或模型调用。此批没有创建或启用用户运行目录里的策略文件。** 合法 mode 只有 disabled、shadow。shadow 必须指定一个 namespace：real 映射显式 OptInReal，simulator 映射 SyntheticOnly，后者仍要求每条来源有 synthetic_fixture=true。不存在 apply 策略或默认自动升格。

示例只用于说明合同，不代表已经启用：

```json
{"mode":"disabled"}
```

```json
{"mode":"shadow","namespace":"simulator"}
```

可选 limits 使用 MemoryJobLimits 的完整五字段对象；省略时沿用队列默认值。worker 租约至少 100 秒。非法 mode、多 namespace、未声明的顶层字段、格式错误均不启动任务。启用真实空间意味着允许额外后台处理，需由主程序/使用者明确选择；不是程序看到历史数据就自动启用。

## 空闲与生命周期

只有一个 task loop，每 2 秒检查一次，错过的 tick 不补跑。前台 guard 占用、已有 worker、录音、转写、播报/合成，或最后一次前台输入不足 10 秒，均先返回等待。读取活动并入队后再次检查空闲，随后最多执行一次 run_once。前台进入仍由 worker control 和 Kimi 单 gate 取消机制保护。loop 不持有 Store 锁等待模型。

root 的 cancel/finish/报告等流程统一 preempt；shutdown 设置关闭标志并 preempt，loop 退出后释放单实例标志。正常结果只有 shadow 候选，没有 apply。调度状态放入现有 metrics.memory_schedule，包含 namespace 和去原文的结果；持续内部错误只提示一次，恢复成功后才允许下一次新提示。

录音/播报状态检查依赖现有 AudioService 的状态合同，前台与后台的最终提交顺序仍以 ForegroundGuard 为准。未在这个模块加入新的设备控制或音频生命周期。

## 不遍历全部历史的发现索引

新增 `memory_schedule_due(session_id PRIMARY KEY, learner_id, namespace)`，只存发生变化的活动 ID。首次创建旁表时一次性回填已有有效 typed 事件所在活动；之后重开不再回填，避免反复制造历史工作。该表在现有 Store 启动事务内迁移。

typed 事件新增/更正/失效、快状态 revision 和 session generation 变化会置 dirty。删除 session 或 learner 会删除其 due。来源删除后没有剩余有效 typed 来源的 due，在下次发现时清除，不创建新任务。

`Store::discover_memory_jobs(prompt_version, limits, synthetic_only, now_ms)` 一次最多检查 4 个 dirty session，每个只取最新 12 条有效 typed 来源，排除普通 legacy、错误归属/空间、无效版本；SyntheticOnly 额外排除未标记原文。读取来源→当前快照入队→清 due 在同一个短写事务中完成。队列已满保留剩余 due；入队失败回滚清除动作。

finished session 不是排除条件。结束后切档/cancel 改变旧 generation 时，已有任务失效且对应 session 再次进入 due，因此即使它不再是 current，也能按新快照被发现。generating 的 session 保留 due，等空闲再处理。幂等、租约、日/会话预算仍由上一批 Store 队列负责，不另造调度平台。

## 回归与真实检验状态

新增 9 项待根代理验证：`tests/memory_discovery.rs` 6 项（首次迁移/重开、历史 finished 再发现、4活动/12来源上限及过滤、满队列/失败回滚、删除不复活、generating 等待与未标记零调用）；scheduler unit 3 项（空闲门槛、缺失/disabled 不 recover、fake 唯一loop/前台等待/关闭回收）。全部使用临时数据库和 fake provider；已 rustfmt。

此前根代理完成真实合成单事件检验：`memory-shadow-real-zh-first.log` 返回 0 条，`memory-shadow-real-en-first.log` 返回 1 条；两者 provider_started=true、shadow_staged、未 apply。它们证明真实调用链可运行，**不代表教育连续性或召回质量达标**。

## 下一份三事件合成用例建议（尚未新增远程调用）

中英各使用相同的结构化三事件，来源均为 simulated、synthetic_fixture=true：

| 事件 | 类型与内容 | 期待候选 |
| --- | --- | --- |
| G | learner_statement：希望比较两种停车阈值，让小车更早停在墙前；明确目标 | learning_goal/stated_goal，限定当前活动 |
| O | observation：报告第一轮 20 cm 阈值碰墙，第二轮 30 cm 阈值停在墙前；两轮速度相同。保留“报告”与模拟性质 | trial_observation/reported_observation，保存对照条件与结果，不推断掌握或硬件确认 |
| P | plan，accepted=true：已接受下次只改变速度、保持 30 cm 阈值的比较实验，但尚未开始 | accepted_pending_experiment/accepted_plan，必须仍为待做 |

评价分开记录：来源 ID/版本/原句匹配；目标、比较结果、待做计划三类覆盖；中英文各自提取覆盖；计划绝不变完成；没有概念掌握/能力/人格标签；没有把三条摘要当更多独立证据。零候选可算协议合法，却不算覆盖通过。此用例仍不测纯跨语语义召回，原有两个跨语缺口继续单独保留。

建议真实评测前先将 fixture 和期待规则落入本地测试，prompt 变更时更新 prompt_version 并保留旧报告，而不是在同一个版本上不断调整直到单用例恰好通过。
