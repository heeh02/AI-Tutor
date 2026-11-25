# Flash 前台 metadata_v2 接入与四路文本验收

用户最新优先级以 demo 级充分性为准，并转向 Destiny 项目分析；后续 AI Tutor 优化与验证工作暂缓，以下已完成证据保留。完整 Goal 未标记完成，尚未运行的项目不改判。

当前源码已实现可选的 DeepSeek Flash 前台 provider，并显式采用 `metadata_v2`：终稿只返回元数据，正文由服务端按已验证片段原样拼接。四个固定实际请求的运行验收全部通过；三个完整回答中，**两个通过内容与应用效果资格，一个英文回答未通过**。取消槽只有未显示前缀，不计作完整回答。默认仍为普通 Kimi，候选未安装，用户策略未改，完整 P0–P5 Goal 保持 **ACTIVE**。

## 实现与运行范围

Flash 接入既有 Workstation 提交、前台准入、取消、片段持久化与显示确认路径。`metadata_v2` 验证请求绑定、片段数与引用，禁止终稿元数据再次携带 `reply_text`，再从片段精确拼接正文并执行完整回复校验；它没有通过忽略空格或接受不一致正文来放宽契约。普通 Kimi 和旧诊断继续保留原有协议。

实际执行使用绑定源码的优化 ignored-library 程序，通过正常应用 dispatch 完成中文流式、英文流式、活动请求取消和整段回复四槽；模型为 `deepseek-flash`、`non_thinking`，**4 次 HTTP 请求、0 自动重试、0 替代槽**。三次完整响应均有 HTTP 200、stop、DONE、EOF；取消槽在精确活动注册及一个持久化前缀被观察后停止，未确认显示，候选未应用，前缀随后从持久表删除。

[实际报告](../runtime/goal-baselines/20261106/teaching-metadata-first/actual-first/report.json)、[预注册方法](../runtime/goal-baselines/20261106/teaching-metadata-first/actual-first/preregistered.json)、[独立运行审查](../runtime/goal-baselines/20261106/teaching-metadata-first/independent-runtime-review.json)

## 内容资格与时刻关联

内容审阅先按固定真值选择内容性片段并判断全回复；该审阅不是声称盲审，没有打开运行计时文件。随后仅把全回复内容、应用及运行门限同时通过的样本关联到保存的持久化观察。英文的正确开头不能覆盖后续解释问题。

| 固定槽 | 运行结论 | 完整内容结论 | 合格观察时刻与终点 |
| --- | --- | --- | --- |
| 中文流式 | RUNTIME_PASS | PASS | 内容性片段 `sequence=1` 首次持久化观察 **1586.9262 ms**；不是显示或发声时刻。 |
| 英文流式 | RUNTIME_PASS | FAIL | **null**。内容审阅选出的序号 0 保留为来源记录，但不赋予合格时延。 |
| 活动流取消 | RUNTIME_PASS，EXERCISED | NOT_ASSESSABLE | **null**。只有一个未显示前缀，没有完整回答。 |
| 中文整段回复 | RUNTIME_PASS | PASS | 整段显示 ACK 后，提交应用被观察到的 **2624.9211 ms**；没有公共片段序号。 |

中文流式序号 0 只是复述已给事实；序号 1 才新增 240 mm ÷ 10 = 24 cm 和记录表措辞。英文把单次 8 cm／85 mm 差异解释为传感器零点“probably”偏移或读数舍入，缺乏分辨率、校准或重复测量依据，因此存在实质解释来源问题；这不等于算术错误、断言恒定 5 mm 偏差或不安全运动。英文记忆候选仍忠实保留原始报告，没有保存该推测原因。

整段回复没有公共片段列表，不能把内部首段入队的 1576.8412 ms 当作整段交付。记录的整段 ACK 返回为 2624.8928 ms，随后观察到提交为 2624.9211 ms。流式正文已独立逐字符核对为持久化片段的精确拼接；整段正文的内部派生由绑定源码支持，并与已显示 turn 和任务状态一致，但没有原始 wire 文本或内部各段字符串可供独立重拼。

表中的合格观察时刻均采用提交起点的应用观察时钟；内部入队使用 provider 调用起点。浏览器绘制和声学首音又是不同终点；本轮只有重复合成输入的小样本，**不作总体速度、P95、协议因果收益或课堂质量结论**。

[冻结内容审查](../runtime/goal-baselines/20261106/teaching-metadata-first/content-review.json)、[内容与运行合并](../runtime/goal-baselines/20261106/teaching-metadata-first/comparison-review.json)

## 来源、应用与清理

