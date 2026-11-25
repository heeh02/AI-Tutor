# Rust 本地语音实测

第十一检查点补充（2026-11-04）：新主程序SHA256 `cb62e4ebbcc7a7000efd89d00aac7057d25aa2d2c57b8e9b87b5b9256545d20a` 通过 `runtime/goal-baselines/20261104/input-capture-reopen-first/report.json` 的真实Windows打开/重开检查。隔离数据库、ASR/TTS/远程模型关闭，唯一稳定麦克风两次Start，各约320/336ms后完整票据Discard；第二轮旧Discard400且当前采集不变。Start RTT 81.50/36.16ms，Discard RTT 17.45/18.19ms，两次最终idle。仅说明请求及owner状态，不等于物理关闭延迟或声学有效性。未保存/检查音频、未转写/播放、未开摄像头/PTZ；进程exit0、forced=false，临时descriptor/策略已清理，安装和既有服务哈希未变。自动语句终点和半双工完整循环的声学验收仍待执行。

本片自动化workspace共696通过、0失败、5忽略，无警告；新的原生与WASM构建通过。以下保留较早探针的独立实测范围，不能与本次窄打开测试合并成全链路指标。

2026-11-01 02:07:48（Asia/Shanghai）。测试由 `rust/ai-tutor/src/bin/audio-probe.rs` 在本机实际执行，证据为 `runtime/rust_audio_probe.json`。

生产语音链使用 Rust CPAL 0.16／WASAPI 采集、whisper-rs 0.16／whisper.cpp 本地推理、Windows SAPI 的 Rust COM 接口播报。无需 Python、PowerShell、浏览器云语音或远程 ASR。多语言 base 权重来源、固定摘要见 `models/whisper-rust-manifest.json`。

以下自动探测结果保留为当时记录。后续成人基本按键语音的用户确认单独列出，不改写探测中的静音、合成样本或未确认听感字段。

| 实测项 | 结果 | 证据 |
|---|---|---|
| 真实麦克风发现和采集 | PASS | OBSBOT Tiny 3 Microphone，48kHz、设备原始双声道，处理第一声道 |
| 真实回调数据 | PASS | 采集95040个单声道样本，1980ms，启动22ms |
| 静音处理 | PASS | 上述实际样本RMS为0，返回重录提示，未产生文字；没有把零回调当成静音 |
| 半双工 | PASS | 真实采集期间的播报请求被拒绝 |
| 本地英文播报流程 | PASS | SAPI报告固定英文句播放完成，3181ms；成人听感未确认 |
| 本地中文播报打断流程 | PASS | 固定长中文句被取消，停止请求返回48ms；这是单次观察值，不是时限保证 |
| 过期播报拒绝 | PASS | 旧语音代次请求未进入播报 |
| 测试结束状态 | PASS | recording=false、state=idle，原始采集数据未存盘 |

本机检测到惠惠（简体中文）、Zira（英文）和David（英文）三种已安装SAPI声音。

为了测试多语言模型，而不把无人说话的麦克风录音伪装为语音质量验收，另用原生Rust COM将两段固定文字直接合成为临时PCM16／16kHz WAV。该过程不通过扬声器播放；随后直接运行本地whisper-rs，删除临时WAV并清零测试样本。

| 合成样本 | 结果 | 本地推理耗时 |
|---|---|---|
| 中文：电机、测距、主控与停车距离 | PASS，匹配“电机／测距／停车”；“墙”转写为同音字“强”，仍需人工校正 | 958ms |
| 英文：小车在墙前停车并亮绿灯 | PASS，完整识别固定英文句 | 872ms |

模型加载109ms。两个临时WAV均确认删除，没有保留麦克风录音或环境转写。合成样本是模型／本地库冒烟测试，不代替成人自然说话、室内噪声、声学回声或儿童语音测试。测试代码没有远程语音调用；没有声称另行做过禁网设备测试。

## 成人基本按键语音用户确认

日期：2026-11-01（Asia/Shanghai）；证据ID：`audio-user-confirmation-20261101-01`；`source_type=user_confirmation`。

