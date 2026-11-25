# 可选半双工语音轮换：合同与实现记录

第十一检查点：已落地独立的后端 `voice_loop.rs`、前端轮换状态与现有 App 接线，以及 capture owner 的显式 `voice_endpoint_v1`。默认手动路径和生产配置保持原样。以下早期草案保留为设计背景，现行接口增加 `expected_input_topology_revision`，所有许可回执包含设备 key/revision、完整会话/语言/loop 关联、phase/revision/rearm_ready。最终workspace 696通过、0失败、5忽略；原生/WASM构建和15个新增HTTP拒绝路径通过。两轮真实Store/合成播放回执测试与两次真实麦克风短时打开/丢弃分别通过，**尚未将真实模型、输入和输出连成完整自动循环验收**。

- 后端只在当前未编辑终稿、同录音代次和 `speech_end + input_closed` 下消费一次自动发送资格。真实的文字流、逐段播放记录、输出 owner 状态和音频 epoch 都满足后，连续等待 500ms，再给重开许可；Start 仍再次核对。后续任何忙态会撤销这一尾间隔。
- 3 秒内存许可每秒续约；服务端检查器每 100ms 检查到期，先同步撤销精确录音/播放代次，再等待控制锁关闭持久播放状态。续约不等待可能较慢的麦克风 Start。上述数值是软件配置，尚非实际设备停止延迟。
- 一个许可可以包含多轮，因而旧请求拒绝不能按整个许可停掉下一轮。失败清理仅在该次操作实际被接受后建立；录音/回复身份的检查与暂停状态变更在同一锁内完成。播报或模型失败进入暂停，不靠 heartbeat 无限续约卡在等待。
- capture owner 在实际释放输入 stream 后才发布终态，完整 PCM 留到 Stop/Discard；未处理录音不能被新 Start 覆盖。20 秒硬上限优先于排队的 VAD 候选。旧 Start 超时只撤销自己的代次。
- UI 只以本次明确成功的 Start/Send 回执推进；编辑（包括改回空）、隐藏、浏览历史、切换、停止、设备变化或未知发送都暂停。后到 state、旧 heartbeat、重新勾选朗读不会恢复。时长上限终稿仅留作人工审阅。

独立模块与回归详情见 `HALF_DUPLEX_UI_20261104.md`、`VOICE_ENDPOINT_20261104.md`。冻结 VAD 的 4 秒思考停顿仍可能截断，500ms 不代表房间余响实测消失；当前暂不发布或默认启用。

## 完整接口两轮集成验证（2026-11-04）

新增 `voice_loop_integration_tests.rs` 两项定向测试已实际通过：`runtime/goal-baselines/20261104/voice-loop-integration-first.log` 为 **2 passed、0 failed、0 ignored，exit 0**；测试运行 2.39 秒，含编译的根侧执行总耗时 26.703 秒。复现命令在项目 `rust` 目录执行：

```text
cargo test -p ai-tutor --lib voice_loop_integration_tests -- --nocapture
```

- `dispatch_two_voice_rounds_are_exclusive_and_submit_each_bound_final_once`：经真实 `dispatch` 依次执行 enable、Start/reserved、Stop、turns、逐段显示 ACK、heartbeat 和下一次 Start。两轮各产生一份用户发言、一次识别 final 和一次教学请求，共四段按顺序播放；采音与输出 owner 的活动状态互斥。重复 Start、Stop、Send 和上一轮迟到 Discard 不增加发言或采集。文字 final、完整显示、单段完成都不足以重开，仍须整条播放收据完成、executor 退出及真实连续 500ms 尾间隔。
- `dispatch_loop_stop_after_reserved_token_prevents_late_capture_open`：Start 已取得并登记精确录音代次，但假输入 owner 尚未执行打开时，经真实 loop Stop 撤销许可；迟到 Start 回执失败，打开次数保持零，没有用户发言或教学请求。两个测试均不直接修改 Lease 的 phase、generation、revision、deadline，也不改生产 debounce 时钟。

这些测试使用仅 `cfg(test)` 可用的 I/O 接点。输入是内存中的合成 PCM 和假输入 stream；假 owner 明确提供 `speech_end` 候选，随后调用生产 `close_endpoint_input`，先执行 stream Drop，再由原 `record_closed` 生成代次绑定的 `input_closed` 回执。Stop 继续经过真实 `finish_input`、AudioService、四字段 ticket 校验，以及识别结束后的当前会话/语言/代次复核。识别文本由独立 loopback 假 ASR 服务经认证和 instance 绑定协议返回，实际客户端的分块、final 和版本校验仍执行；它不调用已运行的识别服务。Kimi 仅替换进程 I/O 为有界脚本 transport，保留请求注册、并发 gate、取消、`SegmentDecoder` 和完整回复校验，后续 Store/显示/播放调度均使用生产路径。假输出 owner 返回明确标为非扬声器实测的 Started/Completed 收据。

