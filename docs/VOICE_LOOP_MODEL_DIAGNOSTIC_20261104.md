# 固定输入与真实模型、播放的两轮语音轮换诊断

本片新增独立、默认忽略的 Windows 库内诊断。旧的两轮 fake-owner 集成测试、真实模型 transport chain、production segment playback 诊断和原 runner 均不修改。

精确测试名：`server::voice_loop_model_diagnostic::real_synthetic_voice_loop_two_rounds`。

## 实际路径与模拟边界

每种语言使用一个新的 Workstation、一个 simulator 学习者和 session，连续执行两轮。固定 `zh_short` / `en_short` WAV 使用原先冻结 SHA 和 SAPI 成人合成语料 manifest；它不是儿童或环境采音。

唯一输入替身位于 `capture_loop` 打开设备的位置，且仅存在于 `cfg(all(test, windows))` 构建。它绑定当前 AudioService 实例中的精确 opaque key；错误 key 或 legacy index 在已安装诊断源时必须拒绝，不能回退到真实麦克风。原 WAV 后明确构造 4000 ms 合成静音尾部，整段不足 20 秒；这是事先声明的有限测试输入，绝不在丢帧、错误、EOF 后无限补静音。原始 16 kHz 单声道 PCM 转为 44.1 kHz，每个墙钟周期最多推送一个 20 ms 块，不追赶式加速。没有新录音文件。

后续全部走现有生产流程：bounded Start/reserved token → `push_samples` → 完整 PCM 缓冲 → Sonora 10 ms 分析 → owner 关闭 source → `recording_endpoint` → bound Stop → 真实 Zipformer final → bound turns → 真实 Kimi ACP segments → 模拟有序 display ACK → 真实 Qwen/CPAL speech-owner → Store 中的真实开始和完成收据 → 500 ms 输出尾部等待与自动 rearm → 第二次 Start。

source Drop 先通知生产线程结束，再 join；之后生产 capture owner 才能发布 `input_closed=true`。该关闭证明针对测试输入源，不证明物理麦克风关闭。诊断不会赋值 lease phase、调用 fake `speech_end()`、伪造 SpeechStarted/Completed，或使用 fake output owner。

独立模拟客户端每 400 ms 发生产 heartbeat。只有实际 `speech_complete`、输出 owner 空闲和生产冷却规则允许后，客户端才请求下一次 Start。完成第二轮并观察 rearm 后明确停止，不开启第三轮。显示 ACK 和客户端心跳为模拟；未验收浏览器行为。

## 完整性与结果

- 两次 Start、两次 join 后的关闭、两次真实 ASR final、两次 teaching 请求、恰好两个 user turn。
- Send 使用实际 ASR 原文；仅独立比较时忽略标点和大小写。错词或截断保留为失败，不替换固定参考文本、不补词、不自动重试。
- 端点必须是相同 recording generation 的真实 `speech_end`，且 `input_closed=true`；其他 reason 不能自动提交。
- 每个完成的 speech segment 必须有匹配 playback ID/epoch 的 `qwen_pcm` first_callback_submission 与 owner Completed 收据，callback frames 非零、predicted_output_drained=true。
- 重复 Start/Stop/Send 被拒绝；若及时观察到正在播报，还会尝试 Start 并记录拒绝覆盖。该额外观察失败不能被标成已覆盖。
- 正常生产候选提交发生在隔离 simulator DB，报告保留最终 session；不能宣称这些候选全部仍是 shadow。
- 每轮保存真实 final、endpoint、segments/receipts、rearm、source lifecycle 与当前 fixture state。`cooldown_observed` 和 `rearm_wait_observed_ms` 是消费者观察，不代替生产计时或声学测量。

开始回调与预测排空不等于扬声器物理首音或可听结束。本片明确 `microphone_opened=false`、`camera_opened=false`、`ptz_enabled=false`、`speaker_output_measured=false`、不改系统音量。输出使用实际默认音频设备，必须由根代理统一安排运行。

## 所有权与运行合同

Qwen 服务仍由新的外层 runner 独占拥有并在 finally 收尾；诊断内拥有独立 managed ASR 实例并将其不可变 client 安装到同一 Workstation AudioService。它不是生产 main 自动托管两项服务的联合生命周期验收，也不是 30 分钟压力验收；legacy ASR 18867 和生产 descriptor/policy 不变。