用户在本轮基本成人按键语音试用询问后反馈：“转写与播报都正常”。据此将按住说话→松开后检查或编辑转写→发送→听到教学回复的基本流程，以及可听播报，记为PASS。这是用户确认，未新增设备采样、录音、延迟测量或自动观测。

用户未单独标注所测语言，不能据此确认英语自然语音。G01中英文全项验收仍为NOT_RUN；播报中按键打断、声学回声拒绝、播报中切档案与旧音频清理也仍为NOT_RUN。原始 `native_probe` 中的 `natural_speech_verified=false` 与 `heard_by_adult=null` 保留，表示当时自动探测的证据范围。

取消修复及自动回归覆盖：

- 录音Start／Stop／Discard携带代次，旧请求不会消费新录音；采集者独立轮询原子代次，控制队列满时仍会释放旧输入和清零缓冲。
- 转写在等待采集结果前取得代次，返回前再次校验；取消等待不会提前释放正在运行的单个ASR槽位。
- 播报替换先停止旧声音，再返回旧任务取消；选定声音后、真正Speak前重新校验代次。
- 采集／ASR状态优先于过期播报状态；关闭标志不依赖排队的Quit命令。
- 有界控制队列为8项，采样容量为20秒，ASR取消／超时回调在本机执行。

已运行6项音频Rust单元测试，全部通过：采样长度、20秒硬容量、队列满时取消清理、旧Start拒绝、被取消等待者的ASR槽位所有权，以及状态优先级。

复现本次命令：

```powershell
cargo test --locked -p ai-tutor --lib audio::tests
cargo build --locked -p ai-tutor --bin audio-probe
.\target\debug\audio-probe.exe "C:\Users\RBMS\Desktop\AI TUTOR"
```

仍为NOT_RUN：英语自然语音实测、播报时按键打断与声学回声检查、播报中切档案及旧音频清理、真实麦克风USB拔插，以及G01完整中英文验收。基本成人按键语音与可听播报已由上述独立用户确认记为PASS；固定合成样本与用户基本流程确认均不扩大为其他场景通过。

## Windows 输入端点只读复查

02:19:06（Asia/Shanghai）额外执行原生 `audio-endpoint-probe`，没有启动采集、播放声音或修改任何系统设置，未中断运行中的服务。结果保存在 `runtime/rust_audio_endpoint_probe.json`。

OBSBOT Tiny 3是Windows当前唯一的活动输入端点，也是控制台、多媒体和通信的默认输入。`IAudioEndpointVolume` 可读取：静音为false，主音量标尺1.0（Windows界面100%）、0.0dB，通道0标尺1.0，硬件支持位标记原值3。

因此，先前实际采样中的全零数据不能用“Windows端点被静音或输入音量为零”来解释。这项查询只反映Windows端点状态，不能证明设备内部麦克风、厂商DSP、物理静音状态或房间内是否存在可识别的人声；查询当时自然语音实测尚未完成。后续基本流程用户确认见上文独立记录，不回写此探测结果。

复现：`cargo run --locked -p ai-tutor --bin audio-endpoint-probe -- "C:\Users\RBMS\Desktop\AI TUTOR"`。辅助程序只调用Get／枚举类音频接口，没有SetMute或SetVolume操作。

## 2026-11-04：进程内稳定输入绑定基础（无设备回归通过）

本片保留 CPAL 0.16，新增 `audio/input_devices.rs`。明确选择的 `device_key` 是本次进程生成的 opaque UUID，指向采集 owner 保留的 CPAL Device 对象。列表重排只改变展示用 `device_id`，不会改选同名设备；重新枚举发现同一端点时保留原对象。CPAL 0.16 WASAPI 公开的 `PartialEq` 内部按 `IMMDevice::GetId` 比较，不使用私有字段、内存布局转换或设备名称判等。进程重启后必须重新获取绑定；它不是可跨进程持久保存的 Windows endpoint ID。

接口保持兼容：

