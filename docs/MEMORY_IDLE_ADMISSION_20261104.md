# 连续空闲后的后台记忆准入

checkpoint22 的实际联合运行暴露了短窗问题：旧条件只要求距上次 Submit/比较保存超过十秒并且当时前台/音频不忙。长回复结束时旧计时早已满足，A 的恢复甚至在 voice-loop rearm 前启动；连续自动录音可能马上再取消它，消耗有限的 attempt。

本片只在 memory_schedule 增加局部连续空闲窗口。生产仍固定十秒、两秒轮询；保留旧 last_input 门限，观测到教学前台、录音、转写、合成、播报或 capture_pending_result 时清空窗口。recording_generation、speech_epoch、last_input 的变化可识别两次轮询之间的短活动。窗口从第一次观察到空闲时开始，属于保守的采样时间，不是假称精确测得最后音频结束的时间戳。

在 discovery 前和实际 claim 前都复核；worker 返回 preempted、foreground_or_worker_busy 或 preempted_before_claim 时，也丢弃旧窗口，避免发生短前台脉冲后下次轮询立即使用旧资格。不增加 max_attempts、不退款、不新造 job、不改变来源/租约校验。持续没有前台活动时，成功后台任务之间可以复用已经成熟的窗口。

独立审阅发现原草案遗漏后两种 worker 返回，以及把待取终稿视为空闲。实际源码已修复，原发现、冻结草案与新复核分别保留。新回归包含真实 Store 队列在短窗内没有 reservation、原 job 被抢占后仍保留已用预算并等新窗口、实际 MemoryWorker 忙态拒绝、待取终稿随后短 ASR 的状态序列。preempted_before_claim 分支使用确定性状态注入，不宣称复现了真实线程竞争。

`memory-idle-admission-first` 的九项定向测试实际通过；不是把测试时钟注入当成真实十秒运行。随后完整 workspace48 为 848/0/10（135.953 秒），加入实际实验 harness 后最终 workspace49 为 851/0/11（127.479 秒），两次均含 UI128、无 warning。真实三调用预检也已完成：同 job 的第二 attempt 首次被观察到在最终 ACK 开始后约 13.829 秒，最后一次未见它在约 13.760 秒；只能报告观察区间，不能把它改写成精确的十秒 claim 遥测。细节见 `BACKGROUND_ACP_MEMORY_20261104.md`。旧 checkpoint22 结果不按新准入规则重判。

边界：轮询无法承诺新输入在最后一次检查后到达时绝不启动；原 MemoryWorker 前台序列化和取消仍处理这类竞争。本片不改变语音 VAD、rearm 时长或用户的输入内容，也不把延迟统计改成更容易通过的指标。