因此本次补齐的是**完整后端接口与状态闭环**，不是浏览器、真实模型、真实扬声器或声学自动轮换验收。VAD 分类本身、真实麦克风声音是否说完、转写准确率、Kimi 进程隔离与真实模型结果、声卡排空/余响、自触发和连续现场对话均未在这两项中测量。现有无效终点/错误或过长 final、旧 token/epoch 及 ASR 协议反例继续承担对应负例覆盖；不能以此次正向两轮测试替代它们。生产 API、设备许可规则和默认手动配置未放宽，统一 workspace 验证由根代理另行执行。

## 早期设计草案（保留背景）

以下状态描述对应最初设计时点，现行实现与验证以本文前两部分为准。

状态：2026-11-04 只读设计，未实现、未运行设备；当前手动录音 UI 保持冻结。数值属于待验证的初始配置，不是已通过的现场指标。

第十检查点补充：稳定设备绑定已实现，真实 Windows 仅枚举验证通过（1个输入设备、两次列表同key/revision、两类无效Start拒绝且状态不变），未开麦/重开/拔插。独立Sonora VAD与四类端点候选已实现并离线测试，尚未接capture：普通及2秒停顿保留完整样本，但6项未达冻结延迟区间下界，4秒思考停顿仍提前结束。详见 `RUST_AUDIO_REPORT.md` 和 `VOICE_ENDPOINT_20261104.md`。本文的完整循环许可、heartbeat、自动Send、播报后重开和UI仍为待实现合同，不因这些基础测试通过而启用。

## 产品范围

默认仍为“检查转写后发送”。用户在当前活动明确点击“开始语音对话 · 使用所选麦克风”后，才进入完整循环：**听取 → 终稿 → 自动发送一次 → 显示并逐段播报 → 播放结束 → 再次听取**。选择模式、设备枚举、语音服务 active 或页面恢复均不是打开麦克风的授权。

第一片只支持当前已配置的 `segments + after_display` 路径，不暗改生产配置。条件不具备时保持手动入口并说明原因。此模式自己的自动发送授权与既有“下次发送自动朗读”分开；关闭自动朗读也暂停轮换。不会新增默认长期记忆、模拟结果确认或实验完成行为。

半双工期间播放与采集互斥。Esc/现有停止键可立即打断并暂停，再由用户点击“继续语音对话”重开；第一片不实现声音触发打断或自动恢复。松手自动发送只是单次输入行为，不能作为本功能完成证据。

## 已有依据与实际缺口

| 接线位置 | 可复用的事实 / 仍缺的能力 |
| --- | --- |
| `rust/tutor-ui/src/recording_draft.rs` | 四字段 ticket、draft revision、owner revision、独立未采用转写/发送待核对槽已存在。用户编辑再改回空也不能自动发送旧终稿。 |
| `rust/tutor-ui/src/app.rs`：RecordStart/RecordStarted/Transcript/Done/PageVisibility | 已有精确迟到 start discard、expected_generation、Send 单在途、未知结果保留与隐藏撤销；下一片只接小型 `voice_loop` 状态模块，不另造录音或提交链。 |
| `rust/ai-tutor/src/recording.rs` | start 在 control 内校验后单次 cancel/bump；stop 在 ASR await 后重核完整绑定。语言原值 `zh/zh-CN/en/en-US` 必须原样保留。 |
| `rust/ai-tutor/src/audio.rs`：capture owner | 现20秒上限只 `take()` 输入stream，仍等Stop收取音频；没有语句终点回执。已新增可选device_key+topology_revision与保留Device的精确路径，旧手动index路径兼容；自动模式必须使用稳定路径，且还需实际重开与拔插验证。 |
| `scripts/speech-service/recognizer.py` | 当前 `enable_endpoint_detection=False`。字幕停止变化、低 RMS、ASR 请求结束都不等于已确认说完。 |
| `rust/ai-tutor/src/segment_speech.rs`、`store/tutor_speech.rs` | 可取得绑定 request/generation 的逐段 owner Completed、`speech_complete`。该字段本身不是重新开麦许可：还须核对 gate、完整文字提交及 worker/其他输出 owner 已退出。 |
| `audio/speech_output.rs`、`audio/pcm_output.rs` | PCM 完成依据是回调已提交且预计设备队列已排空；`speaker_output_measured=false`。SAPI 使用自身完成通知。都不是房间余响消失的实测证据。 |

