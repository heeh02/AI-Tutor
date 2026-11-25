# 语句终点基础：固定方法 v1

第十一检查点补充：显式 `voice_endpoint_v1` 已接到capture owner，默认仍manual。实际Drop输入stream并重核代次/设备后才发布终态；完整PCM保留至Stop/Discard，pending禁止覆盖，20秒上限优先。新增15项无设备回归随workspace 696测试通过，原生/WASM构建成功。真实Windows唯一稳定输入两次短时打开、精确Discard及旧Discard拒绝通过（`input-capture-reopen-first/report.json`），未保存/分析PCM，未验证真实语音终点。完整可选轮换已接线但仍未现场验收；原固定离线方法失败报告及4秒思考停顿风险不变，不调阈值改判通过。下方“未接入”是第十检查点历史状态。

状态：独立模块、固定合成音频与离线诊断已实现。root 已完成统一 workspace 检查（650 个测试通过）和 release 探针首跑；首跑退出 1，未达到全部预冻结目标，完整结果保留。当前录音/自动发送流程没有接入本模块。下方“冻结方法/挑战”保留运行前目标，“首跑实测”单独记录实际结果。

## 冻结方法

`rust/ai-tutor/tests/fixtures/voice_endpoint_v1/method.json` 在任何本片 VAD 结果产生前落盘，SHA256：

`fc66389737aa85015094548e43c793e2398893c3a5f0e0c44e142f4e5cc0d4fe`

沿用锁文件内 `sonora-agc2 = 0.2.0` 的 `vad_wrapper::VoiceActivityDetectorWrapper` 和 `sonora-simd = 0.2.0`。RNN 以内部 24kHz 分析，wrapper 接收 16kHz 单声道 10ms 帧并内部重采样，沿用上游 1500ms 的 RNN 状态周期重置。只有 160 个样本的分析副本变成 FloatS16；输入 PCM 不做降噪、AGC、覆盖或裁切。`NaN/Inf` 不是静音，直接形成设备错误候选。

| 条件 | v1 固定规则 |
| --- | --- |
| 语音确认 | 200ms 窗内至少 120ms 的概率 ≥ 0.60；不要求完全连续。 |
| 后续语音活动 | 已确认后，概率 ≥ 0.35 即刷新最近活动时间。 |
| `speech_end` | 最近活动后连续 2500ms 未达到活动阈值。 |
| `no_speech` | 收到 10000ms 音频仍未满足语音确认；不查询 ASR 字数或 RMS。 |
| `duration_limit` | 收到 20000ms 音频，与安静终点同帧到达时优先标为时长上限。 |
| `device_error` | 所属 generation 的显式设备错误或无效音频；不伪造最终文字。 |

2500ms 终点策略本身会进入“说完到系统首句”的总延迟，不能以 VAD 的微秒耗时替代它。没有根据相同测试集的结果挑阈值；若未来改方法，应新版本并保留本次失败行。

## 模块边界

`audio/voice_endpoint.rs` 提供每次录音独立的 `VoiceEndpoint`：

- `new(recording_generation)`；`process_10ms(generation, &[f32; 160])`；`device_error(generation)`。
- 相同实例只产生一个 `EndpointCandidate`；`terminal()` 可读取该候选。旧 generation 的帧/错误在 VAD 之前丢弃，不污染新状态。
- 候选含 reason、recording_generation、累计收到音频的时间、语音确认时间、最近活动时间。确认时间不是首个字的人工时间标注。
- 候选不含 `input_closed: true`，不关闭输入、不运行 ASR、不 Send、不重开设备。后续 capture owner 必须先真正关闭所属输入，再发布 HALF_DUPLEX 合同中的 terminal。停止收帧/拔设备的墙钟上限仍由 owner 负责，不能将样本时间当成设备仍正常。
- 不用静音补丢失采集帧。此片 probe 对文件末尾不足 10ms 的补零只属于已声明的离线构造。

主代理维护依赖声明，后端代理只加入 `audio.rs` 的模块声明；尚无 capture 调用点。

## 固定音频与 14 行挑战

四个 WAV 为项目既有 SAPI 成人合成中英/混合文本 `zh_short/en_short/mixed_zh/mixed_en`，从既有档案逐字节复制，SHA 固定在 method 中。派生内存样例只去掉完全为零的外部填充，再拼接静音/重复；保留所有其余原样本值，不改原 WAV。源末端是“最后一个非零样本边界”，不是人工标注的语言结束点。

