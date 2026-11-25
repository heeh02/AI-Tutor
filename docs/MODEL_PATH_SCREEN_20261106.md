# 共享 GPU 长运行与教学模型路径进展

用户最新优先级以 demo 级充分性为准，并转向 Destiny 项目分析；后续 AI Tutor 优化与验证工作暂缓。下述完成状态保留，完整 Goal 未标记完成。

当前源码已实现**可选 Flash 前台 metadata_v2**，四个固定实际请求的运行验收全部通过；三个完整回答中 **2/3 内容与应用效果通过**，英文的“probably 零点偏移或舍入”解释缺乏依据。活动取消只有未显示前缀，不计作完整回答。默认仍为普通 Kimi，候选未安装，用户策略未改，完整 Goal 保持 **ACTIVE**。

## 当前 Flash 接入与验收边界

中文流式、英文流式、活动请求取消和整段回复共 4 次 HTTP 请求，0 自动重试／替代槽，所有权与清理核验通过。`metadata_v2` 禁止元数据终稿重复提供正文，由服务端原样拼接已验证片段并校验完整回复；运行一致性不覆盖英文完整内容失败。

| 当前路线 | 运行 | 完整内容 | 合格观察时刻 |
| --- | --- | --- | --- |
| 中文流式 | PASS | PASS | 序号 1 持久化观察 1586.9262 ms。 |
| 英文流式 | PASS | FAIL | null；正确开头不覆盖后续解释问题。 |
| 活动取消 | PASS，EXERCISED | 无完整回答 | null；未 ACK 前缀。 |
| 中文整段回复 | PASS | PASS | 整段显示 ACK 后的交付提交观察 2624.9211 ms；无公共片段序号。 |

只在全回复内容及运行同时通过后作上述关联。内部入队、应用观察、浏览器绘制与声学终点不同；不作 P95、总体速度或默认采用结论。原 `teaching-provider-real-first` 369 文件试验整体 FAIL 与整段 `final_text_mismatch` 保留，缺失原始整段文本，不能推定空格原因。

当前 369 文件源码 manifest 为 `7a896f88ee2d8ea6b72ac9a87529cc0aae7d4e04cbce9e4276a8fd758c3bf6b5`，**991 通过、0 失败、20 忽略**；workspace／native 各有一个 `LegacyV1` dead-code 诊断，优化诊断构建无 warning。native `cfdecbfb…` 与经身份核验的 367 阶段 UI 构建已组成 [29 文件候选](../runtime/goal-releases/20261106-teaching-metadata/manifest.json)，manifest SHA256 `403417c59b70e15164795ea5440da4e7c39df41876157ded8d2ffe9be8448045`，未安装。当前安装主程序仍为 `16c654e1…`。

当前 Flash 的真实音频、浏览器、ASR／TTS／相机／后台记忆联合、30 分钟运行和 release-main 启动均 **NOT_RUN**；`flash-joint-next` 只是未运行提案，本检查点不绑定其可变文件。此前普通 Kimi 的长运行不能替代这些验证。详见 [专项报告](FLASH_FOREGROUND_METADATA_20261106.md)、[独立运行审查](../runtime/goal-baselines/20261106/teaching-metadata-first/independent-runtime-review.json)、[内容与运行合并](../runtime/goal-baselines/20261106/teaching-metadata-first/comparison-review.json)及[检查点](../runtime/goal-checkpoint-20261106-flash-metadata.json)。

## 此前阶段记录

下文原样保留先前各阶段的状态与结论；其中“仅为诊断／尚未接入”对应历史版本，当前状态以上文为准。修改前全文已归档。

当前普通 Kimi 路径的 30 分钟联合运行已通过限定范围。DeepSeek Flash 已完成分开登记的 5 次 API 请求：1 次终稿契约失败、4 次机械通过，**0 次获得完整教学策略与默认采用资格**。较早出现的内容性片段不等于合格时延；生产默认仍是普通 Kimi，用户策略和安装版未改，完整 Goal 继续 ACTIVE。

## DeepSeek Flash：内容更早出现，质量门限未通过

请求分别使用显式关闭思考或 `thinking=enabled`＋`reasoning_effort=low`，每次上限 8192 token/90 秒、无自动重试。原始四槽运行在首个中文 off 请求终稿校验失败后停止，剩余 3 槽保持未执行；后来的 1 次中文 off 和 3 次其余模式调用各自另行登记，不回填原结果。

| 独立尝试 | 首个内容性完整片段序号 | 原始客户端观测时刻 | 结论 |
| --- | ---: | ---: | --- |
| 原失败中文 off | 1 | 1.703 秒 | 只保留失败前缀，终稿与候选不可追溯；不能赋予合格时延。 |
| 后续独立中文 off | 1 | 2.248 秒 | 机械通过；静态检查后直接低速试跑，物理前提不完整。 |
| 新登记中文 low | 1 | 4.775 秒 | 机械通过；把 20 cm 阈值错误推为约 10 cm 最终停车余量。 |
| 新登记英文 low | 0 | 3.917 秒 | 区分“读数不是停车结果”已有内容，但擅自判为静止测量，后续直接要求试跑。 |
| 新登记英文 off | 1 | 1.445 秒 | 机械通过；假设可用的电源控制及试跑前提，未获完整策略通过。 |