当前手动路径没有因此被宣称存在自动轮换缺陷；上述缺口必须在启用新模式前补齐。

## 最小状态机

单一当前循环，不存通用任务队列。UI 状态为：

| 状态 | 唯一向前条件 |
| --- | --- |
| `off` | 显式点击开始；当前活动可写、当前实时窗可见、输入为空、无两类待处理文本、无提交/录音/转写/其他播放在途。 |
| `arming` | 服务端确认当前页面的循环许可及所选设备；随后才发既有 record/start。start 未回时保留关闭意图，迟到 ticket 仅精确 discard。 |
| `listening` | 当前 recording_generation 的 `speech_end` 端点已关闭输入 stream，发一次完整 ticket Stop。 |
| `finalizing` | 完整 ticket 的有效非空 final；本轮开始为空且用户 draft revision 未变、owner/语言/许可仍当前，才进入发送。其他结果进入手动核对或暂停。 |
| `submitting` | 固定 `client_submission_id`、expected_generation 和循环关联，只发一个 turns 请求。任何错误/超时都暂停并保留“发送结果待核对”，不重发。 |
| `reply` | 同 request/generation 的文字显示确认与逐段播放可以交错；此阶段麦克风始终关闭。文字 final 或某一段 Completed 不能进入下一轮。 |
| `cooldown` | 服务端确认整条回复和全部播报完成、全部输出 owner 空闲，经过尾部间隔，再原子校验并允许新的 record/start。 |
| `paused` | 自动动作已撤销；只允许明确“继续”建立新的许可。回到前台、勾回朗读、读到新 state 均不能恢复旧许可。 |

“继续”是新的显式授权，必须重新检查设备、当前活动和两类待处理文本，不清空用户内容。暂停不撤回已显示内容，也不把未完成回复补记成完成。

## 最小后端合同（待实施确认）

后端只保留一个内存中的当前循环许可；不替 UI 自动提交发言，不跨重启恢复许可，不新增数据库中的常开麦偏好。由现 control 锁统一验证，复用录音/播放 owner。

1. `POST /api/sessions/{sid}/voice-loop/enable`
   - body：`{client_loop_id, expected_generation, language, device_key}`。`client_loop_id` 是本次点击生成的唯一页面 nonce，仅关联，不替代现有认证。
   - server 绑定 namespace、learner_id/epoch、sid、语言原值、所选稳定设备身份与递增 `loop_epoch`。已有其他页面许可时拒绝，不能静默接管。
   - 返回 `{client_loop_id, loop_epoch, session_id, session_generation, language, phase:"armed", lease_ms}`。本端点不打开设备，只有随后的显式绑定 record/start 才打开。enable 回执丢失不会留下采集。
2. `POST /api/sessions/{sid}/voice-loop/{client_loop_id}/heartbeat`，body `{loop_epoch}`。
   - 只在可见当前实时窗且本地许可有效时续约；初始建议每 1 秒、3 秒到期。返回单一有界快照 `{loop_epoch, revision, phase, session_generation, rearm_ready, pause_reason}`；不是新的事件总线。
   - 到期先撤销许可，再精确关闭所属 capture/auto speech；无 UI 时不发送终稿、不打开下一次麦克风。timer 是否满足该边界需用假时钟与现场测试验证。
3. `POST /api/sessions/{sid}/voice-loop/{client_loop_id}/stop`，body `{loop_epoch, reason}`。
   - 精确幂等撤销该循环；旧请求不能停止新循环。UI 先本地不可逆暂停，再发送 stop；网络失败由到期兜底，不等待请求才停止本地流程。
   - enable 未回时 UI 只记关闭意图，回执后按 epoch stop；由于 enable 自身不开麦，无需宽泛 session cancel。
4. 现 record/start 与 turns 增加可选 `voice_loop:{client_loop_id,loop_epoch}`；自动 turns 另携原 `recording_generation`，与该循环最近一次有效终稿关联。现完整 Stop/discard ticket 及 expected_generation 保持不变，旧手动客户端不受影响。
   - 后端许可只跟随自己已接受的 start/turns 推进教学 generation；语言变更、切档、结束活动、外部手动动作等独立突变撤销许可。不能把所有 generation 增加都视为许可延续，也不能把自身 start 提前到达的 state 误判为另一录音。
   - 每次重开原子检查同一上一条 request/generation：`phase=completed`、`final_validated=true`、`speech_complete=true`、gate 仍允许、segment executor 已退出、无手动/SAPI/PCM 输出 owner、audio epoch 仍匹配、尾部间隔已过。`rearm_ready` 是提示，record/start 必须再检查，拒绝不能自动重试。
   - 最后一段播完但 final 失败、文本事务尚未完成、播放被打断或 gate 关闭，均不得重开。音频模型 active/ready 与这些条件无关。

