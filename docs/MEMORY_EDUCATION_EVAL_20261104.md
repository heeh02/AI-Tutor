# 合成教育记忆连续性评测入口

## 条件边界、明确偏好与提示支持的四例真实调用

新增固定 `memory_education_boundaries_v1.json`，通过 `--scenario constraints|preferences_and_support` 选择，中英各一次，仍是每次最多一次真实 Kimi 调用、全新 simulator Store、仅 shadow、无设备和自动晋升。原 continuity 语料未改。证据在 `runtime/goal-baselines/20261104/education-boundaries-first/`；方法在调用前冻结程序、schema、prompt、fixture 和二进制哈希。四次均 exit0，耗时16.505、20.684、21.709、35.931秒；这些是完整诊断时间，不是首段时延。

schema v2 / prompt v3 下，中英条件场景分别产生3条 context_constraint：纸桥保留10/15厘米、2/4折、跨度折法同变、没有承重记录及不能判断更牢；阅读保留天气/原因缺失而不捏造下雨；循环保留初始0/1、每次加2、三次及不能从终值推断多一次。循环来源虽标 hinted，条件候选仍为 unknown。未接受的计划均未生成候选。

偏好与作答场景中文4条、英文5条：两者均保留科学词汇的英文先、中文解释后，以及未来适用、可被新要求替代；临时分数提示仅 current_session。独立作答的错误原答案2/5未被悄悄改正，提示后5/6保留换成六分数的提示、hinted等级及尚未检验无提示新题。英文额外一条 revisable_misconception 认为2/5“与直接加分子分母一致”，明确为单次答案的可修订解释；它不是学习者报告的实际思考过程，不能升级为已验证原因或能力标签。

按冻结类别、来源、scope、support规则，6项条件和8项偏好/作答覆盖观察均匹配；结构检查全通过，所有候选仍shadow。根代理和另一代理逐条开放核对没有发现这些样本的实质数值/否定遗漏。原报告人工评审字段未改，不称自动语义正确率、独立盲评、儿童质量或广泛教学质量通过。尤其“提示后答对”没有对照、延迟保持或新题迁移证据，**不构成因果提示效果评测**。保护文件前后哈希一致。

另发现并修复混合来源的长期偏好晋升缺口：一个persistent来源原可授权整个混合候选。类型化晋升现在要求每个被引用来源均是明确persistent的学习者表述/教师设置；旧直接写入入口对类型化来源同样检查，对legacy_report保留既有显式用语/教师确认兼容但改为逐来源满足，并拒明确temporary。候选应拆分，不能把临时请求、计划或回答搭载成长期偏好。旧自然语言启发式仍不是语义真伪验证，本片未追溯重写既有数据或自动启用晋升。

回归先复现原缺口；之后两次夹具/兼容检查失败分别指出teacher_setting必须operator_confirmed，以及未类型化旧事件实际标记为legacy_report而非空。均保留日志，修正源等级和分支后，`workspace-thirty-second.log`完整通过。后续本页原v1/v2记录均为历史证据，不回填成新版本结果。

独立二进制：`rust/ai-tutor/src/bin/memory-education-probe.rs`。版本化语料：`rust/ai-tutor/tests/fixtures/memory_education_probe_v1.json`。本批没有修改生产 prompt、worker、调度策略或其他生产模块，也没有执行远程请求。

## 执行边界

先按现有流程验证当前 Kimi agent hash。之后明确执行以下任一个语言，每次最多一次模型调用：

```text
memory-education-probe --real-synthetic --root <项目根目录> --language zh
memory-education-probe --real-synthetic --root <项目根目录> --language en
```

入口只使用内置合成材料、新建临时 simulator 数据库和单任务/单次尝试/单次日预算。复用当前 Kimi gate、隔离、截止时间及 Store 来源校验。不会打开学习者数据库、启动设备、自动重试或 apply。退出时关闭数据库并清理已知临时文件。

输出保留经过 Store 校验和归一化的**合成**候选内容、kind、claim_type、scope、source_count、source_grade、importance、原句及来源 ID/版本，便于人工核对；不保留 raw provider 文本或未通过校验的候选输出。

## 同一结构的中英语料

两种语言各有四条 typed 原始事件：

1. 明确目标：比较停车距离阈值，使小车停在墙前而不撞墙。
2. 一轮比较报告：同一小车、速度、地面和墙位置；20 cm 一次碰墙，30 cm 一次停在墙前；明确每个阈值只有一次观察，样本不足，不能断言 30 cm 总是可靠。
3. accepted=true 的待做计划：保持 30 cm 阈值，只改变速度比较停车位置；execution_status=not_started，明确尚未实施、没有结果。
4. 临时偏好：只在今天这次活动使用短提示，不是今后的持续偏好。

最小有意义的候选覆盖是 learning_goal、trial_observation、accepted_pending_experiment 各对应自己的来源。临时偏好可以省略，若保存必须为 current_session。没有 learner_answer，所以不期待概念掌握或误解标签。

## 三层结果，不能混作“通过”

`structure` 检查真实开始回执、最多一次预算、shadow-only、来源 ID/版本/唯一计数、来源类型与候选类别匹配、计划来源仍是 pending 类别、临时偏好仍是 session 范围。这里的计划检查仅涉及结构字段，不能证明文本没有偷偷写成“完成了”。

