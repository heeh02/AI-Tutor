# DeepSeek Flash 教学路径初筛

累计完成 **5 次分开登记的 API 请求：1 次终稿契约失败、4 次机械通过，0 次获得完整教学策略或默认采用资格**。原始内容性片段出现得较早，但不能据此填入合格首句时延。生产默认仍为普通 Kimi；用户策略和安装版未切换，完整 Goal 保持 ACTIVE。

## 请求与比较范围

诊断使用固定 `deepseek-flash` 别名，显式请求 `thinking.type=disabled`，或 `enabled`＋`reasoning_effort=low`，不依赖默认思考档。[官方思考模式](https://api-docs.deepseek.com/guides/thinking_mode)、[Chat Completions 接口](https://api-docs.deepseek.com/api/create-chat-completion)

每次上限 8192 token、90 秒；关闭代理、跳转和自动重试，不带工具或额外评审调用。SSE 完整帧、UTF-8、模型标识、`stop`、`[DONE]`、HTTP EOF以及共享 SegmentDecoder/终稿规则分别检查。思考文本丢弃，只保留计数；可见输出与候选单独记录。

输入沿用归档中英请求、来源库、核心提示和 schema，独立复制数据库后应用候选。凭证仅由 root以 CurrentUser DPAPI保存在仓库外，不放入计划、命令行参数、fixture或文档；本报告不包含密钥。

## 尝试账本：失败保持独立

1. **首次预检：0 次请求。** 归档源库存在 sidecar，closed-database guard按规则拒绝，`archive_database_not_closed`，没有读取凭证或进入 HTTP 执行。
2. **v2 路径准备：0 次请求。** 诊断程序未启动；留存 runner记录脚本缺失、exit2。MAX_PATH是准备阶段的 root报告，不能把它改写为 API/模型失败或完整独立复现的原因。
3. **短路径 fixture v3：1 次中文 off，契约失败。** HTTP200、模型标识、SSE结束及3段前缀均有记录，但终稿为 `final_contract_invalid`，剩余3槽保持未执行。完整原始终稿和候选没有保留，具体 typed原因未知，不能从末行形状推断。
4. **新增可观测性的独立中文 off：1 次机械通过。** 单独计划/程序/输出保存了可见原文、终稿和应用效果；共享验证规则未放宽。它不修复或解释上一条未保留的原始终稿。
5. **另行登记的三次调用：中文 low、英文 low、英文 off，均机械通过。** 这不是向原失败报告补写3条成功，也不是一次不中断的四槽成功。

[首次预检审查](../runtime/goal-baselines/20261106/deepseek-flash-first/independent-runtime-review-first.json)、[失败调用审查](../runtime/goal-baselines/20261106/deepseek-flash-first/independent-runtime-review.json)、[独立单次审查](../runtime/goal-baselines/20261106/deepseek-diag-first/independent-runtime-review.json)、[三次调用审查](../runtime/goal-baselines/20261106/deepseek-three-first/independent-runtime-review.json)

## 原始内容时刻，不是合格时延

内容审阅先冻结完整片段序号，再与模式和 `received_from_invocation_ms` 合并。起点为调用入口、凭证/header准备之前；不是首字节、首个 JSON片段、服务端 token时刻或声学首音。事实复述不计首个有内容性的教学。

| 尝试 | 模式 | 首个内容性序号 | 原始观测时刻 | 未获得完整资格的原因 |
| --- | --- | ---: | ---: | --- |
| 原失败中文 | off | 1 | 1.703 秒 | 终稿契约失败；前缀物理执行前提也不完整。 |
| 独立中文 | off | 1 | 2.248 秒 | 直接从静态检查进入低速试跑，当前构建/控制前提未确认。 |
| 新中文 | low | 1 | 4.775 秒 | 后续错误保证20 cm阈值会留下约10 cm最终停车余量。 |
| 新英文 | low | 0 | 3.917 秒 | “不是停车结果”是有用解释，但“静止测量”不是已报告事实，后续试跑前提缺失。 |
| 新英文 | off | 1 | 1.445 秒 | 假设已有可用电源控制并建议试跑，未确认其可安全使用。 |

**全部5条均不赋予合格教学时延。** “有内容性”与“正确、符合完整策略、可交付”是不同门限；后面的问句也不能补救前面已给出的不合格行动建议。独立审阅披露过先前收到非主动请求的时序摘要，因此不声称完美历史盲性；本次三样本审阅的模式、顺序和时间已去除，选择在解盲前冻结。

[失败前缀内容审查](../runtime/goal-baselines/20261106/deepseek-flash-first/semantic-blind-prefix-review.json)、[独立单次内容审查](../runtime/goal-baselines/20261106/deepseek-diag-first/content-review.json)、[三样本内容审查](../runtime/goal-baselines/20261106/deepseek-three-first/content-review.json)

## 教学、候选与实际存储分开评价

中文 low建议的静止、不驱动情况下比较已知距离有教学价值；真正的数量错误在后一句：**30−20=10只是附加几何假设下可能的读数变化，不是最终剩余停车距离**。静态读数稳定不能证明动态制动余量，还缺速度、控制时序、制动与传感器/车体几何信息。不能把此错误简化成“只要装置未知就禁止静态校准”。

四次机械通过中保留的任务/记忆候选均忠实于30 cm报告、尚未行驶及20 cm适用性待确认，没有把试跑建议变成已接受计划，也没有把错误的10 cm保证写入状态；原失败调用未保留的候选不在此结论内。

- 英文 low实际新增 **1 条 active、simulated、current_activity的 reported_progress记忆**，内容是30 cm报告与尚未行驶。来源、版本和活动绑定核验；这不是shadow、拒绝或跨会话偏好。
- 中文 low有一条忠实的 `current_session reported_progress`候选，但未持久化；`rejected_candidates=0`，不能称为“后端拒绝”。
- 部分 fast-state的 `reported_result/attempts`保存的是带“尚未行驶”的初始报告，不能仅因字段名把它当作已执行试验。

数量角色反例及双语输入已保存在 [deepseek-quality-next](../runtime/goal-baselines/20261106/deepseek-quality-next/manifest.json)。它是**数据提案，未注册、未运行，新调用预算为0**；旧十案例固定分母不变，也不把已见反例当作新的留出测试。[数量反例](../runtime/goal-baselines/20261106/deepseek-quality-next/observed-counterexample.json)、[英文记忆来源审计](../runtime/goal-baselines/20261106/deepseek-quality-next/en-memory-source-audit.json)

## 比较边界与当前状态

- 原 Kimi普通档内容时刻约16–21秒，Highspeed约11–13秒；Highspeed英文也有完整策略失败。这些历史观测不认证 DeepSeek速度提升或默认采用。
- 缓存未配对：原失败中文off为0/3053命中，后续独立中文off为2816/3053；新三次命中分别为0/3078、1408/3069、1280/3044。不能把差异归因于思考开关或模型本身。
- Flash是请求/返回的模型别名，fingerprint等元数据不等于固定物理权重版本证明。直接 HTTP和 Kimi ACP传输不同；历史 Kimi profile marker原文不可用，系统提示字节不完全相同。
- 当前候选应用器含新的记忆来源约束，与归档 Kimi时期不同；原始候选质量和存储规则影响分别报告。每种语言/模式只有一个机械成功样本，不构成P95、随机配对基准或课堂质量统计。
- 这里只采集文本，没有 UI/扬声器交付、ASR/TTS/相机联合、真实麦克风、浏览器、声学或 release-main验收。

此前计划记忆修复已完成 **938通过、0失败、17忽略**、native构建和 [memory-grounding候选归档](../runtime/goal-releases/20261106-memory-grounding/manifest.json)，见 [专项记录](PERSISTENT_PLAN_GROUNDING_20261106.md)。后续自然记忆入口修复也已完成：362文件冻结，**956通过、0失败、18忽略、无 warning**，native exit0；[29文件候选](../runtime/goal-releases/20261106-natural-memory-intake/manifest.json)未安装，详见 [自然记忆记录](NATURAL_MEMORY_INTAKE_20261106.md)。它在本次模型测量后应用，不把新源码或候选说成已完成这些实际模型验收；DeepSeek仍是测试专用诊断。

后续应围绕数量角色、物理前提和来源忠实性继续已登记的质量工作。生产仍用普通 Kimi，安装版未替换，完整 Goal继续 ACTIVE。