这些是从调用入口到所选完整内容片段的原始记录，**不是合格首句时延**。调用间缓存命中不同，Flash 是模型别名；直接 HTTP 与 Kimi ACP 的传输、历史 profile marker和候选应用版本也不同。原 Kimi 普通档约 16–21 秒、Highspeed 约 11–13 秒仅作历史观测，且 Highspeed 英文有策略失败，不能据此认证速度提升或切换默认。

英文 low 实际保存了 1 条忠实的 `current_activity reported_progress`，保留“30 cm、尚未行驶”；中文 low 的 `current_session reported_progress` 没有持久化，`rejected_candidates=0`，不能称为后端拒绝。原始事实候选与教学文本中的错误分开评估。

首次预检 0 调用和 v2 路径准备失败均保留；原契约失败没有保存完整原始终稿，新增可观测性及后续成功不能倒推旧失败原因。数量角色反例已保存为未注册的数据提案，尚未执行新质量评测。详见 [DeepSeek 专项报告](DEEPSEEK_FLASH_SCREEN_20261106.md)、[三次运行审查](../runtime/goal-baselines/20261106/deepseek-three-first/independent-runtime-review.json)及[冻结内容审查](../runtime/goal-baselines/20261106/deepseek-three-first/content-review.json)。

## 当前普通路径的 30 分钟联合运行

同一 Workstation 内完成 **42 轮，中文 21 轮、英文 21 轮**；准备完成后的 active＋有界收尾为 **1833.553 秒**，满足 1800–2100 秒窗口。外层 exit0，用时 2068.049 秒。Qwen1.7B 提供全部 42 次终稿，无回退或强制改写；Qwen TTS 完成 116 个正式片段，native CPAL 启动与终态回执逐项匹配。

- 第 22 轮切换英语，正常 settings/enable 转换用时 12.076 ms；学习者、会话和相机 epoch 保持不变，两组输入各开关 21 次并完成 producer join。
- A 在 active 20.070 秒开始后被 Start 抢占；B 在 1524.054 秒开始后被 Submit 抢占。两者均以同一 job 的第二次 attempt 恢复，总计 4 次后台调用，未增加替代任务或自动重试。
- 共 46 次教学 provider 调用：42 前台＋4 后台，44 完成、2 取消、0 错误、0 重试。两个后台候选均为 shadow，最终全库持久 memory 为 0。
- 42 轮没有把停车请求写成已执行或成功，也没有把老师新增的方法写成已接受计划。第 8 轮保存的目标是原始停车意图；pending/result/attempts 保持为空。后台候选分别忠实保留明确持续偏好与已接受但未实施的桥梁比较计划。
- 两侧 GPU 角色在观测中保持就绪，TTS 可见实例不变；CPU ASR、final ASR、TTS、相机、provider、scheduler 及输入源均正常清理。整卡抽样峰值 **10726 MiB**，相机中位处理帧率约 25.15 fps。

本轮教学沿用生产默认的普通 Kimi 路径、`low` 思考档，没有 Highspeed override；保留指标未逐次记录远端 resolved model ID，不扩大为云端物理模型身份认证。

提交接受到首次 native 回调为 **7.161–21.325 秒，中位 12.962 秒**，不是声学首音，也不构成低时延目标通过。此次使用优化 ignored-library 程序和 release 相机 worker，输入为固定成人 SAPI WAV、显示 ACK/heartbeat 为模拟；**未验真实麦克风、浏览器、声学输出、实体 PTZ 或 release-main 启动**。两个后台来源预先带有明确偏好/接受标记，不能据此认定普通自然输入的记忆路径已闭环。

[专项报告](QWEN_MANAGED_SOAK_20261106.md)、[实际报告](../runtime/goal-baselines/20261106/qwen-soak-first/actual-first/report.json)、[独立运行审查](../runtime/goal-baselines/20261106/qwen-soak-first/independent-runtime-review.json)、[独立语义审查](../runtime/goal-baselines/20261106/qwen-soak-first/semantic-review.json)

## ASR先准备、TTS后启动

沿用350文件冻结集及release主程序`e30ee3ebdc4fa16d8e7b392cd404c5166ac85ea3c67aaa397ae30c4ec9639123`，在新fixture中先启用Qwen1.7B ASR、保持TTS停用。ASR预热后只修改fixture的TTS策略，并显式Retry一次。