- `GET /api/audio/devices` 仍返回数组和原 `device_id/name/sample_rate/input_channels/hostapi`，新增 `device_key/input_topology_revision/stable_binding_available`。只有显式请求列表才由现 capture owner 初始化通知与枚举元数据，不创建输入流。HTTP 等待放在 blocking task，owner 队列有界，等待最长 3 秒；原同步 probe 调用保留。
- `audio` 状态新增 `input_topology_revision/stable_binding_available`。启动与读取状态都不枚举设备、不打开麦克风；尚未成功注册通知时 available 为 false，列表的 key 为 null，旧手动选择仍可用。
- `POST /api/audio/record/start` 可选 `device_key` 与 `expected_input_topology_revision`，必须同时提供，且不能混用旧 `device_id`。现 `session_id/expected_generation` 绑定不变。校验在取消教学前执行，owner 在实际开启前、开启后以及服务端发布回执前再次校验；失败不会发布成功绑定。稳定启动回执额外包含 `device_key/input_topology_revision`。
- 不提供稳定字段时保留既有手动 index/默认选择行为。这条兼容路径不具备稳定绑定保证，后续自动轮换必须显式使用稳定字段。

Windows `IMMNotificationClient` 回调仅原子递增 topology revision，不加锁、不枚举、不读属性、不停止设备。默认设备通知过滤为 `eCapture/eAll`，纯输出默认切换不使录音失效。add/remove/state/property 通知没有 flow 参数；为避免回调 I/O，本片保守失效，因此纯输出设备的这些事件也可能要求重新选择输入。这是明确的可用性边界，不能把它描述成只监听麦克风。采集 owner 的现 20ms 泵看到旧 revision 后停止并清零该绑定录音，精确 CAS 废弃它的 recording generation；不修改后来新录音的 generation。状态为 `recording_end_reason=input_topology_changed`，不伪装成 no_speech；已有输入故障路径标记 `device_error`。

录音已经停止、但识别仍在运行时，绑定 revision 也随捕获结果传递。流式返回、Whisper abort/final 和最终公开结果再次检查绑定；过期结果不返回文稿。旧代次的晚结果不覆盖新录音的错误状态。未做 VAD/自动 Send/轮换许可接线；仅按 root 要求导出另一代理的独立 `voice_endpoint` 模块。

新增 10 项无设备回归，已由 root 在 `runtime/goal-baselines/20261104/workspace-twenty-seventh.log` 中统一执行，全部通过：

- `retained_device_key_survives_reorder_and_never_matches_by_name`
- `removed_and_readded_device_never_revives_an_old_key`
- `topology_change_invalidates_even_a_quick_unplug_replug_between_lists`
- `status_and_unknown_binding_do_not_initialize_or_enumerate_devices`
- `pure_output_default_notification_does_not_invalidate_input_binding`
- `topology_change_discards_bound_audio_and_never_advances_a_newer_generation`
- `topology_change_preserves_legacy_manual_capture_and_current_bound_capture`
- `topology_change_after_capture_invalidates_final_without_poisoning_new_capture`
- `unknown_bound_start_is_rejected_by_owner_without_enumerating_devices`
- `invalid_stable_device_binding_never_cancels_teaching_or_reserves_audio`

这些单元测试使用假设备句柄、纯内存回调对象与现 fake owner，不执行 CPAL 枚举/设备打开或模型调用。后续 root 已单独完成真实主程序的只枚举验收，证据见下文；USB 拔插、有效打开、同设备重开、端点通知实际送达、自然语音和音频时延仍未验收，不能以单元测试或枚举结果宣称它们已通过。备份：`runtime/goal-backups/20261104/backend_review/stable-input/`。直接声明既有锁定版本 `windows-core =0.61.2` 供 COM implement 宏使用，不升级 CPAL 或 Windows 包。