环境变量：

- `AI_TUTOR_REAL_VOICE_LOOP=1`
- `AI_TUTOR_VOICE_CHAIN_LANGUAGE=zh|en`
- `AI_TUTOR_VOICE_CHAIN_TTS_ENGINE=qwen`
- `AI_TUTOR_VOICE_CHAIN_RUN=<新的小写唯一名称>`
- `AI_TUTOR_VOICE_CHAIN_TTS_TOKEN=<外层 owned Qwen 私有凭据>`，只走子进程环境，不写文件或打印。

`lab_tts` 复用固定 Qwen 18868；没有通用 endpoint 配置。每种语言最多 2 次教学调用，每轮生成/播放观察期限 180 秒。文件采用 create_new，不覆盖旧结果。

顶层 `report.json` 与新 runner 的字段一致：`test_type=real_synthetic_voice_loop_two_rounds`、language、passed、production_capture_loop、synthetic_capture_source、production_audio_owner、simulated_display_ack、provider_metrics、source_lifecycle、owned_asr_stop、provider_cleanup_confirmed、protected_files_unchanged；rounds 的 case 为 round_1 / round_2，分别记录 completed / rearm_observed。失败同样收尾并保存报告；退出码 0 不构成声学或教学语义质量结论。

## 当前验证状态

根代理已完成首轮实际运行，证据在 `runtime/goal-baselines/20261104/voice-loop-model-first/report.json`。外层 243.98 秒正常退出；自有 Qwen 冷启动到就绪 113.407 秒，显式预热后顺序运行中文和英文，各两轮、合计四次真实教学请求。两个实际 test executable 均为同一冻结字节 SHA `3bdbcf527d9ab5df82e8c51149ba5f9136ef40854fe7f7f5c4c30a1680b99f85`。

四轮均实际采集关闭并 join、Zipformer 原文通过独立完整性比较、原文送入教学、观察到首段播放回调先于全文 final，以及实际冷却和 rearm；七个语音段有匹配的 Qwen/CPAL Started 与 Completed 收据。两种语言每次打开/关闭源均恰好两次；没有第三轮采集。每轮总耗时 24.87–32.23 秒（包括输入、模型与整段播报），观察到的 rearm 等待 527.88–533.00 ms。这些值不是首句响应延迟或声学首音测量。

两个自有 ASR 正常收尾、Job 为空；教学 provider 正常 close；外层 Qwen 以 owner_stop 正常退出，未强制结束服务组。受保护文件在该次运行前后未变。没有打开物理麦克风、相机或 PTZ，没有真实浏览器验收或保存原始音频。

这次运行发生在随后修复历史任务来源写入边界、增加教学提示规则**之前**。实测 exe、当时声明的 47 个源码文件及两个方法单独保存在 `checkpoint17-measured-binaries` / `checkpoint17-measured-sources`；不能把旧链路成功追认成后续 guard/policy 版本的整链实测。它也不证明真人听感、儿童语音、混语、声学首音/静音或 30 分钟联合运行通过。

新增 3 项无设备回归：

- `source_drop_joins_the_producer_before_publishing_closed`
- `diagnostic_source_rejects_other_binding_or_legacy_instead_of_opening_a_device`
- `fixed_transcript_comparison_ignores_punctuation_not_missing_or_changed_words`

另新增 1 项默认忽略的真实诊断，见上述精确测试名。

## 后续同实例相机的接点

库测试的 current_exe 是测试 harness，不能处理生产 `--camera-discover` / `--camera-worker`。后续需使用实例私有的显式 release worker 路径与 SHA，仍沿用现有 Windows Job、stdin RUN gate、子进程收尾；不能使用全局环境替换、裸 PID 杀进程或把相机的真实工作搬进测试 harness。根代理已负责该独立 cfg(test) 接点。本片不写其配置，不打开相机。

本诊断的单轮函数已经独立，后续同 server 模块的持续诊断可窄导出并复用；无需复制模型/采集/播报整链，也不需要修改生产 HTTP 接口来注入 WAV。