| 行 | 构造与运行前目标 |
| --- | --- |
| 4 个单句 | 500ms 前导静音、原句、4000ms 尾静音；完整源末端以后 `speech_end`，源末端到候选 2500–3500ms。 |
| 2 个 2s 思考停顿 | 中→英、英→中，两句之间 2000ms 静音；保留第二句，目标同上。 |
| 2 个 4s 思考停顿 | 同两组、4000ms 静音；专门保留提前终止和后续仍有源语音的风险，不纳入成功完整轮次或质量通过。 |
| 2 个连续讲话 | 中/英原句加 100ms 间隔重复，500ms 前导后再构造 21 秒；目标仅在 20 秒 `duration_limit`。 |
| 静音、低能噪声、低能 100Hz 声 | 11 秒；噪声固定 xorshift32 seed，峰幅 0.002；目标 10 秒 `no_speech`。不是真实麦克风噪声样本。 |
| 设备错误 | 2 秒零音频后显式错误；只形成一个 `device_error`。 |

每行保留输入 SHA、时间边界、概率范围、结果、目标是否满足、输入是否改变、初始化时间与每 10ms 处理的 p50/p95/p99/max。p99 < 10ms 是运行前固定的 CPU 预算，不是并发推理/真实设备保证。所有 14 行均保留；4s 风险行的目标字段为 null，不能被统计成通过。

## 运行与验证状态

root 已执行以下首次命令（此输出目录现在已有报告，不能再次使用；未来执行必须选全新目录且父目录已存在）：

```powershell
target/release/voice-endpoint-probe.exe --offline-synthetic `
  --fixture rust/ai-tutor/tests/fixtures/voice_endpoint_v1 `
  --method-sha256 fc66389737aa85015094548e43c793e2398893c3a5f0e0c44e142f4e5cc0d4fe `
  --output runtime/goal-baselines/20261104/voice-endpoint-first
```

程序只有该固定 WAV 模式，显式检查 method/WAV 哈希、拒绝复用输出目录。报告包含本次可执行文件 SHA；运行前目标、实测结果、生产验收分别存放。必需目标不满足时保留报告并退出 1；不存在降低目标或自动重试调参。

12 个模块回归和 2 个 fixture 回归已纳入 root 统一通过的 650 个测试：短噪声不能积累成旧语音、窗内短空隙、2s/4s 停顿、时长边界优先级、no_speech 前语音确认、旧 generation、重复 terminal、无效输入、原 PCM 不变、真实 RNN 静音、冻结参数一致与 WAV/哈希验证。测试通过不等于全部声学目标通过。

## 首跑实测：预冻结门槛未全部通过

报告：`runtime/goal-baselines/20261104/voice-endpoint-first/report.json`。root 使用 release 可执行文件跑完全部 14 行，退出码 1；`required_targets_pass=false`，method SHA 与运行前冻结值一致。

| 实测分组 | 结果 |
| --- | --- |
| 14 行状态/PCM 不变量 | 全通过：原输入不变、终态不重复，记录完整。 |
| 4 个单句 + 2 个 2s 停顿 | 6 行均形成 `speech_end`，终点后没有任何源语音；距离最后非零源样本边界约 2345–2410ms，低于冻结目标 2500–3500ms 的下界，因此这 6 行目标均为 false。 |
| 2 个 4s 思考停顿 | 两行都在后续源语音之前提前结束，`future_source_speech_after_terminal=true`；保留为未解决的截断风险，目标字段为 null，不计成功。 |
| 2 个连续讲话 | 都在 20000ms 形成 `duration_limit`，目标通过；上限截断后仍有源语音是构造预期，不是完整发言结束。 |
| 静音、低能噪声、低能 100Hz 声 | 都在 10000ms 形成 `no_speech`，3 行目标通过。 |
| 显式设备错误 | 2000ms 形成一次 `device_error`，目标通过。 |
| 处理预算 | 14 行全部通过；最高单行 p99 为 28µs，所有行最大单帧耗时 129.6µs，没有帧达到或超过 10ms。 |

单句与 2s 停顿的 6 行属于**延迟下界目标未达**，不是实际源语音被截断：终点已晚于各行完整源支持区间。这也暴露评测边界的局限——最后非零尾部样本不等于最后一个语音帧，不能仅用此边界断言系统提前截掉了说话。后续若评价真正的语音结束延迟，需要独立人工标注集，并在运行前定义方法；本次不改 method、音频、阈值或下界，不将门槛调整到通过。

上述耗时只来自本机成人合成音频的离线运行，没有麦克风、扬声器、儿童、真实噪声或并发负载验证，也不包含 ASR、回复生成和播放时间。生产自动轮换验收仍为 false。

成人合成语音不能代表儿童、轻声、口音、真实教室或扬声器回声。声学静音也无法区分“想好了”与“还在想”：4s 思考暂停超过当前规则，是明确未解决的产品边界。没有因此启用自动轮换、AEC、持续麦克风或自动发送。

上游依据：[Sonora 0.2.0 VAD wrapper](https://docs.rs/sonora-agc2/0.2.0/sonora_agc2/vad_wrapper/struct.VoiceActivityDetectorWrapper.html)、[Sonora 源码](https://github.com/dignifiedquire/sonora)。实现时已核对本机锁定包源码；没有重新下载或替换算法。