依据：[CPAL 0.16 WASAPI Device 源码](https://github.com/RustAudio/cpal/blob/v0.16.0/src/host/wasapi/device.rs)、[微软 IMMNotificationClient 回调限制](https://learn.microsoft.com/en-us/windows/win32/api/mmdeviceapi/nn-mmdeviceapi-immnotificationclient)、[微软端点 ID 的 opaque 语义](https://learn.microsoft.com/en-us/windows/win32/coreaudio/endpoint-id-strings)。上文旧日期的设备/成人确认不替代本片未执行的稳定绑定验收。

统一验证记录：`runtime/goal-baselines/20261104/workspace-twenty-sixth.log` 首次编译发现 CPAL 类型公共路径错误（`DeviceInner` 未从 crate 根重导出），已按锁定源码改为 `cpal::platform::DeviceInner`。该次失败日志保留，不覆盖为成功。修正后的 `runtime/goal-baselines/20261104/workspace-twenty-seventh.log` 全 workspace **650 项通过、0 失败、5 忽略，无警告**；上列新增 10 项全部为实际 PASS。root 随后使用构建完成的 native16 主程序执行下述验收，主程序 SHA256 为 `672c4f566542cad8cc162a5261fd9f915a57fc37416dd820437f6a7645bf1dfa`。忽略项和未执行的真实硬件验收不因本次通过而升级。

真实主程序只枚举验收：root 执行 `scripts/validation/input_device_binding_smoke.py`，证据 `runtime/goal-baselines/20261104/input-device-binding-first/report.json`，`passed=true`。新 fixture 禁用 ASR/TTS、隔离 provider HOME/PATH，没有模型或真实数据库。启动时 `stable_binding_available=false`；显式查询后为 true。两次均发现 **1 个输入设备**，topology revision 均为 **2**，key 集合 SHA256 均为 `11a44c5fd7bfcb285ec35f2867864e59ddb6efe58b893635054b21621c2675b8`。known-key/stale-revision 与 unknown-key/current-revision 两次 Start 均返回 **HTTP 400**；教学 generation 保持 0、录音 generation 保持 1、Kimi requests/running 保持 0，录音始终 false、音频 idle。未发送有效 Start，未打开采集 stream，也未请求播放、模型、摄像头或云台。自有主程序正常 exit 0、无强制结束，descriptor 由主程序清除，两个自有 disabled 策略文件也已精确删除，fixture/report 保留。

该脚本只保存数量、key 集合哈希、版本及状态，不保存设备名称或原始 key；空列表仍独立报告 `no_input_devices`（退出码 2），不冒充重开通过。另一次 `runtime/goal-baselines/20261104/release-smoke-tenth/report.json` 记录 root 的 5 种无效 binding 检查通过。以上证据只证明惰性初始化、同进程列表绑定稳定与无效请求无副作用；真实 **open/reopen/unplug 继续 NOT_RUN**，端点通知送达和自然语音质量亦未因本次枚举而获验证。

## 2026-11-04：显式语句结束检测接入 capture owner（待统一验证）

本片新增 `audio/capture_endpoint.rs` 及独立 fake owner 测试；`voice_endpoint.rs` 仅增加 owner 可调用的 `duration_limit(generation)`，原阈值、冻结语料与离线报告不变。默认 `EndpointMode::Manual` 继续现手动路径和 20 秒硬缓冲/关闭行为。只有显式 `voice_endpoint_v1` 才建立每代次独立的 VAD，且必须提供有效稳定设备绑定；缺失绑定在取消教学或占用采集之前拒绝。音频层本身不发送教学输入、不重开麦克风。

采集回调仍仅写原始 normalized mono PCM；VAD 在现 capture owner 泵运行。每次从完整缓冲按单调 16k cursor 复制一个完整 10ms 小帧，释放缓冲锁后分析，每个 owner tick 最多处理 4 帧。16k/44.1k/48k 输入使用整数相位的逐样本插值，不重复处理、不为缺少的样本补静音；ASR 保持自己的 cursor。VAD 不裁剪、替换或重写完整 PCM，最终 Stop 仍处理实际捕获的全部样本（原 20 秒上限内）。每帧分析前先处理源错误，再检查 wall 或样本容量到达 20 秒；硬上限优先于同 tick 的积压 speech_end 候选，音频分析时间不会伪填为 20 秒。

状态合同：`capture_pending_result` 表示已闭合但尚未取走的结果；`recording_endpoint` 是 null 或单次有界回执，字段为 `protocol_version:1/method_id/recording_generation/revision:1/reason/cause/input_closed/elapsed_audio_ms/speech_confirmed_at_ms/last_activity_at_ms/captured_samples/sample_rate/capture_closed_elapsed_ms`。reason 为 `speech_end/no_speech/duration_limit/device_error`。只有 owner 的输入 stream Drop 已返回后，复核 recording generation 和设备状态，才发布 `input_closed:true`；这表示本程序该 capture stream 已释放，不声称其他程序也未使用麦克风。候选的已分析音频时间与 owner 观察到的关闭耗时分别记录，不能把前者当作物理关闭时刻。

显式终点发布时 `recording=false/state=idle/capture_pending_result=true`，完整 PCM/Recording 留到既有 Stop 或 Discard；Start 必须在 cancel 前检查 pending（server 由 root 接线），音频层也拒绝覆盖。Stop 清 pending 并把同 endpoint 带到结果，终态可保留至下一代次；新代次/Discard 清除。源错误保留待处置缓冲，但取消 partial ASR；现失效转写校验仍拒绝错误音频。关闭期间发生取消不发布终点，发生拓扑/源错误只可发布 device_error；成功终态发布后再变设备则沿精确 token 撤销/Discard 路径清理，不改写为另一终态、不产生可自动发送的 speech_end。

服务端租约预留窄接口：`start_recording_with_endpoint_reserved(selection, mode, on_reserved: FnOnce(u64)->bool + Send)` 在分配 recording generation 后、ASR begin 和 capture 入队前调用。false 只用 CAS 撤销这一个 token；不能废弃后来新的 generation。现 `start_recording_with_endpoint` wrapper 默认回调 true，旧 `start_recording_selected` wrapper 默认 Manual。Start 回执包含 endpoint_mode；普通手动 Stop 不伪造 speech_end。

新增 15 项无设备回归（代码已 rustfmt，本记录尚未执行 Cargo）：3 项逐帧 cursor/回执时间/双硬 cap；11 项 fake input Drop、取消期间关闭、拓扑期间关闭、已发布终态撤销、完整 PCM 保留、源错误、手动 20 秒、pending/稳定绑定、预留回调拒绝、旧 Start timeout、公开 Stop 回执透传；以及 1 项 VoiceEndpoint 外部 duration_limit 的代次与一次性行为。测试入口为 `audio::capture_endpoint::tests`、`audio::capture_endpoint_tests` 和 `audio::voice_endpoint::tests::owner_duration_limit_keeps_audio_time_and_is_generation_bound_once`。备份在 `runtime/goal-backups/20261104/backend_review/capture-endpoint/`。

Start 超时窄修：原无条件增加 recording generation 改为 `compare_exchange(token, token+1)`；生产等待仍为 8 秒，旧超时不能废弃后来新 token 或清掉其 endpoint/pending。`late_start_timeout_cannot_invalidate_or_clear_a_newer_recording` 使用实际 oneshot 超时（测试注入 5ms）覆盖两种代次，不打开输入。`public_stop_response_preserves_owner_closed_speech_end_receipt` 使用假 owner 和安静 PCM 验证公开 Stop 响应保留完整同 token 的 `input_closed/speech_end` 回执；该样本 text 为空且有检查错误，不构成可自动提交的证据。静态复核确认 streaming 与 Whisper 成功分支也透传同捕获回执，服务端会话 stamp 不删除它；此检查不声称已运行真实非空识别。

现固定 quiet 阈值为 2.5 秒；4 秒思考停顿会被声学结束规则提前截断的已知局限仍保留，未通过改阈值或改冻结预期掩盖。本片未运行设备、模型、真实采音或新延迟测量，不证明儿童/自然中英表达、房间回声或完整自动轮换已通过；后续启用须按独立轮换合同验收。
