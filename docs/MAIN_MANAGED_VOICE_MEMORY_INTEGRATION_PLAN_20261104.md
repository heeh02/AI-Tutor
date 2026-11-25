# 同一工作台托管语音与影子记忆：下一片最小计划

状态：测前设计已于 checkpoint22 实施，实际结果与限制见 `MANAGED_VOICE_MEMORY_VALIDATION_20261104.md`。下文保留原方案；实际两轮方案让 A 在首次 armed 空闲后启动，不额外增加预备教学轮。旧 checkpoint20 长测不追认新增范围。进程启动回执不证明远端已经生成，实际十秒门限和恢复时序也按实测文档澄清。

当前 `voice_loop_soak_diagnostic` 使用同一 Workstation 的真实相机、教学、采集管线和 CPAL 输出，但 Qwen 生命周期由外层 runner 持有，ASR 由诊断直接持有；后台记忆为 `not_exercised`。固定 WAV 和显示 ACK 是模拟输入，未打开物理麦克风，也未验证声学轮换或浏览器显示。即使当前长测成功，也不能写成“main 全托管语音及后台记忆整机联合验收”。`main-managed-qwen-first` 是另一份主程序生命周期证据，不能与长测拼成一次完整运行。

## 1. 复用生产生命周期，不另造服务管理器

新增独立 ignored 诊断，复用已有 `round()`、真实相机采样和精确 worker SHA 配置。隔离 fixture 分发已有 ASR 三文件、TTS 七文件、固定模型硬链接和环境指针；写 fixture 专属 managed ASR/TTS 策略，不写生产 descriptor，不接管旧 18867 或其他服务。Qwen 沿已验证的 stdin 私有 token、实例绑定和 Linux 自有进程组协议运行。

同一 Workstation 调用 `start_local_speech_runtime()`、`start_local_asr_runtime()`；不再使用 `lab_tts()` 手动配置客户端或诊断直接 `OwnedAsrService::spawn()`。分别观察 `ready`、`warmup_completed`、`active` 与实际安装回执，全部成立后才开始语音回合。保留现有 readiness、warmup、停止期限；外层总预算须包含这些现有阶段，不通过放宽内部超时掩盖失败。

结束或错误首先走 `begin_shutdown()` 的音频/采音/运动失效，再 `close()` 回收同一工作台的自有服务；核对 ASR 两 Job/解释器退出与 TTS Linux 清理、bridge 退出、客户端停用。外层 runner 只负责分发、监控测试进程和保留证据，不再自行持有 Qwen。ignored harness 调用的是生产生命周期实现，仍不等于发布版 `main` 的锁、listener、descriptor 启动流程已在同一次测试中覆盖。

## 2. 两份明确的合成后台任务，分开验证两种抢占

直接开启 `memory-policy.json` 的 simulator shadow 不足以制造真实工作：生产语音提交的事件只有原始 `text`，SyntheticOnly 要求来源明确带 `synthetic_fixture=true`；连续录音、转写、生成或播报及输入后的 10 秒门限也会正常阻止 claim。不得取消这些门限、改生产时钟或把零调用称为抢占通过。

在同一 Workstation 的 simulator Store 中建立**独立合成学习者及两份已结束活动**，每份使用预先冻结的少量教育来源并明确标记 `synthetic_fixture=true`。不修改当前语音活动的事件、ASR 原文或版本；两份后台活动不进入当前学习者上下文，也不自动 apply。复用生产 `start_memory_schedule()`、同一 `app.provider`、持久 job 和 lease 校验。