设备 `device_key` 必须来自 Windows 输入 endpoint 的稳定身份或同等可靠的本进程设备绑定；显示名称只供选择。若本片只能提供枚举 index，则自动轮换保持 unavailable，不能用同名设备或默认设备替代所授权麦克风。拔插/默认设备变更/身份无法核实均暂停并重新选择。

## 必要端点与播放事件

在现 `/api/audio/status` 增加一个有界 terminal：`recording_endpoint:{recording_generation,revision,reason,input_closed:true}`，保留至 token 改变。reason 为 `speech_end | no_speech | duration_limit | device_error`。先真正关闭输入，再公布终点；同 token 的重复轮询只触发一次 Stop/discard。

- `speech_end`：由明确的 VAD/endpoint 判定产生；完整 final 与全部许可条件满足才自动发送。
- `no_speech`：精确 discard 并暂停，不反复自动开麦。
- `duration_limit`：仍以现 20 秒为上限；停止后取得的非空文本留待手动核对，不把截断句自动发出。
- `device_error`：暂停，不把缓存噪声当成用户发言；能安全取回的文字仅供核对。

VAD 应在 capture 音频链按 recording_generation 绑定；ASR fallback、临时字幕或语言模型不能伪造端点。后续选择具体算法时再冻结最短语音、结束静音和长停顿参数；中英、慢速表达与思考停顿需要分开验证，不按年龄推断能力。

播放无需新增浏览器“音频结束”猜测：利用现逐段 receipt，在后端形成重开许可。PCM 必须有 Completed 和 predicted_output_drained；SAPI 必须有完成通知；均需 owner 空闲。初始可测试 500ms 的可配置尾部间隔，但不能承诺它覆盖所有房间余响。AEC/声学打断仍独立待验，首次启用前要验证所选输出/输入组合不会把自身尾音发送成用户消息；未通过时保持手动模式。

## 暂停与文案

隐藏、历史浏览/离开实时跟随、切档/会话、教学语言变更、用户输入编辑（含改回空）、手动 Read/录音、关闭朗读或模式、设备变化、网络错误、未知发送结果、ASR/TTS/模型失败均撤销当前许可。旧 ticket/result/终态只可完成所属清理或保留同 owner 的待核对文本，不能驱动新轮次。服务端失联到期是浏览器关闭/冻结时的兜底。

| 中文 | English |
| --- | --- |
| 检查转写后发送（默认） | Review transcript before sending (default) |
| 开始语音对话 · 使用所选麦克风 | Start voice conversation · Use selected microphone |
| 说完后会自动发送；老师播报时不收音。按 Esc 暂停。 | Your turn is sent automatically when you finish. The microphone is off while the tutor speaks. Press Esc to pause. |
| 正在听 · 你可以开始说话 | Listening · You can speak now |
| 正在整理这句话 | Finishing your transcript |
| 老师正在回答 · 麦克风已关闭 | Tutor replying · Microphone off |
| 已暂停：你开始编辑了文字 | Paused: you started editing |
| 发送结果待核对；不会自动重发 | Check the send result; it will not be sent again automatically |
| 继续语音对话 / 回到手动发送 | Continue voice conversation / Return to manual sending |

正常阅读程度只调整解释长短；授权、自动发送、麦克风状态和暂停原因始终可见。所有转写/结果保留原文。

## 可验收的最小切片

先用 fake capture/speech owner 和假时钟验证完整两轮路径：两次授权内采集、两次唯一 Send、两次完整 speech receipts，且任何时刻 capture 与输出 owner 不重叠。再验证：最后段与 final 到达次序互换、worker 未释放、gate 已关、duration/no_speech、编辑后改空、隐藏/租约过期、语言/档案变化、设备拔插、旧 endpoint/start/final/receipt、Send 超时后恰有一份待核对且零次重发。暂停后所有重复/迟到事件产生零次新 Start。

现场测试须单独标注尚未执行：中英及混合表达、长思考停顿、键盘打断、麦克风授权与切换、扬声器尾音自触发、至少 10 轮正常循环。记录 endpoint 延迟、ASR final、提交、首段 owner started、最后 owner complete、下一次 capture opened 的时序；不要把预测排空写成实测扬声器结束或已验证 AEC。