`coverage_observations` 只记录是否出现了期待的 kind/claim_type/来源组合。它既不按候选总数计分，也不通过关键词或正则猜测语义正确。零候选可以协议和结构合法，却会缺失全部最小教育覆盖。

`manual_semantic_review` 固定标为 required_not_performed，并保留 semantic_accuracy、meaningful_omissions、plan_misrecorded_as_completed=null。必须逐项核对：目标忠实；比较条件及两个结果保留；样本不足和不确定性未丢失；计划保持未实施、变量不被改写；临时偏好不变长期；没有凭报告推断能力、掌握或人格。原句确实存在也不等于改写一定正确。

**退出码 0 只表示调用链与结构检查通过，不表示教育质量通过。** 语义准确、遗漏与计划误记完成需人工记录 pass/partial/fail 和具体候选位置；不要用退出码、候选条数或粗略关键词作替代指标。

## 本地测试

新增 4 项 bin unit tests，编写代理未运行 cargo：中英语料结构、有效 fake 候选保留人工待审状态、临时偏好范围错误与 Store 拒绝计划结果分开报告、零候选结构通过但覆盖全缺失。均无远程调用。

先前单事件真实检验 zh=0/en=1 只作为链路记录保留；不能用这份更丰富语料替换或追溯改写旧结果。新报告应同时保存 fixture_version、prompt_version、语言和人工语义结论，便于后续版本比较。本用例不测纯跨语召回，原有跨语缺口仍需独立评估。

## 2026-11-04 首次真实合成链路记录

root 已分别执行中英文一次，日志为 `runtime/goal-baselines/20261104/memory-education-real-zh-first.log` 和 `memory-education-real-en-first.log`。两者各产生 4 条通过 Store 结构校验的 shadow 候选，均未 apply；每语言只有一条 attempt_no=1 的模型调用收据。目标、比较观察、已接受待做实验和临时偏好四类来源组合均出现。

root 对候选逐条检查时，观察到两种语言保留 20 / 30 cm 结果、每个阈值一次与样本不足、下一轮尚未开始、只限今天的短提示。但中文比较候选完全省略了同一小车、速度、地面和墙位置这四项控制条件；英文将它们压缩为 `other listed conditions unchanged`。两者单独召回时都无法完整恢复具体控制条件，虽然现有来源链接仍可定位原报告。这是已观察到的摘要遗漏，不能写成无遗漏或完整教学连续性通过。

上述是开发者对 2 个合成案例的开放核对，不是独立人工盲评或广泛教学质量通过。原 JSON 的人工评价字段保持待审状态，不回写成自动通过；仍需独立语料、明确评审准则及遗漏/错误记录。

## 提示词 v2 的小范围修正

针对上述遗漏，`prompts/memory_consolidation.md` 加入实验候选可独立解释的一般规则：保留源中明确的变化变量、具体控制条件、数值/单位、每条件观察次数、报告结果、不确定性；计划保留固定项、变化项与未实施/无结果状态。源中未给的信息不得补造。600 字符内先去低价值措辞，必要时拆为各自完整的观察候选，仍受最多 5 条限制；不能靠删除关键实验条件和不确定性压缩。准确候选无法容纳时省略该候选，避免保存误导性残片。

`PROMPT_VERSION` 升为 `educational-memory-prompt-v2`；新增独立 `SCHEMA_VERSION=educational-memory-v1`，JSON 形状及 schema_version 保持兼容。此前两常量含义混用，已同步 fake provider 的响应字段。真实请求与 job 始终绑定 prompt_version，response 只按 schema_version 验证。

已核对 Store 的幂等 hash 包含 prompt_version；相同来源与快照在新提示版本下获得新 job ID，旧任务不会被重标为 v2。worker 已有版本预检，旧任务不会进入 provider 或保存候选；其失败保留既有有限重试/预算预留语义（未开始不记作模型调用成功），本批不迁移真实排队数据、不自动重跑历史任务。

新增 3 项回归已在 root 的 `workspace-seventh.log` 通过：`prompt_revision_does_not_change_or_masquerade_as_the_response_schema`、`obsolete_prompt_job_never_calls_provider_or_creates_candidates`、`prompt_revision_has_a_distinct_idempotent_job_without_relabeling_old_work`。提示词规则不通过字符串匹配测试冒充语义质量提升；仍须 root 使用同一中英语料进行 v1/v2 对照。v1 日志原样保留。

## v2 同语料真实调用与开放核对

root 已各运行中英一次，结果保存在 `runtime/goal-baselines/20261104/memory-education-real-zh-v2.log`、`memory-education-real-en-v2.log`，prompt_version 均为 `educational-memory-prompt-v2`。两者仍各 4 条 Store 校验后的 shadow 候选，无 apply。

逐条开放核对观察到：中文比较候选现在列出同一小车、速度、地面与墙位置；英文也完整列出这四项控制条件，不再用“其他条件”代指。两者保留 20/30 cm、每条件 1 次、碰墙/停在墙前、样本不足与不能推断总是可靠。计划仍保留固定 30 cm、只改变速度、尚未开始且无结果；临时短提示仍限本次/今天，不升为持久偏好。

这说明本次 2 个合成案例中，v1 已观察到的控制条件遗漏在 v2 输出里得到修正。仍为开发者知道目标的开放核对，每语言仅一次；不是独立人工盲评、稳定性统计或完整教育质量通过。后续应加入不同变量和更长原句的独立语料，验证摘要不会为满足长度限制而丢失关键条件。