| 固定任务 | 真实协调位置 | 需要观察的结果 |
| --- | --- | --- |
| A：Start 抢占 | 前一回合已完整结束，音频空闲且真实输入静默满 10 秒；等待 job 的真实 `started_at_ms` 后，发下一轮生产 record/start | `cancel_turn()` 使背景让出；开始的是精确新 capture ticket。随后真实 ASR、教学、CPAL 完成，无旧候选提交 |
| B：Submit 抢占 | 真实 ASR final 与完整票据已核验，尚未 POST turns；在该窄窗口满足原有空闲门限，等待第二份 job 真正开始，再提交原 ASR 文本 | 生产 `foreground()` 与 Kimi 同一 gate 的前台登记取消背景；新教学请求完成，未被背景排队越过 |

任务 B 需要 cfg(test) round 增加一个 before-submit 协调点，或拆出原有 capture/finalize 与 submit/finish 两段；协调点不能替换文本、伪造 endpoint、设置 lease phase 或生成播放完成回执。heartbeat 在协调期间持续走原接口。若背景已自然完成而未观察到在途抢占，应如实标记该分支未覆盖，不能自动再造任务补过。

## 3. 固定预算、恢复与通过门槛

**推荐在同一轮验证抢占与恢复**：预声明两份固定 job，每份最多两次尝试，总计最多 4 次实际后台 provider 启动。配置 `max_pending=2`、`max_attempts=2`、日预留 4、每活动预留 2、lease 120 秒；两份任务的累计 reservation 也不得超过 4。reservation 不等于模型已启动或成功，即使某次未启动就失败，也不能返还预算另补尝试。教学与后台调用分别计数。

固定顺序是：A 首次 started → 生产 Start 抢占 → 第一轮语音完成 → 保持空闲、由生产 scheduler 恢复同一 A job → A 完成 shadow；再执行第二轮真实采集，在 Submit 协调点引入 B → B 首次 started → 生产 Submit 抢占 → 第二轮语音完成 → 原 scheduler 恢复同一 B job → B 完成 shadow。只在 A 的恢复结果已记录后引入 B，避免 A 的重试被误认成 B 的首次调用。恢复沿生产 `preempted → pending → claim` 和新 lease receipt，不新造 job、复制来源、修改版本或重抽教学回复。两次取消后的第二次后台调用就是被测恢复行为，不是未声明的额外重试。

抢占通过要求：真实后台启动证据、对应 attempt 的抢占终态和 lease 释放、无该 attempt 的迟到候选提交、原前台请求/ASR 输入/音频 owner 随后完整成功。恢复通过另要求：同一 job 的第二次 attempt 使用新有效 receipt，来源版本仍当前，响应和候选经生产校验，进入 `shadow_ready`，未自动 apply。不能只凭前台恢复或第二个进程启动就声称快→慢整理完成；若仅得到空候选，须分别记“整理调用完成”与“实质候选覆盖不足”，不得补抽凑出候选。候选内容仍需按冻结教育来源独立审阅。

保留真实 10 秒空闲门限；两段恢复等待与协调时间单列，不能算成持续前台推理或用于凑满 1800 秒。若某次背景在抢占前已自然完成，抢占分支应标未覆盖；失败、耗尽、清理未确认或来源失效均保留原结果，不追加第三次尝试、不循环重启服务。来源更正/删除后的晚结果拒绝由既有测试单列，未在这两个固定任务中实际触发时不得新增实测声称。

较窄的备选方案是分阶段：先冻结最多 2 调用的纯抢占验收，再冻结另一阶段的恢复调用预算和输入。前一阶段只证明前台恢复，不能称后台已恢复；若它使用 `max_attempts=1` 已令 job 耗尽，后一阶段也不能悄改限额将其伪装为原生产 pending job 的自然恢复。为避免这种证据断点，推荐上述同一轮、同两份 job 的最多 4 调用方案。

先以两轮真实语音及两份任务的抢占/恢复完成同实例集成，再决定扩展 1800 秒。新报告分开写服务生命周期、语音回合、后台 reservations/started 尝试、shadow 结果、协调等待与资源采样。此预算调整仅为下一片提案；当前检查点 20 仍没有任何后台调用，不追认旧长测已覆盖新增范围。