- ASR在约67.48秒就绪；67.50秒启用TTS；177.66秒两侧全部就绪。
- 先正常停TTS，ASR仍就绪；随后正常停ASR与main。桥接/service/main均exit0，无强制结束，ASR协议无失败。
- 仅1次ASR静音预热＋2次TTS双语预热；没有正式转写/合成、教学、浏览器或输入输出设备请求。
- 整卡704次采样峰值10824MiB；外层命令180.75秒。不能当作冷启动基线、并发推理或任意输入下显存硬上限。

这补齐了此前主要覆盖TTS-first顺序的证据。[实际报告](../runtime/goal-baselines/20261106/gpu-pair-asr-first-next/actual-first/report.json)、[独立复核](../runtime/goal-baselines/20261106/gpu-pair-asr-first-next/independent-runtime-review.json)

## Kimi四次固定对照

当前`k3.1-pro`已使用最低支持思考档`low`；其Highspeed版本官方说明约2–3倍输出速度、约2倍额度消耗，不能把输出吞吐宣传当作有效首句保证，也不能给当前模型凭空设置不支持的关闭思考选项。[官方模型说明](https://www.kimi.com/code/docs/en/kimi-code/models.html)

使用同一受支持的managed OAuth/ACP路径：中文普通→高速，英文高速→普通，总共4次请求、零补跑。每个语言对的应用层系统/用户提示、来源记录和profile marker一致；运行时核验模型选择与`low`，不声称独立验证了云端物理权重。全球默认配置不改，所有owned CLI scratch确认清理。[实际报告](../runtime/goal-baselines/20261106/kimi-highspeed-first/actual-first/report.json)、[运行审查](../runtime/goal-baselines/20261106/kimi-highspeed-first/independent-runtime-review.json)

诊断仅增加`cfg(test)`接口及固定实验，没有改生产默认选择或重写传输。352文件冻结，933项回归通过、0失败、16忽略；优化测试程序SHA256为`a4c9d9b76c27bd2abfc3a18346dca645da5e4126694c30e8ffa0a9c7f83c9e94`。

### 内容盲审与计时口径

在打开模型/时间映射前，四份输出随机排序并移除身份、索引和时间字段进行内容盲审。两组开头都只是事实复述，保守地不计入首个有用教学；四次均选完整`sequence=1`，统一使用`received_from_invocation_ms`，不混入以prompt写入为起点的`first_validated_segment_ms`。

| 语言 | 普通档内容性片段 | Highspeed内容性片段 | 策略结论 |
| --- | ---: | ---: | --- |
| 中文 | 20.68秒 | 10.95秒 | 固定内容准则通过；搭建/静态与动态测量条件仍有说明不足。 |
| 英文 | 16.05秒 | 12.73秒 | 高速片段使用未报告的设备停止控制，不能计作合格教学首句。 |

四份在固定的数字、语言、来源、未执行/未完成及建议不冒充已接受计划等轴上通过；完整既有策略补查又发现高速英语片段预设了可用的设计停止控制，违反`C_STOP/C_SEGMENT`前提。保留原始计时，但不给这一条合格时延值；小样本不构成课堂默认采用或P95证明。[合并审查](../runtime/goal-baselines/20261106/kimi-highspeed-first/comparison-review.json)

英文普通档有一条忠实的`current_session reported_progress`候选；实际未持久化、`rejected_candidates=0`，不能把它称为“后端拒绝”或已验证的慢记忆。原始候选与应用效果均逐项检查。片段被采集用于评估，没有交付到浏览器或扬声器，也没有ASR/相机参与。

初始盲审文件的部分作者说明因编码变成问号，原文件保留；可读版本仅重述说明，判定、来源、候选及已冻结的片段序号/原文未改，评审者仍未读取模型/时间映射。完整策略补查单列，不能让窄范围PASS覆盖新增失败。[可读盲审](../runtime/goal-baselines/20261106/kimi-highspeed-first/semantic-blind-review-readable-v2.json)、[策略补查](../runtime/goal-baselines/20261106/kimi-highspeed-first/semantic-policy-supplement-readable-v2.json)

## 后续

Highspeed 与 DeepSeek 均未切为默认。后续先按已保留的数量角色反例和物理前提要求继续质量工作，再判断模型路径；短时内容出现不能替代完整回复、候选和实际交付的验收。

此前计划记忆修复已完成 938 项回归及 [memory-grounding 候选归档](../runtime/goal-releases/20261106-memory-grounding/manifest.json)。后续自然记忆入口修复已冻结 362 文件，**956 通过、0 失败、18 忽略、无 warning**，native 构建成功；[29 文件候选](../runtime/goal-releases/20261106-natural-memory-intake/manifest.json)已归档、未安装。该候选包含记忆修复，DeepSeek 仍仅为测试诊断，不能将此前模型测量追认到新源码。详见 [自然记忆入口修复](NATURAL_MEMORY_INTAKE_20261106.md)。真实音频、浏览器等余项继续，实体 PTZ 仍暂缓，完整 Goal 保持 ACTIVE。