独立运行审查 **99 项通过、0 项失败**。四槽的固定输入、来源摘要、学习者、会话、请求、generation、provider 指标与 SQLite 记录相互匹配。两条完成流分别有 4／3 个已确认片段；三个完整回复各自应用两个来源支持的任务字段，英文另保存一条活动内报告记忆。来源均为 `simulated`，`hardware_verified=false`，建议的下一次测量没有被写成已接受或已完成工作。

读取六份 SQLite 前均确认没有 WAL／SHM，以 `mode=ro&immutable=1` 打开；读取前后数据库散列一致。清理后的会话 generation 比逐槽交付快照增加 1，任务状态与 revision 保持一致。正常处理器没有暴露完整候选拒绝／省略计数，因此不从最终效果反推所有拒绝原因。

两个 fixture 的 provider 与 server tasks 清理完成，外层拥有的进程正常 exit0、未强制终止，耗时 **10.1044477 秒**；命令终端 receipt 为 **10.2387979 秒**，两者起点不同。审查时 PID 182864 已不存在。取消只证明本地 HTTP 所有权退休，不证明远端计算已经停止。保存的 ASR 清理快照虽为 disabled／inactive／not-ready，`busy=true` 仍如实保留，不能说全部设备 busy 字段均为 false。

## 源码、构建与候选

各阶段身份分别保留，旧结果不追认到新源码：

| 阶段 | 源码数 | 源码 manifest SHA256 | 本阶段结论 |
| --- | ---: | --- | --- |
| teaching-eval-first | 365 | `ede128f72a17de9e95bfeb77c1e499f079c9c7306b65377e1b1d8e9cfb9c36de` | 16 次配对评估；15 终稿通过、1 失败；baseline 6/8、revised 7/8。 |
| teaching-provider-first | 367 | `a2d89d64a4389f2f7a4368e857cb60618c3a406745cad6c6e5e4bd1f0e8349b7` | 可选 provider 接入；977 通过、0 失败、19 忽略；native／WASM 构建完成。 |
| teaching-provider-real-first | 369 | `36b7257c12251b1f5b8040632b16328cae0769eb0a532b41e77a062417adc97f` | 原四槽试验整体 **FAIL**；整段回复 `final_text_mismatch`，未接受或应用该终稿。 |
| teaching-metadata-first | 369 | `7a896f88ee2d8ea6b72ac9a87529cc0aae7d4e04cbce9e4276a8fd758c3bf6b5` | 新协议四槽运行 PASS；完整内容 2/3 通过。 |

当前 369 文件全工作区 **991 通过、0 失败、20 忽略**。workspace 与 native 构建各有一个已知 `LegacyV1` dead-code 诊断，另有汇总 warning 行；优化诊断构建无 warning。native SHA256 为 `cfdecbfbc7fb59b790598a425d9b400a2854d5bcc89d5ae75ca372cd1186dad0`，优化运行程序为 `4b8f2bf132716fb7f5c7f44dfa443d830865861a9b88ec6f374553365d4bf30b`。

UI 使用 367 阶段已构建的 dist：31 个 UI 源文件与当前 369 冻结集及工作区逐字节一致，5 个 dist 文件也一致。这是经身份校验的复用，不是声称在 metadata 阶段重建了 WASM。

[29 文件候选清单](../runtime/goal-releases/20261106-teaching-metadata/manifest.json) SHA256 为 `403417c59b70e15164795ea5440da4e7c39df41876157ded8d2ffe9be8448045`，共 **27,316,911 bytes**，逐文件大小与散列已核对。它是候选差量包，未安装、未发布，也不是独立安装器。当前安装主程序仍为 `16c654e1dae5fbce5431ce1df3a5b3616da655e7106ef7bb011bf5ceefbcaf1d`。默认 Kimi 与用户策略不变的口径来自受控变更范围；本次文档归档没有读取或重新散列私有配置。

原 369 试验的报告、exit101 与独立审查保持不变。其失败整段原始文本未保存，不能指定空格或某种文本差异为根因；新协议成功不会覆盖旧失败。365 配对评估中另有实际保存的拼接失败，是另一条记录，不能混为同一证据。

## 尚未验证

当前 Flash 组合的真实麦克风、声学输出、浏览器、ASR／TTS／相机／后台记忆联合运行、30 分钟运行及 release-main 启动均为 **NOT_RUN**。此前普通 Kimi 的联合与长运行证据保留各自源码、输入和模式边界，不能移植到当前 Flash。`flash-joint-next` 仅为后续提案、**NOT_RUN**；本检查点不绑定仍可修改的提案文件。实体 PTZ 继续按用户安排暂缓。

后续继续处理解释不确定性与来源、当前模型组合的联合交付、真实交互及 P0–P5 余项。完成本阶段不代表完整 Goal 完成。归档链、先前文档快照及本次证明见 [Flash metadata 检查点](../runtime/goal-checkpoint-20261106-flash-metadata.json)。
